//! Administrative Timelock Emergency Execution Cancellation Handler (issue #1012).
//!
//! Allows the governance Security Council to cancel a queued administrative
//! timelock proposal before it can execute. Cancellation is terminal: the
//! queued proposal hash is marked
//! [`TimelockProposalStatus::CANCELLED_BY_COUNCIL`] in persistent storage, every
//! execution entrypoint refuses to process a cancelled hash, and a
//! `TimelockProposalCancelled` audit event is emitted.
//!
//! # Flow
//!
//! 1. An administrative action is queued with `queue_admin_timelock_proposal`,
//!    keyed by the hash of its payload and stamped with `execute_not_before`.
//! 2. If the Security Council detects a malicious proposal, it calls
//!    `cancel_timelock_proposal_by_council`, which writes the terminal
//!    `CANCELLED_BY_COUNCIL` status and emits `TimelockProposalCancelled`.
//! 3. `execute_timelock_proposal` refuses to run any cancelled hash and enforces
//!    the mandatory delay for live proposals.

use soroban_sdk::{contracttype, symbol_short, Address, BytesN, Env, Symbol};

use crate::veto;
use crate::ContractError;

/// Mandatory delay applied to a queued administrative action: 48 hours.
pub const ADMIN_TIMELOCK_DELAY_SECONDS: u64 = 48 * 60 * 60;

/// Topic for the `TimelockProposalCancelled` audit event.
pub const TIMELOCK_PROPOSAL_CANCELLED_EVENT: Symbol = symbol_short!("TLPropCxl");

/// Lifecycle status of a queued administrative timelock proposal.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(non_camel_case_types)]
pub enum TimelockProposalStatus {
    /// Queued and waiting for its timelock to elapse.
    Queued,
    /// The timelock elapsed and the proposal was executed.
    Executed,
    /// Cancelled through the ordinary administrative path.
    Cancelled,
    /// Cancelled by the governance Security Council. Terminal: the proposal can
    /// never be executed.
    CANCELLED_BY_COUNCIL,
}

/// Persistent-storage keys for the cancellation handler.
#[contracttype]
#[derive(Clone)]
pub enum TimelockCancellationKey {
    /// Record for a queued proposal, keyed by the hash of its payload.
    Proposal(BytesN<32>),
    /// Running total of council cancellations (observability).
    CancellationCount,
}

/// A queued administrative action under the mandatory timelock.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedTimelockProposal {
    /// Hash identifying the queued action payload.
    pub hash: BytesN<32>,
    /// Account that queued the action.
    pub proposer: Address,
    /// Ledger timestamp at which the action was queued.
    pub queued_at: u64,
    /// Earliest ledger timestamp at which the action may execute.
    pub execute_not_before: u64,
    /// Current lifecycle status.
    pub status: TimelockProposalStatus,
}

/// Queue an administrative action under the mandatory timelock.
///
/// The `proposer` must authorise the call. Re-queuing a hash that already has a
/// live (`Queued`) proposal returns [`ContractError::AdminChangePending`]; a hash
/// whose previous proposal was cancelled or executed may be queued again.
pub fn queue_admin_timelock_proposal(
    env: &Env,
    proposer: Address,
    hash: BytesN<32>,
    delay_seconds: u64,
) -> Result<QueuedTimelockProposal, ContractError> {
    proposer.require_auth();

    let key = TimelockCancellationKey::Proposal(hash.clone());

    if let Some(existing) = env
        .storage()
        .persistent()
        .get::<_, QueuedTimelockProposal>(&key)
    {
        if existing.status == TimelockProposalStatus::Queued {
            return Err(ContractError::AdminChangePending);
        }
    }

    let queued_at = env.ledger().timestamp();
    let execute_not_before = queued_at
        .checked_add(delay_seconds)
        .ok_or(ContractError::Overflow)?;

    let proposal = QueuedTimelockProposal {
        hash,
        proposer,
        queued_at,
        execute_not_before,
        status: TimelockProposalStatus::Queued,
    };

    env.storage().persistent().set(&key, &proposal);
    Ok(proposal)
}

/// Cancel a queued administrative timelock proposal as the Security Council.
///
/// Marks the stored status `CANCELLED_BY_COUNCIL` and emits the
/// `TimelockProposalCancelled` audit event. Only the configured Security Council
/// may call this, and only while the proposal is still `Queued`.
///
/// # Errors
///
/// - [`ContractError::NotSecurityCouncil`] – caller is not the Security Council.
/// - [`ContractError::ProposalNotFound`]   – no proposal exists for `hash`.
/// - [`ContractError::ProposalNotVetoable`] – the proposal is already terminal.
pub fn cancel_timelock_proposal_by_council(
    env: &Env,
    caller: Address,
    hash: BytesN<32>,
) -> Result<(), ContractError> {
    let council = veto::get_security_council(env).ok_or(ContractError::NotSecurityCouncil)?;
    if caller != council {
        return Err(ContractError::NotSecurityCouncil);
    }
    caller.require_auth();

    let key = TimelockCancellationKey::Proposal(hash.clone());
    let mut proposal: QueuedTimelockProposal = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(ContractError::ProposalNotFound)?;

    if proposal.status != TimelockProposalStatus::Queued {
        return Err(ContractError::ProposalNotVetoable);
    }

    proposal.status = TimelockProposalStatus::CANCELLED_BY_COUNCIL;
    env.storage().persistent().set(&key, &proposal);

    let cancelled = get_cancellation_count(env).saturating_add(1);
    env.storage()
        .persistent()
        .set(&TimelockCancellationKey::CancellationCount, &cancelled);

    env.events().publish(
        (TIMELOCK_PROPOSAL_CANCELLED_EVENT, hash),
        (caller, env.ledger().timestamp()),
    );

    Ok(())
}

/// Execute a queued administrative timelock proposal once its timelock elapsed.
///
/// Refuses to process any cancelled hash, enforcing the emergency cancellation.
///
/// # Errors
///
/// - [`ContractError::ProposalNotFound`]           – no proposal exists for `hash`.
/// - [`ContractError::ProposalAlreadyVetoed`]      – the proposal was cancelled.
/// - [`ContractError::ProposalNotVetoable`]        – the proposal already executed.
/// - [`ContractError::AdminTimelockNotSatisfied`]  – the timelock has not elapsed.
pub fn execute_timelock_proposal(env: &Env, hash: BytesN<32>) -> Result<(), ContractError> {
    let key = TimelockCancellationKey::Proposal(hash);
    let mut proposal: QueuedTimelockProposal = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(ContractError::ProposalNotFound)?;

    // Block every execution path for a cancelled proposal.
    if proposal.status == TimelockProposalStatus::CANCELLED_BY_COUNCIL
        || proposal.status == TimelockProposalStatus::Cancelled
    {
        return Err(ContractError::ProposalAlreadyVetoed);
    }

    if proposal.status != TimelockProposalStatus::Queued {
        return Err(ContractError::ProposalNotVetoable);
    }

    if env.ledger().timestamp() < proposal.execute_not_before {
        return Err(ContractError::AdminTimelockNotSatisfied);
    }

    proposal.status = TimelockProposalStatus::Executed;
    env.storage().persistent().set(&key, &proposal);
    Ok(())
}

/// Read a queued proposal by hash.
pub fn get_timelock_proposal(env: &Env, hash: BytesN<32>) -> Option<QueuedTimelockProposal> {
    env.storage()
        .persistent()
        .get(&TimelockCancellationKey::Proposal(hash))
}

/// Read only the status of a queued proposal.
pub fn get_timelock_proposal_status(env: &Env, hash: BytesN<32>) -> Option<TimelockProposalStatus> {
    get_timelock_proposal(env, hash).map(|proposal| proposal.status)
}

/// Number of proposals cancelled by the Security Council so far.
pub fn get_cancellation_count(env: &Env) -> u64 {
    env.storage()
        .persistent()
        .get(&TimelockCancellationKey::CancellationCount)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContractData, DATA_KEY};
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::{BytesN, Env, TryFromVal};

    fn setup() -> (Env, Address, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let admin = Address::generate(&env);
        let council = Address::generate(&env);

        env.as_contract(&contract_id, || {
            let data = ContractData {
                admin: admin.clone(),
                value: 0,
                max_fee_ceiling: 10_000,
            };
            env.storage().instance().set(&DATA_KEY, &data);
            veto::set_security_council(&env, admin.clone(), council.clone()).unwrap();
        });

        (env, contract_id, admin, council)
    }

    fn hash(env: &Env, byte: u8) -> BytesN<32> {
        BytesN::from_array(env, &[byte; 32])
    }

    fn cancellation_event_emitted(env: &Env) -> bool {
        let events = env.events().all();
        for i in 0..events.len() {
            let (_, topics, _) = events.get(i).unwrap();
            if topics.len() != 2 {
                continue;
            }
            if let Ok(symbol) = Symbol::try_from_val(env, &topics.get(0).unwrap()) {
                if symbol == TIMELOCK_PROPOSAL_CANCELLED_EVENT {
                    return true;
                }
            }
        }
        false
    }

    #[test]
    fn council_cancel_marks_status_and_blocks_execution() {
        let (env, contract_id, admin, council) = setup();
        let proposal_hash = hash(&env, 7);

        env.as_contract(&contract_id, || {
            queue_admin_timelock_proposal(&env, admin.clone(), proposal_hash.clone(), 0).unwrap();

            cancel_timelock_proposal_by_council(&env, council.clone(), proposal_hash.clone())
                .unwrap();

            assert_eq!(
                get_timelock_proposal_status(&env, proposal_hash.clone()),
                Some(TimelockProposalStatus::CANCELLED_BY_COUNCIL)
            );
            // Cancelled proposals can never execute.
            assert_eq!(
                execute_timelock_proposal(&env, proposal_hash.clone()),
                Err(ContractError::ProposalAlreadyVetoed)
            );
            assert_eq!(get_cancellation_count(&env), 1);
        });
    }

    #[test]
    fn council_cancel_emits_audit_event() {
        let (env, contract_id, admin, council) = setup();
        let proposal_hash = hash(&env, 8);

        env.as_contract(&contract_id, || {
            queue_admin_timelock_proposal(&env, admin.clone(), proposal_hash.clone(), 0).unwrap();
            cancel_timelock_proposal_by_council(&env, council.clone(), proposal_hash.clone())
                .unwrap();

            assert!(cancellation_event_emitted(&env));
        });
    }

    #[test]
    fn non_council_cannot_cancel() {
        let (env, contract_id, _admin, _council) = setup();
        let proposal_hash = hash(&env, 9);
        let intruder = Address::generate(&env);

        env.as_contract(&contract_id, || {
            queue_admin_timelock_proposal(&env, intruder.clone(), proposal_hash.clone(), 0)
                .unwrap();

            assert_eq!(
                cancel_timelock_proposal_by_council(&env, intruder.clone(), proposal_hash.clone()),
                Err(ContractError::NotSecurityCouncil)
            );
            // A rejected cancellation must leave the proposal executable.
            assert_eq!(
                get_timelock_proposal_status(&env, proposal_hash.clone()),
                Some(TimelockProposalStatus::Queued)
            );
        });
    }

    #[test]
    fn cancel_unknown_hash_fails() {
        let (env, contract_id, _admin, council) = setup();

        env.as_contract(&contract_id, || {
            assert_eq!(
                cancel_timelock_proposal_by_council(&env, council.clone(), hash(&env, 1)),
                Err(ContractError::ProposalNotFound)
            );
        });
    }

    #[test]
    fn double_council_cancel_fails() {
        let (env, contract_id, admin, council) = setup();
        let proposal_hash = hash(&env, 2);

        env.as_contract(&contract_id, || {
            queue_admin_timelock_proposal(&env, admin.clone(), proposal_hash.clone(), 0).unwrap();
            cancel_timelock_proposal_by_council(&env, council.clone(), proposal_hash.clone())
                .unwrap();

            assert_eq!(
                cancel_timelock_proposal_by_council(&env, council.clone(), proposal_hash.clone()),
                Err(ContractError::ProposalNotVetoable)
            );
        });
    }

    #[test]
    fn execution_before_timelock_fails() {
        let (env, contract_id, admin, _council) = setup();
        let proposal_hash = hash(&env, 3);

        env.as_contract(&contract_id, || {
            queue_admin_timelock_proposal(&env, admin.clone(), proposal_hash.clone(), 100).unwrap();

            assert_eq!(
                execute_timelock_proposal(&env, proposal_hash.clone()),
                Err(ContractError::AdminTimelockNotSatisfied)
            );
            assert_eq!(
                get_timelock_proposal_status(&env, proposal_hash.clone()),
                Some(TimelockProposalStatus::Queued)
            );
        });
    }

    #[test]
    fn execution_after_timelock_succeeds() {
        let (env, contract_id, admin, _council) = setup();
        let proposal_hash = hash(&env, 4);

        env.as_contract(&contract_id, || {
            queue_admin_timelock_proposal(&env, admin.clone(), proposal_hash.clone(), 0).unwrap();

            assert_eq!(
                execute_timelock_proposal(&env, proposal_hash.clone()),
                Ok(())
            );
            assert_eq!(
                get_timelock_proposal_status(&env, proposal_hash.clone()),
                Some(TimelockProposalStatus::Executed)
            );
        });
    }
}
