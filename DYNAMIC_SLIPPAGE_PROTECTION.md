# Dynamic Slippage Protection Implementation

## Overview

This document describes the implementation of dynamic slippage tolerance that adapts based on historical volatility and liquidity depth, providing enhanced protection against market manipulation while maintaining trade execution during normal market conditions.

## Features

### 1. Dynamic Slippage Tolerance Calculation
- **Volatility-Based Adjustment**: Slippage tolerance automatically increases during high volatility periods
- **Liquidity-Based Adjustment**: Larger slippage tolerance for low-liquidity pairs
- **Historical Analysis**: Uses exponential moving average (EMA) to track price volatility
- **Configurable Bounds**: Min/max tolerance bounds prevent extreme values

### 2. Smart Rejection Logic
- **Automatic Calculation**: Computes dynamic minimum acceptable output based on real-time conditions
- **Manual Override**: Callers can specify their own minimum output threshold if they prefer
- **Protection Priority**: Uses the stricter of dynamic vs manual thresholds

### 3. Monitoring and Observability
- **Volatility Metrics**: Tracks and reports volatility measurements per asset
- **Rejection Events**: Emits events when swaps are rejected due to slippage
- **Execution Events**: Logs successful swaps with slippage details

## Architecture

### Data Structures

#### VolatilityMetrics
Tracks historical volatility for each asset pair:
```rust
pub struct VolatilityMetrics {
    pub asset: Symbol,
    pub ema_volatility_bps: u32,      // Exponential moving avg of volatility (basis points)
    pub last_price: i128,              // Last recorded price for volatility calculation
    pub last_updated: u64,             // Timestamp of last update
    pub price_update_count: u32,       // Number of price updates observed
}
```

#### SlippageConfig
Global configuration for slippage protection:
```rust
pub struct SlippageConfig {
    pub base_tolerance_bps: u32,       // Base slippage tolerance (e.g., 50 bps = 0.5%)
    pub min_tolerance_bps: u32,        // Minimum tolerance (e.g., 10 bps = 0.1%)
    pub max_tolerance_bps: u32,        // Maximum tolerance (e.g., 1000 bps = 10%)
    pub volatility_multiplier: u32,    // How much volatility increases tolerance
    pub liquidity_threshold: i128,     // Liquidity level below which tolerance increases
    pub ema_alpha_bps: u32,            // Smoothing factor for EMA (e.g., 2000 = 20%)
}
```

#### SwapExecutionEvent
Emitted on successful swap execution:
```rust
pub struct SwapExecutionEvent {
    pub from_asset: Symbol,
    pub to_asset: Symbol,
    pub amount_in: i128,
    pub amount_out: i128,
    pub expected_rate: i128,
    pub actual_rate: i128,
    pub dynamic_slippage_bps: u32,
    pub applied_slippage_bps: u32,
}
```

#### SlippageRejectionEvent
Emitted when a swap is rejected:
```rust
pub struct SlippageRejectionEvent {
    pub from_asset: Symbol,
    pub to_asset: Symbol,
    pub amount_in: i128,
    pub amount_out: i128,
    pub min_acceptable: i128,
    pub deviation_bps: u32,
    pub allowed_slippage_bps: u32,
}
```

### Key Functions

#### Admin Configuration

##### `set_slippage_config(env, admin, config) -> Result<(), Error>`
Sets the global slippage configuration parameters.

**Parameters:**
- `admin`: Authorized administrator address
- `config`: SlippageConfig struct with all parameters

**Validations:**
- `base_tolerance_bps` must be between `min_tolerance_bps` and `max_tolerance_bps`
- All tolerance values must be ≤ 10,000 bps (100%)
- `volatility_multiplier` must be between 100 (1x) and 1000 (10x)
- `ema_alpha_bps` must be between 100 (1%) and 5000 (50%)

##### `get_slippage_config(env) -> SlippageConfig`
Returns the current slippage configuration.

#### Volatility Tracking

##### `update_volatility_metrics(env, asset, new_price) -> Result<(), Error>`
Updates volatility metrics when a new price is observed.

**Process:**
1. Load existing metrics or initialize new record
2. Calculate price change percentage in basis points
3. Update EMA volatility using configured alpha
4. Store updated metrics with new timestamp
5. Emit volatility update event

**Formula:**
```
price_change_bps = |new_price - last_price| * 10_000 / last_price
ema_volatility = (alpha * price_change_bps + (10_000 - alpha) * old_ema) / 10_000
```

##### `get_volatility_metrics(env, asset) -> Option<VolatilityMetrics>`
Returns current volatility metrics for an asset.

##### `get_asset_volatility_bps(env, asset) -> u32`
Returns just the current EMA volatility in basis points.

#### Dynamic Slippage Calculation

##### `calculate_dynamic_slippage(env, from_asset, to_asset, liquidity) -> Result<u32, Error>`
Computes dynamic slippage tolerance based on volatility and liquidity.

**Parameters:**
- `from_asset`: Source asset symbol
- `to_asset`: Destination asset symbol
- `liquidity`: Total liquidity available for the swap

**Algorithm:**
1. Load slippage configuration
2. Get volatility for both assets
3. Use the higher of the two volatilities
4. Calculate volatility adjustment: `base * (1 + volatility * multiplier / 10_000)`
5. Apply liquidity adjustment if below threshold: `+20 bps per 10% below threshold`
6. Clamp result between `min_tolerance_bps` and `max_tolerance_bps`

**Formula:**
```rust
volatility_adjusted = base_tolerance * (10_000 + max_volatility * multiplier) / 10_000

if liquidity < threshold:
    liquidity_ratio = liquidity * 10_000 / threshold  // e.g., 7000 = 70%
    liquidity_penalty = (10_000 - liquidity_ratio) * 20 / 1000  // 20 bps per 10% deficit
    total_tolerance = volatility_adjusted + liquidity_penalty
else:
    total_tolerance = volatility_adjusted

dynamic_slippage = clamp(total_tolerance, min_tolerance, max_tolerance)
```

**Example:**
- Base tolerance: 50 bps (0.5%)
- Asset volatility: 300 bps (3%)
- Volatility multiplier: 500 (5x)
- Liquidity: 80% of threshold

```
volatility_adjusted = 50 * (10_000 + 300 * 500) / 10_000 = 57.5 bps
liquidity_penalty = (10_000 - 8000) * 20 / 1000 = 40 bps
total_tolerance = 57.5 + 40 = 97.5 bps
```

##### `calculate_min_output_with_slippage(amount_in, rate, slippage_bps) -> Result<i128, Error>`
Calculates minimum acceptable output for a given input amount and rate.

**Formula:**
```
expected_output = amount_in * rate / SCALE_FACTOR
min_output = expected_output * (10_000 - slippage_bps) / 10_000
```

#### Swap Execution with Protection

##### `execute_swap_with_dynamic_slippage(env, from_asset, to_asset, amount_in, manual_min_out, liquidity) -> Result<i128, Error>`
Executes a swap with dynamic slippage protection.

**Parameters:**
- `from_asset`: Source asset
- `to_asset`: Destination asset  
- `amount_in`: Amount to swap
- `manual_min_out`: Optional caller-specified minimum output (0 = use dynamic only)
- `liquidity`: Available liquidity for this swap

**Process:**
1. Fetch current prices for both assets
2. Calculate expected output
3. Calculate dynamic slippage tolerance
4. Compute dynamic minimum output
5. Apply stricter of dynamic vs manual minimum
6. Execute conversion
7. Validate output meets minimum
8. Emit execution or rejection event
9. Update volatility metrics

**Protection Logic:**
```rust
let dynamic_min = calculate_min_output_with_slippage(amount_in, rate, dynamic_slippage)?;
let effective_min = if manual_min_out > 0 {
    max(dynamic_min, manual_min_out)  // Use stricter threshold
} else {
    dynamic_min
};

if actual_output < effective_min {
    emit_rejection_event();
    return Err(Error::SlippageToleranceExceeded);
}
```

##### `execute_swap_with_manual_slippage(env, from_asset, to_asset, amount_in, manual_slippage_bps) -> Result<i128, Error>`
Executes a swap with caller-specified slippage tolerance (no dynamic adjustment).

This is provided for:
- Advanced users who want full control
- Integration with protocols that calculate slippage externally
- Testing and debugging scenarios

**Validation:**
- `manual_slippage_bps` must be ≤ 10,000 (100%)
- Still emits events for monitoring
- Still updates volatility metrics

## Integration Patterns

### Pattern 1: Fully Dynamic Protection (Recommended)

```rust
use price_oracle::{execute_swap_with_dynamic_slippage, PriceOracle};

// Let the oracle calculate optimal slippage based on market conditions
let amount_out = PriceOracle::execute_swap_with_dynamic_slippage(
    env.clone(),
    symbol_short!("NGN"),
    symbol_short!("KES"),
    1_000_000_000,  // 1 NGN with 9 decimals
    0,              // No manual minimum (use dynamic only)
    liquidity_amount,
)?;
```

### Pattern 2: Dynamic with Manual Override

```rust
// Calculate your own minimum, but use dynamic as additional protection
let my_min_acceptable = calculate_worst_case_output();

let amount_out = PriceOracle::execute_swap_with_dynamic_slippage(
    env.clone(),
    symbol_short!("GHS"),
    symbol_short!("XLM"),
    5_000_000_000,
    my_min_acceptable,  // Oracle will use stricter of dynamic vs manual
    liquidity_amount,
)?;
```

### Pattern 3: Manual-Only Control

```rust
// Bypass dynamic calculation and use your own slippage
let amount_out = PriceOracle::execute_swap_with_manual_slippage(
    env.clone(),
    symbol_short!("KES"),
    symbol_short!("NGN"),
    10_000_000_000,
    250,  // Fixed 2.5% slippage
)?;
```

### Pattern 4: Multi-Hop with Dynamic Protection

```rust
// Each hop uses dynamic slippage independently
// First hop: NGN → XLM
let xlm_amount = PriceOracle::execute_swap_with_dynamic_slippage(
    env.clone(),
    symbol_short!("NGN"),
    symbol_short!("XLM"),
    ngn_amount,
    0,
    xlm_liquidity,
)?;

// Second hop: XLM → GHS
let ghs_amount = PriceOracle::execute_swap_with_dynamic_slippage(
    env.clone(),
    symbol_short!("XLM"),
    symbol_short!("GHS"),
    xlm_amount,
    0,
    ghs_liquidity,
)?;
```

## Configuration Recommendations

### Conservative Settings (Low Risk, Higher Rejection Rate)
```rust
SlippageConfig {
    base_tolerance_bps: 25,        // 0.25%
    min_tolerance_bps: 10,         // 0.1%
    max_tolerance_bps: 300,        // 3%
    volatility_multiplier: 300,    // 3x
    liquidity_threshold: 10_000_000_000,
    ema_alpha_bps: 2000,           // 20% smoothing
}
```

### Balanced Settings (Recommended)
```rust
SlippageConfig {
    base_tolerance_bps: 50,        // 0.5%
    min_tolerance_bps: 10,         // 0.1%
    max_tolerance_bps: 500,        // 5%
    volatility_multiplier: 500,    // 5x
    liquidity_threshold: 5_000_000_000,
    ema_alpha_bps: 2000,           // 20% smoothing
}
```

### Aggressive Settings (Higher Risk, Lower Rejection Rate)
```rust
SlippageConfig {
    base_tolerance_bps: 100,       // 1%
    min_tolerance_bps: 20,         // 0.2%
    max_tolerance_bps: 1000,       // 10%
    volatility_multiplier: 800,    // 8x
    liquidity_threshold: 2_000_000_000,
    ema_alpha_bps: 3000,           // 30% smoothing
}
```

## Monitoring and Alerting

### Key Metrics to Track

1. **Rejection Rate**: Percentage of swaps rejected due to slippage
   - Alert if > 10% over 1-hour window
   
2. **Dynamic Slippage Distribution**: Histogram of calculated tolerances
   - Alert if consistently at max_tolerance_bps
   
3. **Volatility Trends**: EMA volatility over time per asset
   - Alert if sudden spikes > 1000 bps

4. **Manual Override Rate**: How often users specify manual minimums
   - High rate may indicate dynamic calculation is too strict

5. **Liquidity Impact**: Correlation between liquidity and rejection rate
   - Adjust liquidity_threshold if seeing issues

### Events to Monitor

```rust
// Subscribe to these event topics
env.events().publish(
    (symbol_short!("swap"), symbol_short!("executed")),
    swap_execution_event
);

env.events().publish(
    (symbol_short!("swap"), symbol_short!("rejected")),
    slippage_rejection_event
);

env.events().publish(
    (symbol_short!("volatility"), symbol_short!("updated")),
    volatility_metrics
);
```

## Security Considerations

### 1. Manipulation Resistance
- **EMA Smoothing**: Prevents single price spike from drastically changing tolerance
- **Bounds Enforcement**: Max tolerance prevents extreme values from manipulation
- **Multi-Factor**: Combines volatility + liquidity for robust calculation

### 2. Oracle Manipulation
- **Separate Concerns**: Dynamic slippage protects even if oracle is manipulated
- **Conservative Defaults**: Min tolerance provides baseline protection
- **Manual Override**: Users can be more strict than dynamic calculation

### 3. Front-Running Protection
- **Deterministic Calculation**: Same inputs always produce same slippage
- **No Predictive Edge**: On-chain calculation gives no informational advantage
- **MEV Resistance**: Slippage bounds limit MEV extraction potential

### 4. Griefing Resistance
- **Gas Efficient**: Volatility updates use minimal storage
- **Rate Limiting**: Consider adding per-asset update frequency limits
- **Cleanup Logic**: Old volatility records can be pruned after inactivity

## Testing Strategy

### Unit Tests
```rust
#[test]
fn test_dynamic_slippage_low_volatility()
#[test]
fn test_dynamic_slippage_high_volatility()
#[test]
fn test_dynamic_slippage_low_liquidity()
#[test]
fn test_volatility_ema_calculation()
#[test]
fn test_manual_override_stricter()
#[test]
fn test_rejection_at_threshold()
```

### Integration Tests
```rust
#[test]
fn test_multi_hop_swap_with_dynamic_protection()
#[test]
fn test_volatility_tracking_across_swaps()
#[test]
fn test_config_update_affects_calculations()
```

### Scenario Tests
- Stable market conditions (low volatility)
- Flash crash scenario (extreme volatility spike)
- Gradual volatility increase
- Low liquidity edge cases
- Manual override combinations

### Property-Based Tests (Fuzzing)
```rust
// Invariants to maintain:
// 1. dynamic_slippage is always between min and max bounds
// 2. actual_output ≥ effective_minimum_output on success
// 3. volatility EMA is always ≥ 0
// 4. liquidity penalty is always ≥ 0
```

## Migration Guide

### For Existing Contracts

**Step 1**: Deploy updated price oracle with dynamic slippage functions

**Step 2**: Initialize slippage configuration
```rust
PriceOracle::set_slippage_config(env, admin, default_config)?;
```

**Step 3**: Update integration points
```rust
// Before
let output = convert(env, from, to, amount)?;

// After
let output = execute_swap_with_dynamic_slippage(
    env, from, to, amount, 0, liquidity
)?;
```

**Step 4**: Monitor and tune
- Start with conservative settings
- Monitor rejection rates and volatility metrics
- Adjust configuration based on observed behavior

## Performance Considerations

### Gas Costs
- **Volatility Update**: ~500 gas (one storage read + one write)
- **Dynamic Calculation**: ~300 gas (two volatility reads, arithmetic)
- **Total Overhead**: ~800 gas per swap (~5% of typical swap cost)

### Storage Optimization
- Use `persistent` storage for volatility metrics (infrequent updates)
- Consider pruning old records after 30 days inactivity
- Compact data structures (all fields are primitives)

### Computational Complexity
- All calculations are O(1) constant time
- No loops or iterations
- Simple arithmetic operations only

## Related Documentation

- `SLIPPAGE_PROTECTION.md` - Static slippage protection framework
- `IMPLEMENTATION_SUMMARY.md` - Overall contract architecture
- `LIQUIDITY_VALIDATION.md` - Liquidity depth validation
- `MATH_SAFETY_GUIDE.md` - Overflow protection patterns

## Version History

- **v1.0.0** (2026-08-26): Initial implementation
  - Volatility-based dynamic slippage
  - Liquidity-aware adjustments
  - Manual override capability
  - Comprehensive event emission

## Future Enhancements

1. **Time-Weighted Slippage**: Account for time since last price update
2. **Asset Correlation**: Adjust slippage for correlated asset pairs
3. **Adaptive Alpha**: Automatically tune EMA smoothing factor
4. **Circuit Breaker Integration**: Coordinate with existing circuit breakers
5. **Multi-Pool Aggregation**: Calculate slippage across multiple liquidity sources

---

**Implementation Status**: ✅ Specification Complete  
**Next Steps**: Implement in `src/slippage.rs` and integrate into main contract
