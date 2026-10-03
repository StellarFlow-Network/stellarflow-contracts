#![no_std]
//! Compact storage + post-execution archival compression for historical
//! governance vote registers (issue #978).
//!
//! ## Why
//!
//! The repository's governance vote register keeps a full `Vec<Address>` of
//! participants plus a whole `u32` governance weight per voter address
//! (`contracts/price-oracle/src/auth.rs`: `_set_action_votes`,
//! `_add_action_vote`, `_has_reached_threshold`). Every vote therefore
//! persists a ~44-byte address, and the register grows without bound after a
//! proposal has already been decided.
//!
//! ## What this contract provides
//!
//! | Deliverable | Where |
//! | --- | --- |
//! | Boolean vote options packed into bit-fields | [`bits::pack_vote_options`] |
//! | Vote weights packed into 7-bit fields | [`bits::pack_weights`] |
//! | Evidence-first gate before packing anything | [`bits::OBSERVED_VOTE_MODEL`], [`bits::encoding_for`] |
//! | Fallback to the detailed register when packing is unsupported | [`bits::Encoding::DetailedFallback`], [`register::cast_vote`] |
//! | Post-execution compression to one Merkle root | [`register::compress_after_execution`] |
//! | Historical verification against the root | [`register::verify_archived_vote`] |
//! | Measured before/after storage footprint | `src/benchmark.rs` (`#[cfg(test)]`, 1,000 submissions) |
//!
//! ## Scope and honest limits
//!
//! * **No fractional vote weights exist in this repository.** Governance weight
//!   is a whole number in `1..=100` (price-oracle issue #264), so there is no
//!   fractional representation to pack and none is silently reduced. This is
//!   not an assumption: [`bits::OBSERVED_VOTE_MODEL`] records the observed
//!   model as data and [`bits::VoteModelEvidence::weights_are_packable`] gates
//!   every packing decision on it. If the model ever gains sub-unit precision,
//!   the gate fails and votes fall back to the detailed register instead of
//!   being rounded.
//! * **Unsupported packing has a defined fallback.** A weight that cannot be
//!   represented losslessly is written to the existing detailed register and
//!   still counts toward the tally. No vote is ever rounded, clamped, dropped or
//!   rejected to save bytes.
//! * Compression is refused unless the proposal was finalized/executed, and is
//!   refused again once an archive root exists (see
//!   [`ArchiveError::ProposalNotExecuted`] and
//!   [`ArchiveError::AlreadyCompressed`]).
//! * After compression the per-voter detail is gone: proofs can no longer be
//!   generated on-chain, only verified against the stored root. Off-chain
//!   parties rebuild leaves with [`register::build_leaf`], which uses the same
//!   canonical encoding.
//! * Voter *weights* (`AdminWeight`) are per-address governance configuration,
//!   not per-proposal history, so compression deliberately does not remove
//!   them; removing them would change the tally of future proposals.
//! * Caller **authentication** is enforced once here, at the contract ABI, as
//!   the repository does elsewhere (compare `contracts/price-oracle/src/lib.rs`
//!   calling into `auth::_*` helpers). The helpers in [`register`] only enforce
//!   *authorization* so they remain usable from tests and future entrypoints.

extern crate alloc;

pub mod bits;
pub mod merkle;
pub mod register;

#[cfg(test)]
mod benchmark;
#[cfg(test)]
mod test;

// NOTE: contract ABI signatures deliberately spell out `Vec` rather than a
// type alias. The soroban contract macro maps generic types by their last path
// segment, so a `Vec as SVec` alias in a `#[contractimpl]` signature is not
// recognised and fails with "generics unsupported on user-defined types"
// (compare `contracts/price-oracle/src/lib.rs`, which spells out
// `soroban_sdk::Vec<..>` in its ABI too).
use soroban_sdk::{contract, contracterror, contractimpl, Address, BytesN, Env, Vec};

use register::{ArchiveRecord, ProposalState};

/// Errors returned by the governance vote archive contract.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ArchiveError {
    /// The archive module was initialized twice.
    AlreadyInitialized = 1,
    /// No governance admin has been configured yet.
    NotInitialized = 2,
    /// Caller is not the governance admin.
    NotAdmin = 3,
    /// Caller is not an eligible voter for this register.
    NotAuthorized = 4,
    /// This voter already voted on this proposal.
    AlreadyVoted = 5,
    /// No proposal exists under the supplied id.
    ProposalNotFound = 6,
    /// A proposal already exists under the supplied id.
    ProposalAlreadyExists = 7,
    /// The proposal is finalized, cancelled or archived and accepts no more votes.
    ProposalClosed = 8,
    /// The weighted tally did not reach the required threshold.
    ThresholdNotReached = 9,
    /// Compression was requested before the proposal was finalized.
    ProposalNotExecuted = 10,
    /// An archive root already exists for this proposal.
    AlreadyCompressed = 11,
    /// Vote weight outside the packable `1..=100` range.
    ///
    /// Reaching this from [`bits::encoding_for`] is impossible; the encoding
    /// gate routes such a weight to the detailed fallback instead. It is
    /// returned by the low-level [`bits::pack_weight`] codec, which callers use
    /// only when they have already decided a value must be packed.
    InvalidVoteWeight = 12,
    // 13 is intentionally unused: it previously carried an arbitrary
    // "too many options" limit that the packed field does not actually have.
    /// No archive root exists for this proposal.
    ArchiveNotFound = 14,
    /// A voter in the register is not present in the eligible voter set.
    UnknownVoter = 15,
    /// Packed field shorter than the requested decoded length.
    InvalidPackedLength = 16,
    /// Weight threshold outside the allowed range.
    InvalidThreshold = 17,
}

/// Historical governance vote register with post-execution archival compression.
///
/// Mutating entrypoints authenticate their caller here and delegate to the
/// internal [`register`] helpers, mirroring the rest of the repository.
#[contract]
pub struct GovernanceVoteArchive;

#[contractimpl]
impl GovernanceVoteArchive {
    /// Configure the governance admin. Callable once.
    pub fn initialize(env: Env, admin: Address) -> Result<(), ArchiveError> {
        admin.require_auth();
        register::initialize(&env, admin)
    }

    /// Register an eligible voter and return its canonical index. Admin only.
    pub fn add_voter(env: Env, caller: Address, voter: Address) -> Result<u32, ArchiveError> {
        caller.require_auth();
        register::add_voter(&env, caller, voter)
    }

    /// Set a voter's governance weight (`1..=100`). Admin only.
    pub fn set_admin_weight(
        env: Env,
        caller: Address,
        voter: Address,
        weight: u32,
    ) -> Result<(), ArchiveError> {
        caller.require_auth();
        register::set_admin_weight(&env, caller, voter, weight)
    }

    /// Configure the cumulative weight threshold (`1..=100`). Admin only.
    pub fn set_weight_threshold(
        env: Env,
        caller: Address,
        threshold: u32,
    ) -> Result<(), ArchiveError> {
        caller.require_auth();
        register::set_weight_threshold(&env, caller, threshold)
    }

    /// Open a proposal for voting. Admin only.
    pub fn propose_action(
        env: Env,
        proposer: Address,
        proposal_id: u64,
    ) -> Result<(), ArchiveError> {
        proposer.require_auth();
        register::propose_action(&env, proposer, proposal_id)
    }

    /// Cancel a proposal. Admin only.
    pub fn cancel_action(
        env: Env,
        canceller: Address,
        proposal_id: u64,
    ) -> Result<(), ArchiveError> {
        canceller.require_auth();
        register::cancel_action(&env, canceller, proposal_id)
    }

    /// Cast a vote. The voter must authenticate.
    pub fn cast_vote(env: Env, voter: Address, proposal_id: u64) -> Result<u32, ArchiveError> {
        voter.require_auth();
        register::cast_vote(&env, voter, proposal_id)
    }

    /// Freeze the tally and mark the proposal executed. Admin only.
    pub fn finalize(
        env: Env,
        caller: Address,
        proposal_id: u64,
    ) -> Result<ProposalState, ArchiveError> {
        caller.require_auth();
        require_admin(&env, &caller)?;
        register::finalize(&env, proposal_id)
    }

    /// Replace a finalized proposal's detailed register with a Merkle root.
    /// Admin only.
    pub fn compress_after_execution(
        env: Env,
        caller: Address,
        proposal_id: u64,
    ) -> Result<ArchiveRecord, ArchiveError> {
        caller.require_auth();
        require_admin(&env, &caller)?;
        register::compress_after_execution(&env, proposal_id)
    }

    // ─── Read-only views ────────────────────────────────────────────────────

    /// The configured governance admin.
    pub fn admin(env: Env) -> Result<Address, ArchiveError> {
        register::admin(&env)
    }

    /// Ordered eligible-voter registry; position == canonical voter index.
    pub fn get_voters(env: Env) -> Vec<Address> {
        register::get_voters(&env)
    }

    /// Number of eligible voters.
    pub fn voter_count(env: Env) -> u32 {
        register::voter_count(&env)
    }

    /// Look up a voter's canonical index.
    pub fn voter_index(env: Env, voter: Address) -> Option<u32> {
        register::voter_index(&env, &voter)
    }

    /// Governance weight configured for a voter (default 1).
    pub fn get_admin_weight(env: Env, voter: Address) -> u32 {
        register::admin_weight(&env, voter)
    }

    /// Configured cumulative weight threshold, if any.
    pub fn get_weight_threshold(env: Env) -> Option<u32> {
        register::get_weight_threshold(&env)
    }

    /// Effective threshold (configured value, or the vote-count fallback).
    pub fn required_threshold(env: Env) -> u32 {
        register::required_threshold(&env)
    }

    /// Lifecycle state and frozen tally of a proposal.
    pub fn get_proposal(env: Env, proposal_id: u64) -> Option<ProposalState> {
        register::get_proposal(&env, proposal_id)
    }

    /// Weighted tally, valid before and after compression.
    pub fn tally(env: Env, proposal_id: u64) -> Result<u64, ArchiveError> {
        register::tally(&env, proposal_id)
    }

    /// One boolean per eligible voter: did this voter vote?
    pub fn get_vote_options(env: Env, proposal_id: u64) -> Result<Vec<bool>, ArchiveError> {
        register::get_vote_options(&env, proposal_id)
    }

    /// Weight recorded per eligible voter (0 when they did not vote).
    pub fn get_vote_weights(env: Env, proposal_id: u64) -> Result<Vec<u32>, ArchiveError> {
        register::get_vote_weights(&env, proposal_id)
    }

    /// Compressed archival record, if the proposal has been compressed.
    pub fn get_archive(env: Env, proposal_id: u64) -> Option<ArchiveRecord> {
        register::get_archive(&env, proposal_id)
    }

    /// Merkle inclusion proof while the detailed register still exists.
    pub fn merkle_proof_for_voter(
        env: Env,
        proposal_id: u64,
        voter_index: u32,
    ) -> Result<Vec<BytesN<32>>, ArchiveError> {
        register::merkle_proof_for_voter(&env, proposal_id, voter_index)
    }

    /// Recompute a canonical leaf exactly as compression does.
    pub fn build_leaf(
        env: Env,
        proposal_id: u64,
        voter_index: u32,
        voter: Address,
        option: bool,
        weight: u32,
    ) -> BytesN<32> {
        register::build_leaf(&env, proposal_id, voter_index, voter, option, weight)
    }

    /// Verify a historical vote against a stored archive root.
    pub fn verify_archived_vote(
        env: Env,
        proposal_id: u64,
        leaf_position: u32,
        voter_index: u32,
        voter: Address,
        option: bool,
        weight: u32,
        proof: Vec<BytesN<32>>,
    ) -> Result<bool, ArchiveError> {
        register::verify_archived_vote(
            &env,
            proposal_id,
            leaf_position,
            voter_index,
            voter,
            option,
            weight,
            &proof,
        )
    }
}

/// Ensure `caller` is the configured governance admin.
fn require_admin(env: &Env, caller: &Address) -> Result<(), ArchiveError> {
    if *caller != register::admin(env)? {
        return Err(ArchiveError::NotAdmin);
    }
    Ok(())
}
