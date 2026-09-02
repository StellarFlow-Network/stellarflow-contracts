use soroban_sdk::{contracttype, Address};

/// Storage keys for contract data.
#[contracttype]
pub enum DataKey {
    /// Whether `initialize` has already been called.
    Initialized,
    /// Address of the first pooled asset.
    TokenA,
    /// Address of the second pooled asset.
    TokenB,
    /// The pool's current reserves and total minted shares (`PoolReserves`).
    Reserves,
    /// A single user's LP share balance, keyed by their address.
    UserShares(Address),
}

/// The constant-product pool's reserves and total outstanding LP shares.
///
/// `total_shares` tracks the sum of every `UserShares` entry so that a
/// user's proportional claim on the pool can be computed as
/// `user_shares / total_shares`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PoolReserves {
    /// Current balance of token A held by the pool.
    pub reserve_a: i128,
    /// Current balance of token B held by the pool.
    pub reserve_b: i128,
    /// Total LP shares minted across all liquidity providers.
    pub total_shares: i128,
}
