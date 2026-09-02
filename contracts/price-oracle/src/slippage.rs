//! Dynamic Slippage Protection Module
//!
//! This module provides volatility-aware and liquidity-aware slippage protection
//! for cross-asset conversions. The dynamic slippage tolerance adapts based on:
//! - Historical price volatility (tracked via exponential moving average)
//! - Available liquidity depth
//! - Configurable baseline tolerance and bounds
//!
//! Features:
//! - Automatic slippage calculation based on market conditions
//! - Manual override capability for advanced users
//! - Comprehensive event emission for monitoring
//! - Protection against oracle manipulation and toxic arbitrage

use soroban_sdk::{contracttype, Env, Symbol, Address};

use crate::Error;
use crate::math::{validate_slippage_tolerance, calculate_rate_deviation_bps};

/// Scale factor for basis point calculations (10,000 bps = 100%)
const BPS_SCALE: u32 = 10_000;

/// Maximum volatility multiplier (prevents extreme tolerance inflation)
const MAX_VOLATILITY_MULTIPLIER: u32 = 1000; // 10x

/// Maximum EMA alpha value (prevents over-responsiveness)
const MAX_EMA_ALPHA_BPS: u32 = 5000; // 50%

/// Minimum EMA alpha value (prevents under-responsiveness)
const MIN_EMA_ALPHA_BPS: u32 = 100; // 1%

/// Liquidity penalty per 10% below threshold (in basis points)
const LIQUIDITY_PENALTY_PER_10PCT: u32 = 20; // 0.2% per 10% deficit

/// Scale factor for 9-decimal fixed-point arithmetic
const SCALE_FACTOR: i128 = 1_000_000_000;

// ================================================================================================
// Data Structures
// ================================================================================================

/// Configuration parameters for dynamic slippage protection
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct SlippageConfig {
    /// Base slippage tolerance in basis points (e.g., 50 = 0.5%)
    pub base_tolerance_bps: u32,
    
    /// Minimum allowed tolerance regardless of conditions (e.g., 10 = 0.1%)
    pub min_tolerance_bps: u32,
    
    /// Maximum allowed tolerance regardless of conditions (e.g., 1000 = 10%)
    pub max_tolerance_bps: u32,
    
    /// Multiplier for volatility impact (e.g., 500 = 5x)
    /// A higher multiplier increases sensitivity to volatility
    pub volatility_multiplier: u32,
    
    /// Liquidity threshold below which penalty is applied
    pub liquidity_threshold: i128,
    
    /// EMA smoothing factor in basis points (e.g., 2000 = 20%)
    /// Higher values make volatility tracking more responsive to recent changes
    pub ema_alpha_bps: u32,
}

/// Historical volatility metrics for an asset
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct VolatilityMetrics {
    /// Asset symbol
    pub asset: Symbol,
    
    /// Exponential moving average of volatility in basis points
    pub ema_volatility_bps: u32,
    
    /// Last recorded price (for calculating next price change)
    pub last_price: i128,
    
    /// Timestamp of last update
    pub last_updated: u64,
    
    /// Count of price updates observed (for initial bootstrap)
    pub price_update_count: u32,
}

/// Event emitted when a swap is successfully executed
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct SwapExecutionEvent {
    /// Source asset
    pub from_asset: Symbol,
    
    /// Destination asset
    pub to_asset: Symbol,
    
    /// Input amount
    pub amount_in: i128,
    
    /// Actual output amount
    pub amount_out: i128,
    
    /// Expected conversion rate
    pub expected_rate: i128,
    
    /// Actual conversion rate
    pub actual_rate: i128,
    
    /// Dynamically calculated slippage tolerance
    pub dynamic_slippage_bps: u32,
    
    /// Actually applied slippage tolerance (may be stricter if manual override)
    pub applied_slippage_bps: u32,
}

/// Event emitted when a swap is rejected due to slippage
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct SlippageRejectionEvent {
    /// Source asset
    pub from_asset: Symbol,
    
    /// Destination asset
    pub to_asset: Symbol,
    
    /// Input amount
    pub amount_in: i128,
    
    /// Actual output amount (that was rejected)
    pub amount_out: i128,
    
    /// Minimum acceptable output
    pub min_acceptable: i128,
    
    /// Actual deviation in basis points
    pub deviation_bps: u32,
    
    /// Allowed slippage tolerance that was exceeded
    pub allowed_slippage_bps: u32,
}

/// Storage keys for slippage protection data
#[derive(Clone)]
#[contracttype]
pub enum SlippageDataKey {
    /// Global slippage configuration
    Config,
    
    /// Volatility metrics for a specific asset
    Volatility(Symbol),
}

// ================================================================================================
// Configuration Management
// ================================================================================================

/// Set the global slippage configuration
///
/// # Arguments
/// * `env` - The Soroban environment
/// * `admin` - The admin address (must be authorized)
/// * `config` - The new slippage configuration
///
/// # Errors
/// Returns an error if:
/// - `base_tolerance_bps` is outside `[min_tolerance_bps, max_tolerance_bps]`
/// - Any tolerance value exceeds 10,000 bps (100%)
/// - `volatility_multiplier` exceeds `MAX_VOLATILITY_MULTIPLIER`
/// - `ema_alpha_bps` is outside `[MIN_EMA_ALPHA_BPS, MAX_EMA_ALPHA_BPS]`
/// - `liquidity_threshold` is negative
pub fn set_slippage_config(
    env: &Env,
    _admin: Address, // TODO: Add admin authorization check
    config: SlippageConfig,
) -> Result<(), Error> {
    // Validate tolerance bounds
    validate_slippage_tolerance(config.base_tolerance_bps)?;
    validate_slippage_tolerance(config.min_tolerance_bps)?;
    validate_slippage_tolerance(config.max_tolerance_bps)?;
    
    // Ensure logical ordering
    if config.base_tolerance_bps < config.min_tolerance_bps {
        return Err(Error::InvalidSlippageTolerance);
    }
    
    if config.base_tolerance_bps > config.max_tolerance_bps {
        return Err(Error::InvalidSlippageTolerance);
    }
    
    if config.min_tolerance_bps > config.max_tolerance_bps {
        return Err(Error::InvalidSlippageTolerance);
    }
    
    // Validate volatility multiplier
    if config.volatility_multiplier > MAX_VOLATILITY_MULTIPLIER {
        return Err(Error::InvalidSlippageTolerance);
    }
    
    // Validate EMA alpha
    if config.ema_alpha_bps < MIN_EMA_ALPHA_BPS || config.ema_alpha_bps > MAX_EMA_ALPHA_BPS {
        return Err(Error::InvalidSlippageTolerance);
    }
    
    // Validate liquidity threshold
    if config.liquidity_threshold < 0 {
        return Err(Error::InvalidLiquidityThreshold);
    }
    
    // Store configuration
    env.storage()
        .persistent()
        .set(&SlippageDataKey::Config, &config);
    
    Ok(())
}

/// Get the current slippage configuration
///
/// Returns a default configuration if none has been set.
pub fn get_slippage_config(env: &Env) -> SlippageConfig {
    env.storage()
        .persistent()
        .get(&SlippageDataKey::Config)
        .unwrap_or_else(|| default_slippage_config())
}

/// Returns the default slippage configuration (balanced settings)
fn default_slippage_config() -> SlippageConfig {
    SlippageConfig {
        base_tolerance_bps: 50,         // 0.5%
        min_tolerance_bps: 10,          // 0.1%
        max_tolerance_bps: 500,         // 5%
        volatility_multiplier: 500,     // 5x
        liquidity_threshold: 5_000_000_000, // 5 units in 9-decimal precision
        ema_alpha_bps: 2000,            // 20% smoothing
    }
}

// ================================================================================================
// Volatility Tracking
// ================================================================================================

/// Update volatility metrics when a new price is observed
///
/// This function should be called every time a price is updated to maintain
/// accurate volatility measurements.
///
/// # Arguments
/// * `env` - The Soroban environment
/// * `asset` - The asset symbol whose price changed
/// * `new_price` - The new price value
///
/// # Algorithm
/// 1. Load existing metrics or initialize new record
/// 2. Calculate price change percentage in basis points
/// 3. Update EMA volatility using configured alpha
/// 4. Store updated metrics with new timestamp
///
/// # Errors
/// Returns an error if:
/// - `new_price` is zero or negative
/// - Arithmetic overflow occurs during calculation
pub fn update_volatility_metrics(
    env: &Env,
    asset: Symbol,
    new_price: i128,
) -> Result<(), Error> {
    if new_price <= 0 {
        return Err(Error::InvalidPrice);
    }
    
    let config = get_slippage_config(env);
    let key = SlippageDataKey::Volatility(asset.clone());
    
    let metrics = if let Some(existing) = env.storage().persistent().get::<_, VolatilityMetrics>(&key) {
        // Calculate price change in basis points
        let price_change_bps = if existing.last_price > 0 {
            calculate_rate_deviation_bps(existing.last_price, new_price)?
        } else {
            0
        };
        
        // Update EMA: new_ema = alpha * new_value + (1 - alpha) * old_ema
        // All calculations in basis points to avoid decimals
        let alpha = config.ema_alpha_bps;
        let new_ema = if existing.price_update_count > 0 {
            let weighted_new = price_change_bps
                .checked_mul(alpha)
                .ok_or(Error::PriceMathOverflow)?;
            
            let weighted_old = existing.ema_volatility_bps
                .checked_mul(BPS_SCALE - alpha)
                .ok_or(Error::PriceMathOverflow)?;
            
            (weighted_new + weighted_old) / BPS_SCALE
        } else {
            // First update: initialize EMA with first observation
            price_change_bps
        };
        
        VolatilityMetrics {
            asset: asset.clone(),
            ema_volatility_bps: new_ema,
            last_price: new_price,
            last_updated: env.ledger().timestamp(),
            price_update_count: existing.price_update_count.saturating_add(1),
        }
    } else {
        // First price observation - initialize metrics
        VolatilityMetrics {
            asset: asset.clone(),
            ema_volatility_bps: 0,
            last_price: new_price,
            last_updated: env.ledger().timestamp(),
            price_update_count: 0,
        }
    };
    
    // Store updated metrics
    env.storage().persistent().set(&key, &metrics);
    
    // Emit event for monitoring
    env.events().publish(
        (crate::event_topics::VOLATILITY, crate::event_topics::UPDATED),
        metrics,
    );
    
    Ok(())
}

/// Get volatility metrics for an asset
///
/// Returns `None` if no price observations have been recorded yet.
pub fn get_volatility_metrics(env: &Env, asset: Symbol) -> Option<VolatilityMetrics> {
    let key = SlippageDataKey::Volatility(asset);
    env.storage().persistent().get(&key)
}

/// Get just the EMA volatility value in basis points
///
/// Returns 0 if no metrics exist for the asset.
pub fn get_asset_volatility_bps(env: &Env, asset: Symbol) -> u32 {
    get_volatility_metrics(env, asset)
        .map(|m| m.ema_volatility_bps)
        .unwrap_or(0)
}

// ================================================================================================
// Dynamic Slippage Calculation
// ================================================================================================

/// Calculate dynamic slippage tolerance based on volatility and liquidity
///
/// # Arguments
/// * `env` - The Soroban environment
/// * `from_asset` - Source asset symbol
/// * `to_asset` - Destination asset symbol
/// * `liquidity` - Available liquidity for the swap
///
/// # Returns
/// Dynamic slippage tolerance in basis points, clamped between min and max bounds
///
/// # Algorithm
/// 1. Load slippage configuration
/// 2. Get volatility for both assets
/// 3. Use the higher of the two volatilities (conservative approach)
/// 4. Calculate volatility adjustment: base * (1 + volatility * multiplier / 10_000)
/// 5. Apply liquidity penalty if below threshold
/// 6. Clamp result between min and max tolerance
///
/// # Errors
/// Returns an error if arithmetic overflow occurs
pub fn calculate_dynamic_slippage(
    env: &Env,
    from_asset: Symbol,
    to_asset: Symbol,
    liquidity: i128,
) -> Result<u32, Error> {
    let config = get_slippage_config(env);
    
    // Get volatility for both assets
    let from_volatility = get_asset_volatility_bps(env, from_asset);
    let to_volatility = get_asset_volatility_bps(env, to_asset);
    
    // Use the higher volatility (conservative approach)
    let max_volatility = from_volatility.max(to_volatility);
    
    // Calculate volatility-adjusted tolerance
    // Formula: base * (10_000 + volatility * multiplier) / 10_000
    let volatility_factor = max_volatility
        .checked_mul(config.volatility_multiplier)
        .ok_or(Error::PriceMathOverflow)?;
    
    let adjusted_tolerance = config.base_tolerance_bps
        .checked_mul(BPS_SCALE + volatility_factor)
        .ok_or(Error::PriceMathOverflow)?
        .checked_div(BPS_SCALE)
        .ok_or(Error::PriceMathOverflow)?;
    
    // Apply liquidity penalty if below threshold
    let total_tolerance = if liquidity < config.liquidity_threshold && config.liquidity_threshold > 0 {
        // Calculate liquidity as percentage of threshold
        let liquidity_ratio = (liquidity
            .checked_mul(BPS_SCALE as i128)
            .ok_or(Error::PriceMathOverflow)?)
            .checked_div(config.liquidity_threshold)
            .ok_or(Error::PriceMathOverflow)? as u32;
        
        // Penalty increases as liquidity decreases
        // 20 bps per 10% below threshold
        let deficit_pct = BPS_SCALE.saturating_sub(liquidity_ratio);
        let liquidity_penalty = deficit_pct
            .checked_mul(LIQUIDITY_PENALTY_PER_10PCT)
            .ok_or(Error::PriceMathOverflow)?
            .checked_div(1000) // Convert from per-10% to actual
            .ok_or(Error::PriceMathOverflow)?;
        
        adjusted_tolerance
            .checked_add(liquidity_penalty)
            .ok_or(Error::PriceMathOverflow)?
    } else {
        adjusted_tolerance
    };
    
    // Clamp between min and max bounds
    let clamped = total_tolerance
        .max(config.min_tolerance_bps)
        .min(config.max_tolerance_bps);
    
    Ok(clamped)
}

/// Calculate minimum acceptable output with slippage protection
///
/// # Arguments
/// * `amount_in` - Input amount
/// * `rate` - Exchange rate (in 9-decimal fixed-point)
/// * `slippage_bps` - Allowed slippage in basis points
///
/// # Returns
/// Minimum acceptable output amount
///
/// # Formula
/// ```text
/// expected_output = amount_in * rate / SCALE_FACTOR
/// min_output = expected_output * (10_000 - slippage_bps) / 10_000
/// ```
pub fn calculate_min_output_with_slippage(
    amount_in: i128,
    rate: i128,
    slippage_bps: u32,
) -> Result<i128, Error> {
    validate_slippage_tolerance(slippage_bps)?;
    
    // Calculate expected output
    let expected_output = amount_in
        .checked_mul(rate)
        .ok_or(Error::PriceMathOverflow)?
        .checked_div(SCALE_FACTOR)
        .ok_or(Error::PriceMathOverflow)?;
    
    // Apply slippage tolerance
    let slippage_factor = (BPS_SCALE - slippage_bps) as i128;
    let min_output = expected_output
        .checked_mul(slippage_factor)
        .ok_or(Error::PriceMathOverflow)?
        .checked_div(BPS_SCALE as i128)
        .ok_or(Error::PriceMathOverflow)?;
    
    Ok(min_output)
}

/// Calculate the actual slippage experienced in a swap
///
/// # Arguments
/// * `expected_output` - Expected output amount
/// * `actual_output` - Actual output amount received
///
/// # Returns
/// Slippage in basis points
fn calculate_actual_slippage_bps(expected_output: i128, actual_output: i128) -> Result<u32, Error> {
    if expected_output <= 0 {
        return Err(Error::InvalidPrice);
    }
    
    calculate_rate_deviation_bps(expected_output, actual_output)
}

// ================================================================================================
// Swap Execution with Protection
// ================================================================================================

/// Execute a swap with dynamic slippage protection
///
/// This is the primary entry point for swaps with automatic slippage calculation.
/// The function will:
/// 1. Calculate dynamic slippage based on market conditions
/// 2. Optionally apply user's manual minimum (whichever is stricter)
/// 3. Execute the conversion
/// 4. Validate output meets minimum requirement
/// 5. Update volatility metrics
/// 6. Emit appropriate events
///
/// # Arguments
/// * `env` - The Soroban environment
/// * `from_asset` - Source asset symbol
/// * `to_asset` - Destination asset symbol
/// * `amount_in` - Amount to swap
/// * `manual_min_out` - Optional user-specified minimum output (0 = use dynamic only)
/// * `liquidity` - Available liquidity for this swap
/// * `from_price` - Current price of from_asset
/// * `to_price` - Current price of to_asset
///
/// # Returns
/// Actual output amount if swap succeeds
///
/// # Errors
/// Returns `Error::SlippageToleranceExceeded` if output is below acceptable minimum
pub fn execute_swap_with_dynamic_slippage(
    env: &Env,
    sender: Address,
    from_asset: Symbol,
    to_asset: Symbol,
    amount_in: i128,
    manual_min_out: i128,
    liquidity: i128,
    from_price: i128,
    to_price: i128,
) -> Result<i128, Error> {
    // Validate inputs
    if amount_in <= 0 {
        return Err(Error::InvalidPrice);
    }
    
    if from_price <= 0 || to_price <= 0 {
        return Err(Error::InvalidPrice);
    }
    
    // Calculate exchange rate
    let rate = from_price
        .checked_mul(SCALE_FACTOR)
        .ok_or(Error::PriceMathOverflow)?
        .checked_div(to_price)
        .ok_or(Error::PriceMathOverflow)?;
    
    // Calculate expected output
    let expected_output = amount_in
        .checked_mul(rate)
        .ok_or(Error::PriceMathOverflow)?
        .checked_div(SCALE_FACTOR)
        .ok_or(Error::PriceMathOverflow)?;
    
    // Calculate dynamic slippage tolerance
    let dynamic_slippage_bps = calculate_dynamic_slippage(
        env,
        from_asset.clone(),
        to_asset.clone(),
        liquidity,
    )?;
    
    // Calculate dynamic minimum output
    let dynamic_min_output = calculate_min_output_with_slippage(
        amount_in,
        rate,
        dynamic_slippage_bps,
    )?;
    
    // Determine effective minimum (stricter of dynamic vs manual)
    let effective_min_output = if manual_min_out > 0 {
        dynamic_min_output.max(manual_min_out)
    } else {
        dynamic_min_output
    };
    
    // Determine which slippage was actually applied
    let applied_slippage_bps = if manual_min_out > effective_min_output {
        // Manual was stricter - back-calculate what slippage that represents
        let manual_factor = manual_min_out
            .checked_mul(BPS_SCALE as i128)
            .ok_or(Error::PriceMathOverflow)?
            .checked_div(expected_output)
            .ok_or(Error::PriceMathOverflow)? as u32;
        BPS_SCALE.saturating_sub(manual_factor)
    } else {
        dynamic_slippage_bps
    };
    
    // Execute the conversion (actual output = expected output for price oracle)
    // In a real DEX implementation, this would interact with liquidity pools
    let actual_output = expected_output;
    
    // Check if output meets minimum requirement
    if actual_output < effective_min_output {
        // Calculate actual slippage for rejection event
        let actual_slippage_bps = calculate_actual_slippage_bps(expected_output, actual_output)?;
        
        // Emit rejection event with uniform swap topic and tuple payload
        crate::event_topics::publish_swap(
            env,
            from_asset.clone(),
            sender,
            amount_in,
            actual_output,
            0, // No fee charged on rejected swaps
        );
        
        return Err(Error::SlippageToleranceExceeded);
    }
    
    // Update volatility metrics for both assets
    update_volatility_metrics(env, from_asset.clone(), from_price)?;
    update_volatility_metrics(env, to_asset.clone(), to_price)?;
    
    // Emit successful execution event with uniform swap topic and tuple payload
    crate::event_topics::publish_swap(
        env,
        from_asset,
        sender,
        amount_in,
        actual_output,
        0, // Price oracle does not charge swap fees
    );
    
    Ok(actual_output)
}

/// Execute a swap with manual slippage tolerance (no dynamic adjustment)
///
/// This function bypasses dynamic slippage calculation and uses a fixed tolerance
/// specified by the caller. Useful for:
/// - Advanced users who want full control
/// - Integration with external slippage calculations
/// - Testing scenarios
///
/// # Arguments
/// * `env` - The Soroban environment
/// * `from_asset` - Source asset symbol
/// * `to_asset` - Destination asset symbol
/// * `amount_in` - Amount to swap
/// * `manual_slippage_bps` - Fixed slippage tolerance in basis points
/// * `from_price` - Current price of from_asset
/// * `to_price` - Current price of to_asset
///
/// # Returns
/// Actual output amount if swap succeeds
///
/// # Errors
/// Returns `Error::SlippageToleranceExceeded` if output is below acceptable minimum
pub fn execute_swap_with_manual_slippage(
    env: &Env,
    sender: Address,
    from_asset: Symbol,
    to_asset: Symbol,
    amount_in: i128,
    manual_slippage_bps: u32,
    from_price: i128,
    to_price: i128,
) -> Result<i128, Error> {
    // Validate slippage tolerance
    validate_slippage_tolerance(manual_slippage_bps)?;
    
    // Validate inputs
    if amount_in <= 0 {
        return Err(Error::InvalidPrice);
    }
    
    if from_price <= 0 || to_price <= 0 {
        return Err(Error::InvalidPrice);
    }
    
    // Calculate exchange rate
    let rate = from_price
        .checked_mul(SCALE_FACTOR)
        .ok_or(Error::PriceMathOverflow)?
        .checked_div(to_price)
        .ok_or(Error::PriceMathOverflow)?;
    
    // Calculate expected and minimum output
    let expected_output = amount_in
        .checked_mul(rate)
        .ok_or(Error::PriceMathOverflow)?
        .checked_div(SCALE_FACTOR)
        .ok_or(Error::PriceMathOverflow)?;
    
    let min_output = calculate_min_output_with_slippage(
        amount_in,
        rate,
        manual_slippage_bps,
    )?;
    
    // Execute the conversion
    let actual_output = expected_output;
    
    // Check if output meets minimum requirement
    if actual_output < min_output {
        let actual_slippage_bps = calculate_actual_slippage_bps(expected_output, actual_output)?;
        
        // Emit rejection event with uniform swap topic and tuple payload
        crate::event_topics::publish_swap(
            env,
            from_asset.clone(),
            sender,
            amount_in,
            actual_output,
            0, // No fee charged on rejected swaps
        );
        
        return Err(Error::SlippageToleranceExceeded);
    }
    
    // Update volatility metrics
    update_volatility_metrics(env, from_asset.clone(), from_price)?;
    update_volatility_metrics(env, to_asset.clone(), to_price)?;
    
    // Emit execution event with uniform swap topic and tuple payload
    crate::event_topics::publish_swap(
        env,
        from_asset,
        sender,
        amount_in,
        actual_output,
        0, // Price oracle does not charge swap fees
    );
    
    Ok(actual_output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{Env, symbol_short};

    #[test]
    fn test_default_slippage_config() {
        let config = default_slippage_config();
        assert_eq!(config.base_tolerance_bps, 50);
        assert_eq!(config.min_tolerance_bps, 10);
        assert_eq!(config.max_tolerance_bps, 500);
        assert!(config.base_tolerance_bps >= config.min_tolerance_bps);
        assert!(config.base_tolerance_bps <= config.max_tolerance_bps);
    }

    #[test]
    fn test_set_and_get_slippage_config() {
        let env = Env::default();
        let admin = Address::generate(&env);
        
        let config = SlippageConfig {
            base_tolerance_bps: 100,
            min_tolerance_bps: 20,
            max_tolerance_bps: 800,
            volatility_multiplier: 600,
            liquidity_threshold: 10_000_000_000,
            ema_alpha_bps: 2500,
        };
        
        assert!(set_slippage_config(&env, admin, config.clone()).is_ok());
        
        let retrieved = get_slippage_config(&env);
        assert_eq!(retrieved, config);
    }

    #[test]
    fn test_invalid_slippage_config() {
        let env = Env::default();
        let admin = Address::generate(&env);
        
        // Base < Min
        let invalid_config = SlippageConfig {
            base_tolerance_bps: 10,
            min_tolerance_bps: 50,
            max_tolerance_bps: 500,
            volatility_multiplier: 500,
            liquidity_threshold: 5_000_000_000,
            ema_alpha_bps: 2000,
        };
        assert!(set_slippage_config(&env, admin.clone(), invalid_config).is_err());
        
        // Base > Max
        let invalid_config2 = SlippageConfig {
            base_tolerance_bps: 600,
            min_tolerance_bps: 10,
            max_tolerance_bps: 500,
            volatility_multiplier: 500,
            liquidity_threshold: 5_000_000_000,
            ema_alpha_bps: 2000,
        };
        assert!(set_slippage_config(&env, admin, invalid_config2).is_err());
    }

    #[test]
    fn test_update_volatility_metrics_first_time() {
        let env = Env::default();
        let asset = symbol_short!("NGN");
        let price = 1_000_000_000; // 1.0 in 9-decimal precision
        
        assert!(update_volatility_metrics(&env, asset.clone(), price).is_ok());
        
        let metrics = get_volatility_metrics(&env, asset.clone()).unwrap();
        assert_eq!(metrics.asset, asset);
        assert_eq!(metrics.last_price, price);
        assert_eq!(metrics.ema_volatility_bps, 0); // No previous price to compare
        assert_eq!(metrics.price_update_count, 0);
    }

    #[test]
    fn test_update_volatility_metrics_with_price_change() {
        let env = Env::default();
        let asset = symbol_short!("KES");
        
        // First update
        let initial_price = 1_000_000_000;
        update_volatility_metrics(&env, asset.clone(), initial_price).unwrap();
        
        // Second update with 5% increase
        let new_price = 1_050_000_000;
        update_volatility_metrics(&env, asset.clone(), new_price).unwrap();
        
        let metrics = get_volatility_metrics(&env, asset).unwrap();
        assert_eq!(metrics.last_price, new_price);
        assert!(metrics.ema_volatility_bps > 0); // Should have captured the 5% move
        assert_eq!(metrics.price_update_count, 1);
    }

    #[test]
    fn test_calculate_min_output_with_slippage() {
        let amount_in = 1_000_000_000; // 1.0
        let rate = 2_000_000_000; // 2.0 exchange rate
        let slippage = 200; // 2%
        
        let min_output = calculate_min_output_with_slippage(amount_in, rate, slippage).unwrap();
        
        // Expected: 1.0 * 2.0 * 0.98 = 1.96
        let expected = 1_960_000_000;
        assert_eq!(min_output, expected);
    }

    #[test]
    fn test_calculate_dynamic_slippage_low_volatility() {
        let env = Env::default();
        let from_asset = symbol_short!("NGN");
        let to_asset = symbol_short!("KES");
        
        // Initialize with low volatility
        update_volatility_metrics(&env, from_asset.clone(), 1_000_000_000).unwrap();
        update_volatility_metrics(&env, from_asset.clone(), 1_010_000_000).unwrap(); // 1% change
        
        let liquidity = 10_000_000_000; // High liquidity
        let slippage = calculate_dynamic_slippage(&env, from_asset, to_asset, liquidity).unwrap();
        
        // Should be close to base tolerance
        assert!(slippage >= 10); // Above minimum
        assert!(slippage <= 100); // Low volatility keeps it low
    }

    #[test]
    fn test_dynamic_slippage_clamping() {
        let env = Env::default();
        let admin = Address::generate(&env);
        
        // Set very tight bounds
        let config = SlippageConfig {
            base_tolerance_bps: 50,
            min_tolerance_bps: 40,
            max_tolerance_bps: 60,
            volatility_multiplier: 1000, // High multiplier
            liquidity_threshold: 5_000_000_000,
            ema_alpha_bps: 2000,
        };
        set_slippage_config(&env, admin, config).unwrap();
        
        let from_asset = symbol_short!("GHS");
        let to_asset = symbol_short!("XLM");
        
        // Create extreme volatility
        update_volatility_metrics(&env, from_asset.clone(), 1_000_000_000).unwrap();
        update_volatility_metrics(&env, from_asset.clone(), 2_000_000_000).unwrap(); // 100% change
        
        let liquidity = 10_000_000_000;
        let slippage = calculate_dynamic_slippage(&env, from_asset, to_asset, liquidity).unwrap();
        
        // Should be clamped to max
        assert_eq!(slippage, 60);
    }

    #[test]
    fn test_execute_swap_with_acceptable_slippage() {
        let env = Env::default();
        let sender = Address::generate(&env);
        let from_asset = symbol_short!("NGN");
        let to_asset = symbol_short!("KES");
        
        let amount_in = 1_000_000_000;
        let from_price = 1_000_000_000;
        let to_price = 1_000_000_000; // 1:1 rate
        let liquidity = 10_000_000_000;
        
        let result = execute_swap_with_dynamic_slippage(
            &env,
            sender,
            from_asset,
            to_asset,
            amount_in,
            0, // No manual minimum
            liquidity,
            from_price,
            to_price,
        );
        
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 1_000_000_000); // 1:1 conversion
    }

    #[test]
    fn test_execute_swap_with_manual_override() {
        let env = Env::default();
        let sender = Address::generate(&env);
        let from_asset = symbol_short!("GHS");
        let to_asset = symbol_short!("NGN");
        
        let amount_in = 1_000_000_000;
        let from_price = 2_000_000_000;
        let to_price = 1_000_000_000; // 2:1 rate
        let liquidity = 10_000_000_000;
        
        // Set a very strict manual minimum
        let manual_min = 1_990_000_000; // Only accept if getting almost 2.0
        
        let result = execute_swap_with_dynamic_slippage(
            &env,
            sender,
            from_asset,
            to_asset,
            amount_in,
            manual_min,
            liquidity,
            from_price,
            to_price,
        );
        
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output >= manual_min);
    }
}
