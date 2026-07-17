//! Centralized event publishing helpers for the StellarFlow Price Oracle.
//! Events use structured topics so frontends can index updates and configuration
//! changes by event type and asset without scanning all transaction logs.

use soroban_sdk::{Address, Env, String, Symbol};

/// Publish a canonical price update event for frontend indexing.
pub fn publish_price_update(env: &Env, asset: Symbol, price: i128, timestamp: u64) {
    env.events().publish(
        (Symbol::new(&env, "price_update"), asset),
        (price, timestamp),
    );
}

/// Publish when a price floor is set for an asset.
pub fn publish_price_floor_set(env: &Env, asset: Symbol, price_floor: i128) {
    env.events().publish(
        (Symbol::new(&env, "price_floor_set"), asset),
        (price_floor,),
    );
}

/// Publish when a price floor rollback occurs for an asset.
pub fn publish_price_floor_rollback(env: &Env, asset: Symbol, previous_floor: i128) {
    env.events().publish(
        (Symbol::new(&env, "price_floor_rollback"), asset),
        (previous_floor,),
    );
}

/// Publish when price bounds are configured for an asset.
pub fn publish_price_bounds_set(env: &Env, asset: Symbol, min_price: i128, max_price: i128) {
    env.events().publish(
        (Symbol::new(&env, "price_bounds_set"), asset),
        (min_price, max_price),
    );
}

/// Publish when price bounds are rolled back for an asset.
pub fn publish_price_bounds_rollback(env: &Env, asset: Symbol, min_price: i128, max_price: i128) {
    env.events().publish(
        (Symbol::new(&env, "price_bounds_rollback"), asset),
        (min_price, max_price),
    );
}

/// Publish when the max price deviation percentage is updated.
pub fn publish_max_deviation_pct_set(env: &Env, max_deviation_bps: i128) {
    env.events().publish(
        (Symbol::new(&env, "max_deviation_pct_set"),),
        (max_deviation_bps,),
    );
}

/// Publish when the max price deviation percentage is rolled back.
pub fn publish_max_deviation_pct_rollback(env: &Env, previous_bps: i128) {
    env.events().publish(
        (Symbol::new(&env, "max_deviation_pct_rollback"),),
        (previous_bps,),
    );
}

/// Publish when asset decimals/meta are set.
pub fn publish_asset_meta_set(env: &Env, asset: Symbol, base_decimals: u32, quote_decimals: u32) {
    env.events().publish(
        (Symbol::new(&env, "asset_meta_set"), asset),
        (base_decimals, quote_decimals),
    );
}

/// Publish when lightweight asset info is set.
pub fn publish_asset_info_set(
    env: &Env,
    asset: Symbol,
    name: Symbol,
    base_decimals: u32,
    quote_decimals: u32,
) {
    env.events().publish(
        (Symbol::new(&env, "asset_info_set"), asset),
        (name, base_decimals, quote_decimals),
    );
}

/// Publish when an asset description is stored.
pub fn publish_asset_description_set(env: &Env, asset: Symbol, description: String) {
    env.events().publish(
        (Symbol::new(&env, "asset_description_set"), asset),
        (description,),
    );
}

/// Publish when emergency halt state is toggled by admins.
pub fn publish_emergency_halt(env: &Env, admin1: Address, admin2: Address, status: bool) {
    env.events().publish(
        (Symbol::new(&env, "emergency_halt"),),
        (admin1, admin2, status),
    );
}
