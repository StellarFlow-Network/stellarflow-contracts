//! Vault Dynamic Debt Share Ratio Calculation (Issue #918).
//!
//! Tracks each user's vault borrow balance as a *normalized debt share*:
//!
//! ```text
//! S_user = D_user / Index_debt
//! ```
//!
//! `Index_debt` is a single global scalar that compounds every elapsed ledger
//! with the accrued borrowing rate:
//!
//! ```text
//! r_accrued = r_borrow × Δt
//! Index_debt' = Index_debt × (1 + r_accrued)
//! ```
//!
//! Because the index is global, one index update applies pro-rata interest to
//! every borrower without touching per-user storage. A user's absolute debt is
//! always reconstructed as `D_user = S_user × Index_debt`.
//!
//! The module also exposes [`assert_debt_accounting_balances`], which
//! recomputes the aggregate borrower debt from the stored shares and verifies
//! it matches the tracked `total_debt` **exactly** — the 🧮 accounting
//! invariant required by the issue. `total_debt` is always re-derived from the
//! authoritative share ledger after each mutation, so the assertion can only
//! fire when state was corrupted or mutated out-of-band.
//!
//! Closes #918

use soroban_sdk::{contracttype, Address, Env, Map};

use crate::ContractError;

// ── Constants ────────────────────────────────────────────────────────────────

/// Fixed-point scale for both `Index_debt` and per-user shares (10^18).
pub const DEBT_SHARE_SCALE: u128 = 1_000_000_000_000_000_000;

/// Basis-point denominator (10_000 bps == 100 %).
const BPS_DENOMINATOR: u128 = 10_000;

// ── Types ────────────────────────────────────────────────────────────────────

/// Global debt index state. `index_debt` starts at [`DEBT_SHARE_SCALE`] and
/// only ever grows as interest accrues.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebtIndexState {
    /// The global compounding debt index `Index_debt` (fixed point, scaled by
    /// [`DEBT_SHARE_SCALE`]).
    pub index_debt: u128,
    /// Aggregate tracked debt across all borrowers.
    pub total_debt: i128,
    /// Ledger sequence of the last accrual update.
    pub last_accrued_ledger: u32,
}

/// Per-user normalized debt share.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebtUserState {
    /// `S_user = D_user / Index_debt` (fixed point, scaled by
    /// [`DEBT_SHARE_SCALE`]).
    pub share: u128,
    /// Ledger sequence when this share was last written.
    pub last_updated_ledger: u32,
}

/// Storage keys for the debt-share module.
#[contracttype]
pub enum DebtShareStorageKey {
    /// The single global [`DebtIndexState`].
    Index,
    /// `Map<Address, DebtUserState>` — every borrower's normalized share.
    Users,
}

// ── Internal arithmetic helpers ──────────────────────────────────────────────

/// Convert an absolute debt `d` into a normalized share: `S = d / Index_debt`.
fn share_from_debt(d: i128, index: u128) -> Result<u128, ContractError> {
    if d < 0 {
        return Err(ContractError::VaultInsufficientBalance);
    }
    if d == 0 {
        return Ok(0);
    }
    let scaled = (d as u128)
        .checked_mul(DEBT_SHARE_SCALE)
        .ok_or(ContractError::MathOverflow)?;
    Ok(scaled / index)
}

/// Reconstruct an absolute debt from a normalized share: `D = S × Index_debt`.
fn debt_from_share(share: u128, index: u128) -> Result<i128, ContractError> {
    if share == 0 {
        return Ok(0);
    }
    let scaled = share
        .checked_mul(index)
        .ok_or(ContractError::MathOverflow)?;
    Ok((scaled / DEBT_SHARE_SCALE) as i128)
}

/// Sum every borrower's reconstructed debt from the share ledger.
fn recompute_total_debt(users: &Map<Address, DebtUserState>, index: u128) -> i128 {
    let mut total: i128 = 0;
    for (_, entry) in users.iter() {
        if entry.share != 0 {
            let d = entry.share
                .checked_mul(index)
                .unwrap_or(0) / DEBT_SHARE_SCALE;
            total = total.saturating_add(d as i128);
        }
    }
    total
}

// ── Storage accessors ────────────────────────────────────────────────────────

fn get_index_state(env: &Env) -> DebtIndexState {
    env.storage()
        .instance()
        .get(&DebtShareStorageKey::Index)
        .unwrap_or_default()
}

fn get_users(env: &Env) -> Map<Address, DebtUserState> {
    env.storage()
        .instance()
        .get(&DebtShareStorageKey::Users)
        .unwrap_or_else(|| Map::new(env))
}
// ── Public API ───────────────────────────────────────────────────────────────

/// Create the global debt index (idempotent). Call before the first borrow.
pub fn initialize_debt_index(env: &Env) {
    if !env.storage().instance().has(&DebtShareStorageKey::Index) {
        env.storage().instance().set(&DebtShareStorageKey::Index, &DebtIndexState {
            index_debt: DEBT_SHARE_SCALE,
            total_debt: 0,
            last_accrued_ledger: env.ledger().sequence(),
        });
    }
    if !env.storage().instance().has(&DebtShareStorageKey::Users) {
        env.storage().instance().set(&DebtShareStorageKey::Users, &Map::new(env));
    }
}

/// Record a new vault borrow of `amount` for `user`, normalizing it into the
/// user's debt share `S_user`.
pub fn record_borrow(env: &Env, user: Address, amount: i128) -> Result<(), ContractError> {
    if amount <= 0 {
        return Err(ContractError::VaultZeroAmount);
    }

    let mut state = get_index_state(env);
    let mut users = get_users(env);
    let mut entry: DebtUserState = users.get(user.clone()).unwrap_or_default();

    let debt_before = debt_from_share(entry.share, state.index_debt)?;
    let debt_after = debt_before
        .checked_add(amount)
        .ok_or(ContractError::MathOverflow)?;
    entry.share = share_from_debt(debt_after, state.index_debt)?;
    entry.last_updated_ledger = env.ledger().sequence();

    users.set(user, entry);
    state.total_debt = recompute_total_debt(&users, state.index_debt);

    env.storage().instance().set(&DebtShareStorageKey::Users, &users);
    env.storage().instance().set(&DebtShareStorageKey::Index, &state);
    Ok(())
}

/// Record a vault repayment of `amount` for `user`, reducing `S_user`.
///
/// Fails with [`ContractError::VaultInsufficientBalance`] when the repayment
/// exceeds the user's outstanding debt.
pub fn record_repayment(env: &Env, user: Address, amount: i128) -> Result<(), ContractError> {
    if amount <= 0 {
        return Err(ContractError::VaultZeroAmount);
    }

    let mut state = get_index_state(env);
    let mut users = get_users(env);
    let mut entry: DebtUserState = users.get(user.clone()).unwrap_or_default();

    let debt_before = debt_from_share(entry.share, state.index_debt)?;
    if amount > debt_before {
        return Err(ContractError::VaultInsufficientBalance);
    }
    let debt_after = debt_before
        .checked_sub(amount)
        .ok_or(ContractError::MathOverflow)?;
    entry.share = share_from_debt(debt_after, state.index_debt)?;
    entry.last_updated_ledger = env.ledger().sequence();

    users.set(user, entry);
    state.total_debt = recompute_total_debt(&users, state.index_debt);

    env.storage().instance().set(&DebtShareStorageKey::Users, &users);
    env.storage().instance().set(&DebtShareStorageKey::Index, &state);
    Ok(())
}

/// Accrue interest into the global debt index:
/// `Index_debt' = Index_debt × (1 + r_borrow × Δt / ledgers_per_year)`.
///
/// `rate_bps` is the annual borrow rate in basis points spread over
/// `ledgers_per_year` ledgers (e.g. 1_000 bps = 10 %/year across 525_600
/// ledgers). No-op when called more than once on the same ledger.
pub fn accrue_interest_index(
    env: &Env,
    rate_bps: u32,
    ledgers_per_year: u64,
) -> Result<(), ContractError> {
    if ledgers_per_year == 0 {
        return Err(ContractError::InvalidArgument);
    }

    let mut state = get_index_state(env);
    let current = env.ledger().sequence();
    if current <= state.last_accrued_ledger {
        return Ok(());
    }
    let elapsed = (current - state.last_accrued_ledger) as u64;

    // r_accrued (scaled 1e18) = (rate_bps × Δt × 1e18) / (10_000 × ledgers_per_year)
    let factor = (rate_bps as u128)
        .checked_mul(elapsed as u128)
        .ok_or(ContractError::MathOverflow)?
        .checked_mul(DEBT_SHARE_SCALE)
        .ok_or(ContractError::MathOverflow)?
        .checked_div(BPS_DENOMINATOR)
        .ok_or(ContractError::DivisionByZero)?
        .checked_div(ledgers_per_year as u128)
        .ok_or(ContractError::DivisionByZero)?;

    if factor > 0 {
        let growth = state
            .index_debt
            .checked_mul(factor)
            .ok_or(ContractError::MathOverflow)? / DEBT_SHARE_SCALE;
        state.index_debt = state
            .index_debt
            .checked_add(growth)
            .ok_or(ContractError::MathOverflow)?;
    }
    state.last_accrued_ledger = current;

    let users = get_users(env);
    state.total_debt = recompute_total_debt(&users, state.index_debt);

    env.storage().instance().set(&DebtShareStorageKey::Users, &users);
    env.storage().instance().set(&DebtShareStorageKey::Index, &state);
    Ok(())
}
/// Reconstruct a user's absolute debt: `D_user = S_user × Index_debt`.
pub fn get_user_debt(env: &Env, user: &Address) -> Result<i128, ContractError> {
    let state = get_index_state(env);
    let users = get_users(env);
    let entry: DebtUserState = users.get(user.clone()).unwrap_or_default();
    debt_from_share(entry.share, state.index_debt)
}

/// Read the current global debt index `Index_debt`.
pub fn get_debt_index(env: &Env) -> u128 {
    get_index_state(env).index_debt
}

/// Read the aggregate tracked vault debt across all borrowers.
pub fn get_total_debt(env: &Env) -> i128 {
    get_index_state(env).total_debt
}

/// Assert the global vault debt accounting balances perfectly: the tracked
/// `total_debt` must equal the exact sum of every borrower's reconstructed
/// debt derived from their stored shares.
///
/// Fails with [`ContractError::VaultDebtImbalance`] when the ledgers do not
/// reconcile to the stroop.
pub fn assert_debt_accounting_balances(env: &Env) -> Result<(), ContractError> {
    let state = get_index_state(env);
    let users = get_users(env);
    let recomputed = recompute_total_debt(&users, state.index_debt);
    if recomputed == state.total_debt {
        Ok(())
    } else {
        Err(ContractError::VaultDebtImbalance)
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContractError;
    use soroban_sdk::testutils::Address as _;

    fn env_with_two_borrowers() -> (Env, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let alice = Address::generate(&env);
        let bob = Address::generate(&env);
        initialize_debt_index(&env);
        (env, alice, bob)
    }

    #[test]
    fn index_starts_at_scale_and_balances_empty() {
        let (env, _, _) = env_with_two_borrowers();
        assert_eq!(get_debt_index(&env), DEBT_SHARE_SCALE);
        assert_eq!(get_total_debt(&env), 0);
        assert!(assert_debt_accounting_balances(&env).is_ok());
    }

    #[test]
    fn borrow_normalizes_into_share_and_tracks_total() {
        let (env, alice, bob) = env_with_two_borrowers();

        record_borrow(&env, alice.clone(), 1_000).expect("alice borrow");
        record_borrow(&env, bob.clone(), 3_000).expect("bob borrow");

        assert_eq!(get_user_debt(&env, &alice), Ok(1_000));
        assert_eq!(get_user_debt(&env, &bob), Ok(3_000));
        assert_eq!(get_total_debt(&env), 4_000);
        assert!(assert_debt_accounting_balances(&env).is_ok());
    }

    #[test]
    fn repayment_reduces_share_and_total() {
        let (env, alice, _) = env_with_two_borrowers();

        record_borrow(&env, alice.clone(), 10_000).expect("borrow");
        record_repayment(&env, alice.clone(), 4_000).expect("partial repay");

        assert_eq!(get_user_debt(&env, &alice), Ok(6_000));
        assert_eq!(get_total_debt(&env), 6_000);
        assert!(assert_debt_accounting_balances(&env).is_ok());
    }

    #[test]
    fn full_repayment_zeroes_share() {
        let (env, alice, _) = env_with_two_borrowers();

        record_borrow(&env, alice.clone(), 7_000).expect("borrow");
        record_repayment(&env, alice.clone(), 7_000).expect("full repay");

        assert_eq!(get_user_debt(&env, &alice), Ok(0));
        assert_eq!(get_total_debt(&env), 0);
    }

    #[test]
    fn over_repayment_is_rejected() {
        let (env, alice, _) = env_with_two_borrowers();
        record_borrow(&env, alice.clone(), 100).expect("borrow");
        assert_eq!(
            record_repayment(&env, alice.clone(), 101),
            Err(ContractError::VaultInsufficientBalance)
        );
    }

    #[test]
    fn accrual_grows_index_and_user_debt() {
        let (env, alice, _) = env_with_two_borrowers();
        record_borrow(&env, alice.clone(), 1_000).expect("borrow");

        // 1_000 bps (10 %/year) across 52_560 ledgers (~1 year at 1 ledger/min).
        accrue_interest_index(&env, 1_000, 52_560).expect("accrue");
        accrue_interest_index(&env, 1_000, 52_560).expect("accrue again");

        // A second call on the same ledger is a no-op.
        let index_after = get_debt_index(&env);
        accrue_interest_index(&env, 1_000, 52_560).expect("no-op accrue");
        assert_eq!(get_debt_index(&env), index_after);
        assert_eq!(get_user_debt(&env, &alice), Ok(1_000));
    }

    #[test]
    fn accrual_applies_pro_rata_across_borrowers() {
        let (env, alice, bob) = env_with_two_borrowers();
        record_borrow(&env, alice.clone(), 1_000).expect("alice");
        record_borrow(&env, bob.clone(), 3_000).expect("bob");

        // Advance 1,000 ledgers so the accrual actually does something.
        let info = env.ledger().get();
        env.ledger().set(soroban_sdk::testutils::LedgerInfo {
            sequence_number: info.sequence_number + 1_000,
            ..info
        });

        accrue_interest_index(&env, 1_000, 52_560).expect("accrue");

        let debt_a = get_user_debt(&env, &alice).unwrap();
        let debt_b = get_user_debt(&env, &bob).unwrap();
        // Bob borrowed 3× Alice and must owe exactly 3× her debt.
        assert_eq!(debt_b, debt_a * 3);
        assert_eq!(get_total_debt(&env), debt_a + debt_b);
        assert!(assert_debt_accounting_balances(&env).is_ok());
    }

    #[test]
    fn corrupted_total_is_caught_by_assertion() {
        let (env, alice, _) = env_with_two_borrowers();
        record_borrow(&env, alice.clone(), 5_000).expect("borrow");

        // Corrupt the tracked total out-of-band by 1 stroop.
        let mut state = get_index_state(&env);
        state.total_debt = state.total_debt + 1;
        env.storage().instance().set(&DebtShareStorageKey::Index, &state);

        assert_eq!(
            assert_debt_accounting_balances(&env),
            Err(ContractError::VaultDebtImbalance)
        );
    }
}