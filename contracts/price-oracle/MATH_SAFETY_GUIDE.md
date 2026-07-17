# Math Safety Quick Reference Guide

## Overview

This guide provides a quick reference for using the overflow-protected math functions in the price oracle's `math.rs` module.

## Key Functions

### 1. `normalize_to_nine(value: i128, native_decimals: u32) -> Result<i128, Error>`

**Purpose:** Normalize any asset price to 9 fixed-point decimals for internal calculations.

**Safety Features:**
- ✅ Rejects extreme values (`i128::MIN`, `i128::MAX`)
- ✅ All operations use `checked_mul()` and `checked_div()`
- ✅ Explicit overflow trapping on power operations
- ✅ Returns `Error::PriceMathOverflow` on any overflow

**Usage:**
```rust
// Convert XLM price (7 decimals) to 9 decimals
match normalize_to_nine(10_000_000, 7) {
    Ok(normalized) => {
        // Use normalized value: 1_000_000_000
    }
    Err(Error::PriceMathOverflow) => {
        // Handle overflow - reject transaction
        panic_with_error!(env, Error::PriceMathOverflow);
    }
}
```

**When to Use:**
- Converting asset prices from native decimals to oracle's internal format
- Before performing cross-asset calculations
- In multi-hop liquidity path calculations

---

### 2. `normalize_to_seven(value: i128, input_decimals: u32) -> Result<i128, Error>`

**Purpose:** Normalize asset prices to 7 fixed-point decimals (alternative precision).

**Safety Features:**
- ✅ Rejects extreme values (`i128::MIN`, `i128::MAX`)
- ✅ All operations use `checked_mul()` and `checked_div()`
- ✅ Explicit overflow trapping on power operations
- ✅ Defensive divide-by-zero checks

**Usage:**
```rust
// Convert NGN price (2 decimals) to 7 decimals
match normalize_to_seven(150, 2) {
    Ok(normalized) => {
        // Use normalized value: 15_000_000
    }
    Err(Error::PriceMathOverflow) => {
        // Handle overflow
        panic_with_error!(env, Error::PriceMathOverflow);
    }
}
```

**When to Use:**
- Legacy integrations requiring 7-decimal precision
- Specific asset calculations with 7-decimal requirement
- Intermediate calculations in multi-step conversions

---

### 3. `calculate_inverse_price(price: i128, decimals: u32) -> Option<i128>`

**Purpose:** Calculate the inverse of a price (e.g., NGN/XLM → XLM/NGN).

**Safety Features:**
- ✅ Rejects zero prices (divide-by-zero protection)
- ✅ Rejects extreme values (`i128::MIN`, `i128::MAX`)
- ✅ All operations use `checked_pow()`, `checked_mul()`, `checked_div()`
- ✅ Returns `None` on any overflow or invalid input

**Usage:**
```rust
// Calculate inverse price for rate conversion
match calculate_inverse_price(2_000_000_000, 9) {
    Some(inverse) => {
        // Use inverse price: 500_000
    }
    None => {
        // Handle error - invalid input or overflow
        panic_with_error!(env, Error::PriceMathOverflow);
    }
}
```

**When to Use:**
- Converting price pairs (A/B → B/A)
- Multi-hop liquidity calculations requiring inverse rates
- Bidirectional price quotations

---

### 4. `calculate_deviation_bps(submitted: i128, consensus: i128) -> Result<u32, Error>`

**Purpose:** Calculate absolute deviation between submitted and consensus prices in basis points.

**Safety Features:**
- ✅ Zero consensus guard (prevents divide-by-zero)
- ✅ Saturating arithmetic for extreme submissions
- ✅ Returns `Error::DeviationConsensusZero` for zero consensus
- ✅ Returns `Error::PriceMathOverflow` on overflow

**Usage:**
```rust
// Validate submitted price against consensus
match calculate_deviation_bps(10_100, 10_000) {
    Ok(deviation_bps) => {
        if deviation_bps > MAX_DEVIATION_BPS {
            // Reject price - too far from consensus
            panic_with_error!(env, Error::FlashCrashDetected);
        }
        // Price is acceptable: deviation_bps = 100 (1%)
    }
    Err(Error::DeviationConsensusZero) => {
        // Cannot calculate deviation - no consensus price
        panic_with_error!(env, Error::DeviationConsensusZero);
    }
    Err(e) => {
        // Handle other errors
        panic_with_error!(env, e);
    }
}
```

**When to Use:**
- Validating submitted prices against median
- Detecting flash crashes and price manipulation
- Quality control for price feeds

---

## Multi-Hop Calculation Pattern

**Safe Pattern for Multi-Hop Liquidity Paths:**

```rust
// Example: Calculate USD price via XLM -> NGN -> USD path

// Step 1: Normalize input price
let xlm_price = normalize_to_nine(10_000_000, 7)?;  // XLM: 7 decimals

// Step 2: Get intermediate rate
let ngn_per_xlm = normalize_to_nine(500_000, 5)?;   // NGN: 5 decimals

// Step 3: Calculate intermediate price with checked arithmetic
let xlm_in_ngn = xlm_price
    .checked_mul(ngn_per_xlm)
    .ok_or(Error::PriceMathOverflow)?
    .checked_div(1_000_000_000)  // Scale factor
    .ok_or(Error::PriceMathOverflow)?;

// Step 4: Get final rate (NGN to USD)
let usd_per_ngn = normalize_to_nine(10_000, 4)?;    // USD: 4 decimals

// Step 5: Calculate final price with checked arithmetic
let xlm_in_usd = xlm_in_ngn
    .checked_mul(usd_per_ngn)
    .ok_or(Error::PriceMathOverflow)?
    .checked_div(1_000_000_000)  // Scale factor
    .ok_or(Error::PriceMathOverflow)?;

// Result: xlm_in_usd with overflow protection throughout
```

**Key Principles:**
1. ✅ Normalize all inputs before calculations
2. ✅ Use `checked_mul()` and `checked_div()` for all arithmetic
3. ✅ Propagate errors immediately with `?` operator
4. ✅ Never ignore `None` or `Err` results
5. ✅ Document each hop for maintainability

---

## Error Handling Best Practices

### DO ✅

```rust
// Explicit error handling
match normalize_to_nine(value, decimals) {
    Ok(result) => {
        // Use result
    }
    Err(Error::PriceMathOverflow) => {
        // Handle overflow explicitly
        panic_with_error!(env, Error::PriceMathOverflow);
    }
}

// Early return pattern
let normalized = normalize_to_nine(value, decimals)?;
```

### DON'T ❌

```rust
// NEVER unwrap without validation
let normalized = normalize_to_nine(value, decimals).unwrap();

// NEVER ignore errors
let _ = normalize_to_nine(value, decimals);

// NEVER bypass checks
let result = value * 10_i128.pow(diff);  // Unchecked arithmetic
```

---

## Common Pitfalls

### 1. Forgetting to Normalize

❌ **Bad:**
```rust
let price_a = 10_000_000;      // 7 decimals (XLM)
let price_b = 100;             // 2 decimals (NGN)
let ratio = price_a / price_b;  // Wrong! Different decimal scales
```

✅ **Good:**
```rust
let price_a = normalize_to_nine(10_000_000, 7)?;  // 1_000_000_000
let price_b = normalize_to_nine(100, 2)?;         // 10_000_000_000
let ratio = price_a
    .checked_div(price_b)
    .ok_or(Error::PriceMathOverflow)?;
```

### 2. Unchecked Arithmetic in Chains

❌ **Bad:**
```rust
let result = (value * multiplier) / divisor;  // Can overflow silently
```

✅ **Good:**
```rust
let result = value
    .checked_mul(multiplier)
    .ok_or(Error::PriceMathOverflow)?
    .checked_div(divisor)
    .ok_or(Error::PriceMathOverflow)?;
```

### 3. Ignoring Inverse Price Failures

❌ **Bad:**
```rust
let inverse = calculate_inverse_price(price, 9).unwrap();
```

✅ **Good:**
```rust
let inverse = calculate_inverse_price(price, 9)
    .ok_or(Error::PriceMathOverflow)?;
```

---

## Testing Checklist

When writing tests for functions using these math utilities:

- [ ] Test with normal values
- [ ] Test with zero values (where applicable)
- [ ] Test with extreme values (`i128::MAX`, `i128::MIN`)
- [ ] Test multi-hop scenarios
- [ ] Test error propagation
- [ ] Test boundary conditions (e.g., exactly at overflow threshold)

**Example Test:**
```rust
#[test]
fn test_multi_hop_overflow_protection() {
    // Test that extreme values are rejected
    assert_eq!(
        normalize_to_nine(i128::MAX, 0),
        Err(Error::PriceMathOverflow)
    );
    
    // Test that normal multi-hop works
    let hop1 = normalize_to_nine(1_000_000, 7).unwrap();
    let hop2 = calculate_inverse_price(hop1, 9).unwrap();
    assert!(hop2 > 0);  // Should complete successfully
}
```

---

## Performance Notes

- Checked arithmetic has **negligible overhead** (compiler intrinsics)
- Early validation adds only a few comparison operations
- No heap allocations or external calls
- Suitable for high-frequency price updates

---

## Summary

**Golden Rules:**
1. Always use `normalize_to_nine()` or `normalize_to_seven()` before calculations
2. Always use `checked_mul()` and `checked_div()` for arithmetic
3. Always handle errors explicitly - never unwrap
4. Always test with extreme values
5. Always document multi-hop logic

**When in Doubt:**
- Prefer checked arithmetic over performance
- Validate inputs early
- Propagate errors immediately
- Add tests for edge cases

For detailed implementation notes, see `MULTI_HOP_OVERFLOW_PROTECTION.md`.
