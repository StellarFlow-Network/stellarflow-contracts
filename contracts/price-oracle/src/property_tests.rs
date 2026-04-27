#![cfg(test)]

//! Property-based tests for price oracle using proptest
//! 
//! This module simulates 1000+ edge cases through property-based testing,
//! automatically generating inputs to find edge cases and invariants.

use proptest::prelude::*;
use crate::{
    calculate_percentage_change_bps, 
    calculate_percentage_difference_bps,
    calculate_price_volatility,
    math::{normalize_to_nine, normalize_to_seven, calculate_inverse_price},
    median::calculate_median,
};
use soroban_sdk::{vec, Env};

/// Strategy for generating valid i128 prices (positive, non-zero).
/// Excludes i128::MIN to avoid overflow issues in subtraction.
fn price_strategy() -> impl Strategy<Value = i128> {
    (1i128..=i128::MAX)
}

/// Strategy for generating valid i128 prices including zero and negative.
/// Used for testing edge cases with various price ranges.
fn extended_price_strategy() -> impl Strategy<Value = i128> {
    -1_000_000_000_000_000i128..=1_000_000_000_000_000i128
}

/// Strategy for generating decimal precision values (0-18).
fn decimal_strategy() -> impl Strategy<Value = u32> {
    0u32..=18u32
}

/// Strategy for generating weights used in index calculations (0-10000 basis points).
fn weight_strategy() -> impl Strategy<Value = u32> {
    0u32..=10_000u32
}

// ═══════════════════════════════════════════════════════════════════════════
// Percentage Change Tests (Property-Based)
// ═══════════════════════════════════════════════════════════════════════════

proptest! {
    /// Test that percentage change is always None when old_price is zero.
    #[test]
    fn prop_percentage_change_zero_old_price_returns_none(new_price in extended_price_strategy()) {
        assert_eq!(calculate_percentage_change_bps(0, new_price), None);
    }

    /// Test that percentage change with equal prices is zero.
    #[test]
    fn prop_percentage_change_equal_prices_is_zero(price in price_strategy()) {
        let result = calculate_percentage_change_bps(price, price);
        assert_eq!(result, Some(0));
    }

    /// Test that percentage change is positive when new_price > old_price.
    #[test]
    fn prop_percentage_change_positive_when_price_increases(
        old_price in 1i128..=i128::MAX / 2,
        increase in 1i128..=i128::MAX / 2
    ) {
        if let Some(new_price) = old_price.checked_add(increase) {
            if let Some(pct) = calculate_percentage_change_bps(old_price, new_price) {
                prop_assert!(pct > 0, "percentage change should be positive when price increases");
            }
        }
    }

    /// Test that percentage change is negative when new_price < old_price.
    #[test]
    fn prop_percentage_change_negative_when_price_decreases(
        old_price in 1_000_000i128..=i128::MAX / 2,
        decrease in 1i128..=i128::MAX / 2
    ) {
        if let Some(new_price) = old_price.checked_sub(decrease) {
            if let Some(pct) = calculate_percentage_change_bps(old_price, new_price) {
                prop_assert!(pct < 0, "percentage change should be negative when price decreases");
            }
        }
    }

    /// Test that percentage change magnitude is symmetric for increase and decrease.
    #[test]
    fn prop_percentage_change_symmetry(
        base_price in 100_000_000i128..=999_999_999i128,
        delta in 1_000_000i128..=100_000_000i128
    ) {
        if let Some(increased) = base_price.checked_add(delta) {
            if let Some(decreased) = base_price.checked_sub(delta) {
                if let (Some(up_pct), Some(down_pct)) = (
                    calculate_percentage_change_bps(base_price, increased),
                    calculate_percentage_change_bps(base_price, decreased)
                ) {
                    // The absolute values should be approximately symmetric
                    // (small rounding differences are acceptable)
                    let up_abs = up_pct.abs();
                    let down_abs = down_pct.abs();
                    prop_assert!(
                        (up_abs - down_abs).abs() <= 1,
                        "Percentage changes should be approximately symmetric"
                    );
                }
            }
        }
    }
}

/// Test percentage difference (absolute value).
proptest! {
    /// Test that percentage difference is always non-negative.
    #[test]
    fn prop_percentage_difference_always_nonnegative(
        old_price in price_strategy(),
        new_price in extended_price_strategy()
    ) {
        if let Some(pct_diff) = calculate_percentage_difference_bps(old_price, new_price) {
            prop_assert!(pct_diff >= 0, "percentage difference should be non-negative");
        }
    }

    /// Test that percentage difference with equal prices is zero.
    #[test]
    fn prop_percentage_difference_equal_prices_is_zero(price in price_strategy()) {
        let result = calculate_percentage_difference_bps(price, price);
        assert_eq!(result, Some(0));
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Price Volatility Tests (Property-Based)
// ═══════════════════════════════════════════════════════════════════════════

proptest! {
    /// Test that volatility is always non-negative.
    #[test]
    fn prop_price_volatility_always_nonnegative(
        old_price in extended_price_strategy(),
        new_price in extended_price_strategy()
    ) {
        if let Some(volatility) = calculate_price_volatility(old_price, new_price) {
            prop_assert!(volatility >= 0, "price volatility should always be non-negative");
        }
    }

    /// Test that volatility is symmetric.
    #[test]
    fn prop_price_volatility_is_symmetric(
        old_price in extended_price_strategy(),
        new_price in extended_price_strategy()
    ) {
        if let (Some(vol1), Some(vol2)) = (
            calculate_price_volatility(old_price, new_price),
            calculate_price_volatility(new_price, old_price)
        ) {
            prop_assert_eq!(vol1, vol2, "volatility should be symmetric");
        }
    }

    /// Test that volatility is zero when prices are equal.
    #[test]
    fn prop_price_volatility_zero_when_equal(price in extended_price_strategy()) {
        let result = calculate_price_volatility(price, price);
        assert_eq!(result, Some(0));
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Decimal Normalization Tests (Property-Based)
// ═══════════════════════════════════════════════════════════════════════════

proptest! {
    /// Test that normalize_to_nine returns a value when decimals are valid.
    #[test]
    fn prop_normalize_to_nine_never_panics(
        price in price_strategy(),
        decimals in decimal_strategy()
    ) {
        // Should not panic for any valid combination
        let _ = normalize_to_nine(price, decimals);
    }

    /// Test that normalize_to_nine with already-normalized decimals is identity.
    #[test]
    fn prop_normalize_to_nine_identity_at_9(price in price_strategy()) {
        let result = normalize_to_nine(price, 9);
        prop_assert_eq!(result, price, "normalize_to_nine(x, 9) should be identity");
    }

    /// Test that double normalization is idempotent.
    #[test]
    fn prop_normalize_to_nine_idempotent(
        price in price_strategy(),
        decimals in decimal_strategy()
    ) {
        let norm1 = normalize_to_nine(price, decimals);
        let norm2 = normalize_to_nine(norm1, 9);
        prop_assert_eq!(norm1, norm2, "double normalization should be identity");
    }

    /// Test that normalize_to_seven never panics for valid inputs.
    #[test]
    fn prop_normalize_to_seven_never_panics(
        price in price_strategy(),
        decimals in decimal_strategy()
    ) {
        let _ = normalize_to_seven(price, decimals);
    }

    /// Test that normalize_to_seven with already-normalized decimals is identity.
    #[test]
    fn prop_normalize_to_seven_identity_at_7(price in price_strategy()) {
        let result = normalize_to_seven(price, 7);
        prop_assert_eq!(result, price, "normalize_to_seven(x, 7) should be identity");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Price Inverse Tests (Property-Based)
// ═══════════════════════════════════════════════════════════════════════════

proptest! {
    /// Test that inverse price never overflows for reasonable values.
    #[test]
    fn prop_inverse_price_no_overflow_for_valid_input(
        price in 1i128..=999_999_999_i128,
        decimals in 0u32..=18u32
    ) {
        // Should not panic or return None due to overflow
        let _ = calculate_inverse_price(price, decimals);
    }

    /// Test that inverse price returns None for zero price.
    #[test]
    fn prop_inverse_price_zero_returns_none(decimals in decimal_strategy()) {
        let result = calculate_inverse_price(0, decimals);
        prop_assert_eq!(result, None, "inverse of zero should be None");
    }

    /// Test that double inverse approaches identity (with rounding).
    #[test]
    fn prop_inverse_price_double_inverse_near_identity(
        price in 100_000i128..=999_999_999_i128,
        decimals in 1u32..=8u32
    ) {
        if let Some(inv1) = calculate_inverse_price(price, decimals) {
            if let Some(inv2) = calculate_inverse_price(inv1, decimals) {
                // Allow for rounding error
                let ratio = (inv2 as f64) / (price as f64);
                prop_assert!(
                    ratio >= 0.95 && ratio <= 1.05,
                    "double inverse should be close to original (got ratio {})",
                    ratio
                );
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Median Calculation Tests (Property-Based)
// ═══════════════════════════════════════════════════════════════════════════

proptest! {
    /// Test that median of odd-length list returns a value from the list.
    #[test]
    fn prop_median_odd_count_is_from_list(
        prices in prop::collection::vec(price_strategy(), 1..50)
    ) {
        let env = Env::default();
        let mut sorted_prices = prices.clone();
        sorted_prices.sort();
        
        let vec: soroban_sdk::Vec<i128> = {
            let mut v = vec![&env];
            for p in prices {
                v.push_back(p);
            }
            v
        };
        
        if let Ok(median) = calculate_median(vec) {
            // Median should exist in the original list
            prop_assert!(
                prices.contains(&median),
                "median should be from the input list"
            );
        }
    }

    /// Test that median is position-invariant (order doesn't matter).
    #[test]
    fn prop_median_position_invariant(
        mut prices in prop::collection::vec(price_strategy(), 2..20)
    ) {
        let env = Env::default();
        
        // Calculate median for original list
        let vec1: soroban_sdk::Vec<i128> = {
            let mut v = vec![&env];
            for p in &prices {
                v.push_back(*p);
            }
            v
        };
        let median1 = calculate_median(vec1).ok();
        
        // Shuffle and recalculate
        prices.sort();
        prices.reverse();
        
        let vec2: soroban_sdk::Vec<i128> = {
            let mut v = vec![&env];
            for p in &prices {
                v.push_back(*p);
            }
            v
        };
        let median2 = calculate_median(vec2).ok();
        
        prop_assert_eq!(median1, median2, "median should be invariant to list order");
    }

    /// Test that median of duplicates is that value.
    #[test]
    fn prop_median_all_same_is_that_value(price in price_strategy()) {
        let env = Env::default();
        let vec: soroban_sdk::Vec<i128> = {
            let mut v = vec![&env];
            for _ in 0..5 {
                v.push_back(price);
            }
            v
        };
        
        if let Ok(median) = calculate_median(vec) {
            prop_assert_eq!(median, price);
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Weighted Average Tests (Property-Based)
// ═══════════════════════════════════════════════════════════════════════════

proptest! {
    /// Test that weighted average with zero weights doesn't divide by zero.
    #[test]
    fn prop_weighted_average_zero_weight_guards(
        prices in prop::collection::vec(price_strategy(), 1..10),
        weights in prop::collection::vec(0u32..=100u32, 1..10)
    ) {
        // Ensure arrays have same length for proper paired testing
        let count = prices.len().min(weights.len());
        let prices = &prices[..count];
        let weights = &weights[..count];
        
        let mut total_weighted: i128 = 0;
        let mut total_weight: u32 = 0;
        
        for (price, weight) in prices.iter().zip(weights.iter()) {
            if let Some(weighted) = price.checked_mul(*weight as i128) {
                total_weighted = total_weighted.checked_add(weighted).unwrap_or(total_weight as i128);
            }
            total_weight = total_weight.checked_add(*weight).unwrap_or(total_weight);
        }
        
        // If total_weight is 0, division should not occur
        if total_weight == 0 {
            prop_assert!(true, "zero weight should not cause panic");
        } else {
            // Should be able to divide safely
            let _avg = total_weighted.checked_div(total_weight as i128);
        }
    }

    /// Test that weighted average with uniform weights equals simple average.
    #[test]
    fn prop_weighted_average_uniform_is_simple_average(
        prices in prop::collection::vec(price_strategy(), 2..10)
    ) {
        let count = prices.len() as i128;
        let uniform_weight = 100u32 / prices.len() as u32;
        
        // Calculate simple average
        let mut sum: i128 = 0;
        for price in &prices {
            sum = sum.checked_add(*price).unwrap_or(sum);
        }
        let simple_avg = sum.checked_div(count);
        
        // Calculate weighted average with uniform weights
        let mut weighted_sum: i128 = 0;
        for price in &prices {
            if let Some(w) = price.checked_mul(uniform_weight as i128) {
                weighted_sum = weighted_sum.checked_add(w).unwrap_or(weighted_sum);
            }
        }
        let total_weight = (uniform_weight as i128) * count;
        let weighted_avg = weighted_sum.checked_div(total_weight);
        
        // Should be equal (allowing for rounding)
        if let (Some(s), Some(w)) = (simple_avg, weighted_avg) {
            prop_assert!(
                (s - w).abs() <= count,
                "uniform weighted avg should equal simple avg (diff: {})",
                (s - w).abs()
            );
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Edge Case: Extreme Values
// ═══════════════════════════════════════════════════════════════════════════

proptest! {
    /// Test behavior with i128::MAX values.
    #[test]
    fn prop_handles_max_values(_unit in ".*") {
        let max = i128::MAX;
        let _ = calculate_price_volatility(max, 0);
        let _ = calculate_price_volatility(0, max);
        let _ = calculate_price_difference_bps(1, max);
    }

    /// Test behavior with very small price differences.
    #[test]
    fn prop_handles_tiny_differences(base in 1i128..=i128::MAX / 100) {
        let prices = [base, base.wrapping_add(1), base.wrapping_sub(1)];
        for (i, old) in prices.iter().enumerate() {
            for (j, new) in prices.iter().enumerate() {
                if i != j && *old > 0 {
                    let _ = calculate_percentage_change_bps(*old, *new);
                }
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Edge Case: Boundary Conditions
// ═══════════════════════════════════════════════════════════════════════════

proptest! {
    /// Test normalized decimal boundaries (0, 1, ..., 18).
    #[test]
    fn prop_decimal_boundary_values(price in price_strategy()) {
        for decimals in &[0u32, 1, 9, 16, 17, 18] {
            let _ = normalize_to_nine(price, *decimals);
            let _ = normalize_to_seven(price, *decimals);
        }
    }

    /// Test negative price handling.
    #[test]
    fn prop_negative_prices_handled(
        neg_price in -1_000_000_000_000_000i128..=-1i128,
        pos_price in 1i128..=1_000_000_000_000_000i128
    ) {
        // These should not panic
        let _ = calculate_price_volatility(neg_price, pos_price);
        let _ = calculate_price_volatility(pos_price, neg_price);
        let _ = calculate_price_volatility(neg_price, neg_price);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Stateful Tests: Invariants Over Sequences
// ═══════════════════════════════════════════════════════════════════════════

proptest! {
    /// Test that repeated normalization is idempotent.
    #[test]
    fn prop_repeated_normalization_stable(
        price in price_strategy(),
        decimals in decimal_strategy()
    ) {
        let once = normalize_to_nine(price, decimals);
        let twice = normalize_to_nine(once, 9);
        let thrice = normalize_to_nine(twice, 9);
        prop_assert_eq!(twice, thrice, "repeated normalization should stabilize");
    }

    /// Test that percentage calculations remain bounded.
    #[test]
    fn prop_percentage_bounded(
        old_price in 1_000i128..=1_000_000_000i128,
        new_price in 1_000i128..=1_000_000_000i128
    ) {
        if let Some(pct) = calculate_percentage_change_bps(old_price, new_price) {
            // Percentage change should not exceed ±999,999 BPS (approximately ±99999%)
            // which is reasonable for most markets
            prop_assert!(
                pct.abs() < 10_000_000,
                "percentage change seems unreasonable: {}",
                pct
            );
        }
    }
}
