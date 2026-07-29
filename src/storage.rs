//! Gas-optimized storage keys and helper utilities for the StellarFlow contract.
//!
//! This module defines all [`contracttype`] storage keys used across the contract,
//! replacing dynamic Map structures with fixed-size tuple keys for gas efficiency.
//! It also provides helper functions for node profile management, subscription
//! rent extension, and asset price TTL management.
use soroban_sdk::{contracttype, Address, Env, Symbol, Map};
use crate::NodeProfile;

/// Fixed-size tuple-based storage keys for gas-optimized lookups.
/// Replaces dynamic Map structures with direct tuple keys.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    /// Subscription record keyed by consumer [`Address`].
    Subscription(Address),
    /// Asset price entry keyed by a [`Symbol`].
    AssetPrice(Symbol),
}

// NOTE: These are single-variant enums, not bare tuple structs. A single-field
// tuple struct like `pub struct StakeKey(Address)` serializes to a plain
// `Vec![address]` with no type tag, so two different bare tuple-struct types
// wrapping the same address (e.g. StakeKey(addr) and RevokedSignerKey(addr))
// collide on the exact same storage slot.
//
// Wrapping each in an enum adds a discriminant to the serialized value — but
// Soroban's `#[contracttype]` enum encoding namespaces only by the *variant
// name* (as a Symbol), not by the Rust type name. Two different enums that
// happen to share a variant name with the same field shape (e.g. two enums
// both using a variant called `Asset(Symbol)`) still collide. Every variant
// name below is therefore kept globally unique across the whole contract's
// storage keys, not just unique within its own enum.

/// Tuple-based stake storage key: (node_address) -> stake_amount
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StakeKey {
    StakeByNode(Address),
}

/// Tuple-based heartbeat storage key: (asset_id) -> timestamp
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HeartbeatKey {
    HeartbeatByAsset(u32),
}

/// Tuple-based node profile storage key: (node_address) -> NodeProfile
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NodeProfileKey {
    ProfileByNode(Address),
}

/// Tuple-based signer storage key: (signer_address) -> unit
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SignerKey {
    SignerByAddress(Address),
}

/// Tuple-based revoked signer storage key: (revoked_address) -> unit
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RevokedSignerKey {
    RevokedByAddress(Address),
}

/// Tuple-based sequence tracker key: (asset_symbol) -> sequence_number
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SequenceKey {
    SequenceByAsset(Symbol),
}

/// Tuple-based feed stake storage key: (node_address, asset_symbol) -> stake_amount
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FeedStakeKey {
    FeedStakeByNode(Address, Symbol),
}

/// Tuple-based asset metrics storage key: (asset_symbol) -> AssetFeedMetrics
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AssetMetricsKey {
    MetricsByAsset(Symbol),
}

/// Tuple-based corridor fee pool storage key: (asset_symbol) -> CorridorFeePool
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CorridorFeeKey {
    FeeByAsset(Symbol),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeedStakeValue {
    pub amount: u64,
    pub last_active: u64,
}

pub const RENT_THRESHOLD: u32 = 259_200;
pub const RENT_EXTEND_TO: u32 = 518_400;

pub const ASSET_TTL_THRESHOLD: u32 = 5_000;
pub const ASSET_TTL_EXTEND_TO: u32 = 100_000;

pub const PROFILE_TTL_THRESHOLD: u32 = 10_000;

/// Default TTL renewal threshold for persistent entries: 31 days (535,680 ledgers).
///
/// Soroban persistent entries have a maximum TTL. This threshold ensures entries
/// are renewed well before expiration so state mutations never cause silent
/// storage expiry during normal contract operation.
///
/// Calculation: 31 days × 24 hours × 60 minutes × 60 seconds / 5-second ledger ≈ 535,680
pub const PERSISTENT_TTL_THRESHOLD: u32 = 535_680;

pub fn get_node_profiles(env: &Env) -> Map<Address, NodeProfile> {
    let key = Symbol::new(env, "NODES");
    if env.storage().persistent().has(&key) {
        extend_persistent_ttl(env, &key);
    }
    env.storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(|| Map::new(env))
}

pub fn extend_subscription_rent(env: &Env, consumer_id: Address) {
    let key = DataKey::Subscription(consumer_id);
    extend_persistent_ttl(env, &key);
}

pub fn check_subscription(env: &Env, consumer_id: Address) -> bool {
    let key = DataKey::Subscription(consumer_id.clone());
    if env.storage().persistent().has(&key) {
        extend_subscription_rent(env, consumer_id);
        true
    } else {
        false
    }
}

pub fn extend_asset_rent(env: &Env, asset: Symbol) -> bool {
    let key = DataKey::AssetPrice(asset);
    if env.storage().persistent().has(&key) {
        extend_persistent_ttl(env, &key);
        true
    } else {
        false
    }
}

/// Pre-flight rent check for storage entries: extends the contract's own
/// instance-storage TTL so gating logic that depends on instance data never
/// trips over an expired instance entry.
pub fn preflight_rent_check(env: &Env) {
    env.storage().instance().extend_ttl(0, ASSET_TTL_THRESHOLD);
}

pub fn extend_persistent_ttl<K>(env: &Env, key: &K)
where
    K: soroban_sdk::IntoVal<Env, soroban_sdk::Val> + Clone,
{
    env.storage().persistent().extend_ttl(key, PERSISTENT_TTL_THRESHOLD, PERSISTENT_TTL_THRESHOLD + 100_000);
}

/// Prune a feed stake entry that has gone stale (issue #522: storage-rent
/// expiry gates for regional validators). Returns `true` if the entry was
/// found and pruned, `false` if it was absent or still active.
pub fn check_and_prune_feed_stake(env: &Env, node: Address, asset: u32) -> bool {
    let key = crate::StakingStorageKey::FeedStake(node.clone(), asset);
    if !env.storage().persistent().has(&key) {
        return false;
    }

    let val: FeedStakeValue = env.storage().persistent().get(&key).unwrap();
    let elapsed = env.ledger().timestamp().saturating_sub(val.last_active);

    if elapsed > RENT_THRESHOLD as u64 {
        env.storage().persistent().remove(&key);

        let mut stakes: Map<Address, u64> = env
            .storage()
            .instance()
            .get(&crate::STAKE_REGISTRY_KEY)
            .unwrap_or_else(|| Map::new(env));
        let node_total = stakes.get(node.clone()).unwrap_or(0);
        let new_node_total = node_total.saturating_sub(val.amount);
        if new_node_total == 0 {
            stakes.remove(node.clone());
        } else {
            stakes.set(node.clone(), new_node_total);
        }
        env.storage().instance().set(&crate::STAKE_REGISTRY_KEY, &stakes);

        let total: u64 = env
            .storage()
            .instance()
            .get(&crate::TOTAL_STAKED_KEY)
            .unwrap_or(0u64);
        let new_total = total.saturating_sub(val.amount);
        env.storage().instance().set(&crate::TOTAL_STAKED_KEY, &new_total);

        true
    } else {
        false
    }
}

/// Refresh a feed stake's activity timestamp and extend its persistent TTL —
/// the "auto-restoration" half of issue #522: an active validator's stake
/// entry is renewed before it can expire, resetting the rent-expiry clock.
pub fn update_feed_stake_activity(env: &Env, node: Address, asset: u32) {
    let key = crate::StakingStorageKey::FeedStake(node, asset);
    if let Some(mut val) = env.storage().persistent().get::<_, FeedStakeValue>(&key) {
        val.last_active = env.ledger().timestamp();
        env.storage().persistent().set(&key, &val);
        env.storage().persistent().extend_ttl(&key, RENT_THRESHOLD, RENT_EXTEND_TO);
    }
}


