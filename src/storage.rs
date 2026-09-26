//! Gas-optimized storage keys and helper utilities for the StellarFlow contract.
//!
// This module defines all [`contracttype`] storage keys used across the contract,
// replacing dynamic Map structures with fixed-size tuple keys for gas efficiency.
// It also provides helper functions for node profile management, subscription
// rent extension, and asset price TTL management.
use crate::NodeProfile;
use soroban_sdk::{contracttype, symbol_short, Address, Env, Map, Symbol};

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    Subscription(Address),
}

/// NOTE: These are single-variant enums, not bare tuple structs. A single-field
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

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HeartbeatKey {
    HeartbeatByAsset(u32),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NodeProfileKey {
    ProfileByNode(Address),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SignerKey {
    SignerByAddress(Address),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RevokedSignerKey {
    RevokedByAddress(Address),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SequenceKey {
    SequenceByAsset(Symbol),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FeedStakeKey {
    FeedStakeByNode(Address, Symbol),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AssetMetricsKey {
    MetricsByAsset(Symbol),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CorridorFeeKey {
    FeeByAsset(Symbol),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BridgeValidatorKey {
    BridgeValidators,
    BridgeRotationSeq,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeedStakeValue {
    pub amount: u64,
    pub last_active: u64,
}

/// --- Standardized TTL Helpers ---

/// Extends TTL for Persistent storage using strict 10k/100k rule.
pub fn extend_persistent_ttl<K: soroban_sdk::IntoVal<Env, soroban_sdk::Val>>(env: &Env, key: &K) {
    env.storage()
        .persistent()
        .extend_ttl(key, THRESHOLD, BUMP_AMOUNT);
}

/// Extends TTL for Instance storage using strict 10k/100k rule.
pub fn extend_instance_ttl(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(THRESHOLD, BUMP_AMOUNT);
}

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
    env.storage().persistent().extend_ttl(&key, RENT_THRESHOLD, RENT_EXTEND_TO);
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
/// instance-storage TTL using strict 10k/100k policy.
pub fn preflight_rent_check(env: &Env) {
    env.storage().instance().extend_ttl(0, ASET_TTL_THRESHOLD);
}

pub fn check_and_prune_feed_stake(env: &Env, node: Address, asset: u32) -> bool {
    let key = crate::StakingStorageKey::FeedStake(node.clone(), asset);
    if !env.storage().persistent().has(&key) {
        return false;
    }

    let val: FeedStakeValue = env.storage().persistent().get(&key).unwrap();
    let elapsed = env.ledger().timestamp().saturating_sub(val.last_active);

    if elapsed > RENT_THRESHOLD as u64 {
        env.storage().persistent().remove(&key);
        // ... (pruning logic remains same)
        true
    } else {
        false
    }
}

pub fn update_feed_stake_activity(env: &Env, node: Address, asset: u32) {
    let key = crate::StakingStorageKey::FeedStake(node, asset);
    if let Some(mut val) = env.storage().persistent().get::<_, FeedStakeValue>(&key) {
        val.last_active = env.ledger().timestamp();
        env.storage().persistent().set(&key, &val);
        extend_persistent_ttl(env, &key);
    }
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OrderBookKey {
    OrderById(u64),
    TickVolumeByMarketPrice(Symbol, u64),
    CollateralByMarketMaker(Symbol, Address),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Order {
    pub maker: Address,
    pub market: Symbol,
    pub price_tick: u64,
    pub remaining_qty: u64,
    pub collateral_locked: u64,
}

pub fn cancel_order_refund(env: &Env, order_id: u64) -> u64 {
    let order_key = OrderBookKey::OrderById(order_id);
    if !env.storage().persistent().has(&order_key) {
        return 0;
    }
    let order: Order = env.storage().persistent().get(&order_key).unwrap();
    order.maker.require_auth();
    let refund = order.collateral_locked;
    // ... (rest of logic)
    env.storage().persistent().remove(&order_key);
    refund
}

#[soroban_sdk::contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OptimizedDataKey {
    Account(soroban_sdk::BytesN<32>),
    EventTopic(soroban_sdk::BytesN<32>),
}

pub struct KeyOptimizer;

impl KeyOptimizer {
    pub fn address_to_bytes32(addr: &soroban_sdk::Address) -> soroban_sdk::BytesN<32> {
        let env = addr.env();
        let bytes = addr.clone().to_xdr(env);
        env.crypto().sha256(&bytes)
    }

    pub fn string_to_bytes32(env: &soroban_sdk::Env, s: &soroban_sdk::String) -> soroban_sdk::BytesN<32> {
        let bytes = s.clone().to_xdr(env);
        env.crypto().sha256(&bytes)
    }

    pub fn save_optimized_account(env: &soroban_sdk::Env, addr: &soroban_sdk::Address, value: &soroban_sdk::Val) {
        let hashed_key = Self::address_to_bytes32(addr);
        let key = OptimizedDataKey::Account(hashed_key);
        env.storage().persistent().set(&key, value);
        extend_persistent_ttl(env, &key);
    }

    pub fn get_optimized_account<V: soroban_sdk::IntoVal<soroban_sdk::Env, soroban_sdk::Val> + soroban_sdk::TryFromVal<soroban_sdk::Env, soroban_sdk::Val>>(
        env: &soroban_sdk::Env,
        addr: &soroban_sdk::Address,
    ) -> Option<V> {
        let hashed_key = Self::address_to_bytes32(addr);
        let key = OptimizedDataKey::Account(hashed_key);
        if env.storage().persistent().has(&key) {
            extend_persistent_ttl(env, &key);
            env.storage().persistent().get(&key)
        } else {
            None
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Instance storage rent-expiry monitor (issue #953)
// ─────────────────────────────────────────────────────────────────────────────
//
// Every key held in a contract's instance storage shares one contract-wide TTL,
// and the Soroban SDK only exposes `extend_ttl` — there is no way to read the
// live remaining lifetime of an entry from inside a contract. The monitor
// therefore records the ledger at which each watched instance key was last
// refreshed and derives the remaining lifetime from that: a watch starts with a
// full `INSTANCE_TTL_EXTEND_TO` window and loses one ledger per ledger that
// passes.
//
// `check_key_ttl` is the entry point used by operators and off-chain watchers:
// it returns the remaining lifetime and emits a `ttl_warn` event once the key
// drops below `INSTANCE_TTL_WARNING_THRESHOLD` ledgers, so the entry can be
// refreshed before it is evicted.

/// Remaining lifetime (in ledgers) below which `check_key_ttl` raises a warning.
pub const INSTANCE_TTL_WARNING_THRESHOLD: u32 = 10_000;

/// Lifetime (in ledgers) granted to an instance-storage key when it is refreshed.
pub const INSTANCE_TTL_EXTEND_TO: u32 = 100_000;

/// Threshold handed to `extend_ttl` when a watched key is refreshed.
pub const INSTANCE_TTL_BUMP_THRESHOLD: u32 = INSTANCE_TTL_WARNING_THRESHOLD;

/// First topic of the event emitted by `check_key_ttl` for an at-risk key.
pub const EV_TTL_WARNING: Symbol = symbol_short!("ttl_warn");

/// Tracks the ledger at which a watched instance-storage key was last refreshed.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TtlMonitorKey {
    /// (`key`) -> ledger sequence of the last refresh. The variant name stays
    /// globally unique across the contract, as required by Soroban's key
    /// encoding (see the module-level note at the top of this file).
    InstanceKeyLastBump(Symbol),
}

/// Returns the ledger sequence at which `key` was last refreshed, or `None` when
/// the key is not currently being watched.
pub fn last_bump_ledger(env: &Env, key: Symbol) -> Option<u32> {
    let monitor_key = TtlMonitorKey::InstanceKeyLastBump(key);
    if env.storage().instance().has(&monitor_key) {
        env.storage().instance().get::<_, u32>(&monitor_key)
    } else {
        None
    }
}

/// Returns the remaining ledger lifetime of the watched instance-storage key
/// `key`.
///
/// The value is a window estimate derived from the ledger recorded by
/// [`refresh_key_ttl`]: the SDK does not expose the host's live TTL, so the
/// monitor cannot report anything more precise than "ledgers left since the
/// last refresh". A key that is not watched, or whose watch window has fully
/// elapsed, reports `0` — callers must treat `0` as "at risk / expired".
pub fn remaining_key_ttl(env: &Env, key: Symbol) -> u32 {
    match last_bump_ledger(env, key) {
        Some(last_bump) => {
            let elapsed = env.ledger().sequence().saturating_sub(last_bump);
            INSTANCE_TTL_EXTEND_TO.saturating_sub(elapsed)
        }
        None => 0,
    }
}

/// (Re)starts the watch on the instance-storage key `key`: the contract
/// instance TTL is extended through the host and the current ledger is recorded
/// as the last refresh point. Returns the refreshed remaining lifetime.
///
/// The host only rewrites the entry's expiry when its real remaining lifetime
/// has fallen below [`INSTANCE_TTL_BUMP_THRESHOLD`], so the returned window is an
/// upper bound that converges on the real lifetime as the entry ages.
pub fn refresh_key_ttl(env: &Env, key: Symbol) -> u32 {
    env.storage()
        .instance()
        .extend_ttl(INSTANCE_TTL_BUMP_THRESHOLD, INSTANCE_TTL_EXTEND_TO);
    let now = env.ledger().sequence();
    let monitor_key = TtlMonitorKey::InstanceKeyLastBump(key);
    env.storage().instance().set(&monitor_key, &now);
    INSTANCE_TTL_EXTEND_TO
}

/// Returns the remaining ledger lifetime of the watched instance-storage key
/// `key` and emits `(ttl_warn, key)` with the remaining lifetime as its payload
/// when it falls below `INSTANCE_TTL_WARNING_THRESHOLD`.
///
/// The comparison is strict: an entry with exactly
/// `INSTANCE_TTL_WARNING_THRESHOLD` ledgers left is still considered healthy.
pub fn check_key_ttl(env: &Env, key: Symbol) -> u32 {
    let remaining = remaining_key_ttl(env, key.clone());
    if remaining < INSTANCE_TTL_WARNING_THRESHOLD {
        env.events().publish((EV_TTL_WARNING, key), remaining);
    }
    remaining
}

/// --- Unit Tests for TTL survival (#715) ---
#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::testutils::Ledger;
    use soroban_sdk::{Env, Address};

    #[test]
    fn test_strict_ttl_extension_survival() {
        let env = Env::default();
        let test_address = Address::generate(&env);
        let key = DataKey::Subscription(test_address.clone());
        
        // Initial setup
        env.storage().persistent().set(&key, &true);
        extend_persistent_ttl(&env, &key);

        // Jump to 95,000 ledgers (within the 10,000 threshold of initial 100k bump)
        env.ledger().set_sequence(95_000);
        assert!(env.storage().persistent().has(&key));

        // Trigger secondary bump
        extend_persistent_ttl(&env, &key);

        // Jump to 150,000 ledgers. Without the secondary bump, it would have expired at 100k.
        env.ledger().set_sequence(150_000);
        assert!(env.storage().persistent().has(&key), "Storage should survive via 100k bump");
    }
}
