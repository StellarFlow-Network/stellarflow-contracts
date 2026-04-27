use soroban_sdk::{contracttype, Address, Symbol};

/// Storage keys for the price oracle storage contract
#[allow(clippy::enum_variant_names)]
#[contracttype]
pub enum DataKey {
    Admin,
    BaseCurrencyPairs,
    PriceData,
    PriceBuffer,
    PriceBoundsData,
    IsLocked,
    PriceFloorData,
    AssetDescription(Symbol),
    PendingAdmin,
    PendingAdminTimestamp,
    AdminUpdateTimestamp,
    RecentEvents,
    Initialized,
    Twap(Symbol),
    VerifiedPrice(Symbol),
    CommunityPrice(Symbol),
    QueryFee,
    Destroyed,
    AssetMeta(Symbol),
    PriceUpdateSubscribers,
    CommunityCouncil,
    EmergencyFrozen,
    ProposedAction(u64),
    ActionVotes(u64),
    ActionIdCounter,
}

/// Decimal metadata for an asset pair
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetMeta {
    pub base_decimals: u32,
    pub quote_decimals: u32,
}

/// Canonical storage format for a price entry
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceData {
    pub price: i128,
    pub timestamp: u64,
    pub provider: Address,
    pub decimals: u32,
}

/// Asset weight for index calculations
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetWeight {
    pub asset: Symbol,
    pub weight: u32,
}
