# Slippage Protection Framework

## Overview

The slippage protection framework provides strict trade boundaries for cross-currency conversions, preventing toxic arbitrage extraction during volatile market conditions. This implementation protects capital pools by validating that computed exchange rates remain within user-specified tolerance thresholds.

## Problem Statement

Processing cross-currency conversions across volatile market corridors without tight trade boundaries exposes capital pools to:
- **Toxic arbitrage extraction**: Sophisticated actors exploiting price discrepancies
- **Flash crashes**: Sudden price movements during low liquidity periods
- **Sandwich attacks**: Front-running and back-running legitimate transactions
- **Oracle manipulation**: Attempts to game pricing mechanisms

## Solution

The framework integrates strict slippage tolerance calculations into `src/math.rs`, enabling instant transaction termination when computed rates deviate from expected values beyond acceptable thresholds.

## API Reference

### Core Functions

#### `validate_slippage_tolerance(slippage_bps: u32) -> Result<(), Error>`

Validates that a slippage tolerance parameter is within acceptable bounds [0, 10,000] basis points (0-100%).

**Example:**
```rust
use crate::math::validate_slippage_tolerance;

// Valid tolerance
validate_slippage_tolerance(200)?; // 2%

// Invalid tolerance
validate_slippage_tolerance(15_000)?; // Returns Error::InvalidSlippageTolerance
```

#### `calculate_rate_deviation_bps(expected_rate: i128, actual_rate: i128) -> Result<u32, Error>`

Calculates the absolute deviation between an expected rate and an actual rate in basis points.

**Formula:** `|actual - expected| * 10_000 / expected`

**Example:**
```rust
use crate::math::calculate_rate_deviation_bps;

let expected = 10_000_000; // Expected rate
let actual = 10_100_000;   // Actual rate (1% higher)

let deviation = calculate_rate_deviation_bps(expected, actual)?;
assert_eq!(deviation, 100); // 100 bps = 1%
```

#### `enforce_slippage_tolerance(expected_rate: i128, actual_rate: i128, max_slippage_bps: u32) -> Result<(), Error>`

**Primary enforcement function.** Validates that the deviation between expected and actual rates does not exceed the specified tolerance. Terminates execution immediately if the threshold is breached.

**Example:**
```rust
use crate::math::enforce_slippage_tolerance;

let expected_rate = 1_000_000_000; // 1:1 conversion
let actual_rate = 1_020_000_000;   // 2% higher
let max_slippage = 300;             // 3% tolerance

// This succeeds because 2% < 3%
enforce_slippage_tolerance(expected_rate, actual_rate, max_slippage)?;

// This would fail if actual_rate was 1_040_000_000 (4% deviation)
```

#### `calculate_min_acceptable_rate(expected_rate: i128, slippage_bps: u32) -> Result<i128, Error>`

Computes the minimum acceptable rate for a given expected rate and slippage tolerance.

**Formula:** `expected_rate * (10_000 - slippage_bps) / 10_000`

**Example:**
```rust
use crate::math::calculate_min_acceptable_rate;

let expected = 10_000_000;
let slippage = 200; // 2%

let min_rate = calculate_min_acceptable_rate(expected, slippage)?;
assert_eq!(min_rate, 9_800_000); // 2% below expected
```

#### `calculate_max_acceptable_rate(expected_rate: i128, slippage_bps: u32) -> Result<i128, Error>`

Computes the maximum acceptable rate for a given expected rate and slippage tolerance.

**Formula:** `expected_rate * (10_000 + slippage_bps) / 10_000`

**Example:**
```rust
use crate::math::calculate_max_acceptable_rate;

let expected = 10_000_000;
let slippage = 200; // 2%

let max_rate = calculate_max_acceptable_rate(expected, slippage)?;
assert_eq!(max_rate, 10_200_000); // 2% above expected
```

## Error Codes

| Error | Code | Description |
|-------|------|-------------|
| `SlippageToleranceExceeded` | 10 | Computed rate deviates too far from expected rate |
| `InvalidSlippageTolerance` | 11 | Slippage tolerance must be between 0 and 10,000 bps |
| `DeviationConsensusZero` | - | Cannot calculate deviation with zero expected rate |
| `PriceMathOverflow` | - | Arithmetic overflow during calculation |

## Usage Patterns

### Pattern 1: Pre-Trade Validation

Calculate acceptable bounds before executing a conversion:

```rust
use crate::math::{calculate_min_acceptable_rate, calculate_max_acceptable_rate};

fn validate_conversion_bounds(
    expected_rate: i128,
    user_slippage: u32,
) -> Result<(i128, i128), Error> {
    let min_rate = calculate_min_acceptable_rate(expected_rate, user_slippage)?;
    let max_rate = calculate_max_acceptable_rate(expected_rate, user_slippage)?;
    
    Ok((min_rate, max_rate))
}
```

### Pattern 2: Post-Conversion Enforcement

Validate the computed rate after calculation:

```rust
use crate::math::enforce_slippage_tolerance;

fn execute_cross_currency_swap(
    amount: i128,
    expected_rate: i128,
    max_slippage_bps: u32,
) -> Result<i128, Error> {
    // Perform conversion calculation
    let actual_rate = compute_exchange_rate()?;
    
    // Enforce slippage protection
    enforce_slippage_tolerance(expected_rate, actual_rate, max_slippage_bps)?;
    
    // Execute the swap
    let output_amount = amount * actual_rate / SCALE_FACTOR;
    Ok(output_amount)
}
```

### Pattern 3: Multi-Hop Conversion Protection

Apply slippage checks at each hop in a multi-currency path:

```rust
use crate::math::enforce_slippage_tolerance;

fn convert_ngn_to_ghs_via_xlm(
    ngn_amount: i128,
    expected_ngn_xlm_rate: i128,
    expected_xlm_ghs_rate: i128,
    slippage_per_hop: u32,
) -> Result<i128, Error> {
    // First hop: NGN → XLM
    let actual_ngn_xlm_rate = get_price_pair(NGN, XLM)?;
    enforce_slippage_tolerance(
        expected_ngn_xlm_rate,
        actual_ngn_xlm_rate,
        slippage_per_hop
    )?;
    
    let xlm_amount = (ngn_amount * actual_ngn_xlm_rate) / SCALE_FACTOR;
    
    // Second hop: XLM → GHS
    let actual_xlm_ghs_rate = get_price_pair(XLM, GHS)?;
    enforce_slippage_tolerance(
        expected_xlm_ghs_rate,
        actual_xlm_ghs_rate,
        slippage_per_hop
    )?;
    
    let ghs_amount = (xlm_amount * actual_xlm_ghs_rate) / SCALE_FACTOR;
    Ok(ghs_amount)
}
```

## Recommended Slippage Tolerances

| Market Condition | Corridor Type | Recommended Slippage |
|-----------------|---------------|---------------------|
| Stable, high liquidity | Fiat → Fiat | 0.1% - 0.5% (10-50 bps) |
| Normal conditions | Fiat → Crypto | 0.5% - 1% (50-100 bps) |
| Moderate volatility | Emerging market pairs | 1% - 2% (100-200 bps) |
| High volatility | Exotic pairs | 2% - 5% (200-500 bps) |
| Extreme conditions | Crisis scenarios | 5% - 10% (500-1000 bps) |

**Note:** Tighter slippage bounds provide better protection but may increase transaction rejection rates during volatile periods.

## Integration Example

Complete example integrating slippage protection into a conversion function:

```rust
use crate::math::{
    enforce_slippage_tolerance,
    normalize_to_nine,
};
use crate::{Error, PriceOracle};

impl PriceOracle {
    /// Convert an amount from one asset to another with slippage protection.
    pub fn convert_with_slippage_protection(
        env: Env,
        from_asset: Symbol,
        to_asset: Symbol,
        amount: i128,
        expected_rate: i128,
        max_slippage_bps: u32,
    ) -> Result<i128, Error> {
        // Get current prices for both assets
        let from_price = Self::get_last_price(env.clone(), from_asset.clone())?;
        let to_price = Self::get_last_price(env.clone(), to_asset.clone())?;
        
        // Calculate actual conversion rate
        let actual_rate = (from_price * SCALE_FACTOR) / to_price;
        
        // Enforce slippage tolerance - transaction fails here if exceeded
        enforce_slippage_tolerance(expected_rate, actual_rate, max_slippage_bps)?;
        
        // Execute conversion
        let output_amount = (amount * actual_rate) / SCALE_FACTOR;
        
        // Emit event for monitoring
        env.events().publish(
            (symbol_short!("convert"), from_asset, to_asset),
            (amount, output_amount, actual_rate, max_slippage_bps)
        );
        
        Ok(output_amount)
    }
}
```

## Testing

The framework includes comprehensive test coverage:

- **Validation tests**: Verify tolerance bounds are correctly enforced
- **Deviation calculation tests**: Test various deviation scenarios
- **Enforcement tests**: Validate accept/reject decisions
- **Boundary tests**: Edge cases at tolerance limits
- **Scenario tests**: Real-world conversion scenarios
- **Scale tests**: Large and small value handling

Run tests with:
```bash
cargo test -p price-oracle --lib math::tests
```

## Security Considerations

1. **Decimal Precision**: Always normalize rates to the same decimal precision before comparison
2. **Overflow Protection**: All arithmetic operations use checked math to prevent overflows
3. **Zero Division**: Expected rate is validated to be non-zero before calculations
4. **Front-Running**: Consider implementing rate locks or commit-reveal patterns for high-value conversions
5. **Oracle Manipulation**: Combine slippage protection with other circuit breakers and heartbeat checks

## Performance

- **Gas Efficiency**: Minimal computational overhead (2-3 multiply/divide operations)
- **Early Termination**: Failed checks terminate immediately, saving gas
- **No Storage**: All calculations are pure functions with no storage reads/writes

## Migration Guide

For existing conversion functions, add slippage protection in three steps:

1. **Add parameter**: Include `max_slippage_bps: u32` to function signature
2. **Calculate expected rate**: Determine the expected rate from oracle data
3. **Add enforcement**: Call `enforce_slippage_tolerance` before executing the conversion

```rust
// Before
pub fn convert(env: Env, from: Symbol, to: Symbol, amount: i128) -> Result<i128, Error> {
    let rate = calculate_rate(from, to)?;
    Ok(amount * rate / SCALE)
}

// After
pub fn convert(
    env: Env,
    from: Symbol,
    to: Symbol,
    amount: i128,
    expected_rate: i128,
    max_slippage_bps: u32,
) -> Result<i128, Error> {
    let actual_rate = calculate_rate(from, to)?;
    enforce_slippage_tolerance(expected_rate, actual_rate, max_slippage_bps)?; // ← Added
    Ok(amount * actual_rate / SCALE)
}
```

## Monitoring and Alerting

Track slippage protection effectiveness with these metrics:

1. **Rejection Rate**: Percentage of conversions rejected due to slippage
2. **Deviation Distribution**: Histogram of actual deviations
3. **Tolerance Usage**: How close rejections are to the tolerance limit
4. **Asset Pair Volatility**: Track which corridors trigger rejections most often

Emit events for monitoring:
```rust
env.events().publish(
    (symbol_short!("slippage"), symbol_short!("exceeded")),
    (expected_rate, actual_rate, max_slippage_bps)
);
```

## Related Documentation

- `IMPLEMENTATION_SUMMARY.md` - Overall contract architecture
- `INTEGRATION.md` - Integration patterns for downstream protocols
- `LIQUIDITY_VALIDATION.md` - Liquidity checks and validation
- `src/math.rs` - Mathematical utilities and formulas

## Version

Framework Version: 1.0.0  
Last Updated: 2026-06-25
