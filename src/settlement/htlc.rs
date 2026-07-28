//! Hash Time-Locked Contract (HTLC) settlement module.
//!
//! Enables trustless cross-border settlement by locking funds behind a
//! SHA-256 hash lock. The beneficiary may claim by presenting the pre-image
//! before a ledger-sequence deadline; the depositor may refund after the
//! deadline expires.
//!
//! # Lifecycle
//!
//! 1. **Deposit** — The depositor locks `amount` with a `hash_lock` and
//!    `deadline_sequence` (ledger height). An [`Htlc`] record is stored.
//! 2. **Claim** — The beneficiary provides the SHA-256 `pre_image`. If
//!    `sha256(pre_image) == hash_lock` and the deadline has not passed,
//!    funds are released.
//! 3. **Refund** — After `deadline_sequence` the depositor may reclaim the
//!    full amount.
//!
//! # Atomicity
//!
//! Both `claim` and `refund` are single-transaction operations. Soroban's
//! transactional atomicity guarantees that partial state mutation is
//! impossible: if the pre-image is invalid or the deadline has not expired,
//! the transaction aborts and all state is reverted.

use soroban_sdk::{contracttype, symbol_short, Address, Bytes, BytesN, Env, Symbol};

use crate::events::{emit_simple2, EV_HTLC_NEW, EV_HTLC_CLAIM, EV_HTLC_REFUND};
use crate::{AssetId, ContractError};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of active HTLCs per depositor to bound storage.
const MAX_ACTIVE_HTLCS: u32 = 64;

/// Minimum deadline offset (in ledger sequences) from the current ledger
/// when creating an HTLC. Prevents immediate-expiry HTLCs that could be
/// used for griefing.
const MIN_DEADLINE_OFFSET: u32 = 10;

/// Maximum deadline offset (in ledger sequences) — caps at ~1 year of
/// ledgers (~365 days at 5s per ledger ≈ 6.3M sequences).
const MAX_DEADLINE_OFFSET: u32 = 6_307_200;

// ---------------------------------------------------------------------------
// Storage keys
// ---------------------------------------------------------------------------

/// Persistent key for an individual HTLC record.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HtlcKey(pub u64);

/// Persistent key for the per-depositor HTLC counter.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HtlcCounterKey(pub Address);

/// Persistent key for the global HTLC nonce (next ID).
const HTLC_NONCE_KEY: Symbol = symbol_short!("HTLCNON");

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Settlement state of an HTLC.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HtlcState {
    /// Funds are locked; awaiting claim or refund.
    Active,
    /// Beneficiary claimed with valid pre-image.
    Claimed,
    /// Depositor refunded after deadline.
    Refunded,
}

/// A single HTLC record.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Htlc {
    /// Unique identifier for this HTLC.
    pub id: u64,
    /// The address that deposited the funds.
    pub depositor: Address,
    /// The address that may claim by presenting the pre-image.
    pub beneficiary: Address,
    /// SHA-256 hash of the secret pre-image (32 bytes).
    pub hash_lock: BytesN<32>,
    /// Ledger sequence height after which the depositor may refund.
    pub deadline_sequence: u32,
    /// Asset identifier being locked (for multi-asset corridors).
    pub asset: AssetId,
    /// Amount locked in stroops.
    pub amount: u64,
    /// Current settlement state.
    pub state: HtlcState,
}

/// Result returned after a successful claim.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ClaimResult {
    pub htlc_id: u64,
    pub amount: u64,
    pub beneficiary: Address,
}

/// Result returned after a successful refund.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct RefundResult {
    pub htlc_id: u64,
    pub amount: u64,
    pub depositor: Address,
}

// ---------------------------------------------------------------------------
// HTLC creation
// ---------------------------------------------------------------------------

/// Create a new HTLC, locking `amount` behind `hash_lock` until
/// `deadline_sequence`.
///
/// # Arguments
/// * `env` - Soroban environment.
/// * `depositor` - The address funding the HTLC. Must authorize the call.
/// * `beneficiary` - The address eligible to claim.
/// * `hash_lock` - SHA-256 hash of the secret pre-image.
/// * `deadline_sequence` - Ledger sequence after which refund is permitted.
/// * `asset` - Asset identifier being locked.
/// * `amount` - Amount in stroops to lock.
///
/// # Errors
/// * [`ContractError::DeadlineTooSoon`] if the deadline is too close.
/// * [`ContractError::DeadlineTooFar`] if the deadline is excessively far.
/// * [`ContractError::ZeroSwapAmount`] if amount is zero.
/// * [`ContractError::Overflow`] if the HTLC counter overflows.
pub fn create_htlc(
    env: &Env,
    depositor: Address,
    beneficiary: Address,
    hash_lock: BytesN<32>,
    deadline_sequence: u32,
    asset: AssetId,
    amount: u64,
) -> Result<Htlc, ContractError> {
    depositor.require_auth();

    if amount == 0 {
        return Err(ContractError::ZeroSwapAmount);
    }

    // Validate deadline bounds.
    let current_seq = env.ledger().sequence();
    if deadline_sequence <= current_seq + MIN_DEADLINE_OFFSET {
        return Err(ContractError::DeadlineTooSoon);
    }
    if deadline_sequence > current_seq + MAX_DEADLINE_OFFSET {
        return Err(ContractError::DeadlineTooFar);
    }

    // Allocate a unique ID.
    let next_id: u64 = env
        .storage()
        .instance()
        .get(&HTLC_NONCE_KEY)
        .unwrap_or(0u64);
    let htlc_id = next_id
        .checked_add(1)
        .ok_or(ContractError::Overflow)?;
    env.storage().instance().set(&HTLC_NONCE_KEY, &htlc_id);

    let htlc = Htlc {
        id: htlc_id,
        depositor: depositor.clone(),
        beneficiary,
        hash_lock,
        deadline_sequence,
        asset,
        amount,
        state: HtlcState::Active,
    };

    // Persist the HTLC record.
    let key = HtlcKey(htlc_id);
    env.storage().persistent().set(&key, &htlc);

    // Track per-depositor active count to prevent storage abuse.
    let counter_key = HtlcCounterKey(depositor.clone());
    let count: u32 = env
        .storage()
        .persistent()
        .get(&counter_key)
        .unwrap_or(0u32);
    if count >= MAX_ACTIVE_HTLCS {
        return Err(ContractError::TooManyActiveHtlcs);
    }
    env.storage()
        .persistent()
        .set(&counter_key, &(count + 1));

    // Emit creation event.
    let _ = emit_simple2(
        &env,
        EV_HTLC_NEW,
        symbol_short!("htlc"),
        (htlc_id, depositor, htlc.beneficiary.clone(), amount),
    );

    Ok(htlc)
}

// ---------------------------------------------------------------------------
// Claim (pre-image verification)
// ---------------------------------------------------------------------------

/// Claim an active HTLC by presenting the secret pre-image.
///
/// Verifies that `sha256(pre_image) == hash_lock` and that the deadline has
/// **not** yet passed. On success the HTLC state transitions to `Claimed`
/// and the full amount is released to the beneficiary.
///
/// # Arguments
/// * `env` - Soroban environment.
/// * `htlc_id` - The HTLC to claim.
/// * `pre_image` - The raw secret pre-image bytes.
///
/// # Errors
/// * [`ContractError::HtlcNotFound`] if no HTLC exists with this ID.
/// * [`ContractError::HtlcNotActive`] if the HTLC has already been settled.
/// * [`ContractError::InvalidPreImage`] if the SHA-256 does not match.
/// * [`ContractError::DeadlineNotReached`] if the deadline has not yet passed.
/// * [`ContractError::Unauthorized`] if the caller is not the beneficiary.
pub fn claim(
    env: &Env,
    htlc_id: u64,
    pre_image: Bytes,
    caller: Address,
) -> Result<ClaimResult, ContractError> {
    caller.require_auth();

    let key = HtlcKey(htlc_id);
    let mut htlc: Htlc = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(ContractError::HtlcNotFound)?;

    // ── Authorization check ────────────────────────────────────────────
    if htlc.beneficiary != caller {
        return Err(ContractError::Unauthorized);
    }

    // ── State check ────────────────────────────────────────────────────
    if htlc.state != HtlcState::Active {
        return Err(ContractError::HtlcNotActive);
    }

    // ── Deadline check — claim must happen before deadline ──────────────
    let current_seq = env.ledger().sequence();
    if current_seq >= htlc.deadline_sequence {
        return Err(ContractError::DeadlineReached);
    }

    // ── SHA-256 pre-image verification ─────────────────────────────────
    let computed_hash = env.crypto().sha256(&pre_image);
    if computed_hash != htlc.hash_lock {
        return Err(ContractError::InvalidPreImage);
    }

    // ── State transition ───────────────────────────────────────────────
    htlc.state = HtlcState::Claimed;
    env.storage().persistent().set(&key, &htlc);

    // Decrement depositor active count.
    let counter_key = HtlcCounterKey(htlc.depositor.clone());
    let count: u32 = env
        .storage()
        .persistent()
        .get(&counter_key)
        .unwrap_or(1u32);
    if count > 0 {
        env.storage()
            .persistent()
            .set(&counter_key, &(count - 1));
    }

    // Emit claim event.
    let _ = emit_simple2(
        &env,
        EV_HTLC_CLAIM,
        symbol_short!("htlc"),
        (htlc_id, caller, htlc.amount),
    );

    Ok(ClaimResult {
        htlc_id,
        amount: htlc.amount,
        beneficiary: htlc.beneficiary,
    })
}

// ---------------------------------------------------------------------------
// Refund (time-lock expiry)
// ---------------------------------------------------------------------------

/// Refund an active HTLC after its deadline has expired.
///
/// Only the original depositor may execute a refund. The deadline ledger
/// sequence must have passed. On success the HTLC state transitions to
/// `Refunded` and the full amount is returned to the depositor.
///
/// # Arguments
/// * `env` - Soroban environment.
/// * `htlc_id` - The HTLC to refund.
/// * `caller` - The address requesting the refund (must be the depositor).
///
/// # Errors
/// * [`ContractError::HtlcNotFound`] if no HTLC exists with this ID.
/// * [`ContractError::HtlcNotActive`] if the HTLC has already been settled.
/// * [`ContractError::DeadlineNotReached`] if the deadline has not yet passed.
/// * [`ContractError::Unauthorized`] if the caller is not the depositor.
pub fn refund(
    env: &Env,
    htlc_id: u64,
    caller: Address,
) -> Result<RefundResult, ContractError> {
    caller.require_auth();

    let key = HtlcKey(htlc_id);
    let mut htlc: Htlc = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(ContractError::HtlcNotFound)?;

    // ── Authorization check ────────────────────────────────────────────
    if htlc.depositor != caller {
        return Err(ContractError::Unauthorized);
    }

    // ── State check ────────────────────────────────────────────────────
    if htlc.state != HtlcState::Active {
        return Err(ContractError::HtlcNotActive);
    }

    // ── Deadline check — refund requires deadline to have passed ────────
    let current_seq = env.ledger().sequence();
    if current_seq < htlc.deadline_sequence {
        return Err(ContractError::DeadlineNotReached);
    }

    // ── State transition ───────────────────────────────────────────────
    htlc.state = HtlcState::Refunded;
    env.storage().persistent().set(&key, &htlc);

    // Decrement depositor active count.
    let counter_key = HtlcCounterKey(htlc.depositor.clone());
    let count: u32 = env
        .storage()
        .persistent()
        .get(&counter_key)
        .unwrap_or(1u32);
    if count > 0 {
        env.storage()
            .persistent()
            .set(&counter_key, &(count - 1));
    }

    // Emit refund event.
    let _ = emit_simple2(
        &env,
        EV_HTLC_REFUND,
        symbol_short!("htlc"),
        (htlc_id, caller, htlc.amount),
    );

    Ok(RefundResult {
        htlc_id,
        amount: htlc.amount,
        depositor: htlc.depositor,
    })
}

// ---------------------------------------------------------------------------
// Query helpers
// ---------------------------------------------------------------------------

/// Load an HTLC record by ID.
pub fn get_htlc(env: &Env, htlc_id: u64) -> Result<Htlc, ContractError> {
    let key = HtlcKey(htlc_id);
    env.storage()
        .persistent()
        .get(&key)
        .ok_or(ContractError::HtlcNotFound)
}

/// Return the number of active HTLCs for a depositor.
pub fn active_htlc_count(env: &Env, depositor: &Address) -> u32 {
    let counter_key = HtlcCounterKey(depositor.clone());
    env.storage()
        .persistent()
        .get(&counter_key)
        .unwrap_or(0u32)
}

/// Return the next HTLC ID that will be assigned.
pub fn next_htlc_id(env: &Env) -> u64 {
    env.storage()
        .instance()
        .get(&HTLC_NONCE_KEY)
        .unwrap_or(0u64)
}

/// Check whether a given HTLC has expired (deadline passed and still active).
pub fn is_expired(env: &Env, htlc: &Htlc) -> bool {
    htlc.state == HtlcState::Active && env.ledger().sequence() >= htlc.deadline_sequence
}

/// Check whether a given HTLC can still be claimed (active and before deadline).
pub fn is_claimable(env: &Env, htlc: &Htlc) -> bool {
    htlc.state == HtlcState::Active && env.ledger().sequence() < htlc.deadline_sequence
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger, LedgerInfo};

    const TEST_ASSET: AssetId = 1;
    const TEST_AMOUNT: u64 = 10_000_000;

    fn setup() -> (Env, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();
        // Set a known ledger sequence so deadline math is predictable.
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
        let depositor = Address::generate(&env);
        let beneficiary = Address::generate(&env);
        (env, depositor, beneficiary)
    }

    fn make_hash_preimage(pre_image: &[u8]) -> (Bytes, BytesN<32>) {
        let env = Env::default();
        let bytes = Bytes::from_slice(&env, pre_image);
        let hash = env.crypto().sha256(&bytes);
        (bytes, hash)
    }

    fn make_pre_image(id: u64) -> Bytes {
        let env = Env::default();
        Bytes::from_slice(&env, &id.to_be_bytes())
    }

    fn make_hash(id: u64) -> BytesN<32> {
        let env = Env::default();
        let bytes = Bytes::from_slice(&env, &id.to_be_bytes());
        env.crypto().sha256(&bytes)
    }

    // ── Create tests ──────────────────────────────────────────────────

    #[test]
    fn create_htlc_success() {
        let (env, dep, ben) = setup();
        let hash_lock = make_hash(42);
        let htlc = create_htlc(&env, dep.clone(), ben.clone(), hash_lock, 200, TEST_ASSET, TEST_AMOUNT).unwrap();

        assert_eq!(htlc.id, 1);
        assert_eq!(htlc.depositor, dep);
        assert_eq!(htlc.beneficiary, ben);
        assert_eq!(htlc.amount, TEST_AMOUNT);
        assert_eq!(htlc.state, HtlcState::Active);
        assert_eq!(htlc.deadline_sequence, 200);
    }

    #[test]
    fn create_htlc_increments_id() {
        let (env, dep, ben) = setup();
        let h1 = create_htlc(&env, dep.clone(), ben.clone(), make_hash(1), 200, TEST_ASSET, 100).unwrap();
        let h2 = create_htlc(&env, dep.clone(), ben.clone(), make_hash(2), 300, TEST_ASSET, 200).unwrap();
        assert_eq!(h1.id + 1, h2.id);
    }

    #[test]
    fn create_htlc_rejects_zero_amount() {
        let (env, dep, ben) = setup();
        let result = create_htlc(&env, dep, ben, make_hash(1), 200, TEST_ASSET, 0);
        assert_eq!(result, Err(ContractError::ZeroSwapAmount));
    }

    #[test]
    fn create_htlc_rejects_deadline_too_soon() {
        let (env, dep, ben) = setup();
        // current seq = 100, deadline = 105 (< 100 + 10 = 110)
        let result = create_htlc(&env, dep, ben, make_hash(1), 105, TEST_ASSET, TEST_AMOUNT);
        assert_eq!(result, Err(ContractError::DeadlineTooSoon));
    }

    #[test]
    fn create_htlc_rejects_deadline_too_far() {
        let (env, dep, ben) = setup();
        // current seq = 100, deadline = 100 + 6_307_200 + 1
        let result = create_htlc(&env, dep, ben, make_hash(1), 100 + MAX_DEADLINE_OFFSET + 1, TEST_ASSET, TEST_AMOUNT);
        assert_eq!(result, Err(ContractError::DeadlineTooFar));
    }

    #[test]
    fn create_htlc_respects_max_active_limit() {
        let (env, dep, ben) = setup();
        for i in 0..MAX_ACTIVE_HTLCS {
            create_htlc(&env, dep.clone(), ben.clone(), make_hash(i as u64), 200 + i, TEST_ASSET, 1).unwrap();
        }
        let result = create_htlc(&env, dep, ben, make_hash(999), 500, TEST_ASSET, 1);
        assert_eq!(result, Err(ContractError::TooManyActiveHtlcs));
    }

    // ── Claim tests ───────────────────────────────────────────────────

    #[test]
    fn claim_success() {
        let (env, dep, ben) = setup();
        let pre_image = make_pre_image(42);
        let hash_lock = env.crypto().sha256(&pre_image);

        let htlc = create_htlc(&env, dep, ben.clone(), hash_lock, 200, TEST_ASSET, TEST_AMOUNT).unwrap();

        // Advance to seq 150 (before deadline 200).
        env.ledger().set(LedgerInfo {
            sequence_number: 150,
            ..env.ledger().get()
        });

        let result = claim(&env, htlc.id, pre_image, ben).unwrap();
        assert_eq!(result.amount, TEST_AMOUNT);

        // Verify state transition.
        let stored = get_htlc(&env, htlc.id).unwrap();
        assert_eq!(stored.state, HtlcState::Claimed);
    }

    #[test]
    fn claim_rejects_invalid_preimage() {
        let (env, dep, ben) = setup();
        let pre_image = make_pre_image(42);
        let hash_lock = env.crypto().sha256(&pre_image);

        let htlc = create_htlc(&env, dep, ben.clone(), hash_lock, 200, TEST_ASSET, TEST_AMOUNT).unwrap();

        let wrong_image = make_pre_image(99);
        let result = claim(&env, htlc.id, wrong_image, ben);
        assert_eq!(result, Err(ContractError::InvalidPreImage));
    }

    #[test]
    fn claim_rejects_after_deadline() {
        let (env, dep, ben) = setup();
        let pre_image = make_pre_image(42);
        let hash_lock = env.crypto().sha256(&pre_image);

        let htlc = create_htlc(&env, dep, ben.clone(), hash_lock, 200, TEST_ASSET, TEST_AMOUNT).unwrap();

        // Advance past deadline.
        env.ledger().set(LedgerInfo {
            sequence_number: 200,
            ..env.ledger().get()
        });

        let result = claim(&env, htlc.id, pre_image, ben);
        assert_eq!(result, Err(ContractError::DeadlineReached));
    }

    #[test]
    fn claim_rejects_wrong_caller() {
        let (env, dep, ben) = setup();
        let pre_image = make_pre_image(42);
        let hash_lock = env.crypto().sha256(&pre_image);

        let htlc = create_htlc(&env, dep.clone(), ben, hash_lock, 200, TEST_ASSET, TEST_AMOUNT).unwrap();

        let wrong_caller = Address::generate(&env);
        let result = claim(&env, htlc.id, pre_image, wrong_caller);
        assert_eq!(result, Err(ContractError::Unauthorized));
    }

    #[test]
    fn claim_rejects_already_claimed() {
        let (env, dep, ben) = setup();
        let pre_image = make_pre_image(42);
        let hash_lock = env.crypto().sha256(&pre_image);

        let htlc = create_htlc(&env, dep, ben.clone(), hash_lock, 200, TEST_ASSET, TEST_AMOUNT).unwrap();

        claim(&env, htlc.id, pre_image, ben.clone()).unwrap();

        let pre_image2 = make_pre_image(42);
        let result = claim(&env, htlc.id, pre_image2, ben);
        assert_eq!(result, Err(ContractError::HtlcNotActive));
    }

    #[test]
    fn claim_decrements_active_count() {
        let (env, dep, ben) = setup();
        let pre_image = make_pre_image(42);
        let hash_lock = env.crypto().sha256(&pre_image);

        let _ = create_htlc(&env, dep.clone(), ben.clone(), hash_lock, 200, TEST_ASSET, TEST_AMOUNT).unwrap();
        assert_eq!(active_htlc_count(&env, &dep), 1);

        claim(&env, 1, pre_image, ben).unwrap();
        assert_eq!(active_htlc_count(&env, &dep), 0);
    }

    // ── Refund tests ──────────────────────────────────────────────────

    #[test]
    fn refund_success() {
        let (env, dep, ben) = setup();
        let hash_lock = make_hash(42);
        let htlc = create_htlc(&env, dep.clone(), ben, hash_lock, 200, TEST_ASSET, TEST_AMOUNT).unwrap();

        // Advance past deadline.
        env.ledger().set(LedgerInfo {
            sequence_number: 200,
            ..env.ledger().get()
        });

        let result = refund(&env, htlc.id, dep.clone()).unwrap();
        assert_eq!(result.amount, TEST_AMOUNT);

        let stored = get_htlc(&env, htlc.id).unwrap();
        assert_eq!(stored.state, HtlcState::Refunded);
    }

    #[test]
    fn refund_rejects_before_deadline() {
        let (env, dep, ben) = setup();
        let hash_lock = make_hash(42);
        let htlc = create_htlc(&env, dep.clone(), ben, hash_lock, 200, TEST_ASSET, TEST_AMOUNT).unwrap();

        // Still at seq 100, deadline is 200.
        let result = refund(&env, htlc.id, dep);
        assert_eq!(result, Err(ContractError::DeadlineNotReached));
    }

    #[test]
    fn refund_rejects_wrong_caller() {
        let (env, dep, ben) = setup();
        let hash_lock = make_hash(42);
        let htlc = create_htlc(&env, dep, ben, hash_lock, 200, TEST_ASSET, TEST_AMOUNT).unwrap();

        env.ledger().set(LedgerInfo {
            sequence_number: 200,
            ..env.ledger().get()
        });

        let wrong_caller = Address::generate(&env);
        let result = refund(&env, htlc.id, wrong_caller);
        assert_eq!(result, Err(ContractError::Unauthorized));
    }

    #[test]
    fn refund_rejects_already_refunded() {
        let (env, dep, ben) = setup();
        let hash_lock = make_hash(42);
        let htlc = create_htlc(&env, dep.clone(), ben, hash_lock, 200, TEST_ASSET, TEST_AMOUNT).unwrap();

        env.ledger().set(LedgerInfo {
            sequence_number: 200,
            ..env.ledger().get()
        });

        refund(&env, htlc.id, dep.clone()).unwrap();
        let result = refund(&env, htlc.id, dep);
        assert_eq!(result, Err(ContractError::HtlcNotActive));
    }

    #[test]
    fn refund_decrements_active_count() {
        let (env, dep, ben) = setup();
        let hash_lock = make_hash(42);
        let _ = create_htlc(&env, dep.clone(), ben, hash_lock, 200, TEST_ASSET, TEST_AMOUNT).unwrap();
        assert_eq!(active_htlc_count(&env, &dep), 1);

        env.ledger().set(LedgerInfo {
            sequence_number: 200,
            ..env.ledger().get()
        });

        refund(&env, 1, dep.clone()).unwrap();
        assert_eq!(active_htlc_count(&env, &dep), 0);
    }

    // ── Query helper tests ────────────────────────────────────────────

    #[test]
    fn get_htlc_not_found() {
        let env = Env::default();
        let result = get_htlc(&env, 999);
        assert_eq!(result, Err(ContractError::HtlcNotFound));
    }

    #[test]
    fn next_htlc_id_starts_at_zero() {
        let env = Env::default();
        assert_eq!(next_htlc_id(&env), 0);
    }

    #[test]
    fn is_expired_true_after_deadline() {
        let (env, dep, ben) = setup();
        let hash_lock = make_hash(42);
        let htlc = create_htlc(&env, dep, ben, hash_lock, 200, TEST_ASSET, TEST_AMOUNT).unwrap();

        env.ledger().set(LedgerInfo {
            sequence_number: 200,
            ..env.ledger().get()
        });

        assert!(is_expired(&env, &htlc));
    }

    #[test]
    fn is_expired_false_before_deadline() {
        let (env, dep, ben) = setup();
        let hash_lock = make_hash(42);
        let htlc = create_htlc(&env, dep, ben, hash_lock, 200, TEST_ASSET, TEST_AMOUNT).unwrap();
        assert!(!is_expired(&env, &htlc));
    }

    #[test]
    fn is_claimable_true_before_deadline() {
        let (env, dep, ben) = setup();
        let hash_lock = make_hash(42);
        let htlc = create_htlc(&env, dep, ben, hash_lock, 200, TEST_ASSET, TEST_AMOUNT).unwrap();
        assert!(is_claimable(&env, &htlc));
    }

    #[test]
    fn is_claimable_false_after_deadline() {
        let (env, dep, ben) = setup();
        let hash_lock = make_hash(42);
        let htlc = create_htlc(&env, dep, ben, hash_lock, 200, TEST_ASSET, TEST_AMOUNT).unwrap();

        env.ledger().set(LedgerInfo {
            sequence_number: 200,
            ..env.ledger().get()
        });

        assert!(!is_claimable(&env, &htlc));
    }

    #[test]
    fn is_claimable_false_after_claim() {
        let (env, dep, ben) = setup();
        let pre_image = make_pre_image(42);
        let hash_lock = env.crypto().sha256(&pre_image);
        let htlc = create_htlc(&env, dep, ben.clone(), hash_lock, 200, TEST_ASSET, TEST_AMOUNT).unwrap();

        claim(&env, htlc.id, pre_image, ben).unwrap();
        assert!(!is_claimable(&env, &htlc));
    }

    #[test]
    fn active_htlc_count_zero_for_unknown() {
        let env = Env::default();
        let addr = Address::generate(&env);
        assert_eq!(active_htlc_count(&env, &addr), 0);
    }
}
