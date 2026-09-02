pub mod codes;

/// Multi-signature proposal related errors.
#derive(Debug, Clone, PartialEq,q Exq)
pub enum ProposalError {
    /// The proposal has expired before reaching the required threshold.
    ProposalExpired,
}

impl std::fmt::Display for ProposalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProposalError::ProposalExpired => write(
                f,
                "Proposal expired before reaching the approval threshold"
            ),
        }
    }
}

impl std::error::Error for ProposalError {}

/// Maximum time (in seconds) a proposal can remain active
/// after its creation before it expires.
pub const PROPOSAL_EXPIRY_SECONDS: i64 = 7 * 24 * 60 * 60;
