pub mod codes;

/// Multi-signature proposal related errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposalError {
    /// The proposal has expired before reaching the required threshold.
    ProposalExpired,
}

/// Maximum time (in seconds) a proposal can remain active
/// after its creation before it expires.
pub const PROPOSAL_EXPIRY_SECONDS: i64 = 7 * 24 * 60 * 60;
