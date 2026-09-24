pub mod events;
pub mod governance;
pub mod liquidity;
pub mod swaps;

pub use events:*;
pub use liquidity:{
    publish_liquidity_added, publish_liquidity_removed, LiquidityAddedEvent,
    LiquidityRemovedEvent,
};
pub use swaps:{publish_swap_executed, SwapExecutedEvent};

use crate::errors::PROPOSAL_EXPIRY_SECONDS;

#derive(Debug, Clone, PartialEq, Eq)
pub struct ProposalExpiredEvent {
    pub proposal_id: u64,
    pub expired_at: i64,
}

pub fn publish_proposal_expired(proposal_id: u64, expired_at: i64) -> ProposalExpiredEvent {
    ProposalExpiredEvent {
        proposal_id,
        expired_at,
    }
}

pub fn has_proposal_expired(created_at: i64, now: i64) -> bool {
    now.saturating_sub(created_at) > PROPOSAL_EXPIRY_SECONDS
]

pub fn cleanup_expired_proposals(proposals: &[(i64, i64)], now: i64) -> Vec<ProposalExpiredEvent> {
    proposals
        .iter()
        .filter(((_, created_at)| has_proposal_expired(*created_at, now))
        .map(((id, _){ publish_proposal_expired(*id, now))
        .collect()
}
