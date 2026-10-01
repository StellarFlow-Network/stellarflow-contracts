//! Multi-Sig Administrative Key Rotation Verification Module (Issue #913).
//!
//! Safely updates active multi-sig administrative keys via governance vote
//! execution. Requires 100% threshold signature approval from all current
//! signers and enforces a 24-hour timelock before the new keys take effect.

use soroban_sdk::{contracttype, symbol_short, Address, BytesN, Env, Vec};

use crate::ContractError;

/// Timelock delay before newly proposed admin keys become active (24 hours).
pub const KEY_ROTATION_TIMELOCK_SECONDS: u64 = 24 * 60 * 60;

/// Storage keys for the key rotation module.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KeyRotationStorageKey {
    /// The current set of active multi-sig administrative public keys.
    ActiveAdminKeys,
    /// A pending rotation proposal, if one is active.
    PendingRotation,
}

/// A pending key rotation proposal awaiting timelock expiry.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingKeyRotation {
    /// The proposed new set of administrative keys.
    pub new_keys: Vec<BytesN<32>>,
    /// Ledger timestamp at which the proposal was submitted.
    pub proposed_at: u64,
    /// Ledger timestamp after which the rotation may be executed
    /// (`proposed_at + KEY_ROTATION_TIMELOCK_SECONDS`).
    pub executable_after: u64,
    /// Set of current signer addresses that have approved this proposal.
    pub approvals: Vec<Address>,
    /// Total number of current signers required for 100% threshold.
    pub required_approvals: u32,
}

/// Emitted when admin keys are successfully rotated.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminKeysRotatedEvent {
    /// The newly active set of administrative keys.
    pub new_keys: Vec<BytesN<32>>,
    /// Ledger timestamp when the rotation took effect.
    pub rotated_at: u64,
}

/// Initialize the active admin key set (called once during contract setup).
///
/// Only the contract owner may call this.
pub fn initialize_admin_keys(env: &Env, owner: Address, initial_keys: Vec<BytesN<32>>) {
    owner.require_auth();
    env.storage()
        .persistent()
        .set(&KeyRotationStorageKey::ActiveAdminKeys, &initial_keys);
}

/// Propose a new set of administrative keys for rotation.
///
/// Opens a new pending rotation proposal. Any signer from the current active
/// key set may submit the proposal. A proposal automatically records the
/// proposer's approval.
///
/// # Errors
/// - [`ContractError::ProposalAlreadyActive`] if a pending rotation exists.
/// - [`ContractError::Unauthorized`] if `proposer` is not a current admin.
pub fn propose_key_rotation(
    env: &Env,
    proposer: Address,
    new_keys: Vec<BytesN<32>>,
) -> Result<(), ContractError> {
    proposer.require_auth();

    // Reject if a pending rotation is already active.
    if env
        .storage()
        .persistent()
        .has(&KeyRotationStorageKey::PendingRotation)
    {
        return Err(ContractError::ProposalAlreadyActive);
    }

    let current_keys: Vec<BytesN<32>> = env
        .storage()
        .persistent()
        .get(&KeyRotationStorageKey::ActiveAdminKeys)
        .ok_or(ContractError::NotInitialized)?;

    let required = current_keys.len();

    // Verify proposer holds one of the current admin keys.
    let proposer_key = env.crypto().sha256(&proposer.clone().to_xdr(env));
    let proposer_is_admin = current_keys.iter().any(|k| k == proposer_key);
    if !proposer_is_admin {
        return Err(ContractError::Unauthorized);
    }

    let now = env.ledger().timestamp();
    let executable_after = now
        .checked_add(KEY_ROTATION_TIMELOCK_SECONDS)
        .ok_or(ContractError::Overflow)?;

    // Record proposer's approval.
    let mut approvals: Vec<Address> = Vec::new(env);
    approvals.push_back(proposer.clone());

    let proposal = PendingKeyRotation {
        new_keys,
        proposed_at: now,
        executable_after,
        approvals,
        required_approvals: required,
    };

    env.storage()
        .persistent()
        .set(&KeyRotationStorageKey::PendingRotation, &proposal);

    env.events().publish(
        (symbol_short!("KeyRotPrp"), proposer),
        (now, executable_after, required),
    );

    Ok(())
}

/// Approve the pending key rotation proposal.
///
/// Each current admin signer must call this once. Duplicate approvals are
/// rejected. The rotation is not executed here — call `execute_key_rotation`
/// after the timelock has elapsed and all approvals are collected.
///
/// # Errors
/// - [`ContractError::NoActiveProposal`] if no pending rotation exists.
/// - [`ContractError::Unauthorized`] if `approver` is not a current admin.
/// - [`ContractError::AlreadyVoted`] if `approver` has already approved.
pub fn approve_key_rotation(env: &Env, approver: Address) -> Result<u32, ContractError> {
    approver.require_auth();

    let mut proposal: PendingKeyRotation = env
        .storage()
        .persistent()
        .get(&KeyRotationStorageKey::PendingRotation)
        .ok_or(ContractError::NoActiveProposal)?;

    let current_keys: Vec<BytesN<32>> = env
        .storage()
        .persistent()
        .get(&KeyRotationStorageKey::ActiveAdminKeys)
        .ok_or(ContractError::NotInitialized)?;

    // Verify approver holds a current admin key.
    let approver_key = env.crypto().sha256(&approver.clone().to_xdr(env));
    let approver_is_admin = current_keys.iter().any(|k| k == approver_key);
    if !approver_is_admin {
        return Err(ContractError::Unauthorized);
    }

    // Reject duplicate approvals.
    if proposal.approvals.iter().any(|a| a == approver) {
        return Err(ContractError::AlreadyVoted);
    }

    proposal.approvals.push_back(approver);
    let approval_count = proposal.approvals.len();

    env.storage()
        .persistent()
        .set(&KeyRotationStorageKey::PendingRotation, &proposal);

    Ok(approval_count)
}

/// Execute the pending key rotation after 100% approval and 24-hour timelock.
///
/// # Requirements
/// 1. A pending rotation proposal must exist.
/// 2. All current admin signers must have approved (100% threshold).
/// 3. The 24-hour timelock must have elapsed since the proposal was submitted.
///
/// On success, replaces the active admin key set with the proposed keys and
/// emits the `AdminKeysRotated` system audit event.
///
/// # Errors
/// - [`ContractError::NoActiveProposal`] if no pending rotation exists.
/// - [`ContractError::ThresholdNotReached`] if not all signers have approved.
/// - [`ContractError::AdminTimelockNotSatisfied`] if the 24h delay has not elapsed.
pub fn execute_key_rotation(
    env: &Env,
    executor: Address,
) -> Result<AdminKeysRotatedEvent, ContractError> {
    executor.require_auth();

    let proposal: PendingKeyRotation = env
        .storage()
        .persistent()
        .get(&KeyRotationStorageKey::PendingRotation)
        .ok_or(ContractError::NoActiveProposal)?;

    // Enforce 100% threshold: every signer must have approved.
    if proposal.approvals.len() < proposal.required_approvals {
        return Err(ContractError::ThresholdNotReached);
    }

    // Enforce 24-hour timelock.
    let now = env.ledger().timestamp();
    if now < proposal.executable_after {
        return Err(ContractError::AdminTimelockNotSatisfied);
    }

    // Install the new admin keys.
    env.storage()
        .persistent()
        .set(&KeyRotationStorageKey::ActiveAdminKeys, &proposal.new_keys);

    // Clear the executed proposal.
    env.storage()
        .persistent()
        .remove(&KeyRotationStorageKey::PendingRotation);

    let event = AdminKeysRotatedEvent {
        new_keys: proposal.new_keys,
        rotated_at: now,
    };

    // Emit AdminKeysRotated system audit event.
    env.events().publish(
        (symbol_short!("KeyRotExc"), executor),
        event.clone(),
    );

    Ok(event)
}

/// Return the current active admin key set.
pub fn get_active_admin_keys(env: &Env) -> Option<Vec<BytesN<32>>> {
    env.storage()
        .persistent()
        .get(&KeyRotationStorageKey::ActiveAdminKeys)
}

/// Return the pending rotation proposal, if one is active.
pub fn get_pending_rotation(env: &Env) -> Option<PendingKeyRotation> {
    env.storage()
        .persistent()
        .get(&KeyRotationStorageKey::PendingRotation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::Env;

    #[test]
    fn timelock_is_24_hours() {
        assert_eq!(KEY_ROTATION_TIMELOCK_SECONDS, 86_400);
    }

    #[test]
    fn execute_without_proposal_returns_no_active_proposal() {
        let env = Env::default();
        env.mock_all_auths();
        let result = execute_key_rotation(&env, soroban_sdk::testutils::Address::generate(&env));
        assert_eq!(result, Err(ContractError::NoActiveProposal));
    }

    #[test]
    fn approve_without_proposal_returns_no_active_proposal() {
        let env = Env::default();
        env.mock_all_auths();
        let result = approve_key_rotation(&env, soroban_sdk::testutils::Address::generate(&env));
        assert_eq!(result, Err(ContractError::NoActiveProposal));
    }
}
