use soroban_sdk::{contracttype, Address};

/// Storage keys for contract data
#[contracttype]
pub enum DataKey {
    Admin,
    BaseCurrencyPairs,
    PriceData,
}

/// Canonical storage format for a price entry.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceData {
    /// The price value stored as a scaled integer.
    pub price: i128,
    /// Ledger timestamp when this price was written.
    pub timestamp: u64,
    /// Address that provided the price update.
    pub provider: Address,
    /// Number of decimals for the price value.
    pub decimals: u32,
    /// Confidence score (0-100, higher is more confident)
    pub confidence_score: u32,
    /// Time-to-live in seconds for this price (per-asset expiration)
    pub ttl: u64,
}

/// A simplified price entry for external consumers.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceEntry {
    pub price: i128,
    pub timestamp: u64,
}
