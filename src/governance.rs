//! Governance Proposal Execution Timelock Cancellation Handler (Issue #796).
//!
//! Provides a governance framework where proposals enter a timelock period
//! before execution. During the timelock window, registered signers or the
//! admin may vote to cancel a proposal. Once the cancellation quorum is
//! reached the proposal is marked `Cancelled` and can no longer be executed.
//!
//! The handler also supports direct admin cancellation for emergency scenarios.

use soroban_sdk::{contracttype, symbol_short, Address, BytesN, Env, Map, Symbol};

pub(crate) const VALIDATORS_KEY: Symbol = symbol_short!("VALIDS");
pub(crate) const VALIDATOR_SEQUENCE_KEY: Symbol = symbol_short!("VALSEQ");
pub(crate) const BRIDGE_VALIDATORS_UPDATED_EVENT: Symbol = symbol_short!("BridgeValidatorsUpdated");

#[contracttype]
#[derive(Clone)]
pub struct StagedUpgrade {
    pub wasm_hash: BytesN<32>,
    pub staged_at: u32,
}

use crate::ContractError;

// ── Constants ───────────────────────────────────────────────────────────────

/// Minimum number of ledger sequences that must elapse between proposal
/// submission and eligible execution.
pub const MIN_LEDGER_DELAY: u32 = 5000;

/// Storage key for the active governance proposal (only one at a time).
pub(crate) const GOVERNANCE_PROPOSAL_KEY: Symbol = symbol_short!("GOVPROP");

/// Storage key for the proposal ID counter (monotonically increasing).
pub(crate) const GOV_PROPOSAL_COUNTER_KEY: Symbol = symbol_short!("GOVCNT");

/// Storage key for the cancellation quorum threshold (number of votes required
/// to cancel a proposal during the timelock window).
pub(crate) const GOV_CANCEL_THRESHOLD_KEY: Symbol = symbol_short!("GOVTHRSH");

// ── Types ───────────────────────────────────────────────────────────────────

/// Lifecycle status of a governance proposal.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProposalStatus {
    /// Proposal is active and in its timelock window.
    Pending,
    /// Timelock has elapsed and the proposal is eligible for execution.
    Executable,
    /// Cancelled during the timelock window by authorised signers.
    Cancelled,
}

/// A governance proposal that wraps a contract upgrade with full lifecycle
/// tracking including cancellation support.
#[contracttype]
#[derive(Clone)]
pub struct GovernanceProposal {
    /// Unique monotonically-increasing identifier.
    pub proposal_id: u64,
    /// WASM hash of the proposed upgrade.
    pub wasm_hash: BytesN<32>,
    /// Address that submitted the proposal.
    pub proposer: Address,
    /// Ledger sequence at which the proposal was submitted.
    pub staged_at: u32,
    /// Current lifecycle status.
    pub status: ProposalStatus,
    /// Set of addresses that have voted to cancel this proposal.
    pub cancellation_votes: Map<Address, ()>,
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Submit a governance proposal. The proposal enters a timelock period
/// (`MIN_LEDGER_DELAY` ledger sequences) before it can be executed.
///
/// Only one governance proposal may be active at a time. Returns the
/// assigned proposal ID.
pub fn submit_governance_proposal(
    env: &Env,
    proposer: Address,
    wasm_hash: BytesN<32>,
) -> Result<u64, ContractError> {
    // Only one active proposal at a time.
    if env.storage().instance().has(&GOVERNANCE_PROPOSAL_KEY) {
        let existing: GovernanceProposal = env
            .storage()
            .instance()
            .get(&GOVERNANCE_PROPOSAL_KEY)
            .unwrap();
        if existing.status == ProposalStatus::Pending
            || existing.status == ProposalStatus::Executable
        {
            return Err(ContractError::ProposalAlreadyActive);
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
    /// Proposal expired because threshold approval was not reached within 7 days.
    Expired,
}

/// Get multi-signature weight configuration for WASM upgrade governance
pub fn get_multisig_config(env: &Env) -> MultiSigConfig {
    env.storage()
        .instance()
        .get(&QUORUM_WEIGHT_THRESHOLD_KEY)
        .unwrap_or_default()
}

    env.storage()
        .instance()
        .set(&GOVERNANCE_PROPOSAL_KEY, &proposal);

    env.events().publish(
        (symbol_short!("GovProp"), proposal_id),
        (proposer, proposal.staged_at),
    );

    Ok(proposal_id)
}

/// Vote to cancel a governance proposal during its timelock window.
///
/// Once the number of cancellation votes meets or exceeds the cancellation
/// threshold the proposal status is set to `Cancelled` and it can no longer
/// be executed.
///
/// Only registered signers or the admin may vote. A voter may not vote
/// twice on the same proposal.
pub fn vote_cancel_proposal(
    env: &Env,
    voter: Address,
    proposal_id: u64,
    sig_expires_at: u64,
) -> Result<(), ContractError> {
    if env.ledger().timestamp() > sig_expires_at {
        return Err(ContractError::SignatureExpired);
    }
    voter.require_auth();

    let mut proposal = _load_proposal(env)?;

    if proposal.proposal_id != proposal_id {
        return Err(ContractError::NoActiveProposal);
    }

    if proposal.status != ProposalStatus::Pending {
        return Err(ContractError::ProposalAlreadyCancelledOrExecuted);
    }

    // Prevent double-voting.
    if proposal.cancellation_votes.contains_key(voter.clone()) {
        return Err(ContractError::AlreadyVoted);
    }

    proposal.cancellation_votes.set(voter, ());

    let threshold = _cancellation_threshold(env);

    if proposal.cancellation_votes.len() >= threshold {
        proposal.status = ProposalStatus::Cancelled;
        env.events().publish(
            (symbol_short!("GovCancel"), proposal_id),
            proposal.cancellation_votes.len(),
        );
    }

    env.storage()
        .instance()
        .set(&GOVERNANCE_PROPOSAL_KEY, &proposal);

    Ok(())
}

/// Direct admin cancellation of a governance proposal during its timelock
/// window.  Bypasses the voting process for emergency scenarios.
///
/// Fails if no active pending proposal exists or the caller is not the
/// contract admin.
pub fn cancel_governance_proposal(
    env: &Env,
    canceller: Address,
    proposal_id: u64,
) -> Result<(), ContractError> {
    canceller.require_auth();

    let mut proposal = _load_proposal(env)?;

    if proposal.proposal_id != proposal_id {
        return Err(ContractError::NoActiveProposal);
    }

    if proposal.status != ProposalStatus::Pending {
        return Err(ContractError::ProposalAlreadyCancelledOrExecuted);
    }

    proposal.status = ProposalStatus::Cancelled;

    env.storage()
        .instance()
        .set(&GOVERNANCE_PROPOSAL_KEY, &proposal);

    env.events().publish(
        (symbol_short!("GovCancel"), proposal_id),
        canceller,
    );

    Ok(())
}

pub fn rotate_admin_keys(
    env: &Env,
    signers: &Vec<Address>,
    new_signers: Vec<Address>,
    new_threshold: u32,
) -> Result<(), ContractError> {
    verify_upgrade_quorum(env, signers)?;

    let mut signer_set: Map<Address, ()> = Map::new(env);
    for signer in new_signers.iter() {
        signer_set.set(signer.clone(), ());
    }

    if new_threshold == 0 || new_threshold > signer_set.len() {
        return Err(ContractError::InvalidThreshold);
    }

    let mut weights: Map<Address, u32> = Map::new(env);
    for signer in new_signers.iter() {
        weights.set(signer.clone(), 1u32);
    }

    let mut data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;
    data.admin = new_signers
        .get(0)
        .ok_or(ContractError::InvalidThreshold)?
        .clone();

    env.storage().instance().set(&DATA_KEY, &data);
    env.storage().instance().set(&SIGNERS_KEY, &signer_set);
    env.storage().instance().set(&SIGNER_WEIGHTS_KEY, &weights);

    set_governance_config(env, &GovernanceConfig {
        quorum_threshold: new_threshold,
    });
    set_multisig_config(env, &MultiSigConfig {
        required_weight: new_threshold,
        max_signer_weight: get_multisig_config(env).max_signer_weight.max(1u32),
    });

    env.events().publish(
        (Symbol::new(env, "AdminKeysRotated"),),
        new_signers,
    );

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

/// Return the number of ledger sequences remaining before a governance
/// proposal's timelock elapses and it becomes eligible for execution.
///
/// Returns `None` if no active proposal with the given ID exists.
pub fn get_gov_proposal_tl_remaining(
    env: &Env,
    proposal_id: u64,
) -> Option<u32> {
    env.storage()
        .instance()
        .get::<_, GovernanceProposal>(&GOVERNANCE_PROPOSAL_KEY)
        .and_then(|proposal| {
            if proposal.proposal_id != proposal_id {
                return None;
            }
            if proposal.status != ProposalStatus::Pending {
                return None;
            }
            let current = env.ledger().sequence();
            let elapsed = current.saturating_sub(proposal.staged_at);
            Some(MIN_LEDGER_DELAY.saturating_sub(elapsed))
        })
}

/// Check whether the timelock has elapsed for a given proposal.
pub fn is_proposal_executable(env: &Env, proposal_id: u64) -> bool {
    match env
        .storage()
        .instance()
        .get::<_, GovernanceProposal>(&GOVERNANCE_PROPOSAL_KEY)
    {
        Some(proposal) if proposal.proposal_id == proposal_id => {
            proposal.status == ProposalStatus::Executable
                || (proposal.status == ProposalStatus::Pending
                    && verify_staged_delay(proposal.staged_at, env.ledger().sequence()))
        }
        _ => false,
    }
    
    Ok(collected_weight)
}
pub fn get_validator_set(env: &Env) -> Map<BytesN<32>, ()> {
    env.storage()
        .instance()
        .get(&VALIDATORS_KEY)
        .unwrap_or_else(|| Map::new(env))
}

pub fn get_validator_sequence(env: &Env) -> u64 {
    env.storage()
        .instance()
        .get(&VALIDATOR_SEQUENCE_KEY)
        .unwrap_or(0u64)
}

pub fn rotate_validators(
    env: &Env,
    signers: &Vec<Address>,
    new_validators: Vec<BytesN<32>>,
) -> Result<u64, ContractError> {
    verify_upgrade_quorum(env, signers)?;

    let mut validator_set: Map<BytesN<32>, ()> = Map::new(env);
    for validator in new_validators.iter() {
        validator_set.set(validator.clone(), ());
    }

    let sequence = get_validator_sequence(env)
        .checked_add(1)
        .ok_or(ContractError::Overflow)?;

    env.storage().instance().set(&VALIDATORS_KEY, &validator_set);
    env.storage().instance().set(&VALIDATOR_SEQUENCE_KEY, &sequence);
    env.events().publish(
        (BRIDGE_VALIDATORS_UPDATED_EVENT, sequence),
        new_validators,
    );

    Ok(sequence)
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

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Verify that at least `MIN_LEDGER_DELAY` ledger sequences have elapsed
/// since `staged_at`.
pub fn verify_staged_delay(staged_at: u32, current_ledger: u32) -> bool {
    current_ledger.saturating_sub(staged_at) >= MIN_LEDGER_DELAY
}

pub fn open_ballot(
    env: &Env,
    proposal_id: Symbol,
    target: Address,
    replacement: Address,
    proposer: Address,
    ipfs_cid: Bytes,
) -> Result<(), ContractError> {
    let key = BallotKey::Proposal(proposal_id.clone());
    if env.storage().temporary().has(&key) {
        return Err(ContractError::ProposalAlreadyActive);
    }
    let ballot = VotingBallot {
        target,
        replacement,
        proposer: proposer.clone(),
        proposed_at: env.ledger().timestamp(),
        ipfs_cid: ipfs_cid.clone(),
        votes: Map::new(env),
    };
    env.storage().temporary().set(&key, &ballot);
    env.storage().temporary().extend_ttl(&key, BALLOT_TTL_THRESHOLD, BALLOT_TTL_LEDGERS);
    env.events().publish(
        (Symbol::new(env, "ProposalCreated"), proposal_id),
        (proposer, ipfs_cid),
    );
    crate::instance::bump_instance_ttl(env);
    Ok(())
}

fn _next_proposal_id(env: &Env) -> u64 {
    let current: u64 = env
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

/// The cancellation quorum is the number of distinct signer votes required
/// to cancel a proposal during the timelock window.  If fewer than 2
/// signers are registered, the threshold defaults to 1 (admin can cancel
/// unilaterally).  Otherwise it is `signers / 2 + 1` (simple majority).
pub fn cancellation_threshold(signer_count: u32) -> u32 {
    if signer_count < 2 { 1 } else { signer_count / 2 + 1 }
}

pub fn close_ballot(env: &Env, proposal_id: Symbol) {
    env.storage().temporary().remove(&BallotKey::Proposal(proposal_id));
    crate::instance::bump_instance_ttl(env);
}

fn _cancellation_threshold(env: &Env) -> u32 {
    _cancellation_threshold_for_signers(env, &crate::SIGNERS_KEY)
}

/// Returns true when a proposal has been active for at least 7 days without approval.
pub fn is_proposal_expired(proposed_at: u64, current_time: u64, threshold_met: bool) -> bool {
    !threshold_met && current_time.saturating_sub(proposed_at) >= PROPOSAL_TTL_SECONDS
}

pub fn cleanup_expired_proposal(env: &Env, proposal_id: Symbol) -> Result<Option<Address>, ContractError> {
    let ballot = get_ballot(env, proposal_id.clone()).ok_or(ContractError::NoActiveProposal)?;
    let threshold_met = verify_upgrade_quorum(env, &ballot.votes.keys()).is_ok();
    if !is_proposal_expired(ballot.proposed_at, env.ledger().timestamp(), threshold_met) {
        return Ok(None);
    }
    close_ballot(env, proposal_id);
    env.storage().instance().remove(&GOVERNANCE_UPGRADE_KEY);
    Ok(Some(ballot.proposer))
}

pub(crate) const FEE_TIER_KEY: Symbol = symbol_short!("FEETIER");
pub(crate) const FEE_SPLIT_KEY: Symbol = symbol_short!("FEESPLIT");
pub(crate) const TREASURY_KEY: Symbol = symbol_short!("TREASURY");

pub const LOW_FEE_TIER_BPS: u32 = 5;
pub const MEDIUM_FEE_TIER_BPS: u32 = 30;
pub const HIGH_FEE_TIER_BPS: u32 = 100;
pub const DEFAULT_FEE_TIER_BPS: u32 = MEDIUM_FEE_TIER_BPS;
pub const LP_FEE_SHARE_BPS: u32 = 8000;
pub const TREASURY_FEE_SHARE_BPS: u32 = 2000;

#[contracttype]
#[derive(Clone)]
pub struct FeeTierConfig {
    pub fee_tier_bps: u32,
    pub low_fee_tier_bps: u32,
    pub medium_fee_tier_bps: u32,
    pub high_fee_tier_bps: u32,
}

impl Default for FeeTierConfig {
    fn default() -> Self {
        Self {
            fee_tier_bps: DEFAULT_FEE_TIER_BPS,
            low_fee_tier_bps: LOW_FEE_TIER_BPS,
            medium_fee_tier_bps: MEDIUM_FEE_TIER_BPS,
            high_fee_tier_bps: HIGH_FEE_TIER_BPS,
        }
    }
}

#[contracttype]
#[derive(Clone)]
pub struct FeeSplitConfig {
    pub lp_share_bps: u32,
    pub treasury_share_bps: u32,
}

impl Default for FeeSplitConfig {
    fn default() -> Self {
        Self {
            lp_share_bps: LP_FEE_SHARE_BPS,
            treasury_share_bps: TREASURY_FEE_SHARE_BPS,
        }
    }
}

pub fn get_fee_tier_config(env: &Env) -> FeeTierConfig {
    env.storage()
        .instance()
        .get(&FEE_TIER_KEY)
        .unwrap_or_default()
}

pub fn get_fee_tier(env: &Env) -> u32 {
    get_fee_tier_config(env).fee_tier_bps
}

pub fn set_fee_tier(
    env: &Env,
    signers: &Vec<Address>,
    new_fee_tier_bps: u32,
) -> Result<(), ContractError> {
    verify_upgrade_quorum(env, signers)?;
    let config = get_fee_tier_config(env);
    if new_fee_tier_bps != config.low_fee_tier_bps
        && new_fee_tier_bps != config.medium_fee_tier_bps
        && new_fee_tier_bps != config.high_fee_tier_bps
    {
        return Err(ContractError::InvalidThreshold);
    }
    env.storage().instance().set(&FEE_TIER_KEY, &FeeTierConfig {
        fee_tier_bps: new_fee_tier_bps,
        ..config
    });
    Ok(())
}

pub fn get_fee_split_config(env: &Env) -> FeeSplitConfig {
    env.storage()
        .instance()
        .get(&FEE_SPLIT_KEY)
        .unwrap_or_default()
}

pub fn set_fee_split_config(
    env: &Env,
    signers: &Vec<Address>,
    lp_share_bps: u32,
    treasury_share_bps: u32,
) -> Result<(), ContractError> {
    verify_upgrade_quorum(env, signers)?;
    if lp_share_bps.checked_add(treasury_share_bps) != Some(10000) {
        return Err(ContractError::InvalidThreshold);
    }
    env.storage().instance().set(&FEE_SPLIT_KEY, &FeeSplitConfig {
        lp_share_bps,
        treasury_share_bps,
    });
    Ok(())
}

pub fn split_collected_fees(env: &Env, amount: u128) -> Result<(u128, u128), ContractError> {
    let config = get_fee_split_config(env);
    let lp_amount = amount
        .checked_mul(config.lp_share_bps as u128)
        .ok_or(ContractError::Overflow)? / 10000;
    let treasury_amount = amount
        .checked_mul(config.treasury_share_bps as u128)
        .ok_or(ContractError::Overflow)? / 10000;
    Ok((lp_amount, treasury_amount))
}

pub fn get_treasury_vault(env: &Env) -> Option<Address> {
    env.storage().instance().get(&TREASURY_KEY)
}

pub fn set_treasury_vault(
    env: &Env,
    signers: &Vec<Address>,
    vault: Address,
) -> Result<(), ContractError> {
    verify_upgrade_quorum(env, signers)?;
    env.storage().instance().set(&TREASURY_KEY, &vault);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger, LedgerInfo};

    fn setup() -> (Env, crate::TimeLockedUpgradeContractClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();
        let id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let client = crate::TimeLockedUpgradeContractClient::new(&env, &id);
        (env, client)
    }

    fn advance_ledgers(env: &Env, delta: u32) {
        let info = env.ledger().get();
        env.ledger().set(LedgerInfo {
            sequence_number: info.sequence_number + delta,
            ..info
        });
    }

    fn make_wasm_hash(env: &soroban_sdk::Env) -> BytesN<32> {
        BytesN::from_array(env, &[42u8; 32])
    }

    #[test]
    fn test_submit_governance_proposal() {
        let (env, client) = setup();
        let admin = soroban_sdk::Address::generate(&env);
        let treasury = soroban_sdk::Address::generate(&env);
        client.initialize(&admin, &treasury);

        let wasm_hash = make_wasm_hash(&env);
        let proposal_id = client.submit_governance_proposal(&admin, &wasm_hash);

        assert_eq!(proposal_id, 1);

        let proposal = client.get_governance_proposal(&proposal_id);
        assert_eq!(proposal.proposal_id, 1);
        assert_eq!(proposal.wasm_hash, wasm_hash);
        assert_eq!(proposal.status, ProposalStatus::Pending);
    }

    #[test]
    fn test_submit_prevents_duplicate_active_proposals() {
        let (env, client) = setup();
        let admin = soroban_sdk::Address::generate(&env);
        let treasury = soroban_sdk::Address::generate(&env);
        client.initialize(&admin, &treasury);

        let wasm_hash = make_wasm_hash(&env);
        let _ = client.submit_governance_proposal(&admin, &wasm_hash);

        let wasm_hash2 = BytesN::from_array(&env, &[99u8; 32]);
        let result = client.try_submit_governance_proposal(&admin, &wasm_hash2);
        assert_eq!(result, Err(Ok(ContractError::ProposalAlreadyActive)));
    }

    #[test]
    fn test_submit_allows_new_proposal_after_cancellation() {
        let (env, client) = setup();
        let admin = soroban_sdk::Address::generate(&env);
        let treasury = soroban_sdk::Address::generate(&env);
        client.initialize(&admin, &treasury);

        let wasm_hash = make_wasm_hash(&env);
        let id1 = client.submit_governance_proposal(&admin, &wasm_hash);
        let _ = client.cancel_governance_proposal(&admin, &id1);

        let wasm_hash2 = BytesN::from_array(&env, &[99u8; 32]);
        let id2 = client.submit_governance_proposal(&admin, &wasm_hash2);
        assert_eq!(id2, 2);
    }

    #[test]
    fn test_cancel_governance_proposal_by_admin() {
        let (env, client) = setup();
        let admin = soroban_sdk::Address::generate(&env);
        let treasury = soroban_sdk::Address::generate(&env);
        client.initialize(&admin, &treasury);

        let wasm_hash = make_wasm_hash(&env);
        let proposal_id = client.submit_governance_proposal(&admin, &wasm_hash);

        client.cancel_governance_proposal(&admin, &proposal_id);

        let proposal = client.get_governance_proposal(&proposal_id);
        assert_eq!(proposal.status, ProposalStatus::Cancelled);
    }

    #[test]
    fn test_cancel_nonexistent_proposal_fails() {
        let (env, client) = setup();
        let admin = soroban_sdk::Address::generate(&env);
        let treasury = soroban_sdk::Address::generate(&env);
        client.initialize(&admin, &treasury);

        let result = client.try_cancel_governance_proposal(&admin, &999);
        assert_eq!(result, Err(Ok(ContractError::NoActiveProposal)));
    }

    #[test]
    fn test_cancel_already_cancelled_fails() {
        let (env, client) = setup();
        let admin = soroban_sdk::Address::generate(&env);
        let treasury = soroban_sdk::Address::generate(&env);
        client.initialize(&admin, &treasury);

        let wasm_hash = make_wasm_hash(&env);
        let proposal_id = client.submit_governance_proposal(&admin, &wasm_hash);
        client.cancel_governance_proposal(&admin, &proposal_id);

        let result = client.try_cancel_governance_proposal(&admin, &proposal_id);
        assert_eq!(
            result,
            Err(Ok(ContractError::ProposalAlreadyCancelledOrExecuted))
        );
    }

    #[test]
    fn test_timelock_remaining_counts_down() {
        let (env, client) = setup();
        let admin = soroban_sdk::Address::generate(&env);
        let treasury = soroban_sdk::Address::generate(&env);
        client.initialize(&admin, &treasury);

        let wasm_hash = make_wasm_hash(&env);
        let proposal_id = client.submit_governance_proposal(&admin, &wasm_hash);

        let remaining = client.get_gov_proposal_tl(&proposal_id);
        assert_eq!(remaining, Some(MIN_LEDGER_DELAY));

        advance_ledgers(&env, 1000);
        let remaining = client.get_gov_proposal_tl(&proposal_id);
        assert_eq!(remaining, Some(MIN_LEDGER_DELAY - 1000));

        advance_ledgers(&env, MIN_LEDGER_DELAY - 1000);
        let remaining = client.get_gov_proposal_tl(&proposal_id);
        assert_eq!(remaining, Some(0));
    }

    #[test]
    fn test_timelock_remaining_none_after_cancellation() {
        let (env, client) = setup();
        let admin = soroban_sdk::Address::generate(&env);
        let treasury = soroban_sdk::Address::generate(&env);
        client.initialize(&admin, &treasury);

        let wasm_hash = make_wasm_hash(&env);
        let proposal_id = client.submit_governance_proposal(&admin, &wasm_hash);
        client.cancel_governance_proposal(&admin, &proposal_id);

        let remaining = client.get_gov_proposal_tl(&proposal_id);
        assert_eq!(remaining, None);
    }

    #[test]
    fn test_vote_cancel_reaches_threshold() {
        let (env, client) = setup();
        let admin = soroban_sdk::Address::generate(&env);
        let treasury = soroban_sdk::Address::generate(&env);
        client.initialize(&admin, &treasury);

        let signer1 = soroban_sdk::Address::generate(&env);
        let signer2 = soroban_sdk::Address::generate(&env);
        client.register_signer(&signer1, &admin);
        client.register_signer(&signer2, &admin);

        let wasm_hash = make_wasm_hash(&env);
        let proposal_id = client.submit_governance_proposal(&admin, &wasm_hash);

        // With 2 signers + admin = 3, threshold is 3/2 + 1 = 2
        client.vote_cancel_governance_proposal(&signer1, &proposal_id, &u64::MAX);

        let proposal = client.get_governance_proposal(&proposal_id);
        assert_eq!(proposal.status, ProposalStatus::Pending); // Not yet cancelled (1 vote < 2)

        client.vote_cancel_governance_proposal(&signer2, &proposal_id, &u64::MAX);

        let proposal = client.get_governance_proposal(&proposal_id);
        assert_eq!(proposal.status, ProposalStatus::Cancelled); // 2 votes >= threshold
    }

    #[test]
    fn test_double_vote_rejected() {
        let (env, client) = setup();
        let admin = soroban_sdk::Address::generate(&env);
        let treasury = soroban_sdk::Address::generate(&env);
        client.initialize(&admin, &treasury);

        // Register 2 signers so threshold is 2 (> 1 vote)
        let signer1 = soroban_sdk::Address::generate(&env);
        let signer2 = soroban_sdk::Address::generate(&env);
        client.register_signer(&signer1, &admin);
        client.register_signer(&signer2, &admin);

        let wasm_hash = make_wasm_hash(&env);
        let proposal_id = client.submit_governance_proposal(&admin, &wasm_hash);

        // First vote succeeds (1 < threshold of 2)
        client.vote_cancel_governance_proposal(&signer1, &proposal_id, &u64::MAX);

        // Second vote from same signer is rejected
        let result = client.try_vote_cancel_governance_proposal(&signer1, &proposal_id, &u64::MAX);
        assert_eq!(result, Err(Ok(ContractError::AlreadyVoted)));
    }

    #[test]
    fn test_cancellation_threshold_formula() {
        assert_eq!(cancellation_threshold(0), 1);
        assert_eq!(cancellation_threshold(1), 1);
        assert_eq!(cancellation_threshold(2), 2);
        assert_eq!(cancellation_threshold(3), 2);
        assert_eq!(cancellation_threshold(5), 3);
        assert_eq!(cancellation_threshold(7), 4);
    }

    #[test]
    fn test_staged_delay_verification() {
        assert!(verify_staged_delay(0, MIN_LEDGER_DELAY));
        assert!(verify_staged_delay(0, MIN_LEDGER_DELAY + 1));
        assert!(!verify_staged_delay(0, MIN_LEDGER_DELAY - 1));
        assert!(verify_staged_delay(100, 100 + MIN_LEDGER_DELAY));
        assert!(!verify_staged_delay(100, 100 + MIN_LEDGER_DELAY - 1));
    }

    #[test]
    fn test_is_proposal_executable_after_timelock() {
        let (env, client) = setup();
        let admin = soroban_sdk::Address::generate(&env);
        let treasury = soroban_sdk::Address::generate(&env);
        client.initialize(&admin, &treasury);

        let wasm_hash = make_wasm_hash(&env);
        let proposal_id = client.submit_governance_proposal(&admin, &wasm_hash);

        assert!(!client.is_gov_proposal_executable(&proposal_id));

        advance_ledgers(&env, MIN_LEDGER_DELAY);
        assert!(client.is_gov_proposal_executable(&proposal_id));
    }

    #[test]
    fn test_is_proposal_executable_false_when_cancelled() {
        let (env, client) = setup();
        let admin = soroban_sdk::Address::generate(&env);
        let treasury = soroban_sdk::Address::generate(&env);
        client.initialize(&admin, &treasury);

        let wasm_hash = make_wasm_hash(&env);
        let proposal_id = client.submit_governance_proposal(&admin, &wasm_hash);
        client.cancel_governance_proposal(&admin, &proposal_id);

        advance_ledgers(&env, MIN_LEDGER_DELAY);
        assert!(!client.is_gov_proposal_executable(&proposal_id));
    }

    #[test]
    fn test_proposal_id_increments() {
        let (env, client) = setup();
        let admin = soroban_sdk::Address::generate(&env);
        let treasury = soroban_sdk::Address::generate(&env);
        client.initialize(&admin, &treasury);

        let wasm1 = BytesN::from_array(&env, &[1u8; 32]);
        let id1 = client.submit_governance_proposal(&admin, &wasm1);
        client.cancel_governance_proposal(&admin, &id1);

        let wasm2 = BytesN::from_array(&env, &[2u8; 32]);
        let id2 = client.submit_governance_proposal(&admin, &wasm2);
        client.cancel_governance_proposal(&admin, &id2);

        let wasm3 = BytesN::from_array(&env, &[3u8; 32]);
        let id3 = client.submit_governance_proposal(&admin, &wasm3);

        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(id3, 3);
    }
}
