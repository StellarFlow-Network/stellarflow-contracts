//! Multi-Sig Timelock Governance Action Execution Guard.
//!
//! Queued governance actions (e.g. upgrades, parameter changes) must not run
//! until a mandatory timelock delay has elapsed since they were enqueued, and
//! must only ever execute against the *exact* payload that was originally
//! queued. This guard provides a single choke-point that enforces both
//! invariants before any queued action is allowed to run.
//!
//! ## Invariants
//!
//! 1. **Timelock delay** — the current ledger timestamp must exceed
//!    `queued_timestamp + TIMELOCK_DELAY`. Exceeding (not merely reaching) the
//!    deadline is required. Execution is gated and reverts with
//!    [`ContractError::TimelockNotExpired`] when called prematurely.
//! 2. **Payload integrity** — the transaction hash offered at execution time
//!    must match the hash of the original queued proposal payload identically.
//!    A mismatched hash reverts with [`ContractError::PayloadHashMismatch`],
//!    preventing a caller from swapping in a different payload after the
//!    timelock has elapsed.

use soroban_sdk::{contracttype, symbol_short, Bytes, BytesN, Env, Symbol};

use crate::ContractError;

/// Mandatory delay between queueing a governance action and allowing it to run.
pub const TIMELOCK_DELAY: u64 = 48 * 60 * 60;

/// Storage key under which the single queued governance action is persisted.
pub const QUEUED_ACTION_KEY: Symbol = symbol_short!("QACTION");

/// A governance action that has been queued and is awaiting its timelock window.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueuedAction {
    /// SHA-256 hash of the original proposal payload committed at queue time.
    pub payload_hash: BytesN<32>,
    /// Ledger timestamp at which the action was queued.
    pub queued_timestamp: u64,
    /// Earliest ledger timestamp at which the action may run
    /// (`queued_timestamp + TIMELOCK_DELAY`).
    pub execute_at: u64,
    /// Whether the queued action has been cancelled.
    pub cancelled: bool,
}

impl QueuedAction {
    /// Returns the SHA-256 digest of a raw proposal payload.
    pub fn hash_payload(env: &Env, payload: &Bytes) -> BytesN<32> {
        env.crypto().sha256(payload)
    }
}

/// Queue a governance action, binding it to the hash of its original payload.
///
/// Records `queued_timestamp = env.ledger().timestamp()` and computes
/// `execute_at = queued_timestamp + TIMELOCK_DELAY`. Returns the persisted
/// [`QueuedAction`].
pub fn queue_action(env: &Env, payload: &Bytes) -> Result<QueuedAction, ContractError> {
    let payload_hash = QueuedAction::hash_payload(env, payload);
    let queued_timestamp = env.ledger().timestamp();
    let execute_at = queued_timestamp
        .checked_add(TIMELOCK_DELAY)
        .ok_or(ContractError::Overflow)?;
    let action = QueuedAction {
        payload_hash,
        queued_timestamp,
        execute_at,
        cancelled: false,
    };
    env.storage().instance().set(&QUEUED_ACTION_KEY, &action);
    Ok(action)
}

/// Read the currently queued action, if any.
pub fn get_queued_action(env: &Env) -> Option<QueuedAction> {
    env.storage().instance().get(&QUEUED_ACTION_KEY)
}

/// Cancel a queued action so it can never be executed.
pub fn cancel_action(env: &Env) {
    env.storage().instance().remove(&QUEUED_ACTION_KEY);
}

/// Assert that the mandatory timelock delay has elapsed for the queued action.
///
/// Succeeds only when `current_timestamp > queued_timestamp + TIMELOCK_DELAY`
/// (i.e. `current_timestamp > execute_at`). Returns
/// [`ContractError::TimelockNotExpired`] when called prematurely.
pub fn assert_timelock_expired(
    action: &QueuedAction,
    current_timestamp: u64,
) -> Result<(), ContractError> {
    if current_timestamp <= action.execute_at {
        return Err(ContractError::TimelockNotExpired);
    }
    Ok(())
}

/// Assert that a transaction hash matches the hash of the originally queued
/// proposal payload identically.
pub fn assert_payload_matches(
    action: &QueuedAction,
    tx_hash: &BytesN<32>,
) -> Result<(), ContractError> {
    if tx_hash != &action.payload_hash {
        return Err(ContractError::PayloadHashMismatch);
    }
    Ok(())
}

/// Verify the current time exceeds the queued deadline (`>`, not `>=`).
pub fn is_timelock_expired(action: &QueuedAction, current_timestamp: u64) -> bool {
    current_timestamp > action.execute_at
}

/// Time remaining (in seconds) until the queued action may run, if a queued
/// action exists. Returns `0` once the timelock has elapsed.
pub fn timelock_remaining(action: &QueuedAction, current_timestamp: u64) -> u64 {
    action
        .execute_at
        .saturating_sub(current_timestamp)
}

/// `queued_timestamp + TIMELOCK_DELAY`, or `None` if the deadline overflows.
pub fn execution_timestamp(queued_timestamp: u64) -> Option<u64> {
    queued_timestamp.checked_add(TIMELOCK_DELAY)
}

/// Execute a queued governance action under the guard.
///
/// Combined gate invoked right before a queued action's side effects are run:
///
/// 1. loads the queued action (fails with [`ContractError::NotInitialized`] if
///    nothing is queued),
/// 2. reverts with [`ContractError::TimelockNotExpired`] if the timelock has
///    not yet elapsed,
/// 3. reverts with [`ContractError::PayloadHashMismatch`] if the supplied
///    transaction hash does not match the queued payload hash.
///
/// On success the caller is expected to perform the action's side effects and
/// then call [`clear_queued_action`].
pub fn verify_action_execution(
    env: &Env,
    tx_hash: &BytesN<32>,
) -> Result<QueuedAction, ContractError> {
    let action: QueuedAction = env
        .storage()
        .instance()
        .get(&QUEUED_ACTION_KEY)
        .ok_or(ContractError::NotInitialized)?;
    if action.cancelled {
        return Err(ContractError::NoPendingUpgrade);
    }
    assert_timelock_expired(&action, env.ledger().timestamp())?;
    assert_payload_matches(&action, tx_hash)?;
    Ok(action)
}

/// Remove the queued action from storage after it has been executed.
pub fn clear_queued_action(env: &Env) {
    env.storage().instance().remove(&QUEUED_ACTION_KEY);
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Ledger;
    use soroban_sdk::Env;

    #[test]
    fn queue_binds_payload_hash_and_deadline() {
        let env = Env::default();
        let payload = Bytes::from_slice(&env, b"governance-action-v1");
        let expected_hash = QueuedAction::hash_payload(&env, &payload);

        let action = queue_action(&env, &payload).unwrap();

        assert_eq!(action.payload_hash, expected_hash);
        assert_eq!(action.queued_timestamp, env.ledger().timestamp());
        assert_eq!(action.execute_at, env.ledger().timestamp() + TIMELOCK_DELAY);
        assert!(!action.cancelled);
    }

    #[test]
    fn execution_deadline_flows_from_queued_timestamp() {
        let env = Env::default();
        let action = QueuedAction {
            payload_hash: BytesN::from_array(&env, &[1u8; 32]),
            queued_timestamp: 1_000,
            execute_at: execution_timestamp(1_000).unwrap(),
            cancelled: false,
        };
        assert_eq!(action.execute_at, 1_000 + TIMELOCK_DELAY);
    }

    #[test]
    fn deadline_overflow_rejected() {
        assert_eq!(execution_timestamp(u64::MAX), None);
    }

    #[test]
    fn requires_timestamp_to_exceed_deadline() {
        let env = Env::default();
        let queued_timestamp = env.ledger().timestamp();
        let execute_at = queued_timestamp + TIMELOCK_DELAY;
        let action = QueuedAction {
            payload_hash: BytesN::from_array(&env, &[1u8; 32]),
            queued_timestamp,
            execute_at,
            cancelled: false,
        };

        // Premature — exactly at the deadline is still not expired.
        assert_eq!(
            assert_timelock_expired(&action, execute_at),
            Err(ContractError::TimelockNotExpired)
        );
        assert!(!is_timelock_expired(&action, execute_at));

        // One second past the deadline is expired.
        assert_eq!(assert_timelock_expired(&action, execute_at + 1), Ok(()));
        assert!(is_timelock_expired(&action, execute_at + 1));
    }

    #[test]
    fn hash_mismatch_reverts() {
        let env = Env::default();
        let queued = BytesN::from_array(&env, &[7u8; 32]);
        let offered = BytesN::from_array(&env, &[9u8; 32]);
        let action = QueuedAction {
            payload_hash: queued,
            queued_timestamp: env.ledger().timestamp(),
            execute_at: env.ledger().timestamp(),
            cancelled: false,
        };

        assert_eq!(
            assert_payload_matches(&action, &offered),
            Err(ContractError::PayloadHashMismatch)
        );
        assert_eq!(assert_payload_matches(&action, &queued), Ok(()));
    }

    #[test]
    fn verify_gates_on_timelock_then_hash() {
        let env = Env::default();
        let hash = BytesN::from_array(&env, &[3u8; 32]);

        // Nothing queued.
        assert_eq!(
            verify_action_execution(&env, &hash),
            Err(ContractError::NotInitialized)
        );

        queue_action(&env, &Bytes::from_slice(&env, b"payload")).unwrap();

        // Premature — timelock not expired wins over the hash check.
        assert_eq!(
            verify_action_execution(&env, &hash),
            Err(ContractError::TimelockNotExpired)
        );

        // Advance past the deadline.
        let now = env.ledger().timestamp();
        env.ledger().with_mut(|l| l.timestamp = now + TIMELOCK_DELAY + 1);

        // Now the hash must match the queued payload hash.
        assert_eq!(
            verify_action_execution(&env, &hash),
            Err(ContractError::PayloadHashMismatch)
        );

        let correct_hash = get_queued_action(&env).unwrap().payload_hash;
        verify_action_execution(&env, &correct_hash).unwrap();

        clear_queued_action(&env);
        assert!(get_queued_action(&env).is_none());
    }
}