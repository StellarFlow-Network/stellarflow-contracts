//! Historical governance vote register: storage model, lifecycle and the
//! post-execution compression path.
//!
//! ## What is stored per proposal
//!
//! ```text
//! Voting active                       after compression (archived)
//! ------------------------------      ----------------------------
//! DataKey::Voters          Vec<Address>   (eligible set, kept — shared config)
//! DataKey::Proposal(id)    ProposalState (kept — lifecycle + final tally)
//! DataKey::VoteBits(id)    Vec<u64>       (removed, folded into the root)
//! DataKey::PackedWeights(id) Vec<u64>     (removed, folded into the root)
//! DataKey::Archive(id)     ArchiveRecord  (written: root + leaf count + tally)
//! ```
//!
//! The pre-optimization representation of the same register — the one still
//! written by `contracts/price-oracle` today — is a `Vec<Address>` of
//! participants (`DataKey::ActionVotes`) plus a whole `u32` governance weight
//! per voter address (`DataKey::AdminWeight`, issue #264). Both keys are
//! declared here so the benchmark can measure the real serialized footprint of
//! the old layout and so that registers written before this optimization stay
//! readable and compressible (backward compatibility).
//!
//! ## Fallback for unsupported packing
//!
//! A vote is written to the compact keys only when [`bits::encoding_for`]
//! proves the weight is losslessly packable. Otherwise it falls back to
//! `DataKey::ActionVotes`, the detailed register the repository already uses,
//! and the tally keeps counting it. A weight is only ever folded into the
//! packed field when it round-trips exactly; the optimization never rounds,
//! clamps, rejects or drops a vote to win bytes. The two layouts are disjoint
//! (a voter is recorded in exactly one of them), which is what lets [`tally`]
//! sum them without double counting.
//!
//! ## Authorization
//!
//! These helpers deliberately do **not** call `require_auth`: they are internal
//! storage routines, matching `contracts/price-oracle/src/auth.rs`. Caller
//! authentication is enforced once, at the contract ABI boundary in
//! [`crate::GovernanceVoteArchive`]. Caller *authorization* (is this the admin /
//! an eligible voter?) is still checked here, so the helpers stay safe if used
//! directly by tests or by future entrypoints.
//!
//! ## Canonical ordering
//!
//! A voter's canonical index is its position in `DataKey::Voters`, which is the
//! register's own insertion order (the same order `get_voters` returns). Packed
//! bit `i` and packed weight field `i` belong to voter index `i`, and Merkle
//! leaves are emitted in strictly ascending voter index. Insertion order is
//! therefore fixed at registration time and the root does not depend on the
//! order in which votes were cast.

use alloc::vec::Vec;
use soroban_sdk::{contracttype, symbol_short, Address, BytesN, Env, Symbol, Vec as SVec};

use crate::bits;
use crate::merkle;
use crate::ArchiveError;

/// Extend persistent register entries when fewer than this many ledgers remain.
pub const REGISTER_TTL_THRESHOLD: u32 = 5_000;
/// Target TTL for persistent register entries.
pub const REGISTER_TTL_EXTEND_TO: u32 = 100_000;

/// Largest configurable cumulative weight threshold.
pub const MAX_WEIGHT_THRESHOLD: u32 = 100;

const ADMIN_KEY: Symbol = symbol_short!("ADMIN");

/// Storage keys owned by the archive module.
///
/// `ActionVotes` / `AdminWeight` are the legacy (detailed) layout; `VoteBits` /
/// `PackedWeights` are the compact layout; `Archive` is the compressed layout.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    /// Ordered eligible-voter registry; position == canonical voter index.
    Voters,
    /// Optional cumulative weight threshold (mirrors price-oracle #264).
    WeightThreshold,
    /// Lifecycle state of a proposal.
    Proposal(u64),
    /// Legacy detailed historical voter register: participants of a proposal.
    ActionVotes(u64),
    /// Legacy per-voter governance weight (default 1 when unset).
    AdminWeight(Address),
    /// Packed boolean vote options: bit `i` == voter `i` voted.
    VoteBits(u64),
    /// Packed 7-bit vote weights aligned with `VoteBits`.
    PackedWeights(u64),
    /// Post-execution archival record replacing the detailed register.
    Archive(u64),
}

/// Lifecycle state of a proposal.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProposalState {
    /// Set once the proposal reached its threshold and was finalized.
    pub executed: bool,
    /// Set when the proposal was withdrawn before finalization.
    pub cancelled: bool,
    /// Number of recorded votes.
    pub vote_count: u32,
    /// Tally frozen at finalization; carried into the archive record verbatim.
    pub final_tally: u64,
}

/// Compressed historical representation of a finalized vote register.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchiveRecord {
    /// Deterministic Merkle root over the canonical leaf list.
    pub root: BytesN<32>,
    /// Number of leaves (participating voters) the root was built from.
    pub leaf_count: u32,
    /// Final weighted tally, copied from `ProposalState::final_tally`.
    pub final_tally: u64,
}

/// A single participating voter, resolved to its canonical index.
#[derive(Clone)]
pub struct Participant {
    pub index: u32,
    pub voter: Address,
    pub weight: u32,
}

// ─── Configuration ──────────────────────────────────────────────────────────

pub fn initialize(env: &Env, admin: Address) -> Result<(), ArchiveError> {
    if env.storage().instance().has(&ADMIN_KEY) {
        return Err(ArchiveError::AlreadyInitialized);
    }
    env.storage().instance().set(&ADMIN_KEY, &admin);
    Ok(())
}

pub fn admin(env: &Env) -> Result<Address, ArchiveError> {
    env.storage()
        .instance()
        .get(&ADMIN_KEY)
        .ok_or(ArchiveError::NotInitialized)
}

/// Register an eligible voter and return its canonical index.
///
/// Idempotent: re-registering an existing voter returns the original index.
pub fn add_voter(env: &Env, caller: Address, voter: Address) -> Result<u32, ArchiveError> {
    let current_admin = admin(env)?;
    if caller != current_admin {
        return Err(ArchiveError::NotAdmin);
    }
    let mut voters = get_voters(env);
    for i in 0..voters.len() {
        if voters.get(i).unwrap() == voter {
            return Ok(i);
        }
    }
    let index = voters.len();
    voters.push_back(voter);
    set_voters(env, &voters);
    Ok(index)
}

pub fn get_voters(env: &Env) -> SVec<Address> {
    let key = DataKey::Voters;
    let voters: SVec<Address> = env
        .storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(|| SVec::new(env));
    if !voters.is_empty() {
        env.storage()
            .persistent()
            .extend_ttl(&key, REGISTER_TTL_THRESHOLD, REGISTER_TTL_EXTEND_TO);
    }
    voters
}

pub fn voter_count(env: &Env) -> u32 {
    get_voters(env).len()
}

/// Look up a voter's canonical index in the eligible set.
pub fn voter_index(env: &Env, voter: &Address) -> Option<u32> {
    let voters = get_voters(env);
    for i in 0..voters.len() {
        if voters.get(i).unwrap() == *voter {
            return Some(i);
        }
    }
    None
}

pub fn is_voter(env: &Env, voter: &Address) -> bool {
    voter_index(env, voter).is_some()
}

/// Set the governance weight for a voter (mirrors price-oracle issue #264).
pub fn set_admin_weight(
    env: &Env,
    caller: Address,
    voter: Address,
    weight: u32,
) -> Result<(), ArchiveError> {
    let current_admin = admin(env)?;
    if caller != current_admin {
        return Err(ArchiveError::NotAdmin);
    }
    if !is_voter(env, &voter) {
        return Err(ArchiveError::UnknownVoter);
    }
    bits::pack_weight(weight)?;
    let key = DataKey::AdminWeight(voter);
    env.storage().persistent().set(&key, &weight);
    env.storage()
        .persistent()
        .extend_ttl(&key, REGISTER_TTL_THRESHOLD, REGISTER_TTL_EXTEND_TO);
    Ok(())
}

/// Weight used for a voter in the detailed layout (defaults to 1, as the
/// repository does for admins that never called `set_admin_weight`).
pub fn admin_weight(env: &Env, voter: Address) -> u32 {
    env.storage()
        .persistent()
        .get(&DataKey::AdminWeight(voter))
        .unwrap_or(1u32)
}

/// Configure a cumulative weight threshold; when unset the vote-count
/// threshold is used (identical rule to price-oracle #264).
pub fn set_weight_threshold(
    env: &Env,
    caller: Address,
    threshold: u32,
) -> Result<(), ArchiveError> {
    let current_admin = admin(env)?;
    if caller != current_admin {
        return Err(ArchiveError::NotAdmin);
    }
    if threshold < 1 || threshold > MAX_WEIGHT_THRESHOLD {
        return Err(ArchiveError::InvalidThreshold);
    }
    env.storage()
        .instance()
        .set(&DataKey::WeightThreshold, &threshold);
    Ok(())
}

pub fn get_weight_threshold(env: &Env) -> Option<u32> {
    env.storage().instance().get(&DataKey::WeightThreshold)
}

/// Fallback vote-count threshold: 2 for small councils, 3 otherwise.
///
/// Byte-for-byte the rule in `contracts/price-oracle/src/auth.rs`.
pub fn required_threshold(env: &Env) -> u32 {
    if let Some(weight_threshold) = get_weight_threshold(env) {
        return weight_threshold;
    }
    if get_voters(env).len() <= 3 {
        2
    } else {
        3
    }
}

// ─── Lifecycle ──────────────────────────────────────────────────────────────

pub fn propose_action(env: &Env, proposer: Address, proposal_id: u64) -> Result<(), ArchiveError> {
    let current_admin = admin(env)?;
    if proposer != current_admin {
        return Err(ArchiveError::NotAuthorized);
    }
    if env.storage().persistent().has(&DataKey::Proposal(proposal_id)) {
        return Err(ArchiveError::ProposalAlreadyExists);
    }
    let state = ProposalState {
        executed: false,
        cancelled: false,
        vote_count: 0,
        final_tally: 0,
    };
    set_proposal(env, proposal_id, &state);
    Ok(())
}

pub fn cancel_action(
    env: &Env,
    canceller: Address,
    proposal_id: u64,
) -> Result<(), ArchiveError> {
    let current_admin = admin(env)?;
    if canceller != current_admin {
        return Err(ArchiveError::NotAuthorized);
    }
    let mut state = load_proposal(env, proposal_id)?;
    if state.cancelled {
        return Err(ArchiveError::ProposalClosed);
    }
    state.cancelled = true;
    set_proposal(env, proposal_id, &state);
    Ok(())
}

pub fn get_proposal(env: &Env, proposal_id: u64) -> Option<ProposalState> {
    env.storage().persistent().get(&DataKey::Proposal(proposal_id))
}

fn load_proposal(env: &Env, proposal_id: u64) -> Result<ProposalState, ArchiveError> {
    get_proposal(env, proposal_id).ok_or(ArchiveError::ProposalNotFound)
}

fn set_proposal(env: &Env, proposal_id: u64, state: &ProposalState) {
    let key = DataKey::Proposal(proposal_id);
    env.storage().persistent().set(&key, state);
    env.storage()
        .persistent()
        .extend_ttl(&key, REGISTER_TTL_THRESHOLD, REGISTER_TTL_EXTEND_TO);
}

/// Cast a vote, packing it when the evidence gate allows.
///
/// A packable weight writes one bit into `VoteBits` and one 7-bit field into
/// `PackedWeights`; no per-voter address is written. A weight the gate cannot
/// pack losslessly is written to the detailed `ActionVotes` register instead
/// (see the module docs), so the vote still counts. Returns the running vote
/// count, matching the repository's `vote_for_action` return value.
pub fn cast_vote(env: &Env, voter: Address, proposal_id: u64) -> Result<u32, ArchiveError> {
    let mut state = load_proposal(env, proposal_id)?;
    ensure_open(env, proposal_id, &state)?;
    let index = voter_index(env, &voter).ok_or(ArchiveError::NotAuthorized)?;
    if has_voted(env, proposal_id, index) {
        return Err(ArchiveError::AlreadyVoted);
    }

    let weight = admin_weight(env, voter.clone());
    match bits::encoding_for(weight) {
        bits::Encoding::Packed => {
            let mut option_bits = get_vote_bits(env, proposal_id);
            bits::set_packed_bit(&mut option_bits, index);
            let mut packed_weights = get_packed_weights(env, proposal_id);
            bits::set_packed_weight(&mut packed_weights, index, weight)?;
            store_vote_bits(env, proposal_id, &option_bits);
            store_packed_weights(env, proposal_id, &packed_weights);
        }
        bits::Encoding::DetailedFallback => {
            let mut participants = get_action_votes(env, proposal_id);
            participants.push_back(voter);
            store_action_votes(env, proposal_id, &participants);
        }
    }

    state.vote_count += 1;
    set_proposal(env, proposal_id, &state);
    Ok(state.vote_count)
}

/// Cast a vote into the legacy detailed register.
///
/// Present for backward compatibility with registers written before this
/// optimization and to give the storage benchmark a faithful "before" path:
/// participants are appended to a `Vec<Address>` exactly as
/// `contracts/price-oracle` does today, and weights stay in `AdminWeight`.
pub fn cast_vote_detailed(
    env: &Env,
    voter: Address,
    proposal_id: u64,
) -> Result<u32, ArchiveError> {
    let mut state = load_proposal(env, proposal_id)?;
    ensure_open(env, proposal_id, &state)?;
    let index = voter_index(env, &voter).ok_or(ArchiveError::NotAuthorized)?;
    let _ = index;
    let mut participants = get_action_votes(env, proposal_id);
    for i in 0..participants.len() {
        if participants.get(i).unwrap() == voter {
            return Err(ArchiveError::AlreadyVoted);
        }
    }
    participants.push_back(voter);
    store_action_votes(env, proposal_id, &participants);

    state.vote_count += 1;
    set_proposal(env, proposal_id, &state);
    Ok(state.vote_count)
}

fn ensure_open(env: &Env, proposal_id: u64, state: &ProposalState) -> Result<(), ArchiveError> {
    if state.executed || state.cancelled {
        return Err(ArchiveError::ProposalClosed);
    }
    if env.storage().persistent().has(&DataKey::Archive(proposal_id)) {
        return Err(ArchiveError::ProposalClosed);
    }
    Ok(())
}

/// Whether a voter slot has already voted, in either layout.
pub fn has_voted(env: &Env, proposal_id: u64, voter_index: u32) -> bool {
    if bits::is_bit_set(&get_vote_bits(env, proposal_id), voter_index) {
        return true;
    }
    let voters = get_voters(env);
    match voters.get(voter_index) {
        Some(voter) => contains_address(&get_action_votes(env, proposal_id), &voter),
        None => false,
    }
}

/// Decode the register into a readable boolean option sequence.
///
/// One entry per eligible voter, in canonical voter-index order; `true` means
/// that voter is recorded as having voted. Covers both layouts.
pub fn get_vote_options(env: &Env, proposal_id: u64) -> Result<SVec<bool>, ArchiveError> {
    let len = voter_count(env);
    let mut out = SVec::new(env);
    for index in 0..len {
        out.push_back(has_voted(env, proposal_id, index));
    }
    Ok(out)
}

/// Decode the weight recorded for each eligible voter, in canonical order.
///
/// `0` means the voter has no recorded vote for this proposal. Packed voters
/// read their round-tripped 7-bit field; fallback voters read the shared
/// `AdminWeight` configuration, exactly as the detailed tally does.
pub fn get_vote_weights(env: &Env, proposal_id: u64) -> Result<SVec<u32>, ArchiveError> {
    let len = voter_count(env);
    let voters = get_voters(env);
    let option_bits = get_vote_bits(env, proposal_id);
    let packed_weights = get_packed_weights(env, proposal_id);
    let detailed = get_action_votes(env, proposal_id);

    let mut out = SVec::new(env);
    for index in 0..len {
        let weight = if bits::is_bit_set(&option_bits, index) {
            bits::packed_weight(&packed_weights, index)?
        } else {
            match voters.get(index) {
                Some(voter) if contains_address(&detailed, &voter) => admin_weight(env, voter),
                _ => 0,
            }
        };
        out.push_back(weight);
    }
    Ok(out)
}

// ─── Tally and finalization ─────────────────────────────────────────────────

/// Weighted tally of a proposal, valid in every lifecycle state.
///
/// * archived -> the frozen final tally stored in the archive record
/// * otherwise -> exact sum of the packed 7-bit weights over set bits, plus
///   the `AdminWeight` (default 1) of every fallback participant
///
/// The two layouts are disjoint, so adding them cannot double count. A register
/// that is entirely detailed contributes only the second term, and a register
/// that is entirely packed only the first, which is why this is byte-for-byte
/// equivalent to the pre-optimization tally in either case.
pub fn tally(env: &Env, proposal_id: u64) -> Result<u64, ArchiveError> {
    load_proposal(env, proposal_id)?;

    if let Some(record) = get_archive(env, proposal_id) {
        return Ok(record.final_tally);
    }

    let len = voter_count(env);
    let option_bits = get_vote_bits(env, proposal_id);
    let weights = get_packed_weights(env, proposal_id);
    let mut total = bits::packed_weight_sum(&option_bits, &weights, len);

    for voter in get_action_votes(env, proposal_id).iter() {
        total = total.saturating_add(admin_weight(env, voter) as u64);
    }
    Ok(total)
}

/// Weighted tally of a legacy detailed register.
pub fn tally_detailed(env: &Env, proposal_id: u64) -> Result<u64, ArchiveError> {
    load_proposal(env, proposal_id)?;
    let mut total: u64 = 0;
    for voter in get_action_votes(env, proposal_id).iter() {
        total = total.saturating_add(admin_weight(env, voter) as u64);
    }
    Ok(total)
}

/// Freeze the outcome of a proposal.
///
/// The tally is computed from whatever register layout is present and stored in
/// `ProposalState::final_tally` **before** any compression can run. A proposal
/// whose tally misses the threshold is rejected without mutating storage.
pub fn finalize(env: &Env, proposal_id: u64) -> Result<ProposalState, ArchiveError> {
    let mut state = load_proposal(env, proposal_id)?;
    if state.executed {
        return Ok(state);
    }
    if state.cancelled {
        return Err(ArchiveError::ProposalClosed);
    }

    let total = tally(env, proposal_id)?;
    let threshold = required_threshold(env) as u64;
    if total < threshold {
        return Err(ArchiveError::ThresholdNotReached);
    }

    state.final_tally = total;
    state.executed = true;
    set_proposal(env, proposal_id, &state);
    Ok(state)
}

// ─── Post-execution compression ─────────────────────────────────────────────

/// Replace a finalized proposal's detailed voter register with a Merkle root.
///
/// Ordering of operations is deliberate: the leaf list and root are computed
/// and persisted **before** any detailed entry is removed, so a failure cannot
/// destroy history that has not been committed to the root.
///
/// Idempotency: calling this a second time returns
/// [`ArchiveError::AlreadyCompressed`] instead of overwriting the stored root.
pub fn compress_after_execution(
    env: &Env,
    proposal_id: u64,
) -> Result<ArchiveRecord, ArchiveError> {
    let state = load_proposal(env, proposal_id)?;
    if !state.executed {
        return Err(ArchiveError::ProposalNotExecuted);
    }
    if env.storage().persistent().has(&DataKey::Archive(proposal_id)) {
        return Err(ArchiveError::AlreadyCompressed);
    }

    let participants = participants(env, proposal_id)?;
    let mut leaves = SVec::new(env);
    for participant in participants.iter() {
        leaves.push_back(merkle::leaf_hash(
            env,
            proposal_id,
            participant.index,
            true,
            participant.weight,
            &participant.voter,
        ));
    }

    let record = ArchiveRecord {
        root: merkle::merkle_root(env, &leaves),
        leaf_count: leaves.len(),
        final_tally: state.final_tally,
    };
    let archive_key = DataKey::Archive(proposal_id);
    env.storage().persistent().set(&archive_key, &record);
    env.storage()
        .persistent()
        .extend_ttl(&archive_key, REGISTER_TTL_THRESHOLD, REGISTER_TTL_EXTEND_TO);

    // The root is durably stored — the detailed per-voter register may go.
    env.storage()
        .persistent()
        .remove(&DataKey::VoteBits(proposal_id));
    env.storage()
        .persistent()
        .remove(&DataKey::PackedWeights(proposal_id));
    env.storage()
        .persistent()
        .remove(&DataKey::ActionVotes(proposal_id));

    Ok(record)
}

pub fn get_archive(env: &Env, proposal_id: u64) -> Option<ArchiveRecord> {
    let key = DataKey::Archive(proposal_id);
    let record: Option<ArchiveRecord> = env.storage().persistent().get(&key);
    if record.is_some() {
        env.storage()
            .persistent()
            .extend_ttl(&key, REGISTER_TTL_THRESHOLD, REGISTER_TTL_EXTEND_TO);
    }
    record
}

/// Recompute a canonical leaf, exactly as compression does.
///
/// Exposed so off-chain indexers and auditors can rebuild leaves from their own
/// record of the historical votes and check them against the stored root.
pub fn build_leaf(
    env: &Env,
    proposal_id: u64,
    voter_index: u32,
    voter: Address,
    option: bool,
    weight: u32,
) -> BytesN<32> {
    merkle::leaf_hash(env, proposal_id, voter_index, option, weight, &voter)
}

/// Merkle inclusion proof for one voter's historical vote.
///
/// Only available while the detailed register is still present — after
/// compression the leaves are gone and proofs must be recomputed off-chain from
/// the canonical encoding (`build_leaf`).
pub fn merkle_proof_for_voter(
    env: &Env,
    proposal_id: u64,
    voter_index: u32,
) -> Result<SVec<BytesN<32>>, ArchiveError> {
    load_proposal(env, proposal_id)?;
    if env
        .storage()
        .persistent()
        .has(&DataKey::Archive(proposal_id))
    {
        return Err(ArchiveError::AlreadyCompressed);
    }

    let participants = participants(env, proposal_id)?;
    let mut leaves = SVec::new(env);
    let mut position: Option<u32> = None;
    for participant in participants.iter() {
        if participant.index == voter_index {
            position = Some(leaves.len());
        }
        leaves.push_back(merkle::leaf_hash(
            env,
            proposal_id,
            participant.index,
            true,
            participant.weight,
            &participant.voter,
        ));
    }

    let position = position.ok_or(ArchiveError::UnknownVoter)?;
    Ok(merkle::merkle_proof(env, &leaves, position))
}

/// Verify a historical vote against a stored archive root.
///
/// `leaf_position` is the leaf's rank in the canonical (ascending voter index)
/// leaf list — i.e. how many participating voters have a lower voter index.
pub fn verify_archived_vote(
    env: &Env,
    proposal_id: u64,
    leaf_position: u32,
    voter_index: u32,
    voter: Address,
    option: bool,
    weight: u32,
    proof: &SVec<BytesN<32>>,
) -> Result<bool, ArchiveError> {
    let record = get_archive(env, proposal_id).ok_or(ArchiveError::ArchiveNotFound)?;
    let leaf = merkle::leaf_hash(env, proposal_id, voter_index, option, weight, &voter);
    Ok(merkle::verify_proof(
        env,
        &record.root,
        &leaf,
        leaf_position,
        record.leaf_count,
        proof,
    ))
}

/// Resolve the participants of a proposal, ascending by canonical voter index.
///
/// Covers both layouts so compression archives every recorded vote: packed
/// voters come from the set bits, fallback voters from the detailed register.
/// The resulting order is the canonical Merkle leaf order.
pub fn participants(env: &Env, proposal_id: u64) -> Result<Vec<Participant>, ArchiveError> {
    let voters = get_voters(env);
    let option_bits = get_vote_bits(env, proposal_id);
    let weights = get_packed_weights(env, proposal_id);
    let mut list: Vec<Participant> = Vec::new();

    for index in 0..voters.len() {
        if bits::is_bit_set(&option_bits, index) {
            list.push(Participant {
                index,
                voter: voters.get(index).unwrap(),
                weight: bits::packed_weight(&weights, index)?,
            });
        }
    }

    for voter in get_action_votes(env, proposal_id).iter() {
        let index = voter_index(env, &voter).ok_or(ArchiveError::UnknownVoter)?;
        let already_listed = list.iter().any(|participant| participant.index == index);
        if !already_listed {
            list.push(Participant {
                index,
                weight: admin_weight(env, voter.clone()),
                voter,
            });
        }
    }

    list.sort_by_key(|participant| participant.index);
    Ok(list)
}

/// Whether an address appears in a voter list. Linear by design: an eligible
/// voter set is small and this keeps the serialized representation honest.
fn contains_address(voters: &SVec<Address>, target: &Address) -> bool {
    for voter in voters.iter() {
        if voter == *target {
            return true;
        }
    }
    false
}

// ─── Raw register accessors ─────────────────────────────────────────────────

pub fn get_vote_bits(env: &Env, proposal_id: u64) -> SVec<u64> {
    env.storage()
        .persistent()
        .get(&DataKey::VoteBits(proposal_id))
        .unwrap_or_else(|| SVec::new(env))
}

pub fn get_packed_weights(env: &Env, proposal_id: u64) -> SVec<u64> {
    env.storage()
        .persistent()
        .get(&DataKey::PackedWeights(proposal_id))
        .unwrap_or_else(|| SVec::new(env))
}

pub fn get_action_votes(env: &Env, proposal_id: u64) -> SVec<Address> {
    env.storage()
        .persistent()
        .get(&DataKey::ActionVotes(proposal_id))
        .unwrap_or_else(|| SVec::new(env))
}

fn set_voters(env: &Env, voters: &SVec<Address>) {
    let key = DataKey::Voters;
    env.storage().persistent().set(&key, voters);
    env.storage()
        .persistent()
        .extend_ttl(&key, REGISTER_TTL_THRESHOLD, REGISTER_TTL_EXTEND_TO);
}

fn store_vote_bits(env: &Env, proposal_id: u64, words: &SVec<u64>) {
    let key = DataKey::VoteBits(proposal_id);
    env.storage().persistent().set(&key, words);
    env.storage()
        .persistent()
        .extend_ttl(&key, REGISTER_TTL_THRESHOLD, REGISTER_TTL_EXTEND_TO);
}

fn store_packed_weights(env: &Env, proposal_id: u64, words: &SVec<u64>) {
    let key = DataKey::PackedWeights(proposal_id);
    env.storage().persistent().set(&key, words);
    env.storage()
        .persistent()
        .extend_ttl(&key, REGISTER_TTL_THRESHOLD, REGISTER_TTL_EXTEND_TO);
}

fn store_action_votes(env: &Env, proposal_id: u64, voters: &SVec<Address>) {
    let key = DataKey::ActionVotes(proposal_id);
    env.storage().persistent().set(&key, voters);
    env.storage()
        .persistent()
        .extend_ttl(&key, REGISTER_TTL_THRESHOLD, REGISTER_TTL_EXTEND_TO);
}
