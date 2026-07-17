# Slippage Protection - Quick Reference Card

## 🎯 Quick Start

```rust
use crate::math::enforce_slippage_tolerance;

// Protect your conversion:
let expected_rate = 1_000_000_000;  // Your expected rate
let actual_rate = get_current_rate()?;  // Rate from oracle
let max_slippage = 200;  // 2% tolerance

// This line protects you:
enforce_slippage_tolerance(expected_rate, actual_rate, max_slippage)?;
// Transaction continues only if within bounds ✓
```

## 📊 Function Cheat Sheet

| Function | Purpose | Example |
|----------|---------|---------|
| `enforce_slippage_tolerance(exp, act, max)` | **Main protection** - Use this! | `enforce_slippage_tolerance(10000, 10200, 300)?` |
| `calculate_rate_deviation_bps(exp, act)` | Get deviation in bps | `let dev = calculate_rate_deviation_bps(10000, 10200)?` |
| `calculate_min_acceptable_rate(exp, slip)` | Get lower bound | `let min = calculate_min_acceptable_rate(10000, 200)?` |
| `calculate_max_acceptable_rate(exp, slip)` | Get upper bound | `let max = calculate_max_acceptable_rate(10000, 200)?` |
| `validate_slippage_tolerance(slip)` | Check if tolerance valid | `validate_slippage_tolerance(200)?` |

## 🎚️ Slippage Tolerance Guide

```rust
// Choose based on market conditions:
const TIGHT_SLIPPAGE: u32 = 50;      // 0.5% - Stable markets
const NORMAL_SLIPPAGE: u32 = 100;    // 1%   - Regular trading
const MODERATE_SLIPPAGE: u32 = 200;  // 2%   - Medium volatility
const HIGH_SLIPPAGE: u32 = 500;      // 5%   - High volatility
const MAX_SLIPPAGE: u32 = 1000;      // 10%  - Extreme conditions
```

## ⚡ Common Patterns

### Pattern 1: Pre-Trade Validation
```rust
let min = calculate_min_acceptable_rate(expected, slippage)?;
let max = calculate_max_acceptable_rate(expected, slippage)?;

if actual_rate < min || actual_rate > max {
    return Err(Error::SlippageToleranceExceeded);
}
```

### Pattern 2: Post-Trade Enforcement (Recommended)
```rust
let actual_rate = compute_rate()?;
enforce_slippage_tolerance(expected_rate, actual_rate, max_slippage)?;
let result = execute_trade(actual_rate)?;
```

### Pattern 3: Multi-Hop Protection
```rust
// Check each hop independently
enforce_slippage_tolerance(exp_rate_1, act_rate_1, slippage)?;
let intermediate = convert_hop_1()?;

enforce_slippage_tolerance(exp_rate_2, act_rate_2, slippage)?;
let final_amount = convert_hop_2(intermediate)?;
```

## 🔢 Basis Points Conversion

| Percentage | Basis Points | Use Case |
|------------|--------------|----------|
| 0.1% | 10 bps | Ultra-tight bounds |
| 0.5% | 50 bps | Stable markets |
| 1% | 100 bps | Normal trading |
| 2% | 200 bps | Moderate volatility |
| 5% | 500 bps | High volatility |
| 10% | 1000 bps | Extreme conditions |

## ❌ Error Codes

| Error | Code | When It Happens |
|-------|------|-----------------|
| `SlippageToleranceExceeded` | 10 | Rate deviates too much |
| `InvalidSlippageTolerance` | 11 | Tolerance > 10,000 bps |
| `DeviationConsensusZero` | - | Expected rate is zero |
| `PriceMathOverflow` | - | Arithmetic overflow |

## 💡 Tips

1. **Always normalize decimals** before comparing rates
2. **Use tighter slippage** for stable pairs (NGN/USD: 50 bps)
3. **Use wider slippage** for volatile pairs (exotic pairs: 500 bps)
4. **Multi-hop**: Apply slippage to each hop independently
5. **Monitor rejections**: Track how often slippage is exceeded

## 🚨 Common Mistakes

❌ **Wrong**: Comparing rates with different decimal places
```rust
enforce_slippage_tolerance(price_7_dec, price_9_dec, 100)?; // BAD
```

✅ **Right**: Normalize first
```rust
let normalized = normalize_to_nine(price_7_dec, 7)?;
enforce_slippage_tolerance(normalized, price_9_dec, 100)?; // GOOD
```

---

❌ **Wrong**: Using percentage instead of basis points
```rust
enforce_slippage_tolerance(exp, act, 2)?; // This is 0.02%, not 2%!
```

✅ **Right**: Use basis points (multiply by 100)
```rust
enforce_slippage_tolerance(exp, act, 200)?; // This is 2%
```

---

❌ **Wrong**: Ignoring the error
```rust
let _ = enforce_slippage_tolerance(exp, act, 200); // Ignoring result!
execute_trade()?; // Executes even if slippage exceeded
```

✅ **Right**: Propagate the error
```rust
enforce_slippage_tolerance(exp, act, 200)?; // Fails transaction if exceeded
execute_trade()?; // Only reached if slippage OK
```

## 🧪 Testing Your Integration

```rust
#[test]
fn test_my_conversion_with_slippage() {
    let expected = 1_000_000_000;
    let slippage = 200; // 2%

    // Should succeed: 1% deviation
    let good_rate = 1_010_000_000;
    let result = my_conversion(expected, good_rate, slippage);
    assert!(result.is_ok());

    // Should fail: 3% deviation
    let bad_rate = 1_030_000_000;
    let result = my_conversion(expected, bad_rate, slippage);
    assert_eq!(result, Err(Error::SlippageToleranceExceeded));
}
```

## 📚 Full Documentation

- **Complete API**: See `SLIPPAGE_PROTECTION.md`
- **Examples**: See `examples/slippage_protection_example.rs`
- **Implementation**: See `src/math.rs`

## 🔗 Quick Links

```rust
// Import all slippage functions:
use crate::math::{
    enforce_slippage_tolerance,
    calculate_rate_deviation_bps,
    calculate_min_acceptable_rate,
    calculate_max_acceptable_rate,
    validate_slippage_tolerance,
};

// Import error types:
use crate::Error;
```

---

**Remember**: Slippage protection is your first line of defense against toxic arbitrage. Always validate rates before executing conversions!
