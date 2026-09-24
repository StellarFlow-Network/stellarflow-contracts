use soroban_sdk::{contracttype, Address, Vec};

/// Storage keys.
#[contracttype]
pub enum DataKey {
    Admin,
    Token,
    Initialized,
    NextOrderId,
    /// Full order record (persistent).
    Order(u64),
    /// Cumulative partial escrow released for an order (`E_partial` sum).
    /// Stored in **instance** storage per the split-payment state machine.
    PartialReleased(u64),
    /// Remaining locked escrow for an order (`E_total - released - refunded`).
    /// Stored in **instance** storage.
    RemainingLocked(u64),
}

/// Per-leg settlement status.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LegStatus {
    Pending,
    Settled,
    Refunded,
}

/// Aggregate order status.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OrderStatus {
    /// At least one leg still pending settlement.
    Open,
    /// Every destination anchor settled successfully.
    Completed,
    /// Unsettled legs were refunded after the 12h window; some legs may have settled.
    Refunded,
}

/// One payout destination in a split remittance.
///
/// `amount` is the precomputed partial escrow release:
/// `E_partial = E_total × p_anchor / 10_000` (basis points).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DestinationLeg {
    pub anchor: Address,
    /// Anchor share in basis points (`p_anchor`). 10_000 = 100%.
    pub proportion_bps: u32,
    /// Locked amount for this leg (`E_partial`).
    pub amount: i128,
    pub status: LegStatus,
}

/// A single remittance order split across multiple destination anchors.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SplitOrder {
    pub id: u64,
    pub sender: Address,
    /// Total escrowed amount `E_total`.
    pub total_amount: i128,
    /// Ledger timestamp when the order was created.
    pub created_at: u64,
    /// Absolute deadline: anchors must settle before this instant
    /// (`created_at + SETTLE_WINDOW_SECS`).
    pub deadline: u64,
    pub status: OrderStatus,
    pub legs: Vec<DestinationLeg>,
}
