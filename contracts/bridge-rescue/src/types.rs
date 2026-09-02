use soroban_sdk::{contracttype, Address, String};

/// Storage keys for contract data.
#[contracttype]
pub enum DataKey {
    /// Marks the contract as initialized; guards against double-init.
    Initialized,
    /// The M-of-N admin committee that must approve a rescue.
    Admins,
    /// Number of distinct admin approvals required to trigger a rescue (the "M" of M-of-N).
    AdminThreshold,
    /// The validator set that attests to permanent cross-chain delivery failure.
    Validators,
    /// Number of distinct validator attestations required to confirm failure proof.
    ValidatorThreshold,
    /// The SAC (Stellar Asset Contract) token address that gets bridged/locked.
    Token,
    /// Monotonically increasing counter used to hand out unique `BridgeLock` ids.
    NextLockId,
    /// The `BridgeLock` record for a given lock id.
    Lock(u64),
    /// Whether a given validator has already voted (submitted failure proof) on a lock.
    /// Composite key: (lock_id, validator).
    ValidatorVote(u64, Address),
    /// Number of distinct validator attestations recorded so far for a lock.
    ValidatorVoteCount(u64),
    /// Whether a given admin has already approved the rescue for a lock.
    /// Composite key: (lock_id, admin).
    AdminApproval(u64, Address),
    /// Number of distinct admin approvals recorded so far for a lock.
    AdminApprovalCount(u64),
}

/// Lifecycle status of a bridge lock.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockStatus {
    /// Funds are held by the contract, awaiting cross-chain delivery (or rescue).
    Locked,
    /// Funds have been returned to the original sender via the rescue flow.
    /// Terminal state — a `Rescued` lock can never be rescued again.
    Rescued,
}

/// A single cross-chain bridge lock record.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BridgeLock {
    /// Unique identifier for this lock.
    pub id: u64,
    /// The account that deposited the tokens (and the account funds return to on rescue).
    pub sender: Address,
    /// The amount of `token` deposited into the contract, in stroops.
    pub amount: i128,
    /// Current lifecycle status of this lock.
    pub status: LockStatus,
    /// Opaque reference to the destination chain / address the bridge was targeting
    /// (e.g. an encoded destination chain id + address). Not interpreted on-chain.
    pub dest_chain_ref: String,
    /// Whether validator consensus has confirmed permanent delivery failure for this lock.
    /// Once `true` this never reverts back to `false`.
    pub validator_confirmed: bool,
}
