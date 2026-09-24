//! Anti-frontrunning commit-reveal order scheme (Issue #761).
//!
//! Two-phase trading protocol that hides the exact terms of a high-value
//! trade until it is ready to be executed, making it unprofitable for MEV
//! bots to frontrun the submission.
//!
//! # Lifecycle
//!
//! 1. **Commit** — The trader submits `sha256(secret ‖ trade_details)` plus a
//!    `collateral_asset`/`collateral_amount` bond and an `expiration_sequence`
//!    deadline. Only the *hash* of the trade terms is stored on-chain, so the
//!    price, size and direction of the intended trade are invisible to
//!    observers.
//! 2. **Reveal** — In a *subsequent* ledger (before the deadline) the trader
//!    submits the raw `secret` and `trade_details`. The contract recomputes
//!    `sha256(secret ‖ trade_details)` and requires it to equal the committed
//!    hash. The validated order is then executed against the on-chain
//!    limit-order book at the committed `price_tick`, and the commitment bond
//!    is returned to the trader.
//! 3. **Forfeit** — If the reveal deadline passes without a valid reveal, the
//!    commitment bond is forfeited to the treasury as a penalty for occupying
//!    a committed, non-revealed slot.
//!
//! # Frontrunning resistance
//!
//! Because phase-1 only publishes a commitment hash, a bot observing the
//! mempool cannot learn the trade's price or size in time to sandwich it. The
//! reveal, which actually moves the price, only becomes public at execution
//! time and binds to the *committed* (previously hidden) terms — the revealed
//! `price_tick` must be exactly the value that produced the stored hash.

use soroban_sdk::{
    contracttype, symbol_short, token, xdr::ToXdr, Address, Bytes, BytesN, Env, Symbol,
};

use crate::events::{emit_simple2, EV_COMMIT_NEW, EV_COMMIT_REVEAL, EV_COMMIT_FORFEIT};
use crate::{
    orders::limit::{self, AssetPair},
    ContractError,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of active (unrevealed/unforfeited) commitments per trader
/// to bound storage usage.
const MAX_ACTIVE_COMMITMENTS: u32 = 64;

/// Minimum reveal window offset (in ledgers) from the committing ledger.
/// Prevents "instant-expiry" commitments that a trader could use to grief the
/// book with no real intent to trade.
const MIN_EXPIRATION_OFFSET: u32 = 10;

/// Maximum reveal window offset (in ledgers) — caps at ~180 days
/// (~180d * 86_400s/d / 5s ≈ 3.1M ledgers).
const MAX_EXPIRATION_OFFSET: u32 = 3_110_400;

// ---------------------------------------------------------------------------
// Storage keys
// ---------------------------------------------------------------------------

/// Global incrementing nonce for the next commitment id.
const COMMIT_NONCE_KEY: Symbol = symbol_short!("CMTNONC");

/// Persistent key for an individual commitment record.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitKey(pub u64);

/// Persistent key for the per-trader active commitment counter.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitCounterKey(pub Address);

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Lifecycle state of a single commitment.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitmentState {
    /// Committed; awaiting reveal or forfeit.
    Active,
    /// Revealed and executed against the order book.
    Revealed,
    /// Deadline passed without a reveal; bond forfeited.
    Forfeited,
}

/// A single anti-frontrunning commitment.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Commitment {
    /// Unique identifier.
    pub id: u64,
    /// The trader who committed and owns the bond.
    pub trader: Address,
    /// `sha256(secret ‖ trade_details)` — the only on-chain hint of the trade.
    pub commitment_hash: BytesN<32>,
    /// The bond asset locked with the commitment.
    pub collateral_asset: Address,
    /// The bond amount locked in stroops.
    pub collateral_amount: i128,
    /// Ledger sequence by which the trader must reveal.
    pub expiration_sequence: u32,
    /// Ledger sequence at which the trade must be priced for execution.
    pub committed_at_sequence: u32,
    /// Current state.
    pub state: CommitmentState,
}

/// The trade terms revealed in phase 2 and executed at reveal time.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct RevealedTrade {
    /// The pair to trade on.
    pub pair: AssetPair,
    /// Execution price in `buy_asset` per unit of `sell_asset`, fixed-point
    /// at the order book's `PRICE_SCALE`.
    pub price_tick: i128,
    /// Base quantity of `sell_asset` to trade.
    pub amount: i128,
    /// Whether this is a buy-side order (locks `buy_asset` spend) or a
    /// sell-side order (locks `sell_asset` collateral).
    pub is_buy: bool,
}

/// Result returned on a successful reveal.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct RevealResult {
    pub commitment_id: u64,
    pub order_id: u64,
    pub trader: Address,
    /// The commitment bond returned to the trader now that the trade was
    /// revealed on time.
    pub bond_returned: i128,
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn next_commitment_id(env: &Env) -> Result<u64, ContractError> {
    let next: u64 = env
        .storage()
        .instance()
        .get(&COMMIT_NONCE_KEY)
        .unwrap_or(0u64);
    let id = next.checked_add(1).ok_or(ContractError::Overflow)?;
    env.storage().instance().set(&COMMIT_NONCE_KEY, &id);
    Ok(id)
}

fn load_commitment(env: &Env, id: u64) -> Result<Commitment, ContractError> {
    env.storage()
        .persistent()
        .get(&CommitKey(id))
        .ok_or(ContractError::CommitmentNotFound)
}

fn save_commitment(env: &Env, commitment: &Commitment) {
    let key = CommitKey(commitment.id);
    env.storage().persistent().set(&key, commitment);
    env.storage()
        .persistent()
        .extend_ttl(
            &key,
            crate::storage::PERSISTENT_TTL_THRESHOLD,
            crate::storage::PERSISTENT_TTL_THRESHOLD,
        );
}

fn increment_counter(env: &Env, trader: &Address) -> Result<(), ContractError> {
    let key = CommitCounterKey(trader.clone());
    let count: u32 = env.storage().persistent().get(&key).unwrap_or(0u32);
    if count >= MAX_ACTIVE_COMMITMENTS {
        return Err(ContractError::TooManyActiveCommitments);
    }
    env.storage().persistent().set(&key, &(count + 1));
    Ok(())
}

fn decrement_counter(env: &Env, trader: &Address) {
    let key = CommitCounterKey(trader.clone());
    let count: u32 = env.storage().persistent().get(&key).unwrap_or(0u32);
    if count > 0 {
        env.storage().persistent().set(&key, &(count - 1));
    }
}

/// Compute `sha256(secret ‖ trade_details)` using tightly packed XDR bytes so
/// the reveal reproduces exactly what was committed.
fn compute_commitment(
    env: &Env,
    secret: &Bytes,
    pair: &AssetPair,
    price_tick: i128,
    amount: i128,
    is_buy: bool,
) -> BytesN<32> {
    let mut buf = Bytes::new(env);
    buf.append(secret);
    buf.append(&pair.sell_asset.clone().to_xdr(env));
    buf.append(&pair.buy_asset.clone().to_xdr(env));
    buf.append(&Bytes::from_array(env, &price_tick.to_be_bytes()));
    buf.append(&Bytes::from_array(env, &amount.to_be_bytes()));
    let side = if is_buy { 1u8 } else { 0u8 };
    buf.append(&Bytes::from_array(env, &side.to_be_bytes()));
    env.crypto().sha256(&buf)
}

// ---------------------------------------------------------------------------
// Phase 1: Commit
// ---------------------------------------------------------------------------

/// Commit to a hidden trade and lock `collateral_amount` of `collateral_asset`
/// as a forfeitable bond until `expiration_sequence`.
///
/// `commitment_hash` must equal `sha256(secret ‖ trade_details)` for the
/// details the trader intends to reveal in phase 2. Only the hash is stored —
/// the trade terms stay private until reveal.
pub fn commit(
    env: &Env,
    trader: Address,
    commitment_hash: BytesN<32>,
    collateral_asset: Address,
    collateral_amount: i128,
    expiration_sequence: u32,
) -> Result<Commitment, ContractError> {
    if collateral_amount <= 0 {
        return Err(ContractError::OrderZeroAmount);
    }
    trader.require_auth();

    let current_seq = env.ledger().sequence();
    if expiration_sequence <= current_seq + MIN_EXPIRATION_OFFSET {
        return Err(ContractError::CommitmentWindowTooShort);
    }
    if expiration_sequence > current_seq + MAX_EXPIRATION_OFFSET {
        return Err(ContractError::CommitmentWindowTooLong);
    }

    // Lock the bond into the contract.
    let token_client = token::Client::new(env, &collateral_asset);
    token_client.transfer(&trader, &env.current_contract_address(), &collateral_amount);

    let id = next_commitment_id(env)?;
    increment_counter(env, &trader)?;

    let commitment = Commitment {
        id,
        trader: trader.clone(),
        commitment_hash,
        collateral_asset: collateral_asset.clone(),
        collateral_amount,
        expiration_sequence,
        committed_at_sequence: current_seq,
        state: CommitmentState::Active,
    };
    save_commitment(env, &commitment);

    let _ = emit_simple2(
        env,
        EV_COMMIT_NEW,
        symbol_short!("commit"),
        (id, trader, collateral_asset, collateral_amount),
    );

    Ok(commitment)
}

// ---------------------------------------------------------------------------
// Phase 2: Reveal
// ---------------------------------------------------------------------------

/// Reveal the trade terms and execute them against the on-chain order book,
/// binding to the committed (hidden) `price_tick`.
///
/// # Frontrunning note
///
/// The revealed `price_tick` here must be exactly the value that produced the
/// stored hash (otherwise the commitment is invalidated), so a bot cannot
/// substitute a modified price. On success the order is placed through the
/// order book and the commitment bond is returned to the trader.
pub fn reveal(
    env: &Env,
    commitment_id: u64,
    trader: Address,
    secret: Bytes,
    pair: AssetPair,
    price_tick: i128,
    amount: i128,
    is_buy: bool,
) -> Result<RevealResult, ContractError> {
    trader.require_auth();

    let mut commitment = load_commitment(env, commitment_id)?;
    if commitment.trader != trader {
        return Err(ContractError::Unauthorized);
    }
    if commitment.state != CommitmentState::Active {
        return Err(ContractError::CommitmentNotActive);
    }
    let current_seq = env.ledger().sequence();
    if current_seq > commitment.expiration_sequence {
        // Deadline passed — the correct action is forfeit, not reveal.
        return Err(ContractError::CommitmentExpired);
    }
    if current_seq == commitment.committed_at_sequence {
        // Phase 2 must land in a *subsequent* ledger to separate the reveal
        // ledger from the commit ledger (prevents same-ledger self-sandwich).
        return Err(ContractError::CommitmentNotRevealWindow);
    }
    if amount <= 0 {
        return Err(ContractError::OrderZeroAmount);
    }
    if price_tick <= 0 {
        return Err(ContractError::OrderInvalidPrice);
    }

    // Verify the revealed terms reproduce the committed hash — the binding
    // commitment check.
    let computed = compute_commitment(env, &secret, &pair, price_tick, amount, is_buy);
    if computed != commitment.commitment_hash {
        return Err(ContractError::CommitmentHashMismatch);
    }

    // Execute against the order book. The order book pulls the order
    // collateral directly from the trader.
    let order = if is_buy {
        limit::place_buy_order(env, trader.clone(), pair, price_tick, amount)?
    } else {
        limit::place_order(env, trader.clone(), pair, price_tick, amount)?
    };

    // The commitment was honored: return the bond to the trader.
    let bond_returned = commitment.collateral_amount;
    if bond_returned > 0 {
        let token_client = token::Client::new(env, &commitment.collateral_asset);
        token_client.transfer(&env.current_contract_address(), &trader, &bond_returned);
    }

    // Mark the commitment settled.
    commitment.state = CommitmentState::Revealed;
    save_commitment(env, &commitment);
    decrement_counter(env, &trader);

    let _ = emit_simple2(
        env,
        EV_COMMIT_REVEAL,
        symbol_short!("commit"),
        (commitment_id, order.id, trader.clone(), price_tick),
    );

    Ok(RevealResult {
        commitment_id,
        order_id: order.id,
        trader,
        bond_returned,
    })
}

// ---------------------------------------------------------------------------
// Forfeit
// ---------------------------------------------------------------------------

/// Forfeit a commitment's bond to the treasury when the reveal deadline
/// passed without a valid reveal.
///
/// Callable by anyone (keeper) after `expiration_sequence`; the bond is
/// transferred to the configured treasury (or kept by the contract if no
/// treasury is set), deterring traders from committing without intent to
/// reveal.
pub fn forfeit(env: &Env, commitment_id: u64) -> Result<u64, ContractError> {
    let mut commitment = load_commitment(env, commitment_id)?;
    if commitment.state != CommitmentState::Active {
        return Err(ContractError::CommitmentNotActive);
    }
    if env.ledger().sequence() <= commitment.expiration_sequence {
        return Err(ContractError::CommitmentNotExpired);
    }

    let amount = commitment.collateral_amount;

    // Route forfeited bond to the treasury if configured.
    if let Some(treasury) = env.storage().instance().get::<_, Address>(&crate::TREASURY_KEY) {
        let token_client = token::Client::new(env, &commitment.collateral_asset);
        token_client.transfer(&env.current_contract_address(), &treasury, &amount);
    }

    commitment.state = CommitmentState::Forfeited;
    save_commitment(env, &commitment);
    decrement_counter(env, &commitment.trader);

    let _ = emit_simple2(
        env,
        EV_COMMIT_FORFEIT,
        symbol_short!("commit"),
        (commitment_id, commitment.trader.clone(), amount),
    );

    Ok(amount as u64)
}

// ---------------------------------------------------------------------------
// Query helpers
// ---------------------------------------------------------------------------

/// Load a commitment by ID.
pub fn get_commitment(env: &Env, commitment_id: u64) -> Result<Commitment, ContractError> {
    load_commitment(env, commitment_id)
}

/// Number of active (unrevealed/unforfeited) commitments for a trader.
pub fn active_commitment_count(env: &Env, trader: &Address) -> u32 {
    env.storage()
        .persistent()
        .get(&CommitCounterKey(trader.clone()))
        .unwrap_or(0u32)
}

/// Whether a commitment is still within its reveal window.
pub fn is_revealable(env: &Env, commitment: &Commitment) -> bool {
    commitment.state == CommitmentState::Active
        && env.ledger().sequence() > commitment.committed_at_sequence
        && env.ledger().sequence() <= commitment.expiration_sequence
}

/// Whether a commitment has expired without a reveal and may be forfeited.
pub fn is_forfeitable(env: &Env, commitment: &Commitment) -> bool {
    commitment.state == CommitmentState::Active && env.ledger().sequence() > commitment.expiration_sequence
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger, LedgerInfo};

    /// Set up an env where the module's storage operations run inside a real
    /// contract invocation (registered + initialized client), matching the
    /// rest of the order-book test suite. This is required because token ops
    /// leave the test env without an active contract context, so bare
    /// `env.storage().instance()` calls fail afterward.
    fn setup(
    ) -> (Env, crate::TimeLockedUpgradeContractClient<'static>, Address, Address, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let trader = Address::generate(&env);
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let sell_issuer = Address::generate(&env);
        let buy_issuer = Address::generate(&env);
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let client = crate::TimeLockedUpgradeContractClient::new(&env, &contract_id);
        // Configure the treasury on the contract instance in a real invocation.
        client.initialize(&admin, &treasury);
        let sell_asset = env.register_stellar_asset_contract(sell_issuer);
        let buy_asset = env.register_stellar_asset_contract(buy_issuer);
        // Generous balances so tests can commit bonds AND fund order collateral.
        mint(&env, &sell_asset, &trader, 100_000);
        mint(&env, &buy_asset, &trader, 100_000);
        env.ledger().set(LedgerInfo {
            timestamp: 1_000_000,
            protocol_version: env.ledger().protocol_version(),
            sequence_number: 100,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 0,
            min_persistent_entry_ttl: 0,
            max_entry_ttl: u32::MAX,
        });
        (env, client, trader, treasury, sell_asset, buy_asset)
    }

    fn mint(env: &Env, asset: &Address, to: &Address, amount: i128) {
        soroban_sdk::token::StellarAssetClient::new(env, asset).mint(to, &amount);
    }

    fn make_commitment(
        env: &Env,
        secret: &[u8],
        pair: &AssetPair,
        price_tick: i128,
        amount: i128,
        is_buy: bool,
    ) -> (Bytes, BytesN<32>) {
        let secret_b = Bytes::from_slice(env, secret);
        let hash = compute_commitment(env, &secret_b, pair, price_tick, amount, is_buy);
        (secret_b, hash)
    }

    #[test]
    fn commit_locks_collateral_and_stores_only_hash() {
        let (env, client, trader, _, sell_asset, buy_asset) = setup();
        let pair = AssetPair { sell_asset: sell_asset.clone(), buy_asset: buy_asset.clone() };
        let (_, hash) = make_commitment(&env, b"secret", &pair, limit::PRICE_SCALE, 1_000, false);

        let commitment = client.commit_order(&trader, &hash, &sell_asset, &1_000, &200);
        assert_eq!(commitment.id, 1);
        assert_eq!(commitment.state, CommitmentState::Active);
        assert_eq!(commitment.expiration_sequence, 200);

        let sell_client = token::Client::new(&env, &sell_asset);
        assert_eq!(sell_client.balance(&trader), 99_000);
        let contract_balance = sell_client.balance(&env.current_contract_address());
        assert!(contract_balance >= 1_000);
    }

    #[test]
    fn commit_rejects_window_too_short() {
        let (env, client, trader, _, sell_asset, _) = setup();
        let pair = AssetPair { sell_asset: sell_asset.clone(), buy_asset: sell_asset.clone() };
        let (_, hash) = make_commitment(&env, b"s", &pair, limit::PRICE_SCALE, 100, false);
        let result = client.try_commit_order(&trader, &hash, &sell_asset, &100, &(100 + MIN_EXPIRATION_OFFSET - 1));
        assert_eq!(result, Err(Ok(ContractError::CommitmentWindowTooShort)));
    }

    #[test]
    fn commit_rejects_zero_collateral() {
        let (env, client, trader, _, sell_asset, _) = setup();
        let pair = AssetPair { sell_asset: sell_asset.clone(), buy_asset: sell_asset.clone() };
        let (_, hash) = make_commitment(&env, b"s", &pair, limit::PRICE_SCALE, 100, false);
        let result = client.try_commit_order(&trader, &hash, &sell_asset, &0, &200);
        assert_eq!(result, Err(Ok(ContractError::OrderZeroAmount)));
    }

    #[test]
    fn reveal_executes_sell_order_at_committed_price_and_returns_bond() {
        let (env, client, trader, _, sell_asset, buy_asset) = setup();
        let pair = AssetPair { sell_asset: sell_asset.clone(), buy_asset: buy_asset.clone() };
        let price = 2 * limit::PRICE_SCALE;
        // Enough for bond (1000) + order collateral (5000).
        let (secret, hash) = make_commitment(&env, b"topsecret", &pair, price, 5_000, false);
        let c = client.commit_order(&trader, &hash, &sell_asset, &1_000, &200);

        env.ledger().set(LedgerInfo { sequence_number: 150, ..env.ledger().get() });

        let result = client.reveal_order(&c.id, &trader, &secret, &pair, &price, &5_000, &false);
        assert_eq!(result.commitment_id, c.id);
        assert!(result.order_id >= 1);
        assert_eq!(result.bond_returned, 1_000);

        let stored = client.get_commitment(&c.id);
        assert_eq!(stored.state, CommitmentState::Revealed);
        let order = client.get_limit_order(&result.order_id).unwrap();
        assert_eq!(order.price_tick, price);
        assert_eq!(order.remaining_amount, 5_000);
        assert_eq!(client.active_commitment_count(&trader), 0);

        // Bond returned: trader spent 1000 bond + 5000 order collateral = 6000.
        let sell_client = token::Client::new(&env, &sell_asset);
        assert_eq!(sell_client.balance(&trader), 100_000 - 5_000);
    }

    #[test]
    fn reveal_rejects_hash_mismatch() {
        let (env, client, trader, _, sell_asset, buy_asset) = setup();
        let pair = AssetPair { sell_asset: sell_asset.clone(), buy_asset: buy_asset.clone() };
        let (secret, hash) = make_commitment(&env, b"correct", &pair, limit::PRICE_SCALE, 1_000, false);
        let c = client.commit_order(&trader, &hash, &sell_asset, &1_000, &200);

        env.ledger().set(LedgerInfo { sequence_number: 150, ..env.ledger().get() });

        let result = client.try_reveal_order(&c.id, &trader, &secret, &pair, &(999 * limit::PRICE_SCALE), &1_000, &false);
        assert_eq!(result, Err(Ok(ContractError::CommitmentHashMismatch)));
    }

    #[test]
    fn reveal_rejects_same_ledger_commit_and_reveal() {
        let (env, client, trader, _, sell_asset, buy_asset) = setup();
        let pair = AssetPair { sell_asset: sell_asset.clone(), buy_asset: buy_asset.clone() };
        let (secret, hash) = make_commitment(&env, b"s", &pair, limit::PRICE_SCALE, 1_000, false);
        let c = client.commit_order(&trader, &hash, &sell_asset, &1_000, &200);

        let result = client.try_reveal_order(&c.id, &trader, &secret, &pair, &limit::PRICE_SCALE, &1_000, &false);
        assert_eq!(result, Err(Ok(ContractError::CommitmentNotRevealWindow)));
    }

    #[test]
    fn reveal_rejects_after_expiration() {
        let (env, client, trader, _, sell_asset, buy_asset) = setup();
        let pair = AssetPair { sell_asset: sell_asset.clone(), buy_asset: buy_asset.clone() };
        let (secret, hash) = make_commitment(&env, b"s", &pair, limit::PRICE_SCALE, 1_000, false);
        let c = client.commit_order(&trader, &hash, &sell_asset, &1_000, &200);

        env.ledger().set(LedgerInfo { sequence_number: 201, ..env.ledger().get() });
        let result = client.try_reveal_order(&c.id, &trader, &secret, &pair, &limit::PRICE_SCALE, &1_000, &false);
        assert_eq!(result, Err(Ok(ContractError::CommitmentExpired)));
    }

    #[test]
    fn forfeit_transfers_bond_to_treasury_after_deadline() {
        let (env, client, trader, treasury, sell_asset, _) = setup();
        let pair = AssetPair { sell_asset: sell_asset.clone(), buy_asset: sell_asset.clone() };
        let (_, hash) = make_commitment(&env, b"s", &pair, limit::PRICE_SCALE, 1_000, false);
        let c = client.commit_order(&trader, &hash, &sell_asset, &500, &200);

        env.ledger().set(LedgerInfo { sequence_number: 201, ..env.ledger().get() });
        let amount = client.forfeit_order(&c.id);
        assert_eq!(amount, 500);

        let stored = client.get_commitment(&c.id);
        assert_eq!(stored.state, CommitmentState::Forfeited);
        let sell_client = token::Client::new(&env, &sell_asset);
        assert_eq!(sell_client.balance(&treasury), 500);
        assert_eq!(client.active_commitment_count(&trader), 0);
    }

    #[test]
    fn forfeit_rejects_before_deadline() {
        let (env, client, trader, _, sell_asset, _) = setup();
        let pair = AssetPair { sell_asset: sell_asset.clone(), buy_asset: sell_asset.clone() };
        let (_, hash) = make_commitment(&env, b"s", &pair, limit::PRICE_SCALE, 1_000, false);
        let c = client.commit_order(&trader, &hash, &sell_asset, &500, &200);

        let result = client.try_forfeit_order(&c.id);
        assert_eq!(result, Err(Ok(ContractError::CommitmentNotExpired)));
    }

    #[test]
    fn non_trader_cannot_reveal() {
        let (env, client, trader, _, sell_asset, buy_asset) = setup();
        let pair = AssetPair { sell_asset: sell_asset.clone(), buy_asset: buy_asset.clone() };
        let (secret, hash) = make_commitment(&env, b"s", &pair, limit::PRICE_SCALE, 1_000, false);
        let c = client.commit_order(&trader, &hash, &sell_asset, &1_000, &200);

        env.ledger().set(LedgerInfo { sequence_number: 150, ..env.ledger().get() });
        let attacker = Address::generate(&env);
        let result = client.try_reveal_order(&c.id, &attacker, &secret, &pair, &limit::PRICE_SCALE, &1_000, &false);
        assert_eq!(result, Err(Ok(ContractError::Unauthorized)));
    }

    #[test]
    fn is_revealable_flags() {
        let (env, client, trader, _, sell_asset, buy_asset) = setup();
        let pair = AssetPair { sell_asset: sell_asset.clone(), buy_asset: buy_asset.clone() };
        let (_, hash) = make_commitment(&env, b"s", &pair, limit::PRICE_SCALE, 1_000, false);
        let c = client.commit_order(&trader, &hash, &sell_asset, &1_000, &200);
        let stored = client.get_commitment(&c.id);

        env.ledger().set(LedgerInfo { sequence_number: 150, ..env.ledger().get() });
        assert!(is_revealable(&env, &stored));
        assert!(!is_forfeitable(&env, &stored));

        env.ledger().set(LedgerInfo { sequence_number: 201, ..env.ledger().get() });
        assert!(!is_revealable(&env, &stored));
        assert!(is_forfeitable(&env, &stored));
    }
}
