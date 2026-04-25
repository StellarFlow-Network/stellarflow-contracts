use soroban_sdk::{contracttype, Address, Symbol};

/// Storage keys for contract data
#[allow(clippy::enum_variant_names)] // Soroban SDK generates these names
#[contracttype]
pub enum DataKey {
    Admin,
    BaseCurrencyPairs,
    /// Legacy flat price map — kept for migration compatibility only.
    PriceData,
    PriceBuffer,
    PriceBoundsData,
    PriceFloorData,
    AssetDescription(Symbol),
    PendingAdmin,
    PendingAdminTimestamp,
    AdminUpdateTimestamp,
    RecentEvents,
    Initialized,
    /// Verified price bucket: written only by whitelisted providers / admins.
    /// Internal math and `get_price` default to this bucket.
    VerifiedPrice(Symbol),
    /// Community price bucket: written by any caller; never used in internal math.
    CommunityPrice(Symbol),
    /// Query fee amount for get_price calls (in stroops).
    QueryFee,
    /// Destroyed flag to mark contract as permanently unusable.
    Destroyed,
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
    pub decimals: u32,
}

/// Full price payload returned to consumers with freshness status.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceDataWithStatus {
    pub data: PriceData,
    pub is_stale: bool,
}

/// Lightweight price payload returned to consumers with freshness status.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceEntryWithStatus {
    pub price: i128,
    pub timestamp: u64,
    pub is_stale: bool,
}

/// Min/max price bounds for an asset to prevent fat-finger errors.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceBounds {
    pub min_price: i128,
    pub max_price: i128,
}

/// A recent activity event for the dashboard feed.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecentEvent {
    pub event_type: soroban_sdk::Symbol,
    pub asset: soroban_sdk::Symbol,
    pub price: i128,
    pub timestamp: u64,
}

/// A single relayer price submission within the current ledger buffer.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceBufferEntry {
    /// The price value submitted by this relayer.
    pub price: i128,
    /// Address of the relayer who submitted this price.
    pub provider: Address,
    /// Timestamp when this price was submitted.
    pub timestamp: u64,
}

/// Buffer containing multiple relayer submissions for median calculation.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceBuffer {
    /// List of price entries from different relayers for the current ledger.
    pub entries: soroban_sdk::Vec<PriceBufferEntry>,
    /// The ledger sequence number this buffer belongs to.
    pub ledger_sequence: u32,
    /// Number of decimals for the price values.
    pub decimals: u32,
    /// Time-to-live in seconds for this buffer.
    pub ttl: u64,
}

/// Health status of the oracle for the Admin Dashboard.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleHealth {
    /// Number of active relayers (whitelisted providers).
    pub active_relayers: u32,
    /// Whether the contract is currently paused.
    pub paused: bool,
    /// Total number of tracked assets.
    pub total_assets: u32,
    /// Current ledger sequence number.
    pub last_ledger: u32,
}
