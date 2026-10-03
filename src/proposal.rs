//! Multi-sig proposal approval state shared with the admin cleanup routines.

use soroban_sdk::{contracttype, Address};

/// Lifecycle status of a multi-sig proposal approval record.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProposalStatus {
    /// Proposal is open and collecting approvals.
    Pending,
    /// Proposal reached its approval threshold.
    Active,
    /// Proposal was approved and executed.
    Approved,
    /// Proposal expired before reaching the threshold.
    Expired,
    /// Proposal was executed (alias / governance variant).
    Executed,
    /// Proposal was rejected.
    Rejected,
}

/// Persistent approval state tracked for a multi-sig proposal.
#[contracttype]
#[derive(Clone)]
pub struct ProposalState {
    /// Ledger timestamp at which the proposal was created.
    pub created_at: u64,
    /// Number of approvals collected so far.
    pub approvals: u32,
    /// Number of approvals required for the threshold.
    pub threshold: u32,
    /// Current lifecycle status.
    pub status: ProposalStatus,
}

/// Storage key for a proposal's approval state, keyed by the proposal
/// identifier address.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProposalStorageKey {
    Proposal(Address),
    /// Votes/approvals sub-record for a proposal.
    Votes(Address),
}
