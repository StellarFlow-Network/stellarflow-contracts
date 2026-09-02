//! Timelock queue for critical administrative actions (issue #731).
//!
//! Enforces a mandatory 48-hour delay on sensitive admin operations — fee
//! parameter changes and contract upgrades — before they take effect. This
//! prevents "flash-admin" attacks where a compromised key could instantly
//! drain the protocol by changing fee rates or hot-swapping contract logic.
//!
//! # Flow
//!
//! 1. Admin calls `queue_admin_action` — the action is serialised and stored
//!    with `execute_not_before = now + 48h`.
//! 2. During the window the admin (or any account they designate) may call
//!    `cancel_action` to veto the pending action.
//! 3. After `execute_not_before` has passed, calling `execute_action` applies
//!    the stored mutation to contract state.
//!
//! Multiple actions of *different* types may be in-flight concurrently.
//! Attempting to queue a second action of the **same** type while one is
//! pending returns [`ContractError::AdminChangePending`].

use soroban_sdk::{contracttype, symbol_short, Address, Env, Symbol};

use crate::{ContractData, ContractError, DATA_KEY};
use crate::fees::{FeesStorageKey, DynamicFeeState};

// ── Constants ────────────────────────────────────────────────────────────────

/// Mandatory delay between queuing and executing an admin action: 48 hours.
pub const ADMIN_ACTION_DELAY_SECONDS: u64 = 48 * 60 * 60;

// ── Storage keys ─────────────────────────────────────────────────────────────

/// Persistent-storage key for the fee-change queue slot.
pub(crate) const QUEUED_FEE_CHANGE_KEY: Symbol = symbol_short!("QFEEMOD");
/// Persistent-storage key for the max-fee-ceiling queue slot.
pub(crate) const QUEUED_FEE_CEILING_KEY: Symbol = symbol_short!("QFEECLG");

// ── Queued action types ───────────────────────────────────────────────────────

/// Parameters for a fee-ceiling update action.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct FeeCeilingUpdateParams {
    pub new_ceiling: u64,
}

/// Parameters for a dynamic fee configuration update action.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct DynamicFeeConfigParams {
    pub asset: crate::AssetId,
    pub min_fee_bps: u32,
    pub max_fee_bps: u32,
    pub period_seconds: u64,
}

/// Discriminates the kind of admin action that has been queued.
///
/// Each variant carries the full set of parameters needed to execute the
/// action when the timelock window expires.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum QueuedActionPayload {
    /// Update the `max_fee_ceiling` stored in [`ContractData`].
    UpdateFeeCeiling(FeeCeilingUpdateParams),
    /// Replace the dynamic fee configuration for a specific corridor asset.
    UpdateDynamicFeeConfig(DynamicFeeConfigParams),
}

/// A timestamped wrapper around a [`QueuedActionPayload`].
///
/// Written to persistent storage by `queue_admin_action`; removed (either by
/// execution or cancellation) during the window.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct QueuedAdminAction {
    /// The action to execute when the timelock expires.
    pub payload: QueuedActionPayload,
    /// Address of the admin that queued this action.
    pub proposer: Address,
    /// Ledger timestamp when the action was queued.
    pub queued_at: u64,
    /// Earliest ledger timestamp at which the action may be executed
    /// (`queued_at + ADMIN_ACTION_DELAY_SECONDS`).
    pub execute_not_before: u64,
}

// ── Storage-key helpers ───────────────────────────────────────────────────────

/// Return the persistent-storage [`Symbol`] key for a given action type.
///
/// Using distinct keys per action type allows concurrent proposals for
/// different action types without conflicts.
fn queue_storage_key(payload: &QueuedActionPayload) -> Symbol {
    match payload {
        QueuedActionPayload::UpdateFeeCeiling(_) => QUEUED_FEE_CEILING_KEY,
        QueuedActionPayload::UpdateDynamicFeeConfig(_) => QUEUED_FEE_CHANGE_KEY,
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Queue an administrative action with a 48-hour timelock.
///
/// The caller must be the current contract admin. If an action of the same
/// type is already pending, returns [`ContractError::AdminChangePending`].
///
/// # Arguments
///
/// * `env`     - Soroban execution environment.
/// * `admin`   - Address of the admin proposing the action; must pass `require_auth`.
/// * `payload` - The action to be queued.
///
/// # Errors
///
/// - [`ContractError::NotInitialized`]   – contract has not been initialized.
/// - [`ContractError::NotAdmin`]         – caller is not the current admin.
/// - [`ContractError::AdminChangePending`] – a same-type action is already queued.
/// - [`ContractError::Overflow`]         – internal timestamp arithmetic overflow.
pub fn queue_admin_action(
    env: &Env,
    admin: Address,
    payload: QueuedActionPayload,
) -> Result<QueuedAdminAction, ContractError> {
    admin.require_auth();

    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;

    if data.admin != admin {
        return Err(ContractError::NotAdmin);
    }

    let key = queue_storage_key(&payload);

    // Block duplicate proposals for the same action type.
    if env.storage().persistent().has(&key) {
        return Err(ContractError::AdminChangePending);
    }

    let queued_at = env.ledger().timestamp();
    let execute_not_before = queued_at
        .checked_add(ADMIN_ACTION_DELAY_SECONDS)
        .ok_or(ContractError::Overflow)?;

    let queued = QueuedAdminAction {
        payload,
        proposer: admin,
        queued_at,
        execute_not_before,
    };

    env.storage().persistent().set(&key, &queued);
    crate::storage::extend_persistent_ttl(env, &key);

    Ok(queued)
}

/// Cancel a pending admin action before the timelock expires.
///
/// Can be called by the current admin at any point during the window —
/// including *after* `execute_not_before` if execution has not yet occurred.
///
/// # Errors
///
/// - [`ContractError::NotInitialized`]      – contract not initialized.
/// - [`ContractError::NotAdmin`]            – caller is not the current admin.
/// - [`ContractError::NoAdminChangePending`] – no queued action of the specified
///   type exists.
pub fn cancel_action(
    env: &Env,
    admin: Address,
    payload_type: QueuedActionPayload,
) -> Result<(), ContractError> {
    admin.require_auth();

    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;

    if data.admin != admin {
        return Err(ContractError::NotAdmin);
    }

    let key = queue_storage_key(&payload_type);

    if !env.storage().persistent().has(&key) {
        return Err(ContractError::NoAdminChangePending);
    }

    env.storage().persistent().remove(&key);
    Ok(())
}

/// Execute a queued admin action once its timelock has expired.
///
/// Applies the stored mutation to contract state and removes the queue entry.
/// Calling before `execute_not_before` returns
/// [`ContractError::AdminTimelockNotSatisfied`].
///
/// # Errors
///
/// - [`ContractError::NotInitialized`]           – contract not initialized.
/// - [`ContractError::NotAdmin`]                 – caller is not the current admin.
/// - [`ContractError::NoAdminChangePending`]     – no queued action of this type.
/// - [`ContractError::AdminTimelockNotSatisfied`] – timelock has not yet elapsed.
/// - [`ContractError::FeeCeilingExceeded`]        – new ceiling would exceed hard cap.
pub fn execute_action(
    env: &Env,
    admin: Address,
    payload_type: QueuedActionPayload,
) -> Result<(), ContractError> {
    admin.require_auth();

    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;

    if data.admin != admin {
        return Err(ContractError::NotAdmin);
    }

    let key = queue_storage_key(&payload_type);

    let queued: QueuedAdminAction = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(ContractError::NoAdminChangePending)?;

    // Enforce the mandatory delay.
    if env.ledger().timestamp() < queued.execute_not_before {
        return Err(ContractError::AdminTimelockNotSatisfied);
    }

    // Apply the action.
    let mut data_mut = data;
    apply_queued_action(env, &queued.payload, &mut data_mut)?;

    // Clean up the queue entry.
    env.storage().persistent().remove(&key);

    Ok(())
}

/// Read a pending queued action without executing or modifying it.
///
/// Returns `None` when no action of the given type is currently queued.
pub fn get_queued_action(
    env: &Env,
    payload_type: QueuedActionPayload,
) -> Option<QueuedAdminAction> {
    let key = queue_storage_key(&payload_type);
    env.storage().persistent().get(&key)
}

/// Return how many seconds remain before a queued action becomes executable.
///
/// Returns `Some(0)` when the action is already executable, `Some(n)` for
/// remaining seconds, or `None` when no action of this type is queued.
pub fn get_action_timelock_remaining(
    env: &Env,
    payload_type: QueuedActionPayload,
) -> Option<u64> {
    let key = queue_storage_key(&payload_type);
    env.storage()
        .persistent()
        .get::<_, QueuedAdminAction>(&key)
        .map(|queued| queued.execute_not_before.saturating_sub(env.ledger().timestamp()))
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Apply the concrete state mutation encoded by a [`QueuedActionPayload`].
fn apply_queued_action(
    env: &Env,
    payload: &QueuedActionPayload,
    data: &mut ContractData,
) -> Result<(), ContractError> {
    match payload {
        QueuedActionPayload::UpdateFeeCeiling(params) => {
            // Hard cap: protocol-wide fee ceiling must not exceed 10 000 bps (100 %).
            const HARD_CAP: u64 = 10_000;
            if params.new_ceiling > HARD_CAP {
                return Err(ContractError::FeeCeilingExceeded);
            }
            data.max_fee_ceiling = params.new_ceiling;
            env.storage().instance().set(&DATA_KEY, data);
        }
        QueuedActionPayload::UpdateDynamicFeeConfig(params) => {
            // Re-validate bounds at execution time (config may have evolved).
            if params.min_fee_bps < 1 || params.max_fee_bps > 100 || params.min_fee_bps >= params.max_fee_bps {
                return Err(ContractError::InvalidVarianceConfig);
            }
            if params.period_seconds < 300 {
                return Err(ContractError::InvalidVarianceConfig);
            }

            let fee_key = FeesStorageKey::DynamicFee(params.asset);
            let mut state: DynamicFeeState = env
                .storage()
                .instance()
                .get(&fee_key)
                .unwrap_or_else(|| DynamicFeeState::new_default());

            state.min_fee_bps = params.min_fee_bps;
            state.max_fee_bps = params.max_fee_bps;
            state.period_seconds = params.period_seconds;
            env.storage().instance().set(&fee_key, &state);
        }
    }
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Env};

    fn bootstrap() -> (Env, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let data = ContractData { admin: admin.clone(), value: 0, max_fee_ceiling: 10_000 };
        env.storage().instance().set(&DATA_KEY, &data);
        (env, admin)
    }

    fn ceiling_payload(new_ceiling: u64) -> QueuedActionPayload {
        QueuedActionPayload::UpdateFeeCeiling(FeeCeilingUpdateParams { new_ceiling })
    }

    #[test]
    fn queue_fee_ceiling_stores_pending_record() {
        let (env, admin) = bootstrap();

        let queued = queue_admin_action(&env, admin.clone(), ceiling_payload(5_000))
            .expect("should queue");

        assert_eq!(queued.proposer, admin);
        assert_eq!(queued.execute_not_before, queued.queued_at + ADMIN_ACTION_DELAY_SECONDS);

        let fetched = get_queued_action(&env, ceiling_payload(0));
        assert!(fetched.is_some());
    }

    #[test]
    fn duplicate_queue_returns_pending_error() {
        let (env, admin) = bootstrap();

        queue_admin_action(&env, admin.clone(), ceiling_payload(5_000)).expect("first queue");

        let err = queue_admin_action(&env, admin.clone(), ceiling_payload(3_000)).unwrap_err();
        assert_eq!(err, ContractError::AdminChangePending);
    }

    #[test]
    fn cancel_action_clears_proposal() {
        let (env, admin) = bootstrap();

        queue_admin_action(&env, admin.clone(), ceiling_payload(5_000)).expect("queue");
        cancel_action(&env, admin.clone(), ceiling_payload(0)).expect("cancel");

        let fetched = get_queued_action(&env, ceiling_payload(0));
        assert!(fetched.is_none());
    }

    #[test]
    fn execute_before_delay_returns_timelock_error() {
        let (env, admin) = bootstrap();

        queue_admin_action(&env, admin.clone(), ceiling_payload(5_000)).expect("queue");

        let err = execute_action(&env, admin.clone(), ceiling_payload(0)).unwrap_err();
        assert_eq!(err, ContractError::AdminTimelockNotSatisfied);
    }

    #[test]
    fn execute_after_delay_updates_fee_ceiling() {
        let (env, admin) = bootstrap();

        queue_admin_action(&env, admin.clone(), ceiling_payload(5_000)).expect("queue");

        // Advance time past the 48-hour window.
        env.ledger().with_mut(|l| {
            l.timestamp += ADMIN_ACTION_DELAY_SECONDS + 1;
        });

        execute_action(&env, admin.clone(), ceiling_payload(0))
            .expect("execute should succeed after timelock");

        let data: ContractData = env.storage().instance().get(&DATA_KEY).unwrap();
        assert_eq!(data.max_fee_ceiling, 5_000);

        // Queue entry should be gone.
        assert!(get_queued_action(&env, ceiling_payload(0)).is_none());
    }

    #[test]
    fn timelock_remaining_decreases_over_time() {
        let (env, admin) = bootstrap();

        queue_admin_action(&env, admin.clone(), ceiling_payload(2_000)).expect("queue");

        let remaining = get_action_timelock_remaining(&env, ceiling_payload(0))
            .expect("should have remaining");
        assert_eq!(remaining, ADMIN_ACTION_DELAY_SECONDS);

        env.ledger().with_mut(|l| {
            l.timestamp += 3600;
        });

        let remaining_after = get_action_timelock_remaining(&env, ceiling_payload(0))
            .expect("still queued");
        assert_eq!(remaining_after, ADMIN_ACTION_DELAY_SECONDS - 3600);
    }

    #[test]
    fn non_admin_cannot_queue_action() {
        let (env, _admin) = bootstrap();
        let outsider = Address::generate(&env);

        let err = queue_admin_action(&env, outsider, ceiling_payload(5_000)).unwrap_err();
        assert_eq!(err, ContractError::NotAdmin);
    }
}
