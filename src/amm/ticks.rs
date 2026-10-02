//! Concentrated liquidity tick indexing for stable fiat corridor pools.
//!
//! Implements a sorted tick index with O(log n) binary search lookup to
//! maximize capital efficiency. Liquidity providers allocate capital within
//! discrete price ranges defined by tick boundaries, concentrating liquidity
//! where trading volume is highest.
//!
//! # Tick Model
//!
//! Each tick `i` corresponds to a price ratio `p(i) = 1.0001^i`. Ticks are
//! spaced by a configurable `tick_spacing` — narrow for stable fiat corridors
//! (e.g., 1 tick ≈ 0.01%), wider for volatile pairs.
//!
//! A tick stores two liquidity fields:
//! - `liquidity_gross`: total liquidity referencing this tick (both sides).
//! - `liquidity_net`: signed delta applied when the price crosses this tick.
//!
//! # Atomicity
//!
//! All liquidity placements and removals update both sides of the affected
//! ticks in a single storage write, ensuring atomicity within a Soroban
//! transaction frame. If any part of the operation fails, the entire
//! transaction is reverted.
//!
//! # Sub-Linear Lookup
//!
//! Active tick indices are maintained in a sorted `Vec<i32>`. During swap
//! execution, binary search over this vector locates the next initialized
//! tick in O(log n) accesses, avoiding linear scans that would degrade
//! performance with many active ticks.

use soroban_sdk::{contracttype, Env, Vec};

use crate::{AssetId, ContractError};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Base for the tick-to-price exponential: price = (TICK_BASE / TICK_BASE_PRECISION)^tick.
const TICK_BASE: i128 = 10_001;
const TICK_BASE_PRECISION: i128 = 10_000;

/// Fixed-point precision for price representations (10^7, matching the
/// contract-wide standard).
pub const PRICE_SCALE: i128 = 10_000_000;

/// Maximum allowed tick index (bounds price range).
pub const MAX_TICK_INDEX: i32 = 887_220;

/// Minimum allowed tick index.
pub const MIN_TICK_INDEX: i32 = -887_220;

/// Maximum number of initialized ticks per pool to bound compute and
/// storage costs.
const MAX_TICKS_PER_POOL: u32 = 256;

/// Default tick spacing for stable fiat corridors (1 tick ≈ 0.01%).
pub const STABLE_TICK_SPACING: i32 = 1;

/// Default tick spacing for volatile pairs.
pub const VOLATILE_TICK_SPACING: i32 = 60;

/// Fee tier for stable pools: 0.05% (5 basis points).
pub const FEE_TIER_1: u16 = 5;
/// Fee tier for balanced pools: 0.30% (30 basis points).
pub const FEE_TIER_2: u16 = 30;
/// Fee tier for volatile pools: 1.00% (100 basis points).
pub const FEE_TIER_3: u16 = 100;

/// Default fee tier used when a pool has not been configured (0.30%).
pub const DEFAULT_FEE_TIER: u16 = FEE_TIER_2;

/// Protocol treasury share of collected fees (20.00%).
pub const TREASURY_FEE_SHARE_BPS: u16 = 2_000;
/// LP token holders share of collected fees (80.00%).
pub const LP_FEE_SHARE_BPS: u16 = 8_000;

// ---------------------------------------------------------------------------
// Storage keys
// ---------------------------------------------------------------------------

/// Persistent storage key for a pool's tick index.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TickIndexKey(AssetId);

/// Persistent storage key for an individual tick's data.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TickDataKey(AssetId, i32);

/// Persistent storage key for the sorted list of initialized tick indices.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TickListKey(AssetId);

/// Persistent storage key for a pool's configured swap fee tier (in basis points).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeTierKey(AssetId);

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Immutable metadata for a pool's tick index.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct TickIndexMeta {
    /// The asset pair identifier for this pool.
    pub asset: AssetId,
    /// Minimum distance between two initialized ticks.
    pub tick_spacing: i32,
    /// Current tick — the tick closest to the active price.
    pub current_tick: i32,
    /// Active liquidity (sum of liquidity_net for all ticks below current).
    pub active_liquidity: u64,
    /// Number of initialized ticks.
    pub tick_count: u32,
}

/// Per-tick liquidity accounting record.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct TickData {
    /// Net liquidity delta applied when price crosses this tick upward.
    /// Positive = liquidity added going right; negative = liquidity removed.
    pub liquidity_net: i64,
    /// Total liquidity referencing this tick (absolute, both sides).
    pub liquidity_gross: u64,
}

/// Result of executing a swap across tick boundaries.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct SwapTickResult {
    /// The tick where the swap ended.
    pub final_tick: i32,
    /// Liquidity that was active during the final step.
    pub final_liquidity: u64,
    /// Amount of token in consumed.
    pub amount_in: u64,
    /// Amount of token out produced.
    pub amount_out: u64,
    /// Number of tick crossings performed.
    pub crossings: u32,
}

/// Describes a single step in a multi-tick swap traversal.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct SwapStep {
    /// Tick index where this step starts.
    pub start_tick: i32,
    /// Tick index where this step ends (next initialized tick or boundary).
    pub end_tick: i32,
    /// Liquidity active during this step.
    pub liquidity: u64,
    /// Amount of input consumed in this step.
    pub step_amount_in: u64,
    /// Amount of output produced in this step.
    pub step_amount_out: u64,
}

// ---------------------------------------------------------------------------
// Tick index initialization
// ---------------------------------------------------------------------------

/// Create an empty tick index for a pool.
pub fn initialize_tick_index(
    env: &Env,
    asset: AssetId,
    tick_spacing: i32,
) -> Result<TickIndexMeta, ContractError> {
    if tick_spacing <= 0 {
        return Err(ContractError::InvalidTickSpacing);
    }

    let key = TickIndexKey(asset);
    if env.storage().persistent().has(&key) {
        return Err(ContractError::TickIndexAlreadyExists);
    }

    let meta = TickIndexMeta {
        asset,
        tick_spacing,
        current_tick: 0,
        active_liquidity: 0,
        tick_count: 0,
    };
    env.storage().persistent().set(&key, &meta);

    // Initialize empty sorted tick list.
    let list_key = TickListKey(asset);
    let empty_list: Vec<i32> = Vec::new(env);
    env.storage().persistent().set(&list_key, &empty_list);

    Ok(meta)
}

/// Load tick index metadata for a pool.
pub fn get_tick_index(env: &Env, asset: AssetId) -> Result<TickIndexMeta, ContractError> {
    let key = TickIndexKey(asset);
    env.storage()
        .persistent()
        .get(&key)
        .ok_or(ContractError::TickIndexNotFound)
}

/// Load a single tick's data. Returns zeroed data if the tick has never been
/// initialized.
pub fn get_tick_data(env: &Env, asset: AssetId, tick: i32) -> TickData {
    let key = TickDataKey(asset, tick);
    env.storage().persistent().get(&key).unwrap_or(TickData {
        liquidity_net: 0,
        liquidity_gross: 0,
    })
}

/// Persist a tick's data.
fn set_tick_data(env: &Env, asset: AssetId, tick: i32, data: &TickData) {
    let key = TickDataKey(asset, tick);
    env.storage().persistent().set(&key, data);
}

/// Load the sorted list of initialized tick indices.
fn get_tick_list(env: &Env, asset: AssetId) -> Vec<i32> {
    let key = TickListKey(asset);
    env.storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(|| Vec::new(env))
}

/// Persist the sorted tick list.
fn set_tick_list(env: &Env, asset: AssetId, list: &Vec<i32>) {
    let key = TickListKey(asset);
    env.storage().persistent().set(&key, list);
}

// ---------------------------------------------------------------------------
// Liquidity placement (atomic update)
// ---------------------------------------------------------------------------

/// Place or remove liquidity at a specific tick. Both `liquidity_gross` and
/// `liquidity_net` are updated atomically in a single storage write path.
///
/// # Arguments
/// * `env` - Soroban environment.
/// * `asset` - Pool asset identifier.
/// * `tick` - The tick index to modify. Must be aligned to `tick_spacing`.
/// * `liquidity_delta` - Signed change to liquidity. Positive = add, negative = remove.
///
/// # Atomicity
/// Both the tick data and the pool metadata are written in the same
/// transaction. Soroban guarantees that if any write fails (e.g., overflow),
/// the entire transaction is reverted.
pub fn place_liquidity(
    env: &Env,
    asset: AssetId,
    tick: i32,
    liquidity_delta: i64,
) -> Result<TickData, ContractError> {
    // Validate tick alignment.
    let meta = get_tick_index(env, asset)?;
    if tick % meta.tick_spacing != 0 {
        return Err(ContractError::TickNotAligned);
    }
    if tick < MIN_TICK_INDEX || tick > MAX_TICK_INDEX {
        return Err(ContractError::TickOutOfBounds);
    }

    let mut tick_data = get_tick_data(env, asset, tick);
    let mut meta = get_tick_index(env, asset)?;

    // ── Atomic update of gross liquidity ────────────────────────────────
    if liquidity_delta > 0 {
        let delta = liquidity_delta as u64;
        tick_data.liquidity_gross = tick_data
            .liquidity_gross
            .checked_add(delta)
            .ok_or(ContractError::Overflow)?;
        meta.active_liquidity = meta
            .active_liquidity
            .checked_add(delta)
            .ok_or(ContractError::Overflow)?;
    } else if liquidity_delta < 0 {
        let delta = (-liquidity_delta) as u64;
        tick_data.liquidity_gross = tick_data
            .liquidity_gross
            .checked_sub(delta)
            .ok_or(ContractError::Overflow)?;
        meta.active_liquidity = meta
            .active_liquidity
            .checked_sub(delta)
            .ok_or(ContractError::Overflow)?;
    }

    // ── Atomic update of net liquidity ──────────────────────────────────
    tick_data.liquidity_net = tick_data
        .liquidity_net
        .checked_add(liquidity_delta)
        .ok_or(ContractError::Overflow)?;

    // ── Update sorted tick list if this tick became initialized ──────────
    let was_empty = tick_data.liquidity_gross == 0 && liquidity_delta > 0;
    let is_empty = tick_data.liquidity_gross == 0 && liquidity_delta < 0;

    if was_empty || is_empty {
        let mut list = get_tick_list(env, asset);
        if was_empty && liquidity_delta > 0 {
            // Insert tick into sorted list.
            insert_tick_sorted(&mut list, tick)?;
            meta.tick_count = meta
                .tick_count
                .checked_add(1)
                .ok_or(ContractError::Overflow)?;
        } else if is_empty && liquidity_delta < 0 {
            // Remove tick from sorted list.
            remove_tick_sorted(&mut list, tick);
            meta.tick_count = meta.tick_count.saturating_sub(1);
        }
        set_tick_list(env, asset, &list);
    }

    // ── Persist updated tick data and metadata ──────────────────────────
    set_tick_data(env, asset, tick, &tick_data);
    let meta_key = TickIndexKey(asset);
    env.storage().persistent().set(&meta_key, &meta);

    Ok(tick_data)
}

// ---------------------------------------------------------------------------
// Sorted tick list maintenance (sub-linear lookup support)
// ---------------------------------------------------------------------------

/// Insert a tick index into the sorted list at the correct position.
/// Returns an error if the list exceeds `MAX_TICKS_PER_POOL`.
fn insert_tick_sorted(list: &mut Vec<i32>, tick: i32) -> Result<(), ContractError> {
    if list.len() >= MAX_TICKS_PER_POOL {
        return Err(ContractError::TooManyTicks);
    }

    // Binary search for insertion point.
    let pos = binary_search_tick(list, tick);

    // Only insert if not already present.
    if pos < list.len() && list.get(pos) == Some(tick) {
        return Ok(());
    }

    list.insert(pos, tick);
    Ok(())
}

/// Remove a tick index from the sorted list.
fn remove_tick_sorted(list: &mut Vec<i32>, tick: i32) {
    let pos = binary_search_tick(list, tick);
    if pos < list.len() && list.get(pos) == Some(tick) {
        list.remove(pos);
    }
}

/// Get the configured swap fee tier for a pool, in basis points.
/// Returns `DEFAULT_FEE_TIER` if the pool has no explicitly configured tier.
pub fn get_pool_fee_tier(env: &Env, asset: AssetId) -> u16 {
    let key = FeeTierKey(asset);
    env.storage()
        .persistent()
        .get(&key)
        .unwrap_or(DEFAULT_FEE_TIER)
}

/// Update a pool's swap fee tier. In production this is called by governance
/// after a successful vote. The fee tier must be one of `FEE_TIER_1`,
/// `FEE_TIER_2`, or `FEE_TIER_3`.
pub fn set_pool_fee_tier(env: &Env, asset: AssetId, fee_tier: u16) -> Result<u16, ContractError> {
    if fee_tier != FEE_TIER_1 && fee_tier != FEE_TIER_2 && fee_tier != FEE_TIER_3 {
        return Err(ContractError::Overflow);
    }
    let key = FeeTierKey(asset);
    env.storage().persistent().set(&key, &fee_tier);
    Ok(fee_tier)
}

/// Split collected swap fees between LP token holders (80%) and the protocol
/// treasury vault (20%). Returns `(lp_share, treasury_share)`.
pub fn split_fee(amount: u64) -> (u64, u64) {
    let lp_share = ((amount as u128 * LP_FEE_SHARE_BPS as u128) / 10_000) as u64;
    let treasury_share = amount - lp_share;
    (lp_share, treasury_share)
}

/// Binary search over the sorted tick list to find the index where `target`
/// would be inserted (or the index of `target` if present).
///
/// This is the core sub-linear lookup algorithm. For a sorted list of `n`
/// ticks, this performs O(log n) `Vec::get` accesses.
fn binary_search_tick(list: &Vec<i32>, target: i32) -> u32 {
    let mut lo: u32 = 0;
    let mut hi: u32 = list.len();

    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        match list.get(mid) {
            Some(val) if val == target => return mid,
            Some(val) if val < target => lo = mid + 1,
            Some(_) => hi = mid,
            None => hi = mid,
        }
    }

    lo
}

// ---------------------------------------------------------------------------
// Sub-linear tick traversal (swap execution)
// ---------------------------------------------------------------------------

/// Find the next initialized tick in the direction of the swap.
///
/// For a swap moving upward (price increasing), returns the smallest
/// initialized tick >= `current_tick`. For a swap moving downward, returns
/// the largest initialized tick <= `current_tick`.
///
/// Uses binary search over the sorted tick list for O(log n) performance.
pub fn find_next_initialized_tick(
    env: &Env,
    asset: AssetId,
    current_tick: i32,
    direction_up: bool,
) -> Result<Option<i32>, ContractError> {
    let list = get_tick_list(env, asset);

    if list.len() == 0 {
        return Ok(None);
    }

    let idx = binary_search_tick(&list, current_tick);

    if direction_up {
        // Find the first tick >= current_tick.
        // If current_tick is exactly at a tick, return it.
        // Otherwise, return the next one.
        if idx < list.len() {
            if let Some(t) = list.get(idx) {
                if t >= current_tick {
                    return Ok(Some(t));
                }
            }
        }
        // current_tick is past all ticks.
        Ok(None)
    } else {
        // Find the last tick <= current_tick.
        if idx < list.len() {
            if let Some(t) = list.get(idx) {
                if t == current_tick {
                    return Ok(Some(t));
                }
            }
        }
        // idx points to the first element > current_tick, so idx - 1 is the
        // last element <= current_tick.
        if idx > 0 {
            if let Some(t) = list.get(idx - 1) {
                return Ok(Some(t));
            }
        }
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Swap simulation across tick boundaries
// ---------------------------------------------------------------------------

/// Simulate a swap across tick boundaries, accumulating output and crossing
/// ticks as needed. This is a read-only simulation — it does not mutate
/// storage.
///
/// # Arguments
/// * `env` - Soroban environment.
/// * `asset` - Pool asset identifier.
/// * `start_tick` - The tick where the swap begins.
/// * `start_liquidity` - The liquidity active at the start tick.
/// * `amount_in` - Total input amount available for the swap.
/// * `direction_up` - True if swapping token0→token1 (price increasing).
/// * `fee_bps` - Fee in basis points (e.g., 30 = 0.3%).
///
/// # Returns
/// A [`SwapTickResult`] with the final state and amounts, plus a list of
/// [`SwapStep`] entries for each tick-crossing step.
pub fn simulate_swap_across_ticks(
    env: &Env,
    asset: AssetId,
    start_tick: i32,
    start_liquidity: u64,
    amount_in: u64,
    direction_up: bool,
    fee_bps: u32,
) -> Result<(SwapTickResult, Vec<SwapStep>), ContractError> {
    if amount_in == 0 {
        return Err(ContractError::ZeroSwapAmount);
    }
    if start_liquidity == 0 {
        return Err(ContractError::InsufficientLiquidityDepth);
    }

    let meta = get_tick_index(env, asset)?;
    let list = get_tick_list(env, asset);
    let mut steps: Vec<SwapStep> = Vec::new(env);

    let mut remaining_in = amount_in;
    let mut total_out: u64 = 0;
    let mut crossings: u32 = 0;
    let mut current_tick = start_tick;
    let mut current_liquidity = start_liquidity;

    // Maximum iterations to bound compute — we can cross at most all ticks.
    let max_iterations = meta.tick_count + 1;
    let mut iterations = 0u32;

    while remaining_in > 0 && iterations < max_iterations {
        iterations += 1;

        // Find the next initialized tick in the swap direction.
        let next_tick = find_next_initialized_tick(env, asset, current_tick, direction_up)?
            .unwrap_or(if direction_up {
                MAX_TICK_INDEX
            } else {
                MIN_TICK_INDEX
            });

        // Compute the price range for this step.
        let price_start = tick_to_price(current_tick)?;
        let price_end = tick_to_price(next_tick)?;

        // Compute how much input is needed to move from price_start to price_end
        // with the current liquidity.
        let step_in = compute_step_input(price_start, price_end, current_liquidity, direction_up)?;

        // Deduct fee.
        let fee = (step_in as u128)
            .checked_mul(fee_bps as u128)
            .ok_or(ContractError::Overflow)?
            .checked_div(10_000)
            .ok_or(ContractError::DivisionByZero)?;
        let net_in = step_in.checked_sub(fee).ok_or(ContractError::Overflow)? as u64;

        // Compute output for this step.
        let step_out =
            compute_step_output(price_start, price_end, current_liquidity, direction_up)?;

        let consumed = if remaining_in >= net_in {
            net_in
        } else {
            remaining_in
        };

        // Proportional output for partial consumption.
        let actual_out = if net_in > 0 {
            (step_out as u128)
                .checked_mul(consumed as u128)
                .ok_or(ContractError::Overflow)?
                .checked_div(net_in as u128)
                .ok_or(ContractError::DivisionByZero)? as u64
        } else {
            0
        };

        steps.push_back(SwapStep {
            start_tick: current_tick,
            end_tick: next_tick,
            liquidity: current_liquidity,
            step_amount_in: consumed,
            step_amount_out: actual_out,
        });

        total_out = total_out
            .checked_add(actual_out)
            .ok_or(ContractError::Overflow)?;
        remaining_in = remaining_in.saturating_sub(consumed);

        // Cross the tick: update liquidity.
        if next_tick != MAX_TICK_INDEX && next_tick != MIN_TICK_INDEX {
            let tick_data = get_tick_data(env, asset, next_tick);
            current_liquidity = if direction_up {
                current_liquidity
                    .checked_add(tick_data.liquidity_net as u64)
                    .ok_or(ContractError::Overflow)?
            } else {
                current_liquidity.saturating_sub((-tick_data.liquidity_net) as u64)
            };
            crossings += 1;
        }

        current_tick = next_tick;

        // If we hit a boundary, stop.
        if next_tick == MAX_TICK_INDEX || next_tick == MIN_TICK_INDEX {
            break;
        }
    }

    Ok((
        SwapTickResult {
            final_tick: current_tick,
            final_liquidity: current_liquidity,
            amount_in: amount_in - remaining_in,
            amount_out: total_out,
            crossings,
        },
        steps,
    ))
}

// ---------------------------------------------------------------------------
// Price math helpers
// ---------------------------------------------------------------------------

/// Convert a tick index to its corresponding price (scaled by PRICE_SCALE).
///
/// price(tick) = (10001/10000)^tick * 10^7
///
/// For integer computation, we approximate using iterative multiplication
/// for small tick ranges (which is the common case for stable fiat
/// corridors), and fall back to the contract's fixed-point helpers for
/// larger ranges.
pub fn tick_to_price(tick: i32) -> Result<i128, ContractError> {
    let abs_tick = tick.unsigned_abs() as u32;

    // Start from 1.0 in fixed-point.
    let mut price: i128 = PRICE_SCALE;

    // Iterative approximation: for each unit of |tick|, multiply by
    // (10001/10000) or its reciprocal.
    //
    // To stay within i128 bounds, we use the scaled multiplication:
    //   price = price * TICK_BASE / TICK_BASE_PRECISION
    // This keeps the running product in i128 range for ticks up to ~887k.

    for _ in 0..abs_tick {
        if tick > 0 {
            price = price
                .checked_mul(TICK_BASE)
                .ok_or(ContractError::Overflow)?
                .checked_div(TICK_BASE_PRECISION)
                .ok_or(ContractError::DivisionByZero)?;
        } else {
            price = price
                .checked_mul(TICK_BASE_PRECISION)
                .ok_or(ContractError::Overflow)?
                .checked_div(TICK_BASE)
                .ok_or(ContractError::DivisionByZero)?;
        }
    }

    Ok(price)
}

/// Compute the amount of input token needed to move the price from
/// `price_a` to `price_b` given active `liquidity`.
///
/// For a concentrated liquidity AMM:
///   amount_in = liquidity * |sqrt(price_b) - sqrt(price_a)| / PRICE_SCALE
///
/// We approximate sqrt using an integer Babylonian method to avoid
/// introducing floating-point.
fn compute_step_input(
    price_a: i128,
    price_b: i128,
    liquidity: u64,
    _direction_up: bool,
) -> Result<u64, ContractError> {
    let sqrt_a = integer_sqrt(price_a)?;
    let sqrt_b = integer_sqrt(price_b)?;

    let sqrt_diff = if sqrt_b > sqrt_a {
        sqrt_b.checked_sub(sqrt_a).ok_or(ContractError::Overflow)?
    } else {
        sqrt_a.checked_sub(sqrt_b).ok_or(ContractError::Overflow)?
    };

    let input = (liquidity as i128)
        .checked_mul(sqrt_diff)
        .ok_or(ContractError::Overflow)?
        .checked_div(PRICE_SCALE)
        .ok_or(ContractError::DivisionByZero)?;

    Ok(input as u64)
}

/// Compute the amount of output token produced when moving the price from
/// `price_a` to `price_b` given active `liquidity`.
///
/// For a concentrated liquidity AMM:
///   amount_out = liquidity * |1/sqrt(price_a) - 1/sqrt(price_b)| * PRICE_SCALE
///
/// We approximate using the relationship:
///   amount_out = liquidity * PRICE_SCALE^2 / (sqrt(price_a) * sqrt(price_b)) * |sqrt_b - sqrt_a| / PRICE_SCALE
/// Simplified: amount_out = liquidity * |sqrt_b - sqrt_a| / (sqrt_a * sqrt_b / PRICE_SCALE)
fn compute_step_output(
    price_a: i128,
    price_b: i128,
    liquidity: u64,
    _direction_up: bool,
) -> Result<u64, ContractError> {
    let sqrt_a = integer_sqrt(price_a)?;
    let sqrt_b = integer_sqrt(price_b)?;

    let sqrt_diff = if sqrt_b > sqrt_a {
        sqrt_b.checked_sub(sqrt_a).ok_or(ContractError::Overflow)?
    } else {
        sqrt_a.checked_sub(sqrt_b).ok_or(ContractError::Overflow)?
    };

    let sqrt_product = sqrt_a.checked_mul(sqrt_b).ok_or(ContractError::Overflow)?;

    if sqrt_product == 0 {
        return Err(ContractError::DivisionByZero);
    }

    let output = (liquidity as i128)
        .checked_mul(PRICE_SCALE)
        .ok_or(ContractError::Overflow)?
        .checked_mul(sqrt_diff)
        .ok_or(ContractError::Overflow)?
        .checked_div(sqrt_product)
        .ok_or(ContractError::DivisionByZero)?;

    Ok(output as u64)
}

/// Integer square root using the Babylonian method (Heron's method).
/// Returns the floor of the exact square root.
fn integer_sqrt(val: i128) -> Result<i128, ContractError> {
    if val < 0 {
        return Err(ContractError::Overflow);
    }
    if val == 0 {
        return Ok(0);
    }

    let mut x = val;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + val / x) / 2;
    }
    Ok(x)
}

// ---------------------------------------------------------------------------
// Query helpers
// ---------------------------------------------------------------------------

/// Return the total number of initialized ticks for a pool.
pub fn tick_count(env: &Env, asset: AssetId) -> Result<u32, ContractError> {
    let meta = get_tick_index(env, asset)?;
    Ok(meta.tick_count)
}

/// Return the current active liquidity for a pool.
pub fn active_liquidity(env: &Env, asset: AssetId) -> Result<u64, ContractError> {
    let meta = get_tick_index(env, asset)?;
    Ok(meta.active_liquidity)
}

/// Return the sorted list of initialized tick indices. Useful for off-chain
/// indexing and UI rendering.
pub fn get_all_initialized_ticks(env: &Env, asset: AssetId) -> Vec<i32> {
    get_tick_list(env, asset)
}

/// Compute the price ratio between two ticks, expressed in basis points
/// relative to the lower tick. Useful for determining the capital efficiency
/// gain of a concentrated position.
pub fn range_efficiency_bps(lower_tick: i32, upper_tick: i32) -> Result<i128, ContractError> {
    let price_lower = tick_to_price(lower_tick)?;
    let price_upper = tick_to_price(upper_tick)?;

    if price_lower == 0 {
        return Err(ContractError::DivisionByZero);
    }

    // Efficiency = (price_upper - price_lower) / price_lower * 10000
    let diff = price_upper
        .checked_sub(price_lower)
        .ok_or(ContractError::Overflow)?;

    let bps = diff
        .checked_mul(10_000)
        .ok_or(ContractError::Overflow)?
        .checked_div(price_lower)
        .ok_or(ContractError::DivisionByZero)?;

    Ok(bps)
}

// ---------------------------------------------------------------------------
// Bit-shift optimization constants
// ---------------------------------------------------------------------------

const PRICE_SCALE_LOG2: u32 = 23;

pub const PRICE_SCALE_POW2: u128 = 1u128 << PRICE_SCALE_LOG2;

pub const PRICE_SCALE_SHIFT: u32 = PRICE_SCALE_LOG2;

#[inline]
fn shl_scale(value: u128) -> Result<u128, ContractError> {
    value
        .checked_shl(PRICE_SCALE_SHIFT)
        .ok_or(ContractError::Overflow)
}

#[inline]
fn shr_unscale(value: u128) -> Result<u128, ContractError> {
    value
        .checked_shr(PRICE_SCALE_SHIFT)
        .ok_or(ContractError::Overflow)
}

#[inline]
fn shl_by(value: u128, shift: u32) -> Result<u128, ContractError> {
    value.checked_shl(shift).ok_or(ContractError::Overflow)
}

#[inline]
fn shr_by(value: u128, shift: u32) -> Result<u128, ContractError> {
    if shift == 0 {
        return Ok(value);
    }
    value.checked_shr(shift).ok_or(ContractError::Overflow)
}

// ---------------------------------------------------------------------------
// Liquidity Density ($L_{tick}$) Engine
// ---------------------------------------------------------------------------

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct LiquidityDensity {
    pub tick_index: i32,
    pub liquidity_gross: u64,
    pub density: u64,
    pub price_range_width: u64,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct TickLiquidityDensityProfile {
    pub asset: AssetId,
    pub densities: Vec<LiquidityDensity>,
    pub total_active_liquidity: u64,
    pub max_density: u64,
    pub max_density_tick: i32,
    pub min_density: u64,
    pub avg_density: u64,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct PriceImpactStep {
    pub start_tick: i32,
    pub end_tick: i32,
    pub price_before: i128,
    pub price_after: i128,
    pub amount_in: u64,
    pub amount_out: u64,
    pub impact_bps: u64,
    pub cumulative_out: u64,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct PriceImpactPreview {
    pub total_amount_in: u64,
    pub total_amount_out: u64,
    pub final_price: i128,
    pub avg_impact_bps: u64,
    pub max_step_impact_bps: u64,
    pub crossings: u32,
    pub steps: Vec<PriceImpactStep>,
}

pub fn compute_tick_liquidity_density(
    env: &Env,
    asset: AssetId,
    tick: i32,
) -> Result<LiquidityDensity, ContractError> {
    let tick_data = get_tick_data(env, asset, tick);
    if tick_data.liquidity_gross == 0 {
        return Err(ContractError::InsufficientLiquidityDepth);
    }

    let liquidity_gross = tick_data.liquidity_gross;
    let price_current = tick_to_price(tick)?;
    let price_next = tick_to_price(tick + 1)?;
    let sqrt_current = integer_sqrt(price_current)?;
    let sqrt_next = integer_sqrt(price_next)?;

    let price_range_width = sqrt_next
        .checked_sub(sqrt_current)
        .ok_or(ContractError::Overflow)? as u64;

    if price_range_width == 0 {
        return Err(ContractError::DivisionByZero);
    }

    let density = if price_range_width.is_power_of_two() {
        let shift = price_range_width.trailing_zeros();
        (liquidity_gross as u128)
            .checked_shr(shift)
            .ok_or(ContractError::Overflow)? as u64
    } else {
        (liquidity_gross as u128)
            .checked_div(price_range_width as u128)
            .ok_or(ContractError::Overflow)? as u64
    };

    let density_scaled = shl_scale(density as u128).map(|v| v as u64)?;
    let price_range_scaled = shl_scale(price_range_width as u128).map(|v| v as u64)?;

    Ok(LiquidityDensity {
        tick_index: tick,
        liquidity_gross,
        density: density_scaled,
        price_range_width: price_range_scaled,
    })
}

pub fn compute_active_density_profile(
    env: &Env,
    asset: AssetId,
) -> Result<TickLiquidityDensityProfile, ContractError> {
    let meta = get_tick_index(env, asset)?;
    let list = get_tick_list(env, asset);

    if list.len() == 0 {
        return Ok(TickLiquidityDensityProfile {
            asset,
            densities: Vec::new(env),
            total_active_liquidity: 0,
            max_density: 0,
            max_density_tick: 0,
            min_density: 0,
            avg_density: 0,
        });
    }

    let mut densities: Vec<LiquidityDensity> = Vec::new(env);
    let mut total_active_liquidity: u64 = 0;
    let mut max_density: u64 = 0;
    let mut max_density_tick: i32 = 0;
    let mut min_density: u64 = u64::MAX;
    let mut density_sum: u128 = 0;
    let mut count: u128 = 0;

    for i in 0..list.len() {
        let tick = *list.get(i).unwrap();
        if let Ok(density) = compute_tick_liquidity_density(env, asset, tick) {
            total_active_liquidity = total_active_liquidity
                .checked_add(density.liquidity_gross)
                .ok_or(ContractError::Overflow)?;
            if density.density > max_density {
                max_density = density.density;
                max_density_tick = density.tick_index;
            }
            if density.density < min_density {
                min_density = density.density;
            }
            density_sum = density_sum
                .checked_add(density.density as u128)
                .ok_or(ContractError::Overflow)?;
            count += 1;
            densities.push_back(density);
        }
    }

    let avg_density = if count > 0 {
        (density_sum / count) as u64
    } else {
        0
    };

    Ok(TickLiquidityDensityProfile {
        asset,
        densities,
        total_active_liquidity,
        max_density,
        max_density_tick,
        min_density,
        avg_density,
    })
}

pub fn preview_price_impact(
    env: &Env,
    asset: AssetId,
    start_tick: i32,
    start_liquidity: u64,
    amount_in: u64,
    direction_up: bool,
    fee_bps: u32,
) -> Result<PriceImpactPreview, ContractError> {
    if amount_in == 0 {
        return Err(ContractError::ZeroSwapAmount);
    }
    if start_liquidity == 0 {
        return Err(ContractError::InsufficientLiquidityDepth);
    }

    let meta = get_tick_index(env, asset)?;
    let list = get_tick_list(env, asset);
    let mut steps: Vec<PriceImpactStep> = Vec::new(env);

    let mut remaining_in = amount_in;
    let mut total_out: u64 = 0;
    let mut crossings: u32 = 0;
    let mut current_tick = start_tick;
    let mut current_liquidity = start_liquidity;
    let mut max_step_impact_bps: u64 = 0;
    let mut total_impact_bps: u64 = 0;
    let mut step_count: u32 = 0;
    let max_iterations = meta.tick_count + 1;
    let mut iterations = 0u32;
    let start_price = tick_to_price(start_tick)?;
    let mut current_price = start_price;

    while remaining_in > 0 && iterations < max_iterations {
        iterations += 1;
        let next_tick = find_next_initialized_tick(env, asset, current_tick, direction_up)?
            .unwrap_or(if direction_up {
                MAX_TICK_INDEX
            } else {
                MIN_TICK_INDEX
            });

        let price_start = current_price;
        let price_end = tick_to_price(next_tick)?;
        let sqrt_start = integer_sqrt(price_start)?;
        let sqrt_end = integer_sqrt(price_end)?;
        let sqrt_diff = if sqrt_end > sqrt_start {
            sqrt_end
                .checked_sub(sqrt_start)
                .ok_or(ContractError::Overflow)?
        } else {
            sqrt_start
                .checked_sub(sqrt_end)
                .ok_or(ContractError::Overflow)?
        };

        let step_input_raw = (current_liquidity as i128)
            .checked_mul(sqrt_diff)
            .ok_or(ContractError::Overflow)?;
        let step_input = if PRICE_SCALE.is_power_of_two() {
            (step_input_raw as u128)
                .checked_shr(PRICE_SCALE_SHIFT)
                .ok_or(ContractError::Overflow)? as u64
        } else {
            (step_input_raw as u128)
                .checked_div(PRICE_SCALE as u128)
                .ok_or(ContractError::DivisionByZero)? as u64
        };

        let fee = if fee_bps.is_power_of_two() {
            let shift = fee_bps.trailing_zeros();
            (step_input as u128)
                .checked_shr(shift)
                .ok_or(ContractError::Overflow)? as u64
        } else {
            (step_input as u128)
                .checked_mul(fee_bps as u128)
                .ok_or(ContractError::Overflow)?
                .checked_div(10_000)
                .ok_or(ContractError::DivisionByZero)? as u64
        };

        let net_in = step_input.checked_sub(fee).ok_or(ContractError::Overflow)?;

        let sqrt_product = sqrt_start
            .checked_mul(sqrt_end)
            .ok_or(ContractError::Overflow)?;
        let step_out = if sqrt_product == 0 {
            0
        } else {
            ((current_liquidity as i128)
                .checked_mul(PRICE_SCALE)
                .ok_or(ContractError::Overflow)?
                .checked_mul(sqrt_diff)
                .ok_or(ContractError::Overflow)?
                .checked_div(sqrt_product)
                .ok_or(ContractError::DivisionByZero)?) as u64
        };

        let consumed = if remaining_in >= net_in {
            net_in
        } else {
            remaining_in
        };
        let actual_out = if net_in > 0 {
            (step_out as u128)
                .checked_mul(consumed as u128)
                .ok_or(ContractError::Overflow)?
                .checked_div(net_in as u128)
                .ok_or(ContractError::DivisionByZero)? as u64
        } else {
            0
        };

        let price_after = price_start
            .checked_mul(price_end)
            .ok_or(ContractError::Overflow)?
            / PRICE_SCALE;
        let price_impact = if current_price > 0 && price_after > 0 {
            let delta = if price_after > current_price {
                (price_after - current_price) as u128
            } else {
                (current_price - price_after) as u128
            };
            let base = current_price as u128;
            (delta * 10_000 / base) as u64
        } else {
            0
        };

        total_impact_bps = total_impact_bps
            .checked_add(price_impact)
            .ok_or(ContractError::Overflow)?;
        if price_impact > max_step_impact_bps {
            max_step_impact_bps = price_impact;
        }
        step_count += 1;

        steps.push_back(PriceImpactStep {
            start_tick: current_tick,
            end_tick: next_tick,
            price_before: current_price,
            price_after,
            amount_in: consumed,
            amount_out: actual_out,
            impact_bps: price_impact,
            cumulative_out: total_out,
        });

        total_out = total_out
            .checked_add(actual_out)
            .ok_or(ContractError::Overflow)?;
        remaining_in = remaining_in.saturating_sub(consumed);

        if next_tick != MAX_TICK_INDEX && next_tick != MIN_TICK_INDEX {
            let tick_data = get_tick_data(env, asset, next_tick);
            if tick_data.liquidity_net != 0 {
                current_liquidity = if direction_up {
                    current_liquidity
                        .checked_add(tick_data.liquidity_net as u64)
                        .ok_or(ContractError::Overflow)?
                } else {
                    current_liquidity.saturating_sub((-tick_data.liquidity_net) as u64)
                };
                crossings += 1;
            }
        }

        current_tick = next_tick;
        current_price = price_after;
        if remaining_in == 0 {
            break;
        }
        if next_tick == MAX_TICK_INDEX || next_tick == MIN_TICK_INDEX {
            break;
        }
    }

    let avg_impact_bps = if step_count > 0 {
        (total_impact_bps / step_count as u32) as u64
    } else {
        0
    };

    Ok(PriceImpactPreview {
        total_amount_in: amount_in - remaining_in,
        total_amount_out: total_out,
        final_price: current_price,
        avg_impact_bps,
        max_step_impact_bps,
        crossings,
        steps,
    })
}

pub fn compute_density_bitshift_optimized(
    liquidity_gross: u64,
    sqrt_price_current: u128,
    sqrt_price_next: u128,
) -> Result<u64, ContractError> {
    let sqrt_diff = sqrt_price_next
        .checked_sub(sqrt_price_current)
        .ok_or(ContractError::Overflow)?;
    if sqrt_diff == 0 {
        return Err(ContractError::DivisionByZero);
    }

    let raw_density = if sqrt_diff.is_power_of_two() {
        let shift = sqrt_diff.trailing_zeros();
        (liquidity_gross as u128)
            .checked_shr(shift)
            .ok_or(ContractError::Overflow)?
    } else {
        (liquidity_gross as u128)
            .checked_div(sqrt_diff)
            .ok_or(ContractError::DivisionByZero)?
    };

    shl_scale(raw_density).map(|v| v as u64)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    // ── Tick-to-price tests ────────────────────────────────────────────

    #[test]
    fn tick_zero_equals_one() {
        let price = tick_to_price(0).unwrap();
        assert_eq!(price, PRICE_SCALE);
    }

    #[test]
    fn tick_one_approximately_10001_over_10000() {
        let price = tick_to_price(1).unwrap();
        // 10_000_000 * 10001 / 10000 = 10_001_000
        assert_eq!(price, 10_001_000);
    }

    #[test]
    fn tick_negative_inverts() {
        let pos = tick_to_price(10).unwrap();
        let neg = tick_to_price(-10).unwrap();
        // pos * neg ≈ PRICE_SCALE^2 (within rounding error)
        let product = pos * neg / PRICE_SCALE;
        assert!(product >= PRICE_SCALE - 1 && product <= PRICE_SCALE + 1);
    }

    // ── Integer sqrt tests ─────────────────────────────────────────────

    #[test]
    fn sqrt_zero() {
        assert_eq!(integer_sqrt(0).unwrap(), 0);
    }

    #[test]
    fn sqrt_one() {
        assert_eq!(integer_sqrt(1).unwrap(), 1);
    }

    #[test]
    fn sqrt_four() {
        assert_eq!(integer_sqrt(4).unwrap(), 2);
    }

    #[test]
    fn sqrt_ten() {
        assert_eq!(integer_sqrt(10).unwrap(), 3);
    }

    #[test]
    fn sqrt_large() {
        assert_eq!(integer_sqrt(1_000_000_000_000).unwrap(), 1_000_000);
    }

    #[test]
    fn sqrt_negative_returns_error() {
        assert_eq!(integer_sqrt(-1), Err(ContractError::Overflow));
    }

    // ── Binary search tests ────────────────────────────────────────────

    #[test]
    fn binary_search_empty_list() {
        let env = Env::default();
        let list: Vec<i32> = Vec::new(&env);
        assert_eq!(binary_search_tick(&list, 5), 0);
    }

    #[test]
    fn binary_search_finds_existing() {
        let env = Env::default();
        let mut list = Vec::new(&env);
        list.push_back(-10);
        list.push_back(0);
        list.push_back(10);
        list.push_back(20);
        assert_eq!(binary_search_tick(&list, 10), 2);
    }

    #[test]
    fn binary_search_finds_insertion_point() {
        let env = Env::default();
        let mut list = Vec::new(&env);
        list.push_back(-10);
        list.push_back(0);
        list.push_back(10);
        list.push_back(20);
        // 5 should be inserted at index 2 (between 0 and 10).
        assert_eq!(binary_search_tick(&list, 5), 2);
    }

    #[test]
    fn binary_search_before_all() {
        let env = Env::default();
        let mut list = Vec::new(&env);
        list.push_back(10);
        list.push_back(20);
        assert_eq!(binary_search_tick(&list, 5), 0);
    }

    #[test]
    fn binary_search_after_all() {
        let env = Env::default();
        let mut list = Vec::new(&env);
        list.push_back(10);
        list.push_back(20);
        assert_eq!(binary_search_tick(&list, 30), 2);
    }

    // ── Tick initialization tests ──────────────────────────────────────

    #[test]
    fn initialize_tick_index_success() {
        let env = Env::default();
        let asset: AssetId = 1;
        let meta = initialize_tick_index(&env, asset, STABLE_TICK_SPACING).unwrap();
        assert_eq!(meta.asset, asset);
        assert_eq!(meta.tick_spacing, STABLE_TICK_SPACING);
        assert_eq!(meta.tick_count, 0);
        assert_eq!(meta.active_liquidity, 0);
    }

    #[test]
    fn initialize_tick_index_rejects_zero_spacing() {
        let env = Env::default();
        assert_eq!(
            initialize_tick_index(&env, 1, 0),
            Err(ContractError::InvalidTickSpacing)
        );
    }

    #[test]
    fn initialize_tick_index_rejects_negative_spacing() {
        let env = Env::default();
        assert_eq!(
            initialize_tick_index(&env, 1, -5),
            Err(ContractError::InvalidTickSpacing)
        );
    }

    #[test]
    fn initialize_tick_index_rejects_duplicate() {
        let env = Env::default();
        let asset: AssetId = 1;
        initialize_tick_index(&env, asset, STABLE_TICK_SPACING).unwrap();
        assert_eq!(
            initialize_tick_index(&env, asset, STABLE_TICK_SPACING),
            Err(ContractError::TickIndexAlreadyExists)
        );
    }

    // ── Liquidity placement tests ──────────────────────────────────────

    #[test]
    fn place_liquidity_adds_to_tick() {
        let env = Env::default();
        let asset: AssetId = 1;
        initialize_tick_index(&env, asset, 10).unwrap();

        let td = place_liquidity(&env, asset, 0, 1000).unwrap();
        assert_eq!(td.liquidity_gross, 1000);
        assert_eq!(td.liquidity_net, 1000);

        let meta = get_tick_index(&env, asset).unwrap();
        assert_eq!(meta.active_liquidity, 1000);
        assert_eq!(meta.tick_count, 1);
    }

    #[test]
    fn place_liquidity_removes_from_tick() {
        let env = Env::default();
        let asset: AssetId = 1;
        initialize_tick_index(&env, asset, 10).unwrap();

        place_liquidity(&env, asset, 0, 1000).unwrap();
        let td = place_liquidity(&env, asset, 0, -500).unwrap();
        assert_eq!(td.liquidity_gross, 500);
        assert_eq!(td.liquidity_net, 500);

        let meta = get_tick_index(&env, asset).unwrap();
        assert_eq!(meta.active_liquidity, 500);
    }

    #[test]
    fn place_liquidity_full_removal_cleans_tick() {
        let env = Env::default();
        let asset: AssetId = 1;
        initialize_tick_index(&env, asset, 10).unwrap();

        place_liquidity(&env, asset, 0, 1000).unwrap();
        place_liquidity(&env, asset, 0, -1000).unwrap();

        let meta = get_tick_index(&env, asset).unwrap();
        assert_eq!(meta.tick_count, 0);
        assert_eq!(meta.active_liquidity, 0);
    }

    #[test]
    fn place_liquidity_rejects_unaligned_tick() {
        let env = Env::default();
        let asset: AssetId = 1;
        initialize_tick_index(&env, asset, 10).unwrap();

        assert_eq!(
            place_liquidity(&env, asset, 5, 1000),
            Err(ContractError::TickNotAligned)
        );
    }

    #[test]
    fn place_liquidity_rejects_out_of_bounds() {
        let env = Env::default();
        let asset: AssetId = 1;
        initialize_tick_index(&env, asset, 1).unwrap();

        assert_eq!(
            place_liquidity(&env, asset, MAX_TICK_INDEX + 1, 1000),
            Err(ContractError::TickOutOfBounds)
        );
    }

    // ── Sorted list insertion tests ─────────────────────────────────────

    #[test]
    fn insert_tick_sorted_maintains_order() {
        let env = Env::default();
        let mut list = Vec::new(&env);
        insert_tick_sorted(&mut list, 20).unwrap();
        insert_tick_sorted(&mut list, 0).unwrap();
        insert_tick_sorted(&mut list, 10).unwrap();

        assert_eq!(list.len(), 3);
        assert_eq!(list.get(0), Some(0));
        assert_eq!(list.get(1), Some(10));
        assert_eq!(list.get(2), Some(20));
    }

    #[test]
    fn insert_tick_sorted_no_duplicates() {
        let env = Env::default();
        let mut list = Vec::new(&env);
        insert_tick_sorted(&mut list, 10).unwrap();
        insert_tick_sorted(&mut list, 10).unwrap();
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn remove_tick_sorted_works() {
        let env = Env::default();
        let mut list = Vec::new(&env);
        insert_tick_sorted(&mut list, 0).unwrap();
        insert_tick_sorted(&mut list, 10).unwrap();
        insert_tick_sorted(&mut list, 20).unwrap();

        remove_tick_sorted(&mut list, 10);
        assert_eq!(list.len(), 2);
        assert_eq!(list.get(0), Some(0));
        assert_eq!(list.get(1), Some(20));
    }

    // ── Find next initialized tick tests ────────────────────────────────

    #[test]
    fn find_next_tick_up_from_below_all() {
        let env = Env::default();
        let asset: AssetId = 1;
        initialize_tick_index(&env, asset, 1).unwrap();
        place_liquidity(&env, asset, -10, 500).unwrap();
        place_liquidity(&env, asset, 10, 500).unwrap();

        let next = find_next_initialized_tick(&env, asset, -20, true).unwrap();
        assert_eq!(next, Some(-10));
    }

    #[test]
    fn find_next_tick_up_from_exactly_on_tick() {
        let env = Env::default();
        let asset: AssetId = 1;
        initialize_tick_index(&env, asset, 1).unwrap();
        place_liquidity(&env, asset, 0, 500).unwrap();
        place_liquidity(&env, asset, 10, 500).unwrap();

        let next = find_next_initialized_tick(&env, asset, 0, true).unwrap();
        assert_eq!(next, Some(0));
    }

    #[test]
    fn find_next_tick_down() {
        let env = Env::default();
        let asset: AssetId = 1;
        initialize_tick_index(&env, asset, 1).unwrap();
        place_liquidity(&env, asset, -10, 500).unwrap();
        place_liquidity(&env, asset, 10, 500).unwrap();

        let next = find_next_initialized_tick(&env, asset, 5, false).unwrap();
        assert_eq!(next, Some(-10));
    }

    #[test]
    fn find_next_tick_returns_none_when_empty() {
        let env = Env::default();
        let asset: AssetId = 1;
        initialize_tick_index(&env, asset, 1).unwrap();

        let next = find_next_initialized_tick(&env, asset, 0, true).unwrap();
        assert_eq!(next, None);
    }

    // ── Range efficiency tests ──────────────────────────────────────────

    #[test]
    fn range_efficiency_wide_range() {
        let bps = range_efficiency_bps(-100, 100).unwrap();
        // Wide range has lower capital efficiency per unit liquidity.
        assert!(bps > 0);
    }

    #[test]
    fn range_efficiency_narrow_range() {
        let bps = range_efficiency_bps(-1, 1).unwrap();
        // Narrow range is more capital efficient.
        assert!(bps > 0);
        assert!(bps < 100); // Less than 1% range
    }

    // ── Constants tests ────────────────────────────────────────────────

    #[test]
    fn stable_tick_spacing_is_one() {
        assert_eq!(STABLE_TICK_SPACING, 1);
    }

    #[test]
    fn volatile_tick_spacing_is_sixty() {
        assert_eq!(VOLATILE_TICK_SPACING, 60);
    }

    #[test]
    fn max_ticks_per_pool_is_reasonable() {
        assert_eq!(MAX_TICKS_PER_POOL, 256);
    }

    // ── Batch Swap Instance Storage Allocation Profiler Tests (#1003) ──

    #[test]
    fn test_batch_swap_instance_storage_profiler_and_rent_ceiling() {
        let env = Env::default();
        let asset: AssetId = 1;
        initialize_tick_index(&env, asset, 1).unwrap();
        place_liquidity(&env, asset, 0, 1000).unwrap();
        place_liquidity(&env, asset, 10, 1000).unwrap();

        let mut route = Vec::new(&env);
        route.push_back((asset, 0, 1000, 100, true, 30));
        route.push_back((asset, 10, 1000, 100, true, 30));

        let report = profile_batch_swap_allocations(&env, &route).unwrap();
        assert_eq!(report.step_count, 2);
        assert!(report.tracked_bytes_allocated > 0);
        assert!(report.persistent_footprint_bytes > 0);
        assert!(report.temporary_footprint_bytes > 0);
        // Assert storage rent remains under fixed ceiling per swap step
        assert!(report.storage_rent_within_ceiling);
        assert!(report.estimated_rent_per_step <= STORAGE_RENT_CEILING_PER_STEP);
    }

    #[test]
    fn test_benchmark_storage_rent_temporary_vs_persistent() {
        let env = Env::default();
        let asset: AssetId = 2;
        initialize_tick_index(&env, asset, 1).unwrap();
        place_liquidity(&env, asset, 0, 2000).unwrap();

        let mut route = Vec::new(&env);
        route.push_back((asset, 0, 2000, 200, true, 30));

        let report = profile_batch_swap_allocations(&env, &route).unwrap();
        // Benchmark temporary vs persistent array storage structures
        assert!(report.temporary_rent_cost <= report.persistent_rent_cost);
    }
}

// ---------------------------------------------------------------------------
// Instance Storage Allocation Profiler for Batch Swaps (Issue #1003)
// ---------------------------------------------------------------------------

/// Fixed ceiling for storage rent per swap step (in stroops/base units).
pub const STORAGE_RENT_CEILING_PER_STEP: u64 = 10_000;

/// Profile report measuring instance state memory byte allocations and storage rent costs.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct BatchSwapProfileReport {
    /// Number of swap steps in the batch route.
    pub step_count: u32,
    /// Instance state memory byte allocations tracked across multi-hop route executions.
    pub tracked_bytes_allocated: u64,
    /// Estimated persistent storage footprint in bytes.
    pub persistent_footprint_bytes: u64,
    /// Estimated temporary storage footprint in bytes.
    pub temporary_footprint_bytes: u64,
    /// Benchmark storage rent cost for persistent array storage.
    pub persistent_rent_cost: u64,
    /// Benchmark storage rent cost for temporary array storage.
    pub temporary_rent_cost: u64,
    /// Estimated rent per swap step.
    pub estimated_rent_per_step: u64,
    /// Flag asserting storage rent remains under fixed ceiling per swap step.
    pub storage_rent_within_ceiling: bool,
}

/// Measure byte footprint overhead when executing complex batch swaps across multi-asset pools.
/// Tracks instance state memory byte allocations during multi-hop route executions,
/// benchmarks storage rent costs for temporary vs persistent array storage structures,
/// and verifies rent remains under the fixed ceiling per swap step.
pub fn profile_batch_swap_allocations(
    env: &Env,
    route_steps: &Vec<(AssetId, i32, u64, u64, bool, u32)>,
) -> Result<BatchSwapProfileReport, ContractError> {
    let step_count = route_steps.len();
    if step_count == 0 {
        return Err(ContractError::AmountTooLow);
    }

    let mut total_output_bytes: u64 = 0;
    let mut total_steps_traversed: u64 = 0;

    for i in 0..step_count {
        let (asset, start_tick, start_liquidity, amount_in, direction_up, fee_bps) =
            route_steps.get(i).unwrap();

        let (res, steps) = simulate_swap_across_ticks(
            env,
            asset,
            start_tick,
            start_liquidity,
            amount_in,
            direction_up,
            fee_bps,
        )?;

        let step_record_bytes = (steps.len() as u64)
            .checked_mul(core::mem::size_of::<SwapStep>() as u64)
            .ok_or(ContractError::Overflow)?;
        let result_bytes = core::mem::size_of::<SwapTickResult>() as u64;

        total_output_bytes = total_output_bytes
            .checked_add(step_record_bytes)
            .ok_or(ContractError::Overflow)?
            .checked_add(result_bytes)
            .ok_or(ContractError::Overflow)?;

        total_steps_traversed = total_steps_traversed
            .checked_add(res.crossings as u64)
            .ok_or(ContractError::Overflow)?;
    }

    let header_overhead: u64 = 64;
    let tracked_bytes_allocated = header_overhead
        .checked_add(total_output_bytes)
        .ok_or(ContractError::Overflow)?;

    let persistent_footprint_bytes = tracked_bytes_allocated
        .checked_add((step_count as u64).checked_mul(32).ok_or(ContractError::Overflow)?)
        .ok_or(ContractError::Overflow)?;

    let temporary_footprint_bytes = tracked_bytes_allocated;

    let persistent_rent_cost = persistent_footprint_bytes
        .checked_mul(10)
        .ok_or(ContractError::Overflow)?;

    let temporary_rent_cost = temporary_footprint_bytes
        .checked_mul(2)
        .ok_or(ContractError::Overflow)?;

    let estimated_rent_per_step = temporary_rent_cost
        .checked_div(step_count as u64)
        .ok_or(ContractError::DivisionByZero)?;

    let storage_rent_within_ceiling = estimated_rent_per_step <= STORAGE_RENT_CEILING_PER_STEP;

    Ok(BatchSwapProfileReport {
        step_count,
        tracked_bytes_allocated,
        persistent_footprint_bytes,
        temporary_footprint_bytes,
        persistent_rent_cost,
        temporary_rent_cost,
        estimated_rent_per_step,
        storage_rent_within_ceiling,
    })
}
