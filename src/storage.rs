use soroban_sdk::{contracttype, Address, Env, Symbol, Map};
use crate::NodeProfile;

/// Fixed-size tuple-based storage keys for gas-optimized lookups.
/// Replaces dynamic Map structures with direct tuple keys.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    Subscription(Address),
    AssetPrice(Symbol),
}

/// Tuple-based stake storage key: (node_address) -> stake_amount
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StakeKey(Address);

/// Tuple-based heartbeat storage key: (asset_id) -> timestamp
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeartbeatKey(u32);

/// Tuple-based node profile storage key: (node_address) -> NodeProfile
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeProfileKey(Address);

/// Tuple-based signer storage key: (signer_address) -> unit
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignerKey(Address);

/// Tuple-based revoked signer storage key: (revoked_address) -> unit
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RevokedSignerKey(Address);

/// Tuple-based sequence tracker key: (asset_symbol) -> sequence_number
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SequenceKey(Symbol);

/// Tuple-based feed stake storage key: (node_address, asset_symbol) -> stake_amount
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeedStakeKey(Address, Symbol);

/// Tuple-based asset metrics storage key: (asset_symbol) -> AssetFeedMetrics
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetMetricsKey(Symbol);

/// Tuple-based corridor fee pool storage key: (asset_symbol) -> CorridorFeePool
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorridorFeeKey(Symbol);

pub const RENT_THRESHOLD: u32 = 259_200;
pub const RENT_EXTEND_TO: u32 = 518_400;

pub const ASSET_TTL_THRESHOLD: u32 = 5_000;
pub const ASSET_TTL_EXTEND_TO: u32 = 100_000;

pub const PROFILE_TTL_THRESHOLD: u32 = 10_000;

pub fn get_node_profiles(env: &Env) -> Map<Address, NodeProfile> {
    let key = Symbol::new(env, "NODES");
    if env.storage().persistent().has(&key) {
        env.storage()
            .persistent()
            .extend_ttl(&key, PROFILE_TTL_THRESHOLD, env.storage().max_ttl());
    }
    env.storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(|| Map::new(env))
}

pub fn extend_subscription_rent(env: &Env, consumer_id: Address) {
    let key = DataKey::Subscription(consumer_id);
    env.storage().persistent().extend_ttl(&key, RENT_THRESHOLD, RENT_EXTEND_TO);
}

pub fn preflight_rent_check(_env: &Env) {}

pub fn check_subscription(env: &Env, consumer_id: Address) -> bool {
    let key = DataKey::Subscription(consumer_id.clone());
    if env.storage().persistent().has(&key) {
        extend_subscription_rent(env, consumer_id);
        true
    } else {
        false
    }
}

/// Pre-flight rent check for storage entries
pub fn preflight_rent_check(env: &Env) {
    // This hook can be extended to check TTL of critical storage entries
    // before executing operations that depend on them.
    // Currently a no-op placeholder for future rent management.
pub fn extend_asset_rent(env: &Env, asset: Symbol) -> bool {
    let key = DataKey::AssetPrice(asset);
    if env.storage().persistent().has(&key) {
        env.storage().persistent().extend_ttl(&key, ASSET_TTL_THRESHOLD, ASSET_TTL_EXTEND_TO);
        true
    } else {
        false
    }
}

pub fn preflight_rent_check(env: &Env) {
    env.storage().instance().extend_ttl(0, ASSET_TTL_THRESHOLD);
}

