#![no_std]
//! # Governance Proposal Early Execution Threshold Verification (Issue #983)
//!
//! Allows a governance proposal that has already cleared a supermajority of
//! the eligible voting supply to execute **before** its standard voting window
//! concludes, while still observing the mandatory 48-hour administrative
//! timelock.
//!
//! ## Problem
//!
//! A proposal whose affirmative vote weight has already crossed the
//! supermajority line cannot be overturned by the remaining votes: even if
//! every outstanding voter voted against it, it would still pass. Making the
//! network wait out the rest of the voting window in that situation is pure
//! latency with no security benefit — but simply letting "close enough"
//! proposals through early is not acceptable either, because the early path
//! must remain a strictly stronger condition than the normal path.
//!
//! ## Design
//!
//! ```text
//! open_proposal ──► voting window open ──┬─► trigger_early_execution  (threshold + timelock)
//!                                        └─► … normal window elapses
//! ```
//!
//! * **The early threshold is strictly above 75 % of `V_total`.** The required
//!   weight is `floor(V_total * 7500 / 10_000)` and the check is
//!   `affirmative > required`, so a proposal sitting at exactly 75 % does *not*
//!   qualify — only one that genuinely exceeds the supermajority line does.
//! * **The voting window is bypassed, the administrative timelock is not.**
//!   [`GovernanceEarlyExecution::trigger_early_execution`] refuses to run
//!   before `opened_at + ADMINISTRATIVE_TIMELOCK_SECONDS`
//!   (`EarlyExecutionError::TimelockNotElapsed`). Early execution only skips
//!   the *voting* remainder; the 48-hour review window is untouched.
//! * **It only triggers while the voting window is still open.** Once the
//!   window has elapsed there is nothing left to bypass, so the call is
//!   rejected with `EarlyExecutionError::VotingWindowElapsed` rather than
//!   silently becoming a second execution path.
//! * **The threshold is re-verified against the live `V_total`.** Eligible
//!   supply can be changed by governance while voting is in progress, so the
//!   check is always evaluated against the stored supply at trigger time and
//!   never against a cached decision.
//! * **`ProposalEarlyExecutionTriggered` is emitted** whenever the early path
//!   is taken, carrying the affirmative weight, the eligible supply and the
//!   weight that was required at that moment.
//!
//! All weight arithmetic uses checked `u128` math and every boundary is
//! exercised in the unit tests below.

use soroban_sdk::{contract, contracterror, contractimpl, contracttype, Address, Env, Map, Symbol};

/// The early-execution supermajority: 75 % expressed in basis points.
///
/// The comparison is strict — affirmative weight must *exceed* this share of
/// the eligible supply, not merely reach it.
pub const EARLY_EXECUTION_THRESHOLD_BPS: u32 = 7_500;

/// Denominator for basis-point arithmetic.
pub const BPS_DENOMINATOR: u32 = 10_000;

/// Mandatory administrative timelock between opening a proposal and executing
/// it: 48 hours, in seconds. Early execution deliberately never bypasses this.
pub const ADMINISTRATIVE_TIMELOCK_SECONDS: u64 = 48 * 60 * 60;

/// Upper bound on distinct affirmative voters per proposal. Soroban storage is
/// metered, so the voter set is capped rather than allowed to grow without
/// limit.
pub const MAX_VOTERS: u32 = 100;

/// Topic emitted when the early execution path is taken.
pub const PROPOSAL_EARLY_EXECUTION_TRIGGERED: &str = "ProposalEarlyExecutionTriggered";

/// Topic emitted when an affirmative vote is recorded.
pub const AFFIRMATIVE_VOTE_RECORDED: &str = "AffirmativeVoteRecorded";

/// Errors returned by the early-execution module.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum EarlyExecutionError {
    /// `initialize` has already been called.
    AlreadyInitialized = 1,
    /// The contract has not been initialised yet.
    NotInitialized = 2,
    /// The caller is not the registered admin.
    NotAdmin = 3,
    /// The eligible voting supply is zero, so no threshold can be evaluated.
    ZeroEligibleSupply = 4,
    /// The voting window must be strictly longer than the administrative
    /// timelock, otherwise the early path could never be reached.
    InvalidVotingWindow = 5,
    /// A live, unexecuted proposal already occupies the slot.
    ProposalAlreadyActive = 6,
    /// No proposal with the requested id is active.
    NoActiveProposal = 7,
    /// The proposal has already been executed (early or otherwise).
    ProposalAlreadyExecuted = 8,
    /// The voting period has already concluded; there is nothing to bypass.
    VotingWindowElapsed = 9,
    /// Affirmative weight does not strictly exceed the early threshold.
    ThresholdNotMet = 10,
    /// The 48-hour administrative timelock has not elapsed yet.
    TimelockNotElapsed = 11,
    /// Checked arithmetic overflowed.
    Overflow = 12,
    /// A vote weight of zero carries no meaning.
    InvalidVoteWeight = 13,
    /// This address has already voted affirmatively on the proposal.
    AlreadyVoted = 14,
    /// The capped voter set is full.
    TooManyVoters = 15,
}

/// The single live governance proposal tracked by this module.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveProposal {
    /// Monotonically increasing proposal identifier.
    pub proposal_id: u64,
    /// Address that opened the proposal.
    pub proposer: Address,
    /// Ledger timestamp at which the proposal was opened.
    pub opened_at: u64,
    /// Ledger timestamp at which the standard voting window concludes.
    pub voting_ends_at: u64,
    /// Ledger timestamp at which the administrative timelock expires and the
    /// proposal becomes executable.
    pub timelock_ends_at: u64,
    /// Accumulated affirmative vote weight.
    pub affirmative_weight: u128,
    /// Whether an execution path has already been taken.
    pub executed: bool,
    /// Whether execution happened through the early path.
    pub early: bool,
}

/// Read-only verification view of a proposal's early-execution eligibility.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EarlyExecutionStatus {
    /// Proposal the status refers to.
    pub proposal_id: u64,
    /// Affirmative weight recorded so far.
    pub affirmative_weight: u128,
    /// Eligible voting supply (`V_total`) the threshold is evaluated against.
    pub total_eligible_supply: u128,
    /// Voting weight a proposal must exceed: `floor(V_total * 75 %)`.
    pub required_weight: u128,
    /// Whether affirmative weight strictly exceeds `required_weight`.
    pub threshold_met: bool,
    /// Seconds remaining in the standard voting window (0 once elapsed).
    pub voting_window_remaining: u64,
    /// Seconds remaining on the administrative timelock (0 once elapsed).
    pub timelock_remaining: u64,
    /// Whether the proposal has been executed.
    pub executed: bool,
    /// Whether the proposal was executed via the early path.
    pub early: bool,
}

/// Result of taking the early-execution path.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EarlyExecutionOutcome {
    /// Proposal that was executed early.
    pub proposal_id: u64,
    /// Affirmative weight at trigger time.
    pub affirmative_weight: u128,
    /// Eligible supply at trigger time.
    pub total_eligible_supply: u128,
    /// Weight that was required at trigger time.
    pub required_weight: u128,
    /// Ledger timestamp at which the early path was triggered.
    pub triggered_at: u64,
    /// Seconds of standard voting window that were skipped.
    pub bypassed_voting_seconds: u64,
    /// Ledger timestamp at which the administrative timelock expired — the
    /// earliest moment at which execution is permitted.
    pub timelock_ends_at: u64,
}

/// Storage keys.
#[contracttype]
pub enum DataKey {
    /// Address permitted to configure eligible supply.
    Admin,
    /// Eligible voting supply (`V_total`).
    EligibleSupply,
    /// Duration of the standard voting window, in seconds.
    VotingWindow,
    /// Identifier assigned to the next proposal.
    NextProposalId,
    /// The live proposal, if any.
    Proposal,
    /// Per-proposal affirmative voters and their weight.
    Voters(u64),
}

/// Weight that affirmative votes must **exceed** for an early execution at the
/// given threshold: `floor(total_eligible_supply * threshold_bps / 10_000)`.
///
/// Flooring is intentional. Combined with the strict `>` comparison in
/// [`exceeds_early_execution_threshold`], it makes the requirement exactly
/// "strictly more than `threshold_bps` of the supply", with no rounding slack
/// that could let a proposal through at exactly the threshold.
pub fn early_execution_required_weight(
    total_eligible_supply: u128,
    threshold_bps: u32,
) -> Result<u128, EarlyExecutionError> {
    total_eligible_supply
        .checked_mul(threshold_bps as u128)
        .ok_or(EarlyExecutionError::Overflow)?
        .checked_div(BPS_DENOMINATOR as u128)
        .ok_or(EarlyExecutionError::Overflow)
}

/// Whether `affirmative_weight` strictly exceeds `threshold_bps` of
/// `total_eligible_supply`.
///
/// A supply of zero is rejected: there is no meaningful share of nothing.
pub fn exceeds_early_execution_threshold(
    affirmative_weight: u128,
    total_eligible_supply: u128,
    threshold_bps: u32,
) -> Result<bool, EarlyExecutionError> {
    if total_eligible_supply == 0 {
        return Err(EarlyExecutionError::ZeroEligibleSupply);
    }
    let required = early_execution_required_weight(total_eligible_supply, threshold_bps)?;
    Ok(affirmative_weight > required)
}

fn load_admin(env: &Env) -> Result<Address, EarlyExecutionError> {
    env.storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(EarlyExecutionError::NotInitialized)
}

fn load_eligible_supply(env: &Env) -> Result<u128, EarlyExecutionError> {
    env.storage()
        .instance()
        .get(&DataKey::EligibleSupply)
        .ok_or(EarlyExecutionError::NotInitialized)
}

fn load_voting_window(env: &Env) -> Result<u64, EarlyExecutionError> {
    env.storage()
        .instance()
        .get(&DataKey::VotingWindow)
        .ok_or(EarlyExecutionError::NotInitialized)
}

fn load_proposal(env: &Env) -> Result<ActiveProposal, EarlyExecutionError> {
    env.storage()
        .instance()
        .get(&DataKey::Proposal)
        .ok_or(EarlyExecutionError::NoActiveProposal)
}

fn load_voters(env: &Env, proposal_id: u64) -> Map<Address, u128> {
    env.storage()
        .instance()
        .get(&DataKey::Voters(proposal_id))
        .unwrap_or_else(|| Map::new(env))
}

/// Evaluate the early-execution conditions for `proposal` without touching
/// storage. Shared by the read-only view, the verification call and the
/// trigger so all three can never drift apart.
fn evaluate(
    env: &Env,
    proposal: &ActiveProposal,
) -> Result<(u128, u128, bool, u64, u64), EarlyExecutionError> {
    let supply = load_eligible_supply(env)?;
    let required = early_execution_required_weight(supply, EARLY_EXECUTION_THRESHOLD_BPS)?;
    let threshold_met = exceeds_early_execution_threshold(
        proposal.affirmative_weight,
        supply,
        EARLY_EXECUTION_THRESHOLD_BPS,
    )?;
    let now = env.ledger().timestamp();
    let voting_window_remaining = proposal.voting_ends_at.saturating_sub(now);
    let timelock_remaining = proposal.timelock_ends_at.saturating_sub(now);
    Ok((
        supply,
        required,
        threshold_met,
        voting_window_remaining,
        timelock_remaining,
    ))
}

#[contract]
pub struct GovernanceEarlyExecution;

#[contractimpl]
impl GovernanceEarlyExecution {
    /// Deploy-time setup.
    ///
    /// `voting_window_seconds` must be strictly longer than
    /// [`ADMINISTRATIVE_TIMELOCK_SECONDS`]; otherwise the voting window would
    /// always close before the timelock expires and the early path could never
    /// be reached.
    pub fn initialize(
        env: Env,
        admin: Address,
        eligible_supply: u128,
        voting_window_seconds: u64,
    ) -> Result<(), EarlyExecutionError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(EarlyExecutionError::AlreadyInitialized);
        }
        if eligible_supply == 0 {
            return Err(EarlyExecutionError::ZeroEligibleSupply);
        }
        if voting_window_seconds <= ADMINISTRATIVE_TIMELOCK_SECONDS {
            return Err(EarlyExecutionError::InvalidVotingWindow);
        }
        admin.require_auth();

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::EligibleSupply, &eligible_supply);
        env.storage()
            .instance()
            .set(&DataKey::VotingWindow, &voting_window_seconds);
        env.storage()
            .instance()
            .set(&DataKey::NextProposalId, &1u64);
        Ok(())
    }

    /// Replace the eligible voting supply (`V_total`). Admin-only.
    ///
    /// Because the early threshold is always re-evaluated against the stored
    /// supply, this also changes whether an in-flight proposal qualifies.
    pub fn set_eligible_supply(
        env: Env,
        caller: Address,
        eligible_supply: u128,
    ) -> Result<(), EarlyExecutionError> {
        let admin = load_admin(&env)?;
        if caller != admin {
            return Err(EarlyExecutionError::NotAdmin);
        }
        if eligible_supply == 0 {
            return Err(EarlyExecutionError::ZeroEligibleSupply);
        }
        caller.require_auth();
        env.storage()
            .instance()
            .set(&DataKey::EligibleSupply, &eligible_supply);
        Ok(())
    }

    /// Open a governance proposal.
    ///
    /// Only one *live* proposal — unexecuted and still inside its voting
    /// window — may exist at a time. A proposal whose window elapsed without
    /// executing is dead and is replaced by the next one, which keeps the
    /// single-slot storage model bounded.
    pub fn open_proposal(env: Env, proposer: Address) -> Result<u64, EarlyExecutionError> {
        load_admin(&env)?;
        proposer.require_auth();

        let now = env.ledger().timestamp();
        if let Some(existing) = env
            .storage()
            .instance()
            .get::<_, ActiveProposal>(&DataKey::Proposal)
        {
            if !existing.executed && now < existing.voting_ends_at {
                return Err(EarlyExecutionError::ProposalAlreadyActive);
            }
        }

        let window = load_voting_window(&env)?;
        let voting_ends_at = now
            .checked_add(window)
            .ok_or(EarlyExecutionError::Overflow)?;
        let timelock_ends_at = now
            .checked_add(ADMINISTRATIVE_TIMELOCK_SECONDS)
            .ok_or(EarlyExecutionError::Overflow)?;

        let proposal_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextProposalId)
            .unwrap_or(1u64);
        env.storage()
            .instance()
            .set(&DataKey::NextProposalId, &(proposal_id + 1));

        let proposal = ActiveProposal {
            proposal_id,
            proposer: proposer.clone(),
            opened_at: now,
            voting_ends_at,
            timelock_ends_at,
            affirmative_weight: 0,
            executed: false,
            early: false,
        };
        env.storage().instance().set(&DataKey::Proposal, &proposal);

        env.events().publish(
            (Symbol::new(&env, "ProposalOpened"), proposal_id),
            (proposer, voting_ends_at),
        );

        Ok(proposal_id)
    }

    /// Record affirmative vote weight for a proposal.
    ///
    /// Voting is only accepted while the window is open and before any
    /// execution. Each address may vote at most once, and the capped voter set
    /// bounds storage cost. Returns the running affirmative total.
    pub fn cast_affirmative_vote(
        env: Env,
        voter: Address,
        proposal_id: u64,
        weight: u128,
    ) -> Result<u128, EarlyExecutionError> {
        if weight == 0 {
            return Err(EarlyExecutionError::InvalidVoteWeight);
        }
        voter.require_auth();

        let mut proposal = load_proposal(&env)?;
        if proposal.proposal_id != proposal_id {
            return Err(EarlyExecutionError::NoActiveProposal);
        }
        if proposal.executed {
            return Err(EarlyExecutionError::ProposalAlreadyExecuted);
        }
        if env.ledger().timestamp() >= proposal.voting_ends_at {
            return Err(EarlyExecutionError::VotingWindowElapsed);
        }

        let mut voters = load_voters(&env, proposal_id);
        if voters.contains_key(voter.clone()) {
            return Err(EarlyExecutionError::AlreadyVoted);
        }
        if voters.len() >= MAX_VOTERS {
            return Err(EarlyExecutionError::TooManyVoters);
        }

        let total = proposal
            .affirmative_weight
            .checked_add(weight)
            .ok_or(EarlyExecutionError::Overflow)?;
        proposal.affirmative_weight = total;
        voters.set(voter.clone(), weight);

        env.storage()
            .instance()
            .set(&DataKey::Voters(proposal_id), &voters);
        env.storage().instance().set(&DataKey::Proposal, &proposal);

        env.events().publish(
            (Symbol::new(&env, AFFIRMATIVE_VOTE_RECORDED), proposal_id),
            (voter, weight, total),
        );

        Ok(total)
    }

    /// Read-only verification view: whether the proposal currently satisfies
    /// the early-execution threshold, and how much of each window remains.
    pub fn early_execution_status(
        env: Env,
        proposal_id: u64,
    ) -> Result<EarlyExecutionStatus, EarlyExecutionError> {
        let proposal = load_proposal(&env)?;
        if proposal.proposal_id != proposal_id {
            return Err(EarlyExecutionError::NoActiveProposal);
        }
        let (supply, required, threshold_met, voting_remaining, timelock_remaining) =
            evaluate(&env, &proposal)?;
        Ok(EarlyExecutionStatus {
            proposal_id,
            affirmative_weight: proposal.affirmative_weight,
            total_eligible_supply: supply,
            required_weight: required,
            threshold_met,
            voting_window_remaining: voting_remaining,
            timelock_remaining,
            executed: proposal.executed,
            early: proposal.early,
        })
    }

    /// Verify that a proposal may take the early-execution path *right now*,
    /// without changing any state.
    ///
    /// This is the pure verification half of the module: it performs the same
    /// checks as [`GovernanceEarlyExecution::trigger_early_execution`] and is
    /// safe to call from off-chain tooling or a pre-flight path.
    pub fn verify_early_execution(env: Env, proposal_id: u64) -> Result<(), EarlyExecutionError> {
        let proposal = load_proposal(&env)?;
        if proposal.proposal_id != proposal_id {
            return Err(EarlyExecutionError::NoActiveProposal);
        }
        if proposal.executed {
            return Err(EarlyExecutionError::ProposalAlreadyExecuted);
        }

        let now = env.ledger().timestamp();
        if now >= proposal.voting_ends_at {
            return Err(EarlyExecutionError::VotingWindowElapsed);
        }

        let (_, _, threshold_met, _, _) = evaluate(&env, &proposal)?;
        if !threshold_met {
            return Err(EarlyExecutionError::ThresholdNotMet);
        }

        if now < proposal.timelock_ends_at {
            return Err(EarlyExecutionError::TimelockNotElapsed);
        }

        Ok(())
    }

    /// Take the early-execution path for a proposal.
    ///
    /// Requires, in order: the proposal is not already executed, the standard
    /// voting window is still open, affirmative weight strictly exceeds 75 %
    /// of the eligible supply, and the 48-hour administrative timelock has
    /// elapsed. On success the proposal is marked executed (early) and
    /// `ProposalEarlyExecutionTriggered` is emitted.
    pub fn trigger_early_execution(
        env: Env,
        proposal_id: u64,
    ) -> Result<EarlyExecutionOutcome, EarlyExecutionError> {
        Self::verify_early_execution(env.clone(), proposal_id)?;

        let mut proposal = load_proposal(&env)?;
        let (supply, required, _, _, _) = evaluate(&env, &proposal)?;
        let now = env.ledger().timestamp();

        let bypassed_voting_seconds = proposal.voting_ends_at.saturating_sub(now);
        let timelock_ends_at = proposal.timelock_ends_at;

        proposal.executed = true;
        proposal.early = true;
        env.storage().instance().set(&DataKey::Proposal, &proposal);

        env.events().publish(
            (
                Symbol::new(&env, PROPOSAL_EARLY_EXECUTION_TRIGGERED),
                proposal_id,
            ),
            (proposal.affirmative_weight, supply, required, now),
        );

        Ok(EarlyExecutionOutcome {
            proposal_id,
            affirmative_weight: proposal.affirmative_weight,
            total_eligible_supply: supply,
            required_weight: required,
            triggered_at: now,
            bypassed_voting_seconds,
            timelock_ends_at,
        })
    }

    /// The registered admin.
    pub fn get_admin(env: Env) -> Result<Address, EarlyExecutionError> {
        load_admin(&env)
    }

    /// The current eligible voting supply (`V_total`).
    pub fn get_eligible_supply(env: Env) -> Result<u128, EarlyExecutionError> {
        load_eligible_supply(&env)
    }

    /// The configured length of the standard voting window, in seconds.
    pub fn get_voting_window(env: Env) -> Result<u64, EarlyExecutionError> {
        load_voting_window(&env)
    }

    /// The live proposal, if any.
    pub fn get_proposal(env: Env) -> Option<ActiveProposal> {
        env.storage().instance().get(&DataKey::Proposal)
    }

    /// The affirmative weight recorded by `voter` on `proposal_id`, if any.
    pub fn get_vote_weight(env: Env, proposal_id: u64, voter: Address) -> Option<u128> {
        load_voters(&env, proposal_id).get(voter)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Events, Ledger, LedgerInfo};
    use soroban_sdk::{Symbol, TryFromVal};

    const SUPPLY: u128 = 1_000_000;
    const WINDOW: u64 = 200_000; // > 48h (172_800s)
    const START: u64 = 1_000_000;

    fn setup() -> (Env, GovernanceEarlyExecutionClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set(LedgerInfo {
            timestamp: START,
            ..env.ledger().get()
        });
        let id = env.register_contract(None, GovernanceEarlyExecution);
        let client = GovernanceEarlyExecutionClient::new(&env, &id);
        (env, client)
    }

    fn at(env: &Env, timestamp: u64) {
        env.ledger().set(LedgerInfo {
            timestamp,
            ..env.ledger().get()
        });
    }

    // ── threshold arithmetic ────────────────────────────────────────────────

    #[test]
    fn required_weight_floors_the_supermajority() {
        assert_eq!(early_execution_required_weight(100, 7_500).unwrap(), 75);
        assert_eq!(early_execution_required_weight(101, 7_500).unwrap(), 75);
        assert_eq!(early_execution_required_weight(102, 7_500).unwrap(), 76);
        assert_eq!(early_execution_required_weight(3, 7_500).unwrap(), 2);
        assert_eq!(early_execution_required_weight(1, 7_500).unwrap(), 0);
        assert_eq!(early_execution_required_weight(0, 7_500).unwrap(), 0);
    }

    #[test]
    fn required_weight_reports_overflow_instead_of_wrapping() {
        assert_eq!(
            early_execution_required_weight(u128::MAX, BPS_DENOMINATOR),
            Err(EarlyExecutionError::Overflow)
        );
    }

    #[test]
    fn threshold_is_exceeded_strictly() {
        // Exactly 75 % is not enough.
        assert!(!exceeds_early_execution_threshold(75, 100, 7_500).unwrap());
        // One unit above is.
        assert!(exceeds_early_execution_threshold(76, 100, 7_500).unwrap());
        // …and on an odd supply the floor keeps the boundary honest.
        assert!(!exceeds_early_execution_threshold(75, 101, 7_500).unwrap());
        assert!(exceeds_early_execution_threshold(76, 101, 7_500).unwrap());
    }

    #[test]
    fn threshold_rejects_a_zero_supply() {
        assert_eq!(
            exceeds_early_execution_threshold(1, 0, 7_500),
            Err(EarlyExecutionError::ZeroEligibleSupply)
        );
    }

    // ── initialization ──────────────────────────────────────────────────────

    #[test]
    fn initialize_installs_configuration() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.initialize(&admin, &SUPPLY, &WINDOW);

        assert_eq!(client.get_admin(), admin);
        assert_eq!(client.get_eligible_supply(), SUPPLY);
        assert_eq!(client.get_voting_window(), WINDOW);
        assert!(client.get_proposal().is_none());
    }

    #[test]
    fn initialize_cannot_run_twice() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.initialize(&admin, &SUPPLY, &WINDOW);

        let result = client.try_initialize(&admin, &SUPPLY, &WINDOW);
        assert_eq!(result, Err(Ok(EarlyExecutionError::AlreadyInitialized)));
    }

    #[test]
    fn initialize_rejects_a_zero_eligible_supply() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let result = client.try_initialize(&admin, &0u128, &WINDOW);
        assert_eq!(result, Err(Ok(EarlyExecutionError::ZeroEligibleSupply)));
    }

    #[test]
    fn initialize_rejects_a_voting_window_that_cannot_reach_early_execution() {
        let (env, client) = setup();
        let admin = Address::generate(&env);

        // Exactly the timelock is not enough — the window would close at the
        // same instant the timelock expires.
        let result = client.try_initialize(&admin, &SUPPLY, &ADMINISTRATIVE_TIMELOCK_SECONDS);
        assert_eq!(result, Err(Ok(EarlyExecutionError::InvalidVotingWindow)));

        // One second more is valid.
        client.initialize(&admin, &SUPPLY, &(ADMINISTRATIVE_TIMELOCK_SECONDS + 1));
        assert_eq!(
            client.get_voting_window(),
            ADMINISTRATIVE_TIMELOCK_SECONDS + 1
        );
    }

    #[test]
    fn operations_require_initialization() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        assert_eq!(
            client.try_open_proposal(&admin),
            Err(Ok(EarlyExecutionError::NotInitialized))
        );
        assert_eq!(
            client.try_get_eligible_supply(),
            Err(Ok(EarlyExecutionError::NotInitialized))
        );
    }

    // ── eligible supply ─────────────────────────────────────────────────────

    #[test]
    fn only_admin_can_change_the_eligible_supply() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let stranger = Address::generate(&env);
        client.initialize(&admin, &SUPPLY, &WINDOW);

        let result = client.try_set_eligible_supply(&stranger, &2_000_000u128);
        assert_eq!(result, Err(Ok(EarlyExecutionError::NotAdmin)));

        client.set_eligible_supply(&admin, &2_000_000u128);
        assert_eq!(client.get_eligible_supply(), 2_000_000u128);
    }

    #[test]
    fn eligible_supply_cannot_be_zero() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.initialize(&admin, &SUPPLY, &WINDOW);

        let result = client.try_set_eligible_supply(&admin, &0u128);
        assert_eq!(result, Err(Ok(EarlyExecutionError::ZeroEligibleSupply)));
    }

    // ── proposal lifecycle ──────────────────────────────────────────────────

    #[test]
    fn open_proposal_records_both_windows() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.initialize(&admin, &SUPPLY, &WINDOW);

        let proposal_id = client.open_proposal(&admin);
        assert_eq!(proposal_id, 1);

        let proposal = client.get_proposal().unwrap();
        assert_eq!(proposal.opened_at, START);
        assert_eq!(proposal.voting_ends_at, START + WINDOW);
        assert_eq!(
            proposal.timelock_ends_at,
            START + ADMINISTRATIVE_TIMELOCK_SECONDS
        );
        assert!(!proposal.executed);
        assert_eq!(proposal.affirmative_weight, 0);
    }

    #[test]
    fn a_live_proposal_blocks_opening_another() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.initialize(&admin, &SUPPLY, &WINDOW);
        client.open_proposal(&admin);

        let result = client.try_open_proposal(&admin);
        assert_eq!(result, Err(Ok(EarlyExecutionError::ProposalAlreadyActive)));
    }

    #[test]
    fn a_proposal_id_is_reused_only_after_the_previous_slot_is_dead() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.initialize(&admin, &SUPPLY, &WINDOW);

        let first = client.open_proposal(&admin);
        assert_eq!(first, 1);

        // Still live: rejected.
        assert_eq!(
            client.try_open_proposal(&admin),
            Err(Ok(EarlyExecutionError::ProposalAlreadyActive))
        );

        // Window elapses without execution: the slot is dead and reusable.
        at(&env, START + WINDOW);
        let second = client.open_proposal(&admin);
        assert_eq!(second, 2);
    }

    #[test]
    fn a_new_proposal_can_be_opened_after_an_early_execution() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let voter = Address::generate(&env);
        client.initialize(&admin, &SUPPLY, &WINDOW);

        let first = client.open_proposal(&admin);
        client.cast_affirmative_vote(&voter, &first, &800_000u128);
        at(&env, START + ADMINISTRATIVE_TIMELOCK_SECONDS);
        client.trigger_early_execution(&first);

        let second = client.open_proposal(&admin);
        assert_eq!(second, 2);
    }

    // ── voting ──────────────────────────────────────────────────────────────

    #[test]
    fn affirmative_votes_accumulate() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let a = Address::generate(&env);
        let b = Address::generate(&env);
        client.initialize(&admin, &SUPPLY, &WINDOW);
        let proposal_id = client.open_proposal(&admin);

        assert_eq!(
            client.cast_affirmative_vote(&a, &proposal_id, &400_000u128),
            400_000
        );
        assert_eq!(
            client.cast_affirmative_vote(&b, &proposal_id, &350_001u128),
            750_001
        );
        assert_eq!(client.get_vote_weight(&proposal_id, &a), Some(400_000));
        assert_eq!(client.get_vote_weight(&proposal_id, &b), Some(350_001));
    }

    #[test]
    fn a_zero_weight_vote_is_rejected() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let voter = Address::generate(&env);
        client.initialize(&admin, &SUPPLY, &WINDOW);
        let proposal_id = client.open_proposal(&admin);

        let result = client.try_cast_affirmative_vote(&voter, &proposal_id, &0u128);
        assert_eq!(result, Err(Ok(EarlyExecutionError::InvalidVoteWeight)));
    }

    #[test]
    fn a_voter_cannot_double_count() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let voter = Address::generate(&env);
        client.initialize(&admin, &SUPPLY, &WINDOW);
        let proposal_id = client.open_proposal(&admin);

        client.cast_affirmative_vote(&voter, &proposal_id, &300_000u128);

        let result = client.try_cast_affirmative_vote(&voter, &proposal_id, &300_000u128);
        assert_eq!(result, Err(Ok(EarlyExecutionError::AlreadyVoted)));
        // The rejected vote left the tally untouched.
        assert_eq!(client.get_proposal().unwrap().affirmative_weight, 300_000);
    }

    #[test]
    fn voting_closes_with_the_voting_window() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let voter = Address::generate(&env);
        client.initialize(&admin, &SUPPLY, &WINDOW);
        let proposal_id = client.open_proposal(&admin);

        at(&env, START + WINDOW);
        let result = client.try_cast_affirmative_vote(&voter, &proposal_id, &100u128);
        assert_eq!(result, Err(Ok(EarlyExecutionError::VotingWindowElapsed)));
    }

    #[test]
    fn voting_is_rejected_for_an_unknown_proposal_id() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let voter = Address::generate(&env);
        client.initialize(&admin, &SUPPLY, &WINDOW);
        client.open_proposal(&admin);

        let result = client.try_cast_affirmative_vote(&voter, &99u64, &100u128);
        assert_eq!(result, Err(Ok(EarlyExecutionError::NoActiveProposal)));
    }

    #[test]
    fn the_voter_set_is_capped() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.initialize(&admin, &SUPPLY, &WINDOW);
        let proposal_id = client.open_proposal(&admin);

        for _ in 0..MAX_VOTERS {
            let voter = Address::generate(&env);
            client.cast_affirmative_vote(&voter, &proposal_id, &1u128);
        }
        let extra = Address::generate(&env);
        let result = client.try_cast_affirmative_vote(&extra, &proposal_id, &1u128);
        assert_eq!(result, Err(Ok(EarlyExecutionError::TooManyVoters)));
    }

    // ── verification view ───────────────────────────────────────────────────

    #[test]
    fn status_reports_the_threshold_and_remaining_windows() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let voter = Address::generate(&env);
        client.initialize(&admin, &SUPPLY, &WINDOW);
        let proposal_id = client.open_proposal(&admin);
        client.cast_affirmative_vote(&voter, &proposal_id, &750_000u128);

        let status = client.early_execution_status(&proposal_id);
        assert_eq!(status.total_eligible_supply, SUPPLY);
        assert_eq!(status.required_weight, 750_000);
        // Exactly 75 % does not strictly exceed the threshold.
        assert!(!status.threshold_met);
        assert_eq!(status.voting_window_remaining, WINDOW);
        assert_eq!(status.timelock_remaining, ADMINISTRATIVE_TIMELOCK_SECONDS);
        assert!(!status.executed);

        let second = Address::generate(&env);
        client.cast_affirmative_vote(&second, &proposal_id, &1u128);
        let status = client.early_execution_status(&proposal_id);
        assert!(status.threshold_met);
    }

    #[test]
    fn status_drains_both_countdowns() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.initialize(&admin, &SUPPLY, &WINDOW);
        let proposal_id = client.open_proposal(&admin);

        at(&env, START + ADMINISTRATIVE_TIMELOCK_SECONDS);
        let status = client.early_execution_status(&proposal_id);
        assert_eq!(status.timelock_remaining, 0);
        assert_eq!(
            status.voting_window_remaining,
            WINDOW - ADMINISTRATIVE_TIMELOCK_SECONDS
        );

        at(&env, START + WINDOW + 10);
        let status = client.early_execution_status(&proposal_id);
        assert_eq!(status.voting_window_remaining, 0);
    }

    // ── early execution ─────────────────────────────────────────────────────

    #[test]
    fn verify_rejects_a_proposal_below_the_supermajority() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let voter = Address::generate(&env);
        client.initialize(&admin, &SUPPLY, &WINDOW);
        let proposal_id = client.open_proposal(&admin);
        client.cast_affirmative_vote(&voter, &proposal_id, &750_000u128);
        at(&env, START + ADMINISTRATIVE_TIMELOCK_SECONDS);

        assert_eq!(
            client.try_verify_early_execution(&proposal_id),
            Err(Ok(EarlyExecutionError::ThresholdNotMet))
        );
        assert_eq!(
            client.try_trigger_early_execution(&proposal_id),
            Err(Ok(EarlyExecutionError::ThresholdNotMet))
        );
    }

    #[test]
    fn the_administrative_timelock_survives_the_early_path() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let voter = Address::generate(&env);
        client.initialize(&admin, &SUPPLY, &WINDOW);
        let proposal_id = client.open_proposal(&admin);
        client.cast_affirmative_vote(&voter, &proposal_id, &990_000u128);

        // The supermajority is in hand immediately, but the 48-hour timelock
        // has not run: the early path must not fire yet.
        at(&env, START + 1);
        assert_eq!(
            client.try_trigger_early_execution(&proposal_id),
            Err(Ok(EarlyExecutionError::TimelockNotElapsed))
        );

        // One second short of the timelock is still too early.
        at(&env, START + ADMINISTRATIVE_TIMELOCK_SECONDS - 1);
        assert_eq!(
            client.try_trigger_early_execution(&proposal_id),
            Err(Ok(EarlyExecutionError::TimelockNotElapsed))
        );
        assert!(!client.get_proposal().unwrap().executed);

        // Exactly at the timelock boundary, the early path opens.
        at(&env, START + ADMINISTRATIVE_TIMELOCK_SECONDS);
        let outcome = client.trigger_early_execution(&proposal_id);
        assert_eq!(
            outcome.triggered_at,
            START + ADMINISTRATIVE_TIMELOCK_SECONDS
        );
        assert_eq!(
            outcome.timelock_ends_at,
            START + ADMINISTRATIVE_TIMELOCK_SECONDS
        );
    }

    #[test]
    fn early_execution_bypasses_the_remaining_voting_window() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let voter = Address::generate(&env);
        client.initialize(&admin, &SUPPLY, &WINDOW);
        let proposal_id = client.open_proposal(&admin);
        client.cast_affirmative_vote(&voter, &proposal_id, &750_001u128);

        let trigger_at = START + ADMINISTRATIVE_TIMELOCK_SECONDS;
        at(&env, trigger_at);
        let outcome = client.trigger_early_execution(&proposal_id);

        assert_eq!(outcome.proposal_id, proposal_id);
        assert_eq!(outcome.affirmative_weight, 750_001);
        assert_eq!(outcome.total_eligible_supply, SUPPLY);
        assert_eq!(outcome.required_weight, 750_000);
        // Everything between the timelock and the natural close was skipped.
        assert_eq!(
            outcome.bypassed_voting_seconds,
            WINDOW - ADMINISTRATIVE_TIMELOCK_SECONDS
        );

        let proposal = client.get_proposal().unwrap();
        assert!(proposal.executed);
        assert!(proposal.early);

        // The event names the path that was taken.
        let events = env.events().all();
        let (_, topics, _) = events.last().unwrap();
        let topic = Symbol::try_from_val(&env, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic, Symbol::new(&env, PROPOSAL_EARLY_EXECUTION_TRIGGERED));
    }

    #[test]
    fn early_execution_recomputes_against_the_live_eligible_supply() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let voter = Address::generate(&env);
        client.initialize(&admin, &SUPPLY, &WINDOW);
        let proposal_id = client.open_proposal(&admin);
        client.cast_affirmative_vote(&voter, &proposal_id, &750_001u128);

        // Governance expands the eligible supply while the proposal is open:
        // 750_001 is now only 37.5 % of the new V_total.
        client.set_eligible_supply(&admin, &2_000_000u128);

        at(&env, START + ADMINISTRATIVE_TIMELOCK_SECONDS);
        assert_eq!(
            client.try_trigger_early_execution(&proposal_id),
            Err(Ok(EarlyExecutionError::ThresholdNotMet))
        );
        assert!(!client.get_proposal().unwrap().executed);
    }

    #[test]
    fn early_execution_is_rejected_once_the_window_has_closed() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let voter = Address::generate(&env);
        client.initialize(&admin, &SUPPLY, &WINDOW);
        let proposal_id = client.open_proposal(&admin);
        client.cast_affirmative_vote(&voter, &proposal_id, &900_000u128);

        at(&env, START + WINDOW);
        assert_eq!(
            client.try_trigger_early_execution(&proposal_id),
            Err(Ok(EarlyExecutionError::VotingWindowElapsed))
        );
    }

    #[test]
    fn early_execution_cannot_be_triggered_twice() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let voter = Address::generate(&env);
        client.initialize(&admin, &SUPPLY, &WINDOW);
        let proposal_id = client.open_proposal(&admin);
        client.cast_affirmative_vote(&voter, &proposal_id, &760_000u128);

        at(&env, START + ADMINISTRATIVE_TIMELOCK_SECONDS);
        client.trigger_early_execution(&proposal_id);
        assert_eq!(
            client.try_trigger_early_execution(&proposal_id),
            Err(Ok(EarlyExecutionError::ProposalAlreadyExecuted))
        );
    }

    #[test]
    fn votes_are_rejected_after_an_early_execution() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let voter = Address::generate(&env);
        let late = Address::generate(&env);
        client.initialize(&admin, &SUPPLY, &WINDOW);
        let proposal_id = client.open_proposal(&admin);
        client.cast_affirmative_vote(&voter, &proposal_id, &800_000u128);

        at(&env, START + ADMINISTRATIVE_TIMELOCK_SECONDS);
        client.trigger_early_execution(&proposal_id);

        let result = client.try_cast_affirmative_vote(&late, &proposal_id, &1u128);
        assert_eq!(
            result,
            Err(Ok(EarlyExecutionError::ProposalAlreadyExecuted))
        );
    }

    #[test]
    fn early_execution_rejects_an_unknown_proposal_id() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.initialize(&admin, &SUPPLY, &WINDOW);
        client.open_proposal(&admin);

        assert_eq!(
            client.try_verify_early_execution(&99u64),
            Err(Ok(EarlyExecutionError::NoActiveProposal))
        );
    }

    #[test]
    fn a_single_unit_supply_still_requires_a_non_zero_yes_vote() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let voter = Address::generate(&env);
        client.initialize(&admin, &1u128, &WINDOW);
        let proposal_id = client.open_proposal(&admin);
        at(&env, START + ADMINISTRATIVE_TIMELOCK_SECONDS);

        // required = floor(1 * 75 %) = 0, so nothing at all is not "more than".
        assert_eq!(
            client.try_trigger_early_execution(&proposal_id),
            Err(Ok(EarlyExecutionError::ThresholdNotMet))
        );

        client.cast_affirmative_vote(&voter, &proposal_id, &1u128);
        at(&env, START + ADMINISTRATIVE_TIMELOCK_SECONDS);
        let outcome = client.trigger_early_execution(&proposal_id);
        assert_eq!(outcome.required_weight, 0);
        assert_eq!(outcome.affirmative_weight, 1);
    }
}
