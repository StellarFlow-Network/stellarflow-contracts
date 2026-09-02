use soroban_sdk::{contracttype, Address, Bytes};

/// Storage keys for contract data.
#[contracttype]
pub enum DataKey {
    /// The contract administrator.
    Admin,
    /// The SEP-41 / SAC token used for both remittance amounts and anchor collateral.
    Token,
    /// Set once `initialize` has run, guarding against double-initialization.
    Initialized,
    /// Monotonically increasing counter used to allocate remittance ids.
    NextRemittanceId,
    /// A single remittance record, keyed by its id.
    Remittance(u64),
    /// The collateral balance (in token stroops) currently staked by an anchor.
    Collateral(Address),
}

/// Lifecycle status of a remittance.
///
/// A remittance starts `Pending` and resolves exactly once, to either
/// `Completed` (the anchor proved the payout happened) or `Refunded` (the
/// sender successfully disputed a missed deadline). It never transitions
/// out of a terminal state.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RemittanceStatus {
    Pending,
    Completed,
    Refunded,
}

/// A single cross-border remittance escrowed by this contract.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Remittance {
    /// Unique identifier for this remittance.
    pub id: u64,
    /// The party who deposited the funds and who is entitled to a refund on dispute.
    pub sender: Address,
    /// The anchor responsible for delivering the off-chain payout.
    pub anchor: Address,
    /// Amount of token (stroops) escrowed by the contract for this remittance.
    pub amount: i128,
    /// Ledger timestamp (seconds) by which the anchor is expected to have completed
    /// the payout. The sender may open a dispute only after `deadline + 86_400`.
    pub deadline: u64,
    /// Current lifecycle status.
    pub status: RemittanceStatus,
    /// Opaque proof supplied by the anchor via `submit_payout_proof`. Empty
    /// (`Bytes::new`) until proof is submitted.
    ///
    /// Not `Option<Bytes>`: this workspace's pinned soroban-sdk 20.x cannot
    /// round-trip an `Option<Bytes>` field through a `#[contracttype]`
    /// derive (`ScVal: TryFrom<&Option<Bytes>>` is not implemented for this
    /// SDK generation), so "no proof yet" is represented by an empty
    /// `Bytes` value instead of `None`.
    pub proof: Bytes,
}
