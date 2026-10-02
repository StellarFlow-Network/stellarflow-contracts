//! Centralized event publishing helpers for the StellarFlow Price Oracle.
//! Events use structured topics so frontends can index updates and configuration
//! changes by event type and asset without scanning all transaction logs.
//!
//! Topic schema convention (indexer contract):
//!  - Topic [0] is ALWAYS the target contract module identifier.
//!  - Topic [1] is the event name.
//!  - Topics [2..] are optional indexed asset/pool keys.
//!  - Maximum 4 topic elements per emitted event.

use soroban_sdk::{symbol_short, Address, Env, String, Symbol};

/// Contract module identifier used as the first event topic for all Price
/// Oracle events. Indexers filter on this value to isolate this contract.
pub const MODULE: Symbol = symbol_short("price_orac");

// Event topic constants for dynamic slippage protection
pub const VOLATILITY: Symbol = symbol_short("volatility");
pub const UPDATED: Symbol = symbol_short("updated");
pub const SWAP: Symbol = symbol_short("swap");
pub const EXECUTED: Symbol = symbol_short("executed");
pub const REJECTED: Symbol = symbol_short("rejected");

/// Maximum number of topic elements allowed by the indexer.
pub const MAX_TOPICS: u32 = 4;

/// Emit an event with the canonical topic schema:
/// `(MODULE, event_name, [key1 [, key2 ]])`.
///
/// Panics if the resulting topic length exceeds `MAX_TOPICS`, so any new event
/// that would break indexer constraints fails fast at the call site.
fn emit<T>(
    env: &Env,
    event_name: Symbol,
    key1: Option<Symbol>,
    key2: Option<Symbol>,
    data: T,
) where
    T: into_val,
{
    let mut topic_count: u32 = 2;
    if key1.is_some() {
        topic_count += 1;
    }
    if key2.is_some() {
        topic_count += 1;
    }
    if topic_count > MAX_TOPICS {
        panic("event topic count exceeds indexer maximum");
    }

    match (key1, key2) {
        (Some(k1), Some(k2)) => {
            env.events().publish((MODULE, event_name, k1, k2), data);
        }
        (Some(k1), None) => {
            env.events().publish((MODULE, event_name, k1), data);
        }
        (None, Some(k2)) => {
            env.events().publish((MODULE, event_name, k2), data);
        }
        (None, None) => {
            env.events().publish((MODULE, event_name), data);
        }
    }
}

/// Publish a canonical price update event for frontend indexing.
pub fn publish_price_update(env: &Env, asset: Symbol, price: i128, timestamp: u64) {
    emit(
        env,
        Symbol::new(env, "price_update"),
        Some(asset),
        None,
        (price, timestamp),
    );
}

/// Publish when a price floor is set for an asset.
pub fn publish_price_floor_set(env: &Env, asset: Symbol, price_floor: i128) {
    emit(
        env,
        Symbol::new(env, "price_floor_set"),
        Some(asset),
        None,
        (price_floor,),
    );
}

/// Publish when a price floor rollback occurs for an asset.
pub fn publish_price_floor_rollback(env: &Env, asset: Symbol, previous_floor: i128) {
    emit(
        env,
        Symbol::new(env, "price_floor_rollback"),
        Some(asset),
        None,
        (previous_floor,),
    );
}

/// Publish when price bounds are configured for an asset.
pub fn publish_price_bounds_set(env: &Env, asset: Symbol, min_price: i128, max_price: i128) {
    emit(
        env,
        Symbol::new(env, "price_bounds_set"),
        Some(asset),
        None,
        (min_price, max_price),
    );
}

/// Publish when price bounds are rolled back for an asset.
pub fn publish_price_bounds_rollback(env: &Env, asset: Symbol, min_price: i128, max_price: i128) {
    emit(
        env,
        Symbol::new(env, "price_bounds_rollback"),
        Some(asset),
        None,
        (min_price, max_price),
    );
}

/// Publish when the max price deviation percentage is updated.
pub fn publish_max_deviation_pct_set(env: &Env, max_deviation_bps: i128) {
    emit(
        env,
        Symbol::new(env, "max_deviation_pct_set"),
        None,
        None,
        (max_deviation_bps,),
    );
}

/// Publish when the max price deviation percentage is rolled back.
pub fn publish_max_deviation_pct_rollback(env: &Env, previous_bps: i128) {
    emit(
        env,
        Symbol::new(env, "max_deviation_pct_rollback"),
        None,
        None,
        (previous_bps,),
    );
}

/// Publish when asset decimals/meta are set.
pub fn publish_asset_meta_set(env: &Env, asset: Symbol, base_decimals: u32, quote_decimals: u32) {
    emit(
        env,
        Symbol::new(env, "asset_meta_set"),
        Some(asset),
        None,
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
    emit(
        env,
        Symbol::new(env, "asset_info_set"),
        Some(asset),
        None,
        (name, base_decimals, quote_decimals),
    );
}

/// Publish when an asset description is stored.
pub fn publish_asset_description_set(env: &Env, asset: Symbol, description: String) {
    emit(
        env,
        Symbol::new(env, "asset_description_set"),
        Some(asset),
        None,
        (description,),
    );
}

/// Publish when emergency halt state is toggled by admins.
pub fn publish_emergency_halt(env: &Env, admin1: Address, admin2: Address, status: bool) {
    emit(
        env,
        Symbol::new(env, "emergency_halt"),
        None,
        None,
        (admin1, admin2, status),
    );
}

/// Publish a swap event for indexer optimization.
///
/// Uses a uniform topic `(MODULE, "swap", pool_id)` and a structured
/// tuple payload `(sender, amount_in, amount_out, fee_paid)` so that the
/// `stellarflow-backend` Horizon event parsers can ingest swap activity
/// consistently across all core contract functions.
pub fn publish_swap(
    env: &Env,
    pool_id: Symbol,
    sender: Address,
    amount_in: i128,
    amount_out: i128,
    fee_paid: i128,
) {
    emit(
        env,
        Symbol::new(env, "swap"),
        Some(pool_id),
        None,
        (sender, amount_in, amount_out, fee_paid),
    );
}

/// Publish when the single-ledger price-impact guard trips (issue #970).
///
/// Emitted from `twap::record_and_evaluate` on the price write whose instant
/// price left the 5% band around the 5-ledger moving average `P_ma`, so
/// indexers and vault operators can see exactly when borrowing was paused.
pub fn publish_price_impact_guard_triggered(
    env: &Env,
    asset: Symbol,
    instant_price: i128,
    moving_average: i128,
    deviation_bps: i128,
) {
    env.events().publish(
        (Symbol::new(env, "price_impact_guard_triggered"), asset),
        (instant_price, moving_average, deviation_bps),
    );
}

/// Publish when the single-ledger price-impact guard clears (issue #970).
///
/// Emitted once the instant price is back inside the 5% band, signalling that
/// borrow entrypoints may resume.
pub fn publish_price_impact_guard_cleared(
    env: &Env,
    asset: Symbol,
    instant_price: i128,
    moving_average: i128,
    deviation_bps: i128,
) {
    env.events().publish(
        (Symbol::new(env, "price_impact_guard_cleared"), asset),
        (instant_price, moving_average, deviation_bps),
    );
}
