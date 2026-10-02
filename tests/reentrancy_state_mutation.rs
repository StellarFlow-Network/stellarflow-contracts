//! End-to-end dynamic reentrancy & state-mutation suite (issue #1000).
//!
//! The existing fuzz infrastructure under `tests/fuzz/` is a *pure-math*
//! property harness: it pulls `src/amm/invariant.rs` and `src/amm/slippage.rs`
//! in with `#[path = ...]` and never touches a `soroban_sdk::Env`, a contract
//! entrypoint, or storage. Nothing in the repository drives randomized, seeded
//! *cross-calling* sequences over the stateful entrypoints, and nothing asserts
//! the accounting / reentrancy invariants this issue names. This file closes
//! that gap with a deterministic, dependency-free integration suite that:
//!
//! 1. Drives a seeded xorshift64 sequence over the auto-compound vault
//!    (`vault_deposit` / `vault_withdraw` / `vault_harvest`) interleaved with
//!    the staking registry (`stake_and_register` / `unstake`), and re-checks the
//!    protocol invariants after *every* step:
//!      * vault reserves == the vault's real token balance (pool accounting),
//!      * vault share supply == the sum of every holder's share balance,
//!      * staking registry total == the sum of every node's stake,
//!      * token conservation across actors / vault / fee recipient.
//! 2. Proves the contract-wide reentrancy lock gates every guarded entrypoint
//!    and that a rejected re-entrant call mutates no state.
//! 3. Proves the RAII guard releases the lock even when a guarded call is
//!    rejected, so a failed call cannot brick the vault.
//! 4. Proves an unauthorized caller cannot move vault state (admin-only config,
//!    and withdrawals of shares the caller does not own).
//!
//! ## Why the reentrancy window is simulated by taking the lock
//!
//! In this pinned soroban-sdk 20.0.0 native test harness a nested
//! sub-invocation that returns `Err` escalates to a non-unwinding panic and
//! aborts the whole test process, so a *live* re-entrant call cannot be
//! observed from a test. Holding the lock directly reproduces the exact state a
//! re-entrant call would meet — which is what `ReentrancyGuard` keys off —
//! without tripping that harness limitation. This is the same technique the
//! in-crate test in `src/vaults/harvest_compound.rs` documents and uses; this
//! suite extends it across the vault and order-book entrypoint surface and adds
//! the "state must not move" assertions.

use soroban_sdk::{testutils::Address as _, token, Address, Env};

use stellarflow_contracts::{
    orders::limit::AssetPair, security::reentrancy, ContractError, TimeLockedUpgradeContract,
    TimeLockedUpgradeContractClient,
};

// ── Deterministic RNG ────────────────────────────────────────────────────────

/// Deterministic xorshift64 PRNG. Inlined rather than compiled in so the suite
/// stays dependency-free (this repo does not carry a `rand` dependency and the
/// fuzz subcrate is excluded from the workspace).
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // xorshift64 requires a non-zero state.
        Self(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Value in `[0, bound)`. `bound` must be greater than zero.
    fn below(&mut self, bound: u64) -> u64 {
        self.next_u64() % bound
    }
}

// ── Fixture ──────────────────────────────────────────────────────────────────

const ACTORS: usize = 4;
const INITIAL_MINT: i128 = 1_000_000;
const STEPS: u32 = 64;

/// Deploy the root contract, initialise it, install the auto-compound vault on
/// a fresh Stellar-asset test token and return everything the invariants need.
fn setup_vault() -> (
    Env,
    TimeLockedUpgradeContractClient<'static>,
    Address, // contract id
    Address, // vault asset
    Address, // fee recipient
    Address, // vault admin
) {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury);

    let asset_issuer = Address::generate(&env);
    let asset = env.register_stellar_asset_contract(asset_issuer);
    let fee_recipient = Address::generate(&env);
    client.init_vault(&admin, &asset, &fee_recipient);

    (env, client, contract_id, asset, fee_recipient, admin)
}

fn mint(env: &Env, asset: &Address, to: &Address, amount: i128) {
    token::StellarAssetClient::new(env, asset).mint(to, &amount);
}

/// `true` while the contract-wide reentrancy lock is held.
fn lock_is_held(env: &Env, contract_id: &Address) -> bool {
    env.as_contract(contract_id, || reentrancy::is_locked(env))
}

/// Flatten the mutable vault configuration into cheap-to-compare scalars so a
/// guarded or unauthorized call can be shown to have left it untouched.
fn vault_config_fields(
    client: &TimeLockedUpgradeContractClient<'_>,
) -> (Address, Address, u32, Address) {
    let config = client
        .vault_config()
        .expect("the vault was initialised by the fixture");
    (
        config.admin,
        config.asset,
        config.fee_bps,
        config.fee_recipient,
    )
}

// ── Invariants ───────────────────────────────────────────────────────────────

/// Re-check every invariant the issue names, against authoritative sources
/// (the token contract and the contract's own getters) rather than any value
/// this test tracks internally.
fn assert_protocol_invariants(
    env: &Env,
    client: &TimeLockedUpgradeContractClient<'_>,
    asset: &Address,
    contract_id: &Address,
    fee_recipient: &Address,
    actors: &[Address; ACTORS],
) {
    let token_client = token::Client::new(env, asset);

    // Vault: reserves match pool accounting.
    let total_assets = client.vault_total_assets();
    assert_eq!(
        total_assets,
        token_client.balance(contract_id),
        "vault reserves drifted from the tracked asset ledger"
    );

    // Vault: total share supply equals the sum of every holder's balance.
    let total_shares = client.vault_total_shares();
    let mut holder_sum: i128 = 0;
    for actor in actors.iter() {
        holder_sum += client.vault_share_balance(actor);
    }
    assert_eq!(
        holder_sum, total_shares,
        "vault share supply != sum of holder balances"
    );

    // Staking registry: total equals the sum of every node's stake.
    let mut stake_sum: u64 = 0;
    for actor in actors.iter() {
        stake_sum += client.get_stake(actor);
    }
    assert_eq!(
        stake_sum,
        client.get_total_staked(),
        "stake registry total != sum of per-node stakes"
    );

    // No negative accounting can be represented or observed.
    assert!(total_assets >= 0, "vault assets went negative");
    assert!(total_shares >= 0, "vault share supply went negative");

    // Token conservation: deposits / withdrawals / harvests only move tokens
    // between the actors, the vault and the fee recipient.
    let mut actor_tokens: i128 = 0;
    for actor in actors.iter() {
        actor_tokens += token_client.balance(actor);
    }
    let conserved =
        actor_tokens + token_client.balance(contract_id) + token_client.balance(fee_recipient);
    assert_eq!(
        conserved,
        INITIAL_MINT * ACTORS as i128,
        "tokens were created or destroyed by the vault"
    );
}

// ── Seeded cross-calling sequence ────────────────────────────────────────────

/// Drive one deterministic cross-calling sequence and assert the invariants
/// after every step — including steps that the contract rejects, which must
/// leave the accounting untouched.
fn run_cross_calling_sequence(seed: u64) {
    let (env, client, contract_id, asset, fee_recipient, _admin) = setup_vault();
    let token_client = token::Client::new(&env, &asset);

    let actors: [Address; ACTORS] = [
        Address::generate(&env),
        Address::generate(&env),
        Address::generate(&env),
        Address::generate(&env),
    ];
    for actor in actors.iter() {
        mint(&env, &asset, actor, INITIAL_MINT);
    }

    let mut rng = Rng::new(seed);
    let mut executed: u32 = 0;

    for _ in 0..STEPS {
        let actor = actors[rng.below(ACTORS as u64) as usize].clone();

        match rng.below(4) {
            // Vault deposit.
            0 => {
                let available = token_client.balance(&actor);
                if available > 0 {
                    let amount = 1 + rng.below(available as u64) as i128;
                    if let Ok(Ok(minted)) = client.try_vault_deposit(&actor, &amount) {
                        assert!(minted > 0, "an accepted deposit must mint shares");
                        executed += 1;
                    }
                }
            }

            // Vault withdrawal, bounded by the caller's own share balance so the
            // outbound token transfer can always settle.
            1 => {
                let held = client.vault_share_balance(&actor);
                if held > 0 {
                    let shares = 1 + rng.below(held as u64) as i128;
                    if let Ok(Ok(owed)) = client.try_vault_withdraw(&actor, &shares) {
                        assert!(owed > 0, "an accepted withdrawal must pay out");
                        executed += 1;
                    }
                }
            }

            // Permissionless yield harvest, bounded by the keeper's balance.
            2 => {
                let available = token_client.balance(&actor);
                if available > 0 {
                    let amount = 1 + rng.below(available as u64) as i128;
                    if let Ok(Ok(harvest)) = client.try_vault_harvest(&actor, &amount) {
                        assert_eq!(harvest.gross_yield, amount);
                        assert_eq!(harvest.total_assets, client.vault_total_assets());
                        executed += 1;
                    }
                }
            }

            // Staking registry: stake when flat, otherwise unstake.
            _ => {
                if client.get_stake(&actor) == 0 {
                    let amount = 1 + rng.below(1_000);
                    if let Ok(Ok(record)) = client.try_stake_and_register(&actor, &amount) {
                        assert_eq!(record.amount, amount);
                        executed += 1;
                    }
                } else if let Ok(Ok(returned)) = client.try_unstake(&actor) {
                    assert!(returned > 0, "a successful unstake must return the stake");
                    executed += 1;
                }
            }
        }

        assert_protocol_invariants(&env, &client, &asset, &contract_id, &fee_recipient, &actors);
    }

    assert!(
        executed > 0,
        "seeded sequence (seed={seed}) exercised no state-changing entrypoint"
    );
}

#[test]
fn seeded_cross_calling_sequence_preserves_accounting_invariants() {
    for seed in [0x5160_0001_u64, 0x5160_0002, 0x5160_0003] {
        run_cross_calling_sequence(seed);
    }
}

// ── Reentrancy ───────────────────────────────────────────────────────────────

/// With the contract-wide lock held (the exact state a re-entrant call meets
/// mid-execution), every guarded entrypoint must be rejected with
/// `ReentrancyDetected` and must not move any state.
#[test]
fn reentrancy_lock_blocks_every_guarded_entrypoint_and_leaves_state_untouched() {
    let (env, client, contract_id, asset, fee_recipient, _admin) = setup_vault();
    let token_client = token::Client::new(&env, &asset);

    let holder = Address::generate(&env);
    mint(&env, &asset, &holder, 20_000);
    let minted = client.vault_deposit(&holder, &10_000);
    assert_eq!(minted, 10_000);

    // Snapshot everything a guarded call could touch.
    let assets_before = client.vault_total_assets();
    let shares_before = client.vault_total_shares();
    let holder_shares_before = client.vault_share_balance(&holder);
    let (admin_before, asset_before, fee_bps_before, recipient_before) =
        vault_config_fields(&client);
    let vault_balance_before = token_client.balance(&contract_id);
    let fee_balance_before = token_client.balance(&fee_recipient);

    // Enter the mid-execution window: take the lock exactly as
    // `ReentrancyGuard::new` does.
    env.as_contract(&contract_id, || {
        reentrancy::lock(&env).expect("lock should start free");
        assert!(reentrancy::is_locked(&env));
    });

    let caller = Address::generate(&env);
    let pair = AssetPair {
        sell_asset: asset.clone(),
        buy_asset: Address::generate(&env),
    };

    // Vault entrypoints guarded by `ReentrancyGuard`.
    assert!(matches!(
        client.try_vault_deposit(&holder, &100),
        Err(Ok(ContractError::ReentrancyDetected))
    ));
    assert!(matches!(
        client.try_vault_withdraw(&holder, &1),
        Err(Ok(ContractError::ReentrancyDetected))
    ));
    assert!(matches!(
        client.try_vault_harvest(&holder, &100),
        Err(Ok(ContractError::ReentrancyDetected))
    ));
    assert!(matches!(
        client.try_vault_flash_loan(&holder, &100),
        Err(Ok(ContractError::ReentrancyDetected))
    ));

    // Order-book entrypoints share the same contract-wide lock.
    assert!(matches!(
        client.try_place_limit_order(&caller, &pair, &1, &100),
        Err(Ok(ContractError::ReentrancyDetected))
    ));
    assert!(matches!(
        client.try_place_buy_limit_order(&caller, &pair, &1, &100),
        Err(Ok(ContractError::ReentrancyDetected))
    ));
    assert!(matches!(
        client.try_cancel_limit_order(&caller, &0),
        Err(Ok(ContractError::ReentrancyDetected))
    ));
    assert!(matches!(
        client.try_match_limit_orders(&0, &1, &100),
        Err(Ok(ContractError::ReentrancyDetected))
    ));
    assert!(matches!(
        client.try_match_market_order(&caller, &pair, &100, &true),
        Err(Ok(ContractError::ReentrancyDetected))
    ));
    assert!(matches!(
        client.try_withdraw_order_balance(&caller, &asset, &100),
        Err(Ok(ContractError::ReentrancyDetected))
    ));

    // No guarded path mutated anything.
    assert_eq!(client.vault_total_assets(), assets_before);
    assert_eq!(client.vault_total_shares(), shares_before);
    assert_eq!(client.vault_share_balance(&holder), holder_shares_before);
    assert_eq!(
        vault_config_fields(&client),
        (admin_before, asset_before, fee_bps_before, recipient_before)
    );
    assert_eq!(token_client.balance(&contract_id), vault_balance_before);
    assert_eq!(token_client.balance(&fee_recipient), fee_balance_before);

    // Release the simulated in-flight lock: the lock was the only reason those
    // calls were rejected, so the very same entrypoint now succeeds.
    env.as_contract(&contract_id, || {
        reentrancy::unlock(&env);
        assert!(!reentrancy::is_locked(&env));
    });
    let minted_after = client.vault_deposit(&holder, &100);
    assert_eq!(minted_after, 100);
    assert_eq!(client.vault_total_assets(), assets_before + 100);
}

/// A guarded call that is rejected *inside* the vault (rather than by the lock)
/// must still release the lock on the way out; otherwise a single failed call
/// would brick every guarded entrypoint until the next ledger.
#[test]
fn rejected_guarded_calls_release_the_lock() {
    let (env, client, contract_id, asset, _fee_recipient, _admin) = setup_vault();
    let token_client = token::Client::new(&env, &asset);

    let holder = Address::generate(&env);
    mint(&env, &asset, &holder, 20_000);
    client.vault_deposit(&holder, &10_000);
    let held = client.vault_share_balance(&holder);

    assert!(!lock_is_held(&env, &contract_id));

    // Rejected after the guard was acquired: more shares than the caller owns.
    let res = client.try_vault_withdraw(&holder, &(held + 1));
    assert!(matches!(
        res,
        Err(Ok(ContractError::VaultInsufficientShares))
    ));
    assert!(
        !lock_is_held(&env, &contract_id),
        "guard leaked after a rejected withdrawal"
    );

    // Rejected before any state change, but still after acquiring the guard.
    let res = client.try_vault_deposit(&holder, &0);
    assert!(matches!(res, Err(Ok(ContractError::VaultZeroAmount))));
    assert!(
        !lock_is_held(&env, &contract_id),
        "guard leaked after a rejected deposit"
    );

    // The vault is still usable, and the successful call also clears the lock.
    let minted = client.vault_deposit(&holder, &100);
    assert_eq!(minted, 100);
    assert!(!lock_is_held(&env, &contract_id));
}

// ── Unauthorized state mutation ──────────────────────────────────────────────

/// A caller that does not own the shares, or is not the vault admin, must not
/// be able to move vault state.
#[test]
fn unauthorized_callers_cannot_mutate_vault_state() {
    let (env, client, contract_id, asset, fee_recipient, admin) = setup_vault();
    let token_client = token::Client::new(&env, &asset);

    let holder = Address::generate(&env);
    mint(&env, &asset, &holder, 10_000);
    client.vault_deposit(&holder, &10_000);

    let attacker = Address::generate(&env);
    let assets_before = client.vault_total_assets();
    let shares_before = client.vault_total_shares();
    let holder_shares_before = client.vault_share_balance(&holder);
    let config_before = vault_config_fields(&client);

    // Vault configuration is admin-gated: a non-admin cannot raise the fee...
    let res = client.try_set_vault_performance_fee(&attacker, &500);
    assert!(matches!(res, Err(Ok(ContractError::NotAdmin))));

    // ...and even the admin cannot push it past the hard cap.
    let res = client.try_set_vault_performance_fee(&admin, &2_001);
    assert!(matches!(
        res,
        Err(Ok(ContractError::VaultInvalidPerformanceFee))
    ));

    // A caller holding no shares cannot burn someone else's position.
    let res = client.try_vault_withdraw(&attacker, &holder_shares_before);
    assert!(matches!(
        res,
        Err(Ok(ContractError::VaultInsufficientShares))
    ));

    // A zero-amount deposit cannot mint anything.
    let res = client.try_vault_deposit(&attacker, &0);
    assert!(matches!(res, Err(Ok(ContractError::VaultZeroAmount))));

    // No unauthorized call moved any state.
    assert_eq!(client.vault_total_assets(), assets_before);
    assert_eq!(client.vault_total_shares(), shares_before);
    assert_eq!(client.vault_share_balance(&holder), holder_shares_before);
    assert_eq!(client.vault_share_balance(&attacker), 0);
    assert_eq!(vault_config_fields(&client), config_before);
    assert_eq!(token_client.balance(&contract_id), assets_before);
    assert_eq!(token_client.balance(&fee_recipient), 0);
}
