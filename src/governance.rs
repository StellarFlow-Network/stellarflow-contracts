use soroban_sdk::{contracttype, symbol_short, Address, BytesN, Env, Map, Symbol, Vec};
use crate::{ContractData, ContractError, DATA_KEY, SIGNERS_KEY};

const BALLOT_TTL_LEDGERS: u32 = 17_280;
const BALLOT_TTL_THRESHOLD: u32 = 5_000;

pub(crate) const GOVERNANCE_UPGRADE_KEY: Symbol = symbol_short!("GOVUPG");
pub(crate) const GOVERNANCE_CONFIG_KEY: Symbol = symbol_short!("GVNCFG");
pub(crate) const SIGNER_WEIGHTS_KEY: Symbol = symbol_short!("SIGWT");
pub(crate) const QUORUM_WEIGHT_THRESHOLD_KEY: Symbol = symbol_short!("QWTH");
pub(crate) const PROPOSAL_WEIGHT_KEY: Symbol = symbol_short!("PROPWT");

#[contracttype]
#[derive(Clone)]
pub struct GovernanceConfig {
    pub quorum_threshold: u32,
}

#[contracttype]
#[derive(Clone)]
pub struct MultiSigConfig {
    /// Total weight required for quorum (N in N-of-M)
    pub required_weight: u32,
    /// Maximum weight any single signer can hold
    pub max_signer_weight: u32,
}

impl Default for MultiSigConfig {
    fn default() -> Self {
        Self {
            required_weight: 1,
            max_signer_weight: 1,
        }
    }
}
impl Default for GovernanceConfig {
    fn default() -> Self {
        Self { quorum_threshold: 2 }
    }
}

/// Proposal state enumeration for governance lifecycle management.
///
/// Proposals transition through states as they move through voting, approval,
/// and execution phases. The `Vetoed` state is terminal and prevents execution.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProposalState {
    /// Proposal has been created and is awaiting voting.
    Pending,
    /// Proposal is currently in the voting/discussion phase.
    Active,
    /// Proposal has been approved by the required threshold and awaits execution.
    Approved,
    /// Proposal was rejected during voting (failed to reach threshold).
    Rejected,
    /// Proposal has been executed and is complete.
    Executed,
    /// Proposal was vetoed by the Security Council (terminal state).
    Vetoed,
}

/// Get multi-signature weight configuration for WASM upgrade governance
pub fn get_multisig_config(env: &Env) -> MultiSigConfig {
    env.storage()
        .instance()
        .get(&QUORUM_WEIGHT_THRESHOLD_KEY)
        .unwrap_or_default()
}

/// Set multi-signature weight configuration for WASM upgrade governance
pub fn set_multisig_config(env: &Env, config: &MultiSigConfig) {
    env.storage()
        .instance()
        .set(&QUORUM_WEIGHT_THRESHOLD_KEY, config);
}

/// Get the weight for a specific signer (returns 0 if signer not registered)
pub fn get_signer_weight(env: &Env, signer: &Address) -> u32 {
    let weights: Map<Address, u32> = env
        .storage()
        .instance()
        .get(&SIGNER_WEIGHTS_KEY)
        .unwrap_or_else(|| Map::new(env));
    weights.get(signer.clone()).unwrap_or(0u32)
}

/// Register or update a signer's weight in multi-sig governance
pub fn set_signer_weight(env: &Env, signer: &Address, weight: u32) {
    let mut weights: Map<Address, u32> = env
        .storage()
        .instance()
        .get(&SIGNER_WEIGHTS_KEY)
        .unwrap_or_else(|| Map::new(env));
    if weight == 0 {
        weights.remove(signer.clone());
    } else {
        weights.set(signer.clone(), weight);
    }
    env.storage()
        .instance()
        .set(&SIGNER_WEIGHTS_KEY, &weights);
}
pub fn get_governance_config(env: &Env) -> GovernanceConfig {
    env.storage()
        .instance()
        .get(&GOVERNANCE_CONFIG_KEY)
        .unwrap_or_default()
}

pub fn set_governance_config(env: &Env, config: &GovernanceConfig) {
    env.storage().instance().set(&GOVERNANCE_CONFIG_KEY, config);
}

pub fn verify_upgrade_quorum(env: &Env, signers: &Vec<Address>) -> Result<(), ContractError> {
    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;

    let authorized_signers: Map<Address, ()> = env
        .storage()
        .instance()
        .get(&SIGNERS_KEY)
        .unwrap_or_else(|| Map::new(env));

    let config = get_governance_config(env);
    let multisig_config = get_multisig_config(env);
    
    // Legacy count-based check
    let mut valid_count: u32 = 0;
    let mut collected_weight: u32 = 0;
    let mut seen_signers: Map<Address, ()> = Map::new(env);
    
    for signer in signers.iter() {
        // Skip duplicate signers
        if seen_signers.contains_key(signer.clone()) {
            continue;
        }
        seen_signers.set(signer.clone(), ());
        
        // Check if signer is authorized (admin or in authorized_signers)
        let is_authorized = signer == data.admin || authorized_signers.contains_key(signer.clone());
        if !is_authorized {
            continue;
        }
        
        valid_count += 1;
        
        // Get weight for this signer (admin gets weight 1 if not explicitly set)
        let weight = if signer == data.admin {
            get_signer_weight(env, &data.admin).max(1u32)
        } else {
            get_signer_weight(env, &signer)
        };
        
        collected_weight = collected_weight.checked_add(weight)
            .ok_or(ContractError::Overflow)?;
    }

    // Fail if count-based quorum not met
    if valid_count < config.quorum_threshold {
        return Err(ContractError::ThresholdNotReached);
    }
    
    // Fail if weight-based quorum not met
    if collected_weight < multisig_config.required_weight {
        return Err(ContractError::ThresholdNotReached);
    }
    
    Ok(())
}

#[contracttype]
#[derive(Clone)]
pub struct StagedUpgrade {
    pub new_wasm_hash: BytesN<32>,
    pub proposer: Address,
    pub staged_at: u64,
    /// Earliest ledger timestamp at which the replacement may execute.
    pub execute_at: u64,
}

#[contracttype]
#[derive(Clone)]
pub struct GovernanceUpgradeProposal {
    pub new_wasm_hash: BytesN<32>,
    pub proposer: Address,
    pub staged_at: u64,
    pub signers: Vec<Address>,
}

/// Event emitted when a governance upgrade is proposed
pub fn calculate_collected_weight(env: &Env, signers: &Vec<Address>, data: &ContractData) -> Result<u32, ContractError> {
    let authorized_signers: Map<Address, ()> = env
        .storage()
        .instance()
        .get(&SIGNERS_KEY)
        .unwrap_or_else(|| Map::new(env));
    
    let mut collected_weight: u32 = 0;
    let mut seen_signers: Map<Address, ()> = Map::new(env);
    
    for signer in signers.iter() {
        // Skip duplicate signers
        if seen_signers.contains_key(signer.clone()) {
            continue;
        }
        seen_signers.set(signer.clone(), ());
        
        // Check if signer is authorized
        let is_authorized = signer == data.admin || authorized_signers.contains_key(signer.clone());
        if !is_authorized {
            continue;
        }
        
        // Get weight for this signer (admin gets weight 1 if not explicitly set)
        let weight = if signer == data.admin {
            get_signer_weight(env, &data.admin).max(1u32)
        } else {
            get_signer_weight(env, &signer)
        };
        
        collected_weight = collected_weight.checked_add(weight)
            .ok_or(ContractError::Overflow)?;
    }
    
    Ok(collected_weight)
}
#[contracttype]
#[derive(Clone)]
pub struct GovernanceUpgradeProposedEvent {
    pub new_wasm_hash: BytesN<32>,
    pub proposer: Address,
    pub signers: Vec<Address>,
    pub staged_at: u64,
    pub required_weight: u32,
    pub collected_weight: u32,
}
pub fn verify_staged_delay(staged_at: u64, current_time: u64, delay_seconds: u64) -> bool {
    current_time.saturating_sub(staged_at) >= delay_seconds
}

#[contracttype]
pub enum BallotKey {
    Proposal(Symbol),
}

#[contracttype]
#[derive(Clone)]
pub struct VotingBallot {
    pub target: Address,
    pub replacement: Address,
    pub proposer: Address,
    pub proposed_at: u64,
    pub votes: Map<Address, ()>,
}

pub fn open_ballot(
    env: &Env,
    proposal_id: Symbol,
    target: Address,
    replacement: Address,
    proposer: Address,
) -> Result<(), ContractError> {
    let key = BallotKey::Proposal(proposal_id);
    if env.storage().temporary().has(&key) {
        return Err(ContractError::ProposalAlreadyActive);
    }
    let ballot = VotingBallot {
        target,
        replacement,
        proposer,
        proposed_at: env.ledger().timestamp(),
        votes: Map::new(env),
    };
    env.storage().temporary().set(&key, &ballot);
    env.storage().temporary().extend_ttl(&key, BALLOT_TTL_THRESHOLD, BALLOT_TTL_LEDGERS);
    crate::instance::bump_instance_ttl(env);
    Ok(())
}

pub fn cast_vote(
    env: &Env,
    proposal_id: Symbol,
    voter: Address,
) -> Result<VotingBallot, ContractError> {
    let key = BallotKey::Proposal(proposal_id);
    let mut ballot: VotingBallot = env
        .storage()
        .temporary()
        .get(&key)
        .ok_or(ContractError::NoActiveProposal)?;
    if ballot.votes.contains_key(voter.clone()) {
        return Err(ContractError::AlreadyVoted);
    }
    ballot.votes.set(voter, ());
    env.storage().temporary().set(&key, &ballot);
    env.storage().temporary().extend_ttl(&key, BALLOT_TTL_THRESHOLD, BALLOT_TTL_LEDGERS);
    crate::instance::bump_instance_ttl(env);
    Ok(ballot)
}

pub fn get_ballot(env: &Env, proposal_id: Symbol) -> Option<VotingBallot> {
    env.storage().temporary().get(&BallotKey::Proposal(proposal_id))
}

pub fn close_ballot(env: &Env, proposal_id: Symbol) {
    env.storage().temporary().remove(&BallotKey::Proposal(proposal_id));
    crate::instance::bump_instance_ttl(env);
}

pub fn verify_block_height(target_height: u32, active_index: u32) -> bool {
    target_height > active_index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verify_block_height() {
        assert!(verify_block_height(101, 100));
        assert!(!verify_block_height(100, 100));
        assert!(!verify_block_height(99, 100));
    }
}
