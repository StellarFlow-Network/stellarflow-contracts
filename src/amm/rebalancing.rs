//! Automated Liquidity Pool Reserve Rebalancing Hook
//!
//! Provides a hook interface for designated vault strategies to:
//! - Adjust liquidity concentration ranges (tick boundaries)
//! - Rebalance single-sided surplus inventory to preserve 50/50 target ratio
//! - Enforce slippage bounds on all internal rebalancing swaps

use soroban_sdk::{contracttype, Env, Vec, Address, Symbol};

use crate::{
    AssetId, ContractError, MIN_TICK_INDEX, MAX_TICK_INDEX, PRICE_SCALE,
    amm::ticks::{get_tick_index, get_tick_data, place_liquidity, TickData, TickIndexMeta},
    amm::slippage::enforce_slippage,
    amm::invariant::compute_swap_out,
    amm::ticks::{tick_to_price, simulate_swap_across_ticks, SwapStep},
    math::sqrt_ratio,
};

/// Maximum allowed slippage for rebalancing swaps (in basis points).
/// 100 bps = 1% max slippage
pub const MAX_REBALANCE_SLIPPAGE_BPS: u32 = 100;

/// Default target ratio for reserves (50/50 = 5000 basis points)
pub const TARGET_RATIO_BPS: u32 = 5000;

/// Minimum deviation from target ratio (in bps) to trigger rebalancing.
/// 50 bps = 0.5% deviation
pub const REBALANCE_THRESHOLD_BPS: u32 = 50;

/// Maximum number of tick positions a vault strategy can manage.
pub const MAX_VAULT_TICK_POSITIONS: u32 = 10;

/// Storage key for rebalancing configuration.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RebalanceConfigKey(pub AssetId);

/// Storage key for vault strategy authorization.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultStrategyKey(pub AssetId, pub Address);

/// Storage key for tracked rebalancing positions.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultPositionKey(pub AssetId, pub Address, pub i32, pub i32);

/// Configuration for automated rebalancing.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct RebalanceConfig {
    /// The asset pair identifier for this pool.
    pub asset: AssetId,
    /// Whether automated rebalancing is enabled.
    pub enabled: bool,
    /// Target reserve ratio in basis points (5000 = 50/50).
    pub target_ratio_bps: u32,
    /// Minimum deviation from target to trigger rebalance (bps).
    pub threshold_bps: u32,
    /// Maximum slippage allowed for rebalancing swaps (bps).
    pub max_slippage_bps: u32,
    /// Fee in basis points for rebalancing swaps.
    pub rebalance_fee_bps: u32,
    /// Minimum time between rebalances (seconds).
    pub min_rebalance_interval: u64,
    /// Timestamp of last rebalance.
    pub last_rebalance_at: u64,
}

impl Default for RebalanceConfig {
    fn default() -> Self {
        Self {
            asset: 0,
            enabled: false,
            target_ratio_bps: TARGET_RATIO_BPS,
            threshold_bps: REBALANCE_THRESHOLD_BPS,
            max_slippage_bps: MAX_REBALANCE_SLIPPAGE_BPS,
            rebalance_fee_bps: 30, // 0.3%
            min_rebalance_interval: 3600, // 1 hour
            last_rebalance_at: 0,
        }
    }
}

/// Authorization record for a vault strategy.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct VaultStrategy {
    /// The vault strategy contract address.
    pub strategy: Address,
    /// Whether this strategy is authorized to rebalance.
    pub authorized: bool,
    /// Maximum tick range width this strategy can manage.
    pub max_tick_range: i32,
    /// Maximum liquidity this strategy can deploy.
    pub max_liquidity: u64,
    /// Timestamp when authorization was granted.
    pub authorized_at: u64,
}

/// A vault strategy's liquidity position within a tick range.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct VaultPosition {
    /// The vault strategy address.
    pub strategy: Address,
    /// Lower tick boundary (inclusive).
    pub lower_tick: i32,
    /// Upper tick boundary (exclusive).
    pub upper_tick: i32,
    /// Liquidity deployed in this position.
    pub liquidity: u64,
    /// Token0 reserves in this position.
    pub reserve0: u128,
    /// Token1 reserves in this position.
    pub reserve1: u128,
    /// Timestamp when position was last updated.
    pub updated_at: u64,
}

/// Result of a rebalancing operation.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct RebalanceResult {
    /// Whether rebalancing was executed.
    pub executed: bool,
    /// Amount of token0 swapped.
    pub amount0_swapped: u128,
    /// Amount of token1 swapped.
    pub amount1_swapped: u128,
    /// Number of tick positions adjusted.
    pub positions_adjusted: u32,
    /// Gas cost estimate.
    pub gas_used: u64,
}

/// Compute the current reserve ratio in basis points.
/// Returns (reserve0 / (reserve0 + reserve1)) * 10000
pub fn compute_reserve_ratio(reserve0: u128, reserve1: u128) -> Result<u32, ContractError> {
    let total = reserve0
        .checked_add(reserve1)
        .ok_or(ContractError::Overflow)?;

    if total == 0 {
        return Err(ContractError::DivisionByZero);
    }

    let ratio = (reserve0 as u128)
        .checked_mul(10_000)
        .ok_or(ContractError::Overflow)?
        .checked_div(total)
        .ok_or(ContractError::DivisionByZero)?;

    Ok(ratio as u32)
}

/// Check if the pool reserves have deviated beyond the rebalancing threshold.
pub fn should_rebalance(
    reserve0: u128,
    reserve1: u128,
    target_ratio_bps: u32,
    threshold_bps: u32,
) -> Result<bool, ContractError> {
    let current_ratio = compute_reserve_ratio(reserve0, reserve1)?;
    let diff = if current_ratio > target_ratio_bps {
        current_ratio - target_ratio_bps
    } else {
        target_ratio_bps - current_ratio
    };
    Ok(diff >= threshold_bps)
}

/// Calculate the amounts needed to rebalance to the target ratio.
/// Returns (amount0_to_swap, amount1_to_swap, direction)
/// direction: true = swap token0 for token1, false = swap token1 for token0
pub fn calculate_rebalance_amounts(
    reserve0: u128,
    reserve1: u128,
    target_ratio_bps: u32,
) -> Result<(u128, u128, bool), ContractError> {
    let total = reserve0
        .checked_add(reserve1)
        .ok_or(ContractError::Overflow)?;

    if total == 0 {
        return Err(ContractError::DivisionByZero);
    }

    let target_reserve0 = (total as u128)
        .checked_mul(target_ratio_bps as u128)
        .ok_or(ContractError::Overflow)?
        .checked_div(10_000)
        .ok_or(ContractError::DivisionByZero)?;

    let current_ratio = compute_reserve_ratio(reserve0, reserve1)?;

    if current_ratio > target_ratio_bps {
        // Too much token0, need to swap token0 for token1
        let excess0 = reserve0
            .checked_sub(target_reserve0)
            .ok_or(ContractError::Overflow)?;
        // Calculate how much token1 we'd get for excess0
        let amount1_out = compute_swap_out(excess0, reserve0, reserve1)?;
        Ok((excess0, amount1_out, true))
    } else {
        // Too much token1, need to swap token1 for token0
        let target_reserve1 = total
            .checked_sub(target_reserve0)
            .ok_or(ContractError::Overflow)?;
        let excess1 = reserve1
            .checked_sub(target_reserve1)
            .ok_or(ContractError::Overflow)?;
        let amount0_out = compute_swap_out(excess1, reserve1, reserve0)?;
        Ok((amount0_out, excess1, false))
    }
}

/// Load the rebalancing configuration for a pool.
pub fn get_rebalance_config(env: &Env, asset: AssetId) -> Result<RebalanceConfig, ContractError> {
    let key = RebalanceConfigKey(asset);
    env.storage()
        .persistent()
        .get(&key)
        .ok_or(ContractError::NotInitialized)
}

/// Save the rebalancing configuration for a pool.
pub fn set_rebalance_config(env: &Env, config: &RebalanceConfig) {
    let key = RebalanceConfigKey(config.asset);
    env.storage().persistent().set(&key, config);
}

/// Authorize a vault strategy to manage rebalancing for a pool.
pub fn authorize_vault_strategy(
    env: &Env,
    asset: AssetId,
    strategy: Address,
    max_tick_range: i32,
    max_liquidity: u64,
) -> Result<VaultStrategy, ContractError> {
    let key = VaultStrategyKey(asset, strategy.clone());
    let vault = VaultStrategy {
        strategy: strategy.clone(),
        authorized: true,
        max_tick_range,
        max_liquidity,
        authorized_at: env.ledger().timestamp(),
    };
    env.storage().persistent().set(&key, &vault);
    Ok(vault)
}

/// Deauthorize a vault strategy.
pub fn deauthorize_vault_strategy(env: &Env, asset: AssetId, strategy: Address) -> Result<(), ContractError> {
    let key = VaultStrategyKey(asset, strategy);
    env.storage().persistent().remove(&key);
    Ok(())
}

/// Check if a vault strategy is authorized for a pool.
pub fn is_vault_strategy_authorized(env: &Env, asset: AssetId, strategy: &Address) -> Result<bool, ContractError> {
    let key = VaultStrategyKey(asset, strategy.clone());
    match env.storage().persistent().get(&key) {
        Some(vault) => Ok(vault.authorized),
        None => Ok(false),
    }
}

/// Get vault strategy authorization details.
pub fn get_vault_strategy(env: &Env, asset: AssetId, strategy: &Address) -> Result<VaultStrategy, ContractError> {
    let key = VaultStrategyKey(asset, strategy.clone());
    env.storage()
        .persistent()
        .get(&key)
        .ok_or(ContractError::NotRegistered)
}

/// Save a vault position.
pub fn set_vault_position(env: &Env, position: &VaultPosition) {
    let key = VaultPositionKey(
        position.strategy.clone(),
        position.lower_tick,
        position.upper_tick,
    );
    env.storage().persistent().set(&key, position);
}

/// Load a vault position.
pub fn get_vault_position(
    env: &Env,
    asset: AssetId,
    strategy: &Address,
    lower_tick: i32,
    upper_tick: i32,
) -> Result<VaultPosition, ContractError> {
    let key = VaultPositionKey(asset, strategy.clone(), lower_tick, upper_tick);
    env.storage()
        .persistent()
        .get(&key)
        .ok_or(ContractError::NotRegistered)
}

/// Remove a vault position.
pub fn remove_vault_position(
    env: &Env,
    asset: AssetId,
    strategy: &Address,
    lower_tick: i32,
    upper_tick: i32,
) {
    let key = VaultPositionKey(asset, strategy.clone(), lower_tick, upper_tick);
    env.storage().persistent().remove(&key);
}

/// Adjust liquidity concentration range for a vault strategy position.
/// This allows the vault to shift its position to a new tick range.
pub fn adjust_liquidity_range(
    env: &Env,
    asset: AssetId,
    strategy: &Address,
    old_lower_tick: i32,
    old_upper_tick: i32,
    new_lower_tick: i32,
    new_upper_tick: i32,
) -> Result<VaultPosition, ContractError> {
    // Verify strategy is authorized
    let vault = get_vault_strategy(env, asset, strategy)?;
    if !vault.authorized {
        return Err(ContractError::Unauthorized);
    }

    // Validate new tick range
    if new_lower_tick >= new_upper_tick {
        return Err(ContractError::InvalidInput);
    }
    if new_upper_tick - new_lower_tick > vault.max_tick_range {
        return Err(ContractError::InvalidInput);
    }
    if new_lower_tick < MIN_TICK_INDEX || new_upper_tick > MAX_TICK_INDEX {
        return Err(ContractError::TickOutOfBounds);
    }

    // Load old position
    let mut position = get_vault_position(env, asset, strategy, old_lower_tick, old_upper_tick)?;

    // Remove liquidity from old ticks
    place_liquidity(env, asset, old_lower_tick, -(position.liquidity as i64))?;
    place_liquidity(env, asset, old_upper_tick, position.liquidity as i64)?;

    // Add liquidity to new ticks
    place_liquidity(env, asset, new_lower_tick, position.liquidity as i64)?;
    place_liquidity(env, asset, new_upper_tick, -(position.liquidity as i64))?;

    // Update position
    position.lower_tick = new_lower_tick;
    position.upper_tick = new_upper_tick;
    position.updated_at = env.ledger().timestamp();

    // Save new position
    remove_vault_position(env, asset, strategy, old_lower_tick, old_upper_tick);
    set_vault_position(env, &position);

    Ok(position)
}

/// Execute a rebalancing swap with slippage protection.
/// This performs the actual token swap to restore 50/50 ratio.
fn execute_rebalance_swap(
    env: &Env,
    asset: AssetId,
    reserve0: u128,
    reserve1: u128,
    amount_in: u128,
    direction_token0_to_token1: bool,
    min_amount_out: u128,
    fee_bps: u32,
) -> Result<u128, ContractError> {
    // Get current tick and liquidity
    let meta = get_tick_index(env, asset)?;
    let current_tick = meta.current_tick;
    let active_liquidity = meta.active_liquidity;

    // Simulate the swap across ticks
    let (result, _steps) = simulate_swap_across_ticks(
        env,
        asset,
        current_tick,
        active_liquidity,
        amount_in as u64,
        direction_token0_to_token1,
        fee_bps,
    )?;

    // Apply slippage check
    let amount_out = enforce_slippage(result.amount_out as u128, min_amount_out)?;

    Ok(amount_out)
}

/// Execute automated rebalancing for a pool.
/// This is the main entry point for the rebalancing hook.
pub fn execute_rebalance(
    env: &Env,
    asset: AssetId,
    caller: &Address,
    reserve0: u128,
    reserve1: u128,
) -> Result<RebalanceResult, ContractError> {
    // Load config
    let mut config = get_rebalance_config(env, asset)?;

    // Check if rebalancing is enabled
    if !config.enabled {
        return Ok(RebalanceResult {
            executed: false,
            amount0_swapped: 0,
            amount1_swapped: 0,
            positions_adjusted: 0,
            gas_used: 0,
        });
    }

    // Check caller authorization (must be authorized vault strategy)
    if !is_vault_strategy_authorized(env, asset, caller)? {
        return Err(ContractError::Unauthorized);
    }

    // Check minimum rebalance interval
    let now = env.ledger().timestamp();
    if now.saturating_sub(config.last_rebalance_at) < config.min_rebalance_interval {
        return Ok(RebalanceResult {
            executed: false,
            amount0_swapped: 0,
            amount1_swapped: 0,
            positions_adjusted: 0,
            gas_used: 0,
        });
    }

    // Check if rebalancing is needed
    if !should_rebalance(reserve0, reserve1, config.target_ratio_bps, config.threshold_bps)? {
        return Ok(RebalanceResult {
            executed: false,
            amount0_swapped: 0,
            amount1_swapped: 0,
            positions_adjusted: 0,
            gas_used: 0,
        });
    }

    // Calculate rebalance amounts
    let (amount0_in, amount1_in, direction) =
        calculate_rebalance_amounts(reserve0, reserve1, config.target_ratio_bps)?;

    // Calculate minimum output with slippage protection
    let max_slippage = config.max_slippage_bps;
    let (min_amount0_out, min_amount1_out) = if direction {
        // Swapping token0 for token1
        let expected_out = amount1_in;
        let min_out = expected_out
            .checked_mul(10_000 - max_slippage as u128)
            .ok_or(ContractError::Overflow)?
            .checked_div(10_000)
            .ok_or(ContractError::DivisionByZero)?;
        (0, min_out)
    } else {
        // Swapping token1 for token0
        let expected_out = amount0_in;
        let min_out = expected_out
            .checked_mul(10_000 - max_slippage as u128)
            .ok_or(ContractError::Overflow)?
            .checked_div(10_000)
            .ok_or(ContractError::DivisionByZero)?;
        (min_out, 0)
    };

    // Execute the swap with slippage protection
    let amount_out = if direction {
        execute_rebalance_swap(
            env,
            asset,
            reserve0,
            reserve1,
            amount0_in,
            true,
            min_amount1_out,
            config.rebalance_fee_bps,
        )?
    } else {
        execute_rebalance_swap(
            env,
            asset,
            reserve1,
            reserve0,
            amount1_in,
            false,
            min_amount0_out,
            config.rebalance_fee_bps,
        )?
    };

    // Update last rebalance timestamp
    config.last_rebalance_at = now;
    set_rebalance_config(env, &config);

    Ok(RebalanceResult {
        executed: true,
        amount0_swapped: if direction { amount0_in } else { amount_out },
        amount1_swapped: if direction { amount_out } else { amount1_in },
        positions_adjusted: 0,
        gas_used: 0, // Would be calculated from actual execution
    })
}

/// Rebalance a specific vault position's reserves to target ratio.
/// This handles single-sided surplus within a concentrated position.
pub fn rebalance_vault_position(
    env: &Env,
    asset: AssetId,
    strategy: &Address,
    lower_tick: i32,
    upper_tick: i32,
    pool_reserve0: u128,
    pool_reserve1: u128,
) -> Result<RebalanceResult, ContractError> {
    // Verify strategy authorization
    let vault = get_vault_strategy(env, asset, strategy)?;
    if !vault.authorized {
        return Err(ContractError::Unauthorized);
    }

    // Load rebalancing config
    let config = get_rebalance_config(env, asset)?;

    // Load position
    let position = get_vault_position(env, asset, strategy, lower_tick, upper_tick)?;

    // Check if position has single-sided surplus
    let position_ratio = compute_reserve_ratio(position.reserve0, position.reserve1)?;
    let target = config.target_ratio_bps;

    let diff = if position_ratio > target {
        position_ratio - target
    } else {
        target - position_ratio
    };

    if diff < config.threshold_bps {
        return Ok(RebalanceResult {
            executed: false,
            amount0_swapped: 0,
            amount1_swapped: 0,
            positions_adjusted: 0,
            gas_used: 0,
        });
    }

    // Calculate amounts to rebalance within position
    let pos_total = position.reserve0
        .checked_add(position.reserve1)
        .ok_or(ContractError::Overflow)?;

    let target_reserve0 = (pos_total as u128)
        .checked_mul(target as u128)
        .ok_or(ContractError::Overflow)?
        .checked_div(10_000)
        .ok_or(ContractError::DivisionByZero)?;

    let (amount0_swapped, amount1_swapped) = if position.reserve0 > target_reserve0 {
        // Too much token0 in position
        let excess = position.reserve0 - target_reserve0;
        let amount1 = compute_swap_out(excess, pool_reserve0, pool_reserve1)?;
        (excess, amount1)
    } else {
        // Too much token1 in position
        let target_reserve1 = pos_total - target_reserve0;
        let excess = position.reserve1 - target_reserve1;
        let amount0 = compute_swap_out(excess, pool_reserve1, pool_reserve0)?;
        (amount0, excess)
    };

    // Execute with slippage protection
    let min_amount_out = if position.reserve0 > target_reserve0 {
        amount1_swapped
            .checked_mul(10_000 - config.max_slippage_bps as u128)
            .ok_or(ContractError::Overflow)?
            .checked_div(10_000)
            .ok_or(ContractError::DivisionByZero)?
    } else {
        amount0_swapped
            .checked_mul(10_000 - config.max_slippage_bps as u128)
            .ok_or(ContractError::Overflow)?
            .checked_div(10_000)
            .ok_or(ContractError::DivisionByZero)?
    };

    let _ = execute_rebalance_swap(
        env,
        asset,
        pool_reserve0,
        pool_reserve1,
        amount0_swapped.max(amount1_swapped),
        position.reserve0 > target_reserve0,
        min_amount_out,
        config.rebalance_fee_bps,
    )?;

    Ok(RebalanceResult {
        executed: true,
        amount0_swapped,
        amount1_swapped,
        positions_adjusted: 1,
        gas_used: 0,
    })
}

/// Get all vault positions for a strategy.
pub fn get_vault_positions(
    env: &Env,
    asset: AssetId,
    strategy: &Address,
) -> Vec<VaultPosition> {
    // In a real implementation, this would iterate over storage
    // For now, return empty vec as we don't have an index
    Vec::new(env)
}

/// Initialize rebalancing for a pool.
pub fn initialize_rebalancing(
    env: &Env,
    asset: AssetId,
    admin: &Address,
    config: RebalanceConfig,
) -> Result<(), ContractError> {
    // Verify admin authorization
    admin.require_auth();

    // Check if already initialized
    let key = RebalanceConfigKey(asset);
    if env.storage().persistent().has(&key) {
        return Err(ContractError::AlreadyInitialized);
    }

    // Validate config
    if config.target_ratio_bps == 0 || config.target_ratio_bps > 10_000 {
        return Err(ContractError::InvalidInput);
    }
    if config.max_slippage_bps > 10_000 {
        return Err(ContractError::InvalidInput);
    }

    let mut final_config = config;
    final_config.asset = asset;
    final_config.last_rebalance_at = env.ledger().timestamp();

    set_rebalance_config(env, &final_config);
    Ok(())
}

/// Update rebalancing configuration (admin only).
pub fn update_rebalance_config(
    env: &Env,
    asset: AssetId,
    admin: &Address,
    enabled: Option<bool>,
    target_ratio_bps: Option<u32>,
    threshold_bps: Option<u32>,
    max_slippage_bps: Option<u32>,
    rebalance_fee_bps: Option<u32>,
    min_rebalance_interval: Option<u64>,
) -> Result<RebalanceConfig, ContractError> {
    admin.require_auth();

    let mut config = get_rebalance_config(env, asset)?;

    if let Some(v) = enabled {
        config.enabled = v;
    }
    if let Some(v) = target_ratio_bps {
        if v == 0 || v > 10_000 {
            return Err(ContractError::InvalidInput);
        }
        config.target_ratio_bps = v;
    }
    if let Some(v) = threshold_bps {
        if v > 10_000 {
            return Err(ContractError::InvalidInput);
        }
        config.threshold_bps = v;
    }
    if let Some(v) = max_slippage_bps {
        if v > 10_000 {
            return Err(ContractError::InvalidInput);
        }
        config.max_slippage_bps = v;
    }
    if let Some(v) = rebalance_fee_bps {
        if v > 10_000 {
            return Err(ContractError::InvalidInput);
        }
        config.rebalance_fee_bps = v;
    }
    if let Some(v) = min_rebalance_interval {
        config.min_rebalance_interval = v;
    }

    set_rebalance_config(env, &config);
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger, LedgerInfo};

    fn setup_env() -> Env {
        let env = Env::default();
        env.ledger().set(LedgerInfo {
            timestamp: 1_000_000,
            protocol_version: env.ledger().protocol_version(),
            sequence_number: env.ledger().sequence(),
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 0,
            min_persistent_entry_ttl: 0,
            max_entry_ttl: u32::MAX,
        });
        env
    }

    #[test]
    fn test_compute_reserve_ratio_equal() {
        let ratio = compute_reserve_ratio(1_000_000, 1_000_000).unwrap();
        assert_eq!(ratio, 5000); // 50%
    }

    #[test]
    fn test_compute_reserve_ratio_token0_heavy() {
        let ratio = compute_reserve_ratio(2_000_000, 1_000_000).unwrap();
        assert_eq!(ratio, 6666); // ~66.66%
    }

    #[test]
    fn test_compute_reserve_ratio_token1_heavy() {
        let ratio = compute_reserve_ratio(1_000_000, 3_000_000).unwrap();
        assert_eq!(ratio, 2500); // 25%
    }

    #[test]
    fn test_compute_reserve_ratio_zero_total() {
        assert_eq!(compute_reserve_ratio(0, 0), Err(ContractError::DivisionByZero));
    }

    #[test]
    fn test_should_rebalance_no_deviation() {
        let result = should_rebalance(1_000_000, 1_000_000, 5000, 50).unwrap();
        assert!(!result);
    }

    #[test]
    fn test_should_rebalance_small_deviation() {
        // 51% vs 50% = 100 bps deviation, threshold 50 bps
        let result = should_rebalance(1_020_000, 980_000, 5000, 50).unwrap();
        assert!(result);
    }

    #[test]
    fn test_should_rebalance_below_threshold() {
        // 50.3% vs 50% = 30 bps deviation, threshold 50 bps
        let result = should_rebalance(1_006_000, 994_000, 5000, 50).unwrap();
        assert!(!result);
    }

    #[test]
    fn test_calculate_rebalance_amounts_token0_heavy() {
        // Pool has 60% token0, target 50%
        let (amt0, amt1, dir) = calculate_rebalance_amounts(1_200_000, 800_000, 5000).unwrap();
        assert!(dir); // swap token0 for token1
        assert!(amt0 > 0);
        assert!(amt1 > 0);
    }

    #[test]
    fn test_calculate_rebalance_amounts_token1_heavy() {
        // Pool has 40% token0, target 50%
        let (amt0, amt1, dir) = calculate_rebalance_amounts(800_000, 1_200_000, 5000).unwrap();
        assert!(!dir); // swap token1 for token0
        assert!(amt0 > 0);
        assert!(amt1 > 0);
    }

    #[test]
    fn test_rebalance_config_defaults() {
        let config = RebalanceConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.target_ratio_bps, TARGET_RATIO_BPS);
        assert_eq!(config.threshold_bps, REBALANCE_THRESHOLD_BPS);
        assert_eq!(config.max_slippage_bps, MAX_REBALANCE_SLIPPAGE_BPS);
    }

    #[test]
    fn test_adjust_liquidity_range_validation() {
        let env = setup_env();
        let asset: AssetId = 1;
        let admin = Address::generate(&env);
        let strategy = Address::generate(&env);

        // Initialize tick index
        crate::amm::ticks::initialize_tick_index(&env, asset, 1).unwrap();

        // Authorize strategy
        authorize_vault_strategy(&env, asset, strategy.clone(), 100, 10000).unwrap();

        // Try to adjust with invalid range (lower >= upper)
        let result = adjust_liquidity_range(
            &env,
            asset,
            &strategy,
            0, 10,
            10, 10, // invalid: lower == upper
        );
        assert_eq!(result, Err(ContractError::InvalidInput));

        // Try with reversed range
        let result = adjust_liquidity_range(
            &env,
            asset,
            &strategy,
            0, 10,
            10, 0, // invalid: lower > upper
        );
        assert_eq!(result, Err(ContractError::InvalidInput));
    }

    #[test]
    fn test_execute_rebalance_not_enabled() {
        let env = setup_env();
        let asset: AssetId = 1;
        let admin = Address::generate(&env);
        let strategy = Address::generate(&env);

        // Initialize with rebalancing disabled
        let config = RebalanceConfig {
            asset,
            enabled: false,
            ..Default::default()
        };
        set_rebalance_config(&env, &config);
        authorize_vault_strategy(&env, asset, strategy.clone(), 100, 10000).unwrap();

        let result = execute_rebalance(&env, asset, &strategy, 1_200_000, 800_000).unwrap();
        assert!(!result.executed);
    }

    #[test]
    fn test_execute_rebalance_unauthorized_caller() {
        let env = setup_env();
        let asset: AssetId = 1;
        let admin = Address::generate(&env);
        let strategy = Address::generate(&env);
        let unauthorized = Address::generate(&env);

        let config = RebalanceConfig {
            asset,
            enabled: true,
            ..Default::default()
        };
        set_rebalance_config(&env, &config);
        authorize_vault_strategy(&env, asset, strategy.clone(), 100, 10000).unwrap();

        let result = execute_rebalance(&env, asset, &unauthorized, 1_200_000, 800_000);
        assert_eq!(result, Err(ContractError::Unauthorized));
    }

    #[test]
    fn test_execute_rebalance_below_threshold() {
        let env = setup_env();
        let asset: AssetId = 1;
        let admin = Address::generate(&env);
        let strategy = Address::generate(&env);

        let config = RebalanceConfig {
            asset,
            enabled: true,
            threshold_bps: 500, // 5% threshold
            ..Default::default()
        };
        set_rebalance_config(&env, &config);
        authorize_vault_strategy(&env, asset, strategy.clone(), 100, 10000).unwrap();

        // Only 1% deviation - below threshold
        let result = execute_rebalance(&env, asset, &strategy, 1_010_000, 990_000).unwrap();
        assert!(!result.executed);
    }

    #[test]
    fn test_execute_rebalance_success() {
        let env = setup_env();
        let asset: AssetId = 1;
        let admin = Address::generate(&env);
        let strategy = Address::generate(&env);

        let config = RebalanceConfig {
            asset,
            enabled: true,
            threshold_bps: 50, // 0.5% threshold
            max_slippage_bps: 100, // 1% slippage
            ..Default::default()
        };
        set_rebalance_config(&env, &config);
        authorize_vault_strategy(&env, asset, strategy.clone(), 100, 10000).unwrap();

        // Initialize tick index for swap simulation
        crate::amm::ticks::initialize_tick_index(&env, asset, 1).unwrap();
        crate::amm::ticks::place_liquidity(&env, asset, 0, 10000).unwrap();

        // 60/40 split - 10% deviation, should trigger rebalance
        let result = execute_rebalance(&env, asset, &strategy, 1_200_000, 800_000).unwrap();
        assert!(result.executed);
        assert!(result.amount0_swapped > 0 || result.amount1_swapped > 0);
    }

    #[test]
    fn test_initialize_rebalancing_duplicate() {
        let env = setup_env();
        let asset: AssetId = 1;
        let admin = Address::generate(&env);

        let config = RebalanceConfig {
            asset,
            enabled: true,
            ..Default::default()
        };

        initialize_rebalancing(&env, asset, &admin, config.clone()).unwrap();
        let result = initialize_rebalancing(&env, asset, &admin, config);
        assert_eq!(result, Err(ContractError::AlreadyInitialized));
    }

    #[test]
    fn test_vault_strategy_authorization() {
        let env = setup_env();
        let asset: AssetId = 1;
        let admin = Address::generate(&env);
        let strategy = Address::generate(&env);

        authorize_vault_strategy(&env, asset, strategy.clone(), 100, 10000).unwrap();

        let vault = get_vault_strategy(&env, asset, &strategy).unwrap();
        assert_eq!(vault.strategy, strategy);
        assert!(vault.authorized);
        assert_eq!(vault.max_tick_range, 100);
        assert_eq!(vault.max_liquidity, 10000);

        // Check authorization
        let authorized = is_vault_strategy_authorized(&env, asset, &strategy).unwrap();
        assert!(authorized);

        // Deauthorize
        deauthorize_vault_strategy(&env, asset, strategy.clone()).unwrap();
        let authorized = is_vault_strategy_authorized(&env, asset, &strategy).unwrap();
        assert!(!authorized);
    }
}