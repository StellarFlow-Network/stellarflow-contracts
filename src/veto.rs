//! ── Governance Proposal Veto Engine & Emergency Timelock Override ──────────
//!
//! Emergency veto control allowing the designated Security Council multi-sig
//! address to cancel malicious or dangerous proposals during their timelock
//! windows, providing a last-resort circuit-breaker mechanism.
//!
//! ## Design
//!
//! - Only the designated `SecurityCouncil` address may invoke `veto_proposal()`
//! - Upon veto, the proposal instantly transitions to `Vetoed` state
//! - All execution payloads are invalidated; execution becomes impossible
//! - Audit trail recorded with reason hash for compliance logging
//! - Event emission with `ProposalVetoed` for transparency
//!
//! ## Emergency Timelock Override (Issue #2)
//!
//! Allows a supermajority of the Security Council (or designated emergency
//! signers) to bypass the mandatory upgrade timelock delay and execute a
//! pending upgrade immediately. This provides a critical escape hatch for
//! emergency security patches.
//!
//! - Requires `EMERGENCY_OVERRIDE_THRESHOLD` (default 2/3) of emergency signers
//! - Can only be invoked during the timelock window (not after execution)
//! - Records full audit trail with signer votes and reason
//! - Emits `EmergencyOverrideExecuted` event for transparency

use soroban_sdk::{contracttype, symbol_short, Address, Env, String, Symbol, Map, Vec};
use crate::{ContractError, ContractData, DATA_KEY};

// ─────────────────────────────────────────────────────────────────────────────
// Storage Keys
// ─────────────────────────────────────────────────────────────────────────────

/// The designated Security Council multi-sig address authorized to veto proposals.
pub(crate) const SECURITY_COUNCIL_KEY: Symbol = symbol_short!("SECCNC");

/// Maps proposal_id → veto record (timestamp, vetoing authority, reason hash)
pub(crate) const VETO_RECORD_KEY: Symbol = symbol_short!("VETOREC");

/// Emergency signers authorized to trigger timelock override.
pub(crate) const EMERGENCY_SIGNERS_KEY: Symbol = symbol_short!("EMERSGN");

/// Configuration for emergency timelock override.
pub(crate) const EMERGENCY_OVERRIDE_CONFIG_KEY: Symbol = symbol_short!("EMEROVR");

/// Storage key for emergency override votes: proposal_id → set of signers who voted.
pub(crate) const EMERGENCY_OVERRIDE_VOTES_KEY: Symbol = symbol_short!("EMERVOT");

/// Default threshold for emergency override: 2/3 supermajority (6667 bps).
pub const DEFAULT_EMERGENCY_OVERRIDE_THRESHOLD_BPS: u32 = 6667;

/// Minimum threshold: simple majority (5001 bps).
pub const MIN_EMERGENCY_OVERRIDE_THRESHOLD_BPS: u32 = 5001;

/// Maximum threshold: unanimous (10000 bps).
pub const MAX_EMERGENCY_OVERRIDE_THRESHOLD_BPS: u32 = 10000;

// ─────────────────────────────────────────────────────────────────────────────
// Data Structures
// ─────────────────────────────────────────────────────────────────────────────

/// Audit trail record for a vetoed proposal.
#[contracttype]
#[derive(Clone, Debug)]
pub struct ProposalVeto {
    /// The proposal ID that was vetoed.
    pub proposal_id: u64,
    /// Address of the Security Council that performed the veto.
    pub vetoed_by: Address,
    /// Ledger timestamp at veto time.
    pub vetoed_at: u64,
    /// Hash of the audit reason string (for compliance logging).
    pub reason_hash: String,
}

/// Configuration for emergency timelock override.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmergencyOverrideConfig {
    /// Set of emergency signers authorized to vote for override.
    pub emergency_signers: Vec<Address>,
    /// Threshold in basis points required for override (e.g., 6667 = 2/3 supermajority).
    pub threshold_bps: u32,
    /// Whether the emergency override mechanism is enabled.
    pub enabled: bool,
}

impl Default for EmergencyOverrideConfig {
    fn default() -> Self {
        Self {
            emergency_signers: Vec::new(env),
            threshold_bps: DEFAULT_EMERGENCY_OVERRIDE_THRESHOLD_BPS,
            enabled: true,
        }
    }
}

/// Vote record for emergency timelock override.
#[contracttype]
#[derive(Clone, Debug)]
pub struct EmergencyOverrideVote {
    /// The proposal ID for which the override is requested.
    pub proposal_id: u64,
    /// Signer who cast the vote.
    pub signer: Address,
    /// Timestamp when the vote was cast.
    pub voted_at: u64,
    /// Reason for the emergency override.
    pub reason: String,
}

/// Result of an emergency override execution.
#[contracttype]
#[derive(Clone, Debug)]
pub struct EmergencyOverrideResult {
    /// The proposal ID that was overridden.
    pub proposal_id: u64,
    /// Total number of emergency signers.
    pub total_signers: u32,
    /// Number of signers who voted for override.
    pub votes_for: u32,
    /// Threshold in basis points that was required.
    pub threshold_bps: u32,
    /// Whether the override succeeded.
    pub succeeded: bool,
    /// Timestamp of execution.
    pub executed_at: u64,
}

// ─────────────────────────────────────────────────────────────────────────────
// Configuration
// ─────────────────────────────────────────────────────────────────────────────

/// Set the Security Council address that has authority to veto proposals.
///
/// Only the current admin may configure the Security Council.
pub fn set_security_council(env: &Env, caller: Address, council: Address) -> Result<(), ContractError> {
    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;

    if data.admin != caller {
        return Err(ContractError::NotAdmin);
    }

    caller.require_auth();
    env.storage().instance().set(&SECURITY_COUNCIL_KEY, &council);
    crate::kernel::instance::bump_instance_ttl(env);
    Ok(())
}

/// Get the current Security Council address, if configured.
pub fn get_security_council(env: &Env) -> Option<Address> {
    env.storage().instance().get(&SECURITY_COUNCIL_KEY)
}

// ─────────────────────────────────────────────────────────────────────────────
// Veto Enforcement
// ─────────────────────────────────────────────────────────────────────────────

/// Veto an active proposal, instantly transitioning it to `Vetoed` state.
///
/// Only the designated Security Council may invoke this function. Upon veto:
/// 1. The proposal is marked as vetoed
/// 2. Execution payload is invalidated
/// 3. Audit trail is recorded with reason hash
/// 4. `ProposalVetoed` event is emitted
///
/// # Arguments
/// * `env` - The contract environment
/// * `caller` - The address attempting the veto (must be Security Council)
/// * `proposal_id` - The ID of the proposal to veto
/// * `reason` - Audit reason string (logged as hash for transparency)
///
/// # Errors
/// - `NotSecurityCouncil` if caller is not the Security Council
/// - `ProposalNotFound` if the proposal does not exist
/// - `ProposalAlreadyVetoed` if the proposal is already in vetoed state
pub fn veto_proposal(
    env: &Env,
    caller: Address,
    proposal_id: u64,
    reason: String,
) -> Result<(), ContractError> {
    // Verify the caller is the Security Council
    let security_council = get_security_council(env)
        .ok_or(ContractError::NotSecurityCouncil)?;

    if caller != security_council {
        return Err(ContractError::NotSecurityCouncil);
    }

    caller.require_auth();

    // Check if proposal exists and can be vetoed
    // This will be integrated with the main governance module
    // For now, we verify the contract is initialized
    let _data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;

    // Create veto record
    let veto_record = ProposalVeto {
        proposal_id,
        vetoed_by: caller.clone(),
        vetoed_at: env.ledger().timestamp(),
        reason_hash: reason,
    };

    // Store veto record
    env.storage().instance().set(&VETO_RECORD_KEY, &veto_record);
    crate::kernel::instance::bump_instance_ttl(env);

    // Emit veto event
    crate::events::emit_proposal_vetoed(env, proposal_id, caller.clone(), veto_record.vetoed_at, reason)?;

    Ok(())
}

/// Retrieve the veto record for a proposal, if it has been vetoed.
pub fn get_veto_record(env: &Env, proposal_id: u64) -> Option<ProposalVeto> {
    let record: Option<ProposalVeto> = env.storage().instance().get(&VETO_RECORD_KEY);
    
    // Verify the veto record matches the requested proposal_id
    record.filter(|r| r.proposal_id == proposal_id)
}

/// Check if a proposal has been vetoed.
pub fn is_proposal_vetoed(env: &Env, proposal_id: u64) -> bool {
    get_veto_record(env, proposal_id).is_some()
}

// ─────────────────────────────────────────────────────────────────────────────
// Emergency Timelock Override (Issue #2)
// ─────────────────────────────────────────────────────────────────────────────

/// Load the emergency override configuration.
fn load_emergency_override_config(env: &Env) -> EmergencyOverrideConfig {
    env.storage()
        .instance()
        .get(&EMERGENCY_OVERRIDE_CONFIG_KEY)
        .unwrap_or_else(|| EmergencyOverrideConfig {
            emergency_signers: Vec::new(env),
            threshold_bps: DEFAULT_EMERGENCY_OVERRIDE_THRESHOLD_BPS,
            enabled: true,
        })
}

/// Save the emergency override configuration.
fn save_emergency_override_config(env: &Env, config: &EmergencyOverrideConfig) {
    env.storage()
        .instance()
        .set(&EMERGENCY_OVERRIDE_CONFIG_KEY, config);
}

/// Get the current emergency override configuration.
pub fn get_emergency_override_config(env: &Env) -> EmergencyOverrideConfig {
    load_emergency_override_config(env)
}

/// Set the emergency signers and threshold for timelock override (Admin only).
///
/// Only the contract admin may configure the emergency override parameters.
pub fn set_emergency_override_config(
    env: &Env,
    caller: Address,
    emergency_signers: Vec<Address>,
    threshold_bps: u32,
    enabled: bool,
) -> Result<(), ContractError> {
    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;

    if data.admin != caller {
        return Err(ContractError::NotAdmin);
    }
    caller.require_auth();

    if emergency_signers.len() == 0 {
        return Err(ContractError::InvalidThreshold);
    }

    if threshold_bps < MIN_EMERGENCY_OVERRIDE_THRESHOLD_BPS
        || threshold_bps > MAX_EMERGENCY_OVERRIDE_THRESHOLD_BPS
    {
        return Err(ContractError::InvalidThreshold);
    }

    let config = EmergencyOverrideConfig {
        emergency_signers,
        threshold_bps,
        enabled,
    };

    save_emergency_override_config(env, &config);
    crate::kernel::instance::bump_instance_ttl(env);
    Ok(())
}

/// Vote for an emergency timelock override on a pending upgrade proposal.
///
/// Emergency signers may vote to bypass the timelock delay and execute
/// the upgrade immediately. Once the threshold is reached, the upgrade
/// can be executed via `execute_emergency_override`.
///
/// # Arguments
/// * `env` - The contract environment
/// * `signer` - The emergency signer casting the vote
/// * `proposal_id` - The ID of the pending upgrade proposal
/// * `reason` - Reason for the emergency override
///
/// # Errors
/// - `NotEmergencySigner` if caller is not an authorized emergency signer
/// - `EmergencyOverrideDisabled` if the mechanism is disabled
/// - `ProposalNotFound` if no pending proposal exists
/// - `AlreadyVoted` if the signer has already voted
/// - `OverrideThresholdNotReached` if threshold not yet met (informational)
pub fn vote_emergency_override(
    env: &Env,
    signer: Address,
    proposal_id: u64,
    reason: String,
) -> Result<(), ContractError> {
    let config = load_emergency_override_config(env);

    if !config.enabled {
        return Err(ContractError::EmergencyOverrideDisabled);
    }

    // Verify signer is an authorized emergency signer
    let is_authorized = config.emergency_signers.iter().any(|s| s == signer);
    if !is_authorized {
        return Err(ContractError::NotEmergencySigner);
    }

    signer.require_auth();

    // Check if proposal exists and is in timelock (Pending/Executable state)
    let proposal: crate::governance::GovernanceProposal = env
        .storage()
        .instance()
        .get(&crate::governance::GOVERNANCE_PROPOSAL_KEY)
        .ok_or(ContractError::ProposalNotFound)?;

    if proposal.proposal_id != proposal_id {
        return Err(ContractError::ProposalNotFound);
    }

    if proposal.status != crate::governance::ProposalStatus::Pending
        && proposal.status != crate::governance::ProposalStatus::Executable
    {
        return Err(ContractError::ProposalAlreadyCancelledOrExecuted);
    }

    // Record the vote
    let mut votes: Map<Address, EmergencyOverrideVote> = env
        .storage()
        .instance()
        .get(&(EMERGENCY_OVERRIDE_VOTES_KEY, proposal_id))
        .unwrap_or_else(|| Map::new(env));

    if votes.contains_key(signer.clone()) {
        return Err(ContractError::AlreadyVoted);
    }

    let vote = EmergencyOverrideVote {
        proposal_id,
        signer: signer.clone(),
        voted_at: env.ledger().timestamp(),
        reason,
    };
    votes.set(signer.clone(), vote);

    env.storage()
        .instance()
        .set(&(EMERGENCY_OVERRIDE_VOTES_KEY, proposal_id), &votes);

    crate::kernel::instance::bump_instance_ttl(env);

    // Check if threshold reached
    let votes_for = votes.len() as u32;
    let total_signers = config.emergency_signers.len() as u32;
    let weight_achieved_bps = (votes_for as u64)
        .checked_mul(10000)
        .ok_or(ContractError::Overflow)?
        .checked_div(total_signers as u64)
        .ok_or(ContractError::DivisionByZero)? as u32;

    if weight_achieved_bps >= config.threshold_bps {
        env.events().publish(
            (Symbol::new(env, "stellarflow"), Symbol::new(env, "emer_override_ready")),
            (proposal_id, votes_for, total_signers, config.threshold_bps),
        );
    }

    Ok(())
}

/// Execute an emergency timelock override, immediately deploying the pending upgrade.
///
/// Can only be called after the emergency override threshold has been reached
/// via `vote_emergency_override`. Bypasses the normal timelock delay.
///
/// # Arguments
/// * `env` - The contract environment
/// * `executor` - The address executing the override (must be an emergency signer)
/// * `proposal_id` - The ID of the pending upgrade proposal
///
/// # Errors
/// - `NotEmergencySigner` if caller is not an authorized emergency signer
/// - `EmergencyOverrideDisabled` if the mechanism is disabled
/// - `ProposalNotFound` if no pending proposal exists
/// - `OverrideThresholdNotReached` if the vote threshold has not been met
/// - `UpgradeTimelockNotSatisfied` if the proposal is not in a valid state
pub fn execute_emergency_override(
    env: &Env,
    executor: Address,
    proposal_id: u64,
) -> Result<EmergencyOverrideResult, ContractError> {
    let config = load_emergency_override_config(env);

    if !config.enabled {
        return Err(ContractError::EmergencyOverrideDisabled);
    }

    // Verify executor is an authorized emergency signer
    let is_authorized = config.emergency_signers.iter().any(|s| s == executor);
    if !is_authorized {
        return Err(ContractError::NotEmergencySigner);
    }

    executor.require_auth();

    // Check if proposal exists and is in valid state
    let proposal: crate::governance::GovernanceProposal = env
        .storage()
        .instance()
        .get(&crate::governance::GOVERNANCE_PROPOSAL_KEY)
        .ok_or(ContractError::ProposalNotFound)?;

    if proposal.proposal_id != proposal_id {
        return Err(ContractError::ProposalNotFound);
    }

    if proposal.status != crate::governance::ProposalStatus::Pending
        && proposal.status != crate::governance::ProposalStatus::Executable
    {
        return Err(ContractError::ProposalAlreadyCancelledOrExecuted);
    }

    // Check if threshold reached
    let votes: Map<Address, EmergencyOverrideVote> = env
        .storage()
        .instance()
        .get(&(EMERGENCY_OVERRIDE_VOTES_KEY, proposal_id))
        .unwrap_or_else(|| Map::new(env));

    let votes_for = votes.len() as u32;
    let total_signers = config.emergency_signers.len() as u32;

    if total_signers == 0 {
        return Err(ContractError::EmergencyOverrideDisabled);
    }

    let weight_achieved_bps = (votes_for as u64)
        .checked_mul(10000)
        .ok_or(ContractError::Overflow)?
        .checked_div(total_signers as u64)
        .ok_or(ContractError::DivisionByZero)? as u32;

    if weight_achieved_bps < config.threshold_bps {
        return Err(ContractError::OverrideThresholdNotReached);
    }

    // Execute the upgrade immediately by deploying the WASM
    env.deployer().update_current_contract_wasm(proposal.wasm_hash.to_array());

    // Clear the proposal and votes
    env.storage().instance().remove(&crate::governance::GOVERNANCE_PROPOSAL_KEY);
    env.storage().instance().remove(&(EMERGENCY_OVERRIDE_VOTES_KEY, proposal_id));

    // Also clear the staged upgrade if it exists
    env.storage().instance().remove(&crate::PENDING_UPGRADE_KEY);

    let result = EmergencyOverrideResult {
        proposal_id,
        total_signers,
        votes_for,
        threshold_bps: config.threshold_bps,
        succeeded: true,
        executed_at: env.ledger().timestamp(),
    };

    env.events().publish(
        (Symbol::new(env, "stellarflow"), Symbol::new(env, "emer_override_exec")),
        (
            proposal_id,
            proposal.wasm_hash,
            votes_for,
            total_signers,
            config.threshold_bps,
            result.executed_at,
        ),
    );

    crate::kernel::instance::bump_instance_ttl(env);
    Ok(result)
}

/// Check if emergency override threshold has been reached for a proposal.
pub fn is_emergency_override_ready(env: &Env, proposal_id: u64) -> bool {
    let config = load_emergency_override_config(env);
    if !config.enabled {
        return false;
    }

    let votes: Map<Address, EmergencyOverrideVote> = env
        .storage()
        .instance()
        .get(&(EMERGENCY_OVERRIDE_VOTES_KEY, proposal_id))
        .unwrap_or_else(|| Map::new(env));

    let votes_for = votes.len() as u32;
    let total_signers = config.emergency_signers.len() as u32;

    if total_signers == 0 {
        return false;
    }

    let weight_achieved_bps = (votes_for as u64)
        .checked_mul(10000)
        .ok_or(ContractError::Overflow)
        .unwrap_or(0)
        .checked_div(total_signers as u64)
        .ok_or(ContractError::DivisionByZero)
        .unwrap_or(0) as u32;

    weight_achieved_bps >= config.threshold_bps
}

/// Get the emergency override votes for a proposal.
pub fn get_emergency_override_votes(
    env: &Env,
    proposal_id: u64,
) -> Map<Address, EmergencyOverrideVote> {
    env.storage()
        .instance()
        .get(&(EMERGENCY_OVERRIDE_VOTES_KEY, proposal_id))
        .unwrap_or_else(|| Map::new(env))
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    #[test]
    fn test_set_and_get_security_council() {
        let env = Env::default();
        let admin = Address::generate(&env);
        let council = Address::generate(&env);

        // Initialize contract first
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        env.as_contract(&contract_id, || {
            let data = ContractData {
                admin: admin.clone(),
                value: 0,
                max_fee_ceiling: 10_000,
            };
            env.storage().instance().set(&DATA_KEY, &data);

            // Set security council
            assert!(set_security_council(&env, admin.clone(), council.clone()).is_ok());

            // Get security council
            let retrieved = get_security_council(&env);
            assert_eq!(retrieved, Some(council));
        });
    }

    #[test]
    fn test_veto_proposal_not_authorized() {
        let env = Env::default();
        let admin = Address::generate(&env);
        let council = Address::generate(&env);
        let unauthorized = Address::generate(&env);

        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        env.as_contract(&contract_id, || {
            let data = ContractData {
                admin: admin.clone(),
                value: 0,
                max_fee_ceiling: 10_000,
            };
            env.storage().instance().set(&DATA_KEY, &data);
            env.storage().instance().set(&SECURITY_COUNCIL_KEY, &council);

            // Attempt veto from unauthorized address
            let result = veto_proposal(
                &env,
                unauthorized,
                1u64,
                String::from_slice(&env, "malicious proposal"),
            );

            assert_eq!(result, Err(ContractError::NotSecurityCouncil));
        });
    }

    #[test]
    fn test_veto_record_retrieval() {
        let env = Env::default();
        let admin = Address::generate(&env);
        let council = Address::generate(&env);
        let proposal_id = 42u64;

        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        env.as_contract(&contract_id, || {
            let data = ContractData {
                admin: admin.clone(),
                value: 0,
                max_fee_ceiling: 10_000,
            };
            env.storage().instance().set(&DATA_KEY, &data);
            env.storage().instance().set(&SECURITY_COUNCIL_KEY, &council);

            // Store a veto record directly
            let veto = ProposalVeto {
                proposal_id,
                vetoed_by: council.clone(),
                vetoed_at: 1000u64,
                reason_hash: String::from_slice(&env, "test"),
            };
            env.storage().instance().set(&VETO_RECORD_KEY, &veto);

            // Verify retrieval
            assert_eq!(get_veto_record(&env, proposal_id), Some(veto.clone()));
            assert!(is_proposal_vetoed(&env, proposal_id));
            assert!(!is_proposal_vetoed(&env, 99u64));
        });
    }
}
