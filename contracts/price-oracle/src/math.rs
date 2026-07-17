use soroban_sdk::{Env, String};

use crate::Error;

/// Format a scaled integer price into a human-readable decimal string.
///
/// Inserts a decimal point at the position indicated by `decimals`.
/// Works entirely with byte arrays — no `format!`, no `std`, no heap allocations
/// beyond the final Soroban `String`.
///
/// # Examples
/// ```text
/// format_price(env, 75050, 2)  => "750.50"
/// format_price(env, 50,    3)  => "0.050"
/// format_price(env, 1,     0)  => "1"
/// format_price(env, 0,     2)  => "0.00"
/// ```

// pub fn format_price(env: &Env, price: i128, decimals: u32) -> String {
//     // --- 1. Convert the absolute value to ASCII digits in a fixed buffer ------
//     // i128::MAX is 39 digits; 1 sign + 39 digits + 1 dot + 1 NUL = 42 bytes is safe.
//     const BUF: usize = 42;
//     let mut digits = [0u8; BUF]; // ASCII digit buffer (filled right-to-left)
//     let mut len = 0usize;
//     let negative = price < 0;
//     // Use u128 so we can safely negate i128::MIN without overflow.
//     let mut remaining: u128 = if negative {
//         (price as i128).unsigned_abs()
//     } else {
//         price as u128
//     };

//     // Edge case: price == 0
//     if remaining == 0 {
//         digits[BUF - 1] = b'0';
//         len = 1;
//     } else {
//         while remaining > 0 {
//             len += 1;
//             digits[BUF - len] = b'0' + (remaining % 10) as u8;
//             remaining /= 10;
//         }
//     }
//     // digits[BUF-len .. BUF] now holds the ASCII digits, most-significant first.

//     // --- 2. Build the output byte slice into a second fixed buffer ------------
//     // Max output length: 1 (sign) + 39 (digits) + 1 (dot) = 41 bytes.
//     let mut out = [0u8; 41];
//     let mut pos = 0usize;

//     let decimals = decimals as usize;

//     if negative {
//         out[pos] = b'-';
//         pos += 1;
//     }

//     if decimals == 0 {
//         // No decimal point needed — copy digits straight through.
//         let src = &digits[BUF - len..BUF];
//         out[pos..pos + len].copy_from_slice(src);
//         pos += len;
//     } else if len <= decimals {
//         // The integer part is zero; we need leading "0." and possibly leading
//         // fractional zeros.  e.g. price=50, decimals=3 → "0.050"
//         out[pos] = b'0';
//         pos += 1;
//         out[pos] = b'.';
//         pos += 1;

//         // Pad with zeros until we reach the actual digits.
//         let leading_zeros = decimals - len;
//         for _ in 0..leading_zeros {
//             out[pos] = b'0';
//             pos += 1;
//         }

//         let src = &digits[BUF - len..BUF];
//         out[pos..pos + len].copy_from_slice(src);
//         pos += len;
//     } else {
//         // Normal case: integer part has (len - decimals) digits.
//         let int_len = len - decimals;
//         let src = &digits[BUF - len..BUF];

//         out[pos..pos + int_len].copy_from_slice(&src[..int_len]);
//         pos += int_len;

//         out[pos] = b'.';
//         pos += 1;

//         out[pos..pos + decimals].copy_from_slice(&src[int_len..]);
//         pos += decimals;
//     }

//     // --- 3. Wrap in a Soroban String ------------------------------------------
//     // `from_bytes` expects a byte slice, not a soroban_sdk::Bytes.
//     String::from_bytes(env, &out[..pos])
// }

/// Calculate the absolute deviation between a submitted price and the consensus
/// median, expressed in basis points (bps).
///
/// Formula: `|submitted - consensus| * 10_000 / consensus`
///
/// Both values must already be normalized to the same decimal precision before
/// calling. Use [`normalize_to_nine`] if the inputs have different native precisions.
///
/// # Errors
/// - `Error::DeviationConsensusZero` — when `consensus` is zero (divide-by-zero guard).
/// - `Error::PriceMathOverflow` — on arithmetic overflow.
///
/// # Examples
/// ```text
/// calculate_deviation_bps(10_100, 10_000) => Ok(100)   // 1 % = 100 bps
/// calculate_deviation_bps(10_000, 10_000) => Ok(0)     // identical prices
/// calculate_deviation_bps(500, 0)         => Err(DeviationConsensusZero)
/// ```
#[inline]
pub fn calculate_deviation_bps(submitted: i128, consensus: i128) -> Result<u32, Error> {
    if consensus == 0 {
        return Err(Error::DeviationConsensusZero);
    }
    let diff = if submitted >= consensus {
        submitted - consensus
    } else {
        consensus - submitted
    };
    // diff * 10_000 / consensus — use saturating mul so extreme submissions
    // (e.g. i128::MAX) don't panic; they saturate to u32::MAX which maps to
    // the highest DeviationTier (Manipulation).
    let bps = match diff.checked_mul(10_000) {
        Some(v) => v.checked_div(consensus).ok_or(Error::PriceMathOverflow)?,
        None => i128::MAX,
    };
    Ok(bps.min(u32::MAX as i128) as u32)
}

#[inline]
pub fn normalize_to_seven(value: i128, input_decimals: u32) -> Result<i128, Error> {
    // Early trap: validate input value is within safe range
    if value == i128::MIN || value == i128::MAX {
        return Err(Error::PriceMathOverflow);
    }

    if input_decimals < 7 {
        let diff = 7 - input_decimals;
        let multiplier = 10_i128
            .checked_pow(diff)
            .ok_or(Error::PriceMathOverflow)?;
        
        // Explicit overflow trap before multiplication
        value
            .checked_mul(multiplier)
            .ok_or(Error::PriceMathOverflow)
    } else if input_decimals > 7 {
        let diff = input_decimals - 7;
        let divisor = 10_i128
            .checked_pow(diff)
            .ok_or(Error::PriceMathOverflow)?;
        
        // Explicit divide-by-zero trap (though 10^n cannot be zero)
        if divisor == 0 {
            return Err(Error::PriceMathOverflow);
        }
        
        value
            .checked_div(divisor)
            .ok_or(Error::PriceMathOverflow)
    } else {
        Ok(value)
    }
}

/// Normalize a raw price to 9 fixed-point decimals regardless of the asset's
/// native decimal precision.
///
/// All internal math uses 9-decimal fixed-point so that developers never need
/// to write different logic for different assets.
///
/// Formula: `price * 10^(9 - native_decimals)`
///
/// This function uses checked arithmetic throughout to prevent integer truncation
/// during multi-hop liquidity path calculations.
///
/// # Examples
/// ```text
/// normalize_to_nine(1_000_000_0, 7)  => 1_000_000_000  (XLM, 7 dec → 9 dec)
/// normalize_to_nine(100,         2)  => 10_000_000_000  (NGN, 2 dec → 9 dec)
/// normalize_to_nine(1_000_000_000, 9) => 1_000_000_000  (already 9 dec, no-op)
/// normalize_to_nine(1_000_000_000_00, 11) => 1_000_000_000 (scale down)
/// ```
#[inline]
pub fn normalize_to_nine(value: i128, native_decimals: u32) -> Result<i128, Error> {
    const TARGET: u32 = 9;
    const INTERIOR_SCALE: i128 = 1_000_000_000_000_000; // 10^15

    // NOTE: INTERIOR_SCALE is chosen so that the final result remains within
    // the project's 9-decimal fixed-point footprint by dividing back down
    // after the translation.

    // Early trap: validate input value is within safe range for scaled arithmetic
    if value == i128::MIN || value == i128::MAX {
        return Err(Error::PriceMathOverflow);
    }

    // Explicit overflow trap on initial scaling operation
    let scaled = value
        .checked_mul(INTERIOR_SCALE)
        .ok_or(Error::PriceMathOverflow)?;

    let normalized_in_interior_space = if native_decimals < TARGET {
        let diff = TARGET - native_decimals;

        // Trap power overflow early
        let multiplier = 10_i128
            .checked_pow(diff)
            .ok_or(Error::PriceMathOverflow)?;

        // Use checked_mul to explicitly trap multiplication overflow
        scaled
            .checked_mul(multiplier)
            .ok_or(Error::PriceMathOverflow)?
    } else if native_decimals > TARGET {
        let diff = native_decimals - TARGET;

        // Trap power overflow early
        let divisor = 10_i128
            .checked_pow(diff)
            .ok_or(Error::PriceMathOverflow)?;

        // Explicit divide-by-zero trap (defensive, 10^n cannot be zero)
        require_nonzero_denominator(divisor)?;

        // Use checked_div to trap any division anomalies
        scaled
            .checked_div(divisor)
            .ok_or(Error::PriceMathOverflow)?
    } else {
        scaled
    };

    // Final checked division to scale back down
    require_nonzero_denominator(INTERIOR_SCALE)?;
    normalized_in_interior_space
        .checked_div(INTERIOR_SCALE)
        .ok_or(Error::PriceMathOverflow)
}

/// Calculate the inverse of a price (e.g., NGN/XLM → XLM/NGN).
///
/// Uses a fixed-point scale factor of `10^decimals` so that the result
/// preserves the same decimal precision as the input.
///
/// Formula: `(10^decimals * 10^decimals) / price`
///
/// This function uses Soroban's native checked arithmetic to explicitly trap
/// overflow errors during multi-hop regional asset calculations.
///
/// # Returns
/// `Some(inverse)` on success, or `None` when `price` is zero (divide-by-zero)
/// or when overflow occurs.
///
/// # Examples
/// ```text
/// calculate_inverse_price(2_000, 3)  => Some(500_000)   // 1/2.000 = 0.500 (scaled)
/// calculate_inverse_price(0,     7)  => None             // divide-by-zero guard
/// ```
#[inline]
pub fn calculate_inverse_price(price: i128, decimals: u32) -> Option<i128> {
    // Explicit early trap: zero price guard
    if price == 0 {
        return None;
    }
    
    // Explicit early trap: extreme value guard
    if price == i128::MIN || price == i128::MAX {
        return None;
    }
    
    // Trap power overflow explicitly
    let scale = 10_i128.checked_pow(decimals)?;
    
    // Trap multiplication overflow explicitly
    let numerator = scale.checked_mul(scale)?;
    
    // Trap division overflow/error explicitly
    numerator.checked_div(price)
}

/// Require that a denominator is non-zero before performing division.
///
/// Returns `Ok(())` when `n != 0`, or `Err(Error::InvalidDenominator)` when `n` is zero.
/// Call this proactively before every division to prevent runtime panics
/// and to provide a clear error signal to callers.
#[inline]
pub fn require_nonzero_denominator(n: i128) -> Result<(), Error> {
    if n == 0 {
        Err(Error::InvalidDenominator)
    } else {
        Ok(())
    }
}

    // --- 3. Wrap in a Soroban String ------------------------------------------
    let text = core::str::from_utf8(&out[..pos]).expect("price formatting produced invalid utf-8");
    String::from_str(env, text)
/// Validate that a slippage tolerance is within acceptable bounds.
///
/// Slippage tolerance must be in the range [0, 10_000] basis points (0-100%).
/// This prevents configuration errors where unrealistic slippage values
/// could either block all trades or provide no protection.
///
/// # Arguments
/// * `slippage_bps` - The slippage tolerance in basis points
///
/// # Returns
/// `Ok(())` if valid, or `Err(Error::InvalidSlippageTolerance)` if out of range.
///
/// # Examples
/// ```text
/// validate_slippage_tolerance(100)    => Ok(())   // 1% is valid
/// validate_slippage_tolerance(500)    => Ok(())   // 5% is valid
/// validate_slippage_tolerance(10_000) => Ok(())   // 100% is valid (max)
/// validate_slippage_tolerance(10_001) => Err(InvalidSlippageTolerance)
/// ```
pub fn validate_slippage_tolerance(slippage_bps: u32) -> Result<(), Error> {
    const MAX_SLIPPAGE_BPS: u32 = 10_000; // 100%
    
    if slippage_bps > MAX_SLIPPAGE_BPS {
        Err(Error::InvalidSlippageTolerance)
    } else {
        Ok(())
    }
}

/// Calculate the absolute deviation between an expected rate and an actual rate
/// in basis points.
///
/// This is used to verify that cross-currency conversion rates fall within
/// acceptable slippage bounds to protect against toxic arbitrage and market manipulation.
///
/// Formula: `|actual - expected| * 10_000 / expected`
///
/// Both values must be in the same decimal precision before calling.
///
/// # Arguments
/// * `expected_rate` - The expected or reference exchange rate
/// * `actual_rate` - The actual computed exchange rate
///
/// # Returns
/// The absolute deviation in basis points, or an error if `expected_rate` is zero.
///
/// # Errors
/// - `Error::DeviationConsensusZero` — when `expected_rate` is zero (divide-by-zero guard).
/// - `Error::PriceMathOverflow` — on arithmetic overflow.
///
/// # Examples
/// ```text
/// calculate_rate_deviation_bps(10_000, 10_100) => Ok(100)   // 1% deviation
/// calculate_rate_deviation_bps(10_000, 9_500)  => Ok(500)   // 5% deviation
/// calculate_rate_deviation_bps(10_000, 10_000) => Ok(0)     // no deviation
/// calculate_rate_deviation_bps(0, 10_000)      => Err(DeviationConsensusZero)
/// ```
pub fn calculate_rate_deviation_bps(expected_rate: i128, actual_rate: i128) -> Result<u32, Error> {
    if expected_rate == 0 {
        return Err(Error::DeviationConsensusZero);
    }
    
    let diff = if actual_rate >= expected_rate {
        actual_rate - expected_rate
    } else {
        expected_rate - actual_rate
    };
    
    // diff * 10_000 / expected_rate
    let bps = match diff.checked_mul(10_000) {
        Some(v) => v.checked_div(expected_rate).ok_or(Error::PriceMathOverflow)?,
        None => i128::MAX,
    };
    
    Ok(bps.min(u32::MAX as i128) as u32)
}

/// Enforce slippage tolerance for cross-currency conversions.
///
/// This function validates that the deviation between expected and actual rates
/// does not exceed the user-specified slippage tolerance. It immediately terminates
/// execution with `Error::SlippageToleranceExceeded` if the threshold is breached.
///
/// # Arguments
/// * `expected_rate` - The expected exchange rate for the conversion
/// * `actual_rate` - The actual computed exchange rate
/// * `max_slippage_bps` - Maximum allowed deviation in basis points (e.g., 50 = 0.5%)
///
/// # Returns
/// `Ok(())` if the rate is within tolerance, or an error if validation fails.
///
/// # Errors
/// - `Error::SlippageToleranceExceeded` — when actual deviation exceeds `max_slippage_bps`
/// - `Error::InvalidSlippageTolerance` — when `max_slippage_bps` is out of valid range
/// - `Error::DeviationConsensusZero` — when `expected_rate` is zero
/// - `Error::PriceMathOverflow` — on arithmetic overflow
///
/// # Examples
/// ```text
/// // 1% deviation with 2% tolerance → OK
/// enforce_slippage_tolerance(10_000, 10_100, 200) => Ok(())
///
/// // 5% deviation with 2% tolerance → Error
/// enforce_slippage_tolerance(10_000, 10_500, 200) => Err(SlippageToleranceExceeded)
///
/// // Invalid tolerance (>100%)
/// enforce_slippage_tolerance(10_000, 10_100, 15_000) => Err(InvalidSlippageTolerance)
/// ```
pub fn enforce_slippage_tolerance(
    expected_rate: i128,
    actual_rate: i128,
    max_slippage_bps: u32,
) -> Result<(), Error> {
    // Validate the slippage tolerance parameter
    validate_slippage_tolerance(max_slippage_bps)?;
    
    // Calculate actual deviation
    let actual_deviation_bps = calculate_rate_deviation_bps(expected_rate, actual_rate)?;
    
    // Check if deviation exceeds tolerance
    if actual_deviation_bps > max_slippage_bps {
        return Err(Error::SlippageToleranceExceeded);
    }
    
    Ok(())
}

/// Calculate the minimum acceptable rate given an expected rate and slippage tolerance.
///
/// This helper computes the lower bound for rate validation, useful for setting
/// up conversion boundaries before executing a trade.
///
/// Formula: `expected_rate * (10_000 - slippage_bps) / 10_000`
///
/// # Arguments
/// * `expected_rate` - The expected exchange rate
/// * `slippage_bps` - Maximum allowed slippage in basis points
///
/// # Returns
/// The minimum acceptable rate, or an error on overflow.
///
/// # Examples
/// ```text
/// calculate_min_acceptable_rate(10_000, 200) => Ok(9_800)  // 2% slippage
/// calculate_min_acceptable_rate(10_000, 500) => Ok(9_500)  // 5% slippage
/// ```
pub fn calculate_min_acceptable_rate(
    expected_rate: i128,
    slippage_bps: u32,
) -> Result<i128, Error> {
    validate_slippage_tolerance(slippage_bps)?;
    
    let slippage_multiplier = 10_000_i128
        .checked_sub(slippage_bps as i128)
        .ok_or(Error::PriceMathOverflow)?;
    
    let min_rate = expected_rate
        .checked_mul(slippage_multiplier)
        .ok_or(Error::PriceMathOverflow)?
        .checked_div(10_000)
        .ok_or(Error::PriceMathOverflow)?;
    
    Ok(min_rate)
}

/// Calculate the maximum acceptable rate given an expected rate and slippage tolerance.
///
/// This helper computes the upper bound for rate validation.
///
/// Formula: `expected_rate * (10_000 + slippage_bps) / 10_000`
///
/// # Arguments
/// * `expected_rate` - The expected exchange rate
/// * `slippage_bps` - Maximum allowed slippage in basis points
///
/// # Returns
/// The maximum acceptable rate, or an error on overflow.
///
/// # Examples
/// ```text
/// calculate_max_acceptable_rate(10_000, 200) => Ok(10_200)  // 2% slippage
/// calculate_max_acceptable_rate(10_000, 500) => Ok(10_500)  // 5% slippage
/// ```
pub fn calculate_max_acceptable_rate(
    expected_rate: i128,
    slippage_bps: u32,
) -> Result<i128, Error> {
    validate_slippage_tolerance(slippage_bps)?;
    
    let slippage_multiplier = 10_000_i128
        .checked_add(slippage_bps as i128)
        .ok_or(Error::PriceMathOverflow)?;
    
    let max_rate = expected_rate
        .checked_mul(slippage_multiplier)
        .ok_or(Error::PriceMathOverflow)?
        .checked_div(10_000)
        .ok_or(Error::PriceMathOverflow)?;
    
    Ok(max_rate)
}

#[cfg(test)]
mod tests {
    extern crate alloc;

    use super::*;
    use alloc::string::ToString;
    use soroban_sdk::Env;

    // --- format_price tests ---------------------------------------------------
    // NOTE: commented out because format_price itself is commented out pending
    // a decision on whether to re-enable the formatted string output feature.

    // #[test]
    // fn test_format_price_normal() {
    //     let env = Env::default();
    //     // 75050 with 2 decimals → "750.50"
    //     let s = format_price(&env, 75050, 2);
    //     assert_eq!(s.to_string(), "750.50");
    // }

    // #[test]
    // fn test_format_price_small_value() {
    //     let env = Env::default();
    //     // 50 with 3 decimals → "0.050"
    //     let s = format_price(&env, 50, 3);
    //     assert_eq!(s.to_string(), "0.050");
    // }

    // #[test]
    // fn test_format_price_no_decimals() {
    //     let env = Env::default();
    //     // 12345 with 0 decimals → "12345"
    //     let s = format_price(&env, 12345, 0);
    //     assert_eq!(s.to_string(), "12345");
    // }

    // #[test]
    // fn test_format_price_zero() {
    //     let env = Env::default();
    //     // 0 with 2 decimals → "0.00"
    //     let s = format_price(&env, 0, 2);
    //     assert_eq!(s.to_string(), "0.00");
    // }

    // #[test]
    // fn test_format_price_exact_decimal_boundary() {
    //     let env = Env::default();
    //     // 1 with 1 decimal → "0.1"
    //     let s = format_price(&env, 1, 1);
    //     assert_eq!(s.to_string(), "0.1");
    // }

    // #[test]
    // fn test_format_price_negative() {
    //     let env = Env::default();
    //     // -75050 with 2 decimals → "-750.50"
    //     let s = format_price(&env, -75050, 2);
    //     assert_eq!(s.to_string(), "-750.50");
    // }

    // --- calculate_deviation_bps tests ----------------------------------------

    #[test]
    fn test_deviation_bps_identical() {
        assert_eq!(calculate_deviation_bps(10_000, 10_000), Ok(0));
    }

    #[test]
    fn test_deviation_bps_above_consensus() {
        // 10_100 vs 10_000 → 100 bps (1 %)
        assert_eq!(calculate_deviation_bps(10_100, 10_000), Ok(100));
    }

    #[test]
    fn test_deviation_bps_below_consensus() {
        // 9_800 vs 10_000 → 200 bps (2 %)
        assert_eq!(calculate_deviation_bps(9_800, 10_000), Ok(200));
    }

    #[test]
    fn test_deviation_bps_zero_consensus() {
        assert_eq!(
            calculate_deviation_bps(500, 0),
            Err(Error::DeviationConsensusZero)
        );
    }

    #[test]
    fn test_deviation_bps_extreme_saturates_to_u32_max() {
        let result = calculate_deviation_bps(i128::MAX, 1);
        assert_eq!(result, Ok(u32::MAX));
    }

    // --- geometric_mean tests ------------------------------------------------

    #[test]
    fn test_geometric_mean_equal_values() {
        // sqrt(100 * 100) = 100
        assert_eq!(geometric_mean(100, 100), Some(100));
    }

    #[test]
    fn test_geometric_mean_different_values() {
        // sqrt(4 * 9) = sqrt(36) = 6
        assert_eq!(geometric_mean(4, 9), Some(6));
    }

    #[test]
    fn test_geometric_mean_zero() {
        assert_eq!(geometric_mean(0, 1000), Some(0));
        assert_eq!(geometric_mean(1000, 0), Some(0));
    }

    #[test]
    fn test_geometric_mean_negative_returns_none() {
        assert_eq!(geometric_mean(-1, 100), None);
        assert_eq!(geometric_mean(100, -1), None);
    }

    #[test]
    fn test_geometric_mean_scaled_prices() {
        // sqrt(1_000_000 * 4_000_000) = sqrt(4_000_000_000_000) = 2_000_000
        assert_eq!(geometric_mean(1_000_000, 4_000_000), Some(2_000_000));
    }


    #[test]
    fn test_normalize_to_seven_scale_up() {
        assert_eq!(normalize_to_seven(150, 2), Ok(15_000_000));
    }

    #[test]
    fn test_normalize_to_seven_scale_down() {
        assert_eq!(normalize_to_seven(100_000_000, 9), Ok(1_000_000));
    }

    #[test]
    fn test_normalize_to_seven_no_scale() {
        assert_eq!(normalize_to_seven(1234567, 7), Ok(1234567));
    }

    // --- normalize_to_nine tests ---------------------------------------------

    #[test]
    fn test_normalize_to_nine_scale_up_from_7() {
        // XLM has 7 decimals: multiply by 10^2
        assert_eq!(normalize_to_nine(10_000_000, 7), Ok(1_000_000_000));
    }

    #[test]
    fn test_normalize_to_nine_scale_up_from_2() {
        // NGN has 2 decimals: multiply by 10^7
        assert_eq!(normalize_to_nine(100, 2), Ok(10_000_000_000));
    }

    #[test]
    fn test_normalize_to_nine_no_scale() {
        // Already 9 decimals — no-op
        assert_eq!(normalize_to_nine(1_000_000_000, 9), Ok(1_000_000_000));
    }

    #[test]
    fn test_normalize_to_nine_scale_down() {
        // 11 decimals → divide by 10^2
        assert_eq!(normalize_to_nine(100_000_000_000, 11), Ok(1_000_000_000));
    }

    #[test]
    fn test_normalize_to_nine_zero_decimals() {
        // 0 native decimals → multiply by 10^9
        assert_eq!(normalize_to_nine(1, 0), Ok(1_000_000_000));
    }

    // --- Overflow protection tests for multi-hop calculations -----------------

    #[test]
    fn test_normalize_to_nine_extreme_value_rejection() {
        // Extreme values should be trapped early to prevent overflow
        assert_eq!(
            normalize_to_nine(i128::MAX, 0),
            Err(Error::PriceMathOverflow)
        );
        assert_eq!(
            normalize_to_nine(i128::MIN, 0),
            Err(Error::PriceMathOverflow)
        );
    }

    #[test]
    fn test_normalize_to_nine_large_safe_value() {
        // Large but safe values should still work
        let large_value = 1_000_000_000_000_000_i128; // 10^15
        let result = normalize_to_nine(large_value, 9);
        assert!(result.is_ok());
    }

    #[test]
    fn test_normalize_to_seven_extreme_value_rejection() {
        // Extreme values should be trapped early
        assert_eq!(
            normalize_to_seven(i128::MAX, 0),
            Err(Error::PriceMathOverflow)
        );
        assert_eq!(
            normalize_to_seven(i128::MIN, 0),
            Err(Error::PriceMathOverflow)
        );
    }

    #[test]
    fn test_calculate_inverse_price_extreme_values() {
        // Extreme values should return None to prevent overflow
        assert_eq!(calculate_inverse_price(i128::MAX, 9), None);
        assert_eq!(calculate_inverse_price(i128::MIN, 9), None);
    }

    #[test]
    fn test_calculate_inverse_price_safe_values() {
        // Normal operation with safe values
        assert_eq!(calculate_inverse_price(2_000, 3), Some(500_000));
        assert_eq!(calculate_inverse_price(1_000_000_000, 9), Some(1_000_000_000));
    }

    #[test]
    fn test_multi_hop_simulation_no_overflow() {
        // Simulate a multi-hop path: Asset A -> B -> C
        // Each hop normalizes and calculates, ensuring no overflow
        let asset_a_price = 1_000_000_000; // 9 decimals
        let asset_b_price = 2_000_000_000; // 9 decimals
        
        // First hop: A to B
        let hop1 = normalize_to_nine(asset_a_price, 9);
        assert!(hop1.is_ok());
        
        // Second hop: B to C (via inverse)
        let inverse_b = calculate_inverse_price(asset_b_price, 9);
        assert!(inverse_b.is_some());
        
        // The chain should complete without overflow
        assert_eq!(hop1.unwrap(), 1_000_000_000);
        assert_eq!(inverse_b.unwrap(), 500_000);
    }

    // --- Slippage tolerance validation tests ----------------------------------

    #[test]
    fn test_validate_slippage_tolerance_valid_values() {
        assert_eq!(validate_slippage_tolerance(0), Ok(()));
        assert_eq!(validate_slippage_tolerance(50), Ok(()));
        assert_eq!(validate_slippage_tolerance(100), Ok(()));
        assert_eq!(validate_slippage_tolerance(500), Ok(()));
        assert_eq!(validate_slippage_tolerance(1_000), Ok(()));
        assert_eq!(validate_slippage_tolerance(10_000), Ok(()));
    }

    #[test]
    fn test_validate_slippage_tolerance_invalid_values() {
        assert_eq!(
            validate_slippage_tolerance(10_001),
            Err(Error::InvalidSlippageTolerance)
        );
        assert_eq!(
            validate_slippage_tolerance(50_000),
            Err(Error::InvalidSlippageTolerance)
        );
        assert_eq!(
            validate_slippage_tolerance(u32::MAX),
            Err(Error::InvalidSlippageTolerance)
        );
    }

    // --- Rate deviation calculation tests -------------------------------------

    #[test]
    fn test_calculate_rate_deviation_bps_no_deviation() {
        assert_eq!(calculate_rate_deviation_bps(10_000, 10_000), Ok(0));
    }

    #[test]
    fn test_calculate_rate_deviation_bps_positive_deviation() {
        // 1% deviation
        assert_eq!(calculate_rate_deviation_bps(10_000, 10_100), Ok(100));
        // 5% deviation
        assert_eq!(calculate_rate_deviation_bps(10_000, 10_500), Ok(500));
        // 10% deviation
        assert_eq!(calculate_rate_deviation_bps(10_000, 11_000), Ok(1_000));
    }

    #[test]
    fn test_calculate_rate_deviation_bps_negative_deviation() {
        // -1% deviation (absolute value)
        assert_eq!(calculate_rate_deviation_bps(10_000, 9_900), Ok(100));
        // -5% deviation (absolute value)
        assert_eq!(calculate_rate_deviation_bps(10_000, 9_500), Ok(500));
        // -10% deviation (absolute value)
        assert_eq!(calculate_rate_deviation_bps(10_000, 9_000), Ok(1_000));
    }

    #[test]
    fn test_calculate_rate_deviation_bps_zero_expected() {
        assert_eq!(
            calculate_rate_deviation_bps(0, 10_000),
            Err(Error::DeviationConsensusZero)
        );
    }

    #[test]
    fn test_calculate_rate_deviation_bps_extreme_deviation() {
        // Very large deviation should saturate to u32::MAX
        let result = calculate_rate_deviation_bps(1, i128::MAX);
        assert_eq!(result, Ok(u32::MAX));
    }

    // --- Slippage enforcement tests --------------------------------------------

    #[test]
    fn test_enforce_slippage_tolerance_within_bounds() {
        // 1% deviation with 2% tolerance
        assert_eq!(enforce_slippage_tolerance(10_000, 10_100, 200), Ok(()));
        // 0.5% deviation with 1% tolerance
        assert_eq!(enforce_slippage_tolerance(10_000, 10_050, 100), Ok(()));
        // Exact boundary case
        assert_eq!(enforce_slippage_tolerance(10_000, 10_500, 500), Ok(()));
    }

    #[test]
    fn test_enforce_slippage_tolerance_exceeds_bounds() {
        // 5% deviation with 2% tolerance
        assert_eq!(
            enforce_slippage_tolerance(10_000, 10_500, 200),
            Err(Error::SlippageToleranceExceeded)
        );
        // 10% deviation with 5% tolerance
        assert_eq!(
            enforce_slippage_tolerance(10_000, 11_000, 500),
            Err(Error::SlippageToleranceExceeded)
        );
        // Negative deviation exceeds tolerance
        assert_eq!(
            enforce_slippage_tolerance(10_000, 9_000, 500),
            Err(Error::SlippageToleranceExceeded)
        );
    }

    #[test]
    fn test_enforce_slippage_tolerance_invalid_tolerance() {
        assert_eq!(
            enforce_slippage_tolerance(10_000, 10_100, 15_000),
            Err(Error::InvalidSlippageTolerance)
        );
    }

    #[test]
    fn test_enforce_slippage_tolerance_zero_expected() {
        assert_eq!(
            enforce_slippage_tolerance(0, 10_000, 500),
            Err(Error::DeviationConsensusZero)
        );
    }

    // --- Min/Max acceptable rate calculation tests -----------------------------

    #[test]
    fn test_calculate_min_acceptable_rate() {
        // 2% slippage: 10_000 * (10_000 - 200) / 10_000 = 9_800
        assert_eq!(calculate_min_acceptable_rate(10_000, 200), Ok(9_800));
        // 5% slippage
        assert_eq!(calculate_min_acceptable_rate(10_000, 500), Ok(9_500));
        // 10% slippage
        assert_eq!(calculate_min_acceptable_rate(10_000, 1_000), Ok(9_000));
        // 0% slippage
        assert_eq!(calculate_min_acceptable_rate(10_000, 0), Ok(10_000));
        // 100% slippage
        assert_eq!(calculate_min_acceptable_rate(10_000, 10_000), Ok(0));
    }

    #[test]
    fn test_calculate_max_acceptable_rate() {
        // 2% slippage: 10_000 * (10_000 + 200) / 10_000 = 10_200
        assert_eq!(calculate_max_acceptable_rate(10_000, 200), Ok(10_200));
        // 5% slippage
        assert_eq!(calculate_max_acceptable_rate(10_000, 500), Ok(10_500));
        // 10% slippage
        assert_eq!(calculate_max_acceptable_rate(10_000, 1_000), Ok(11_000));
        // 0% slippage
        assert_eq!(calculate_max_acceptable_rate(10_000, 0), Ok(10_000));
    }

    #[test]
    fn test_calculate_min_max_rate_invalid_tolerance() {
        assert_eq!(
            calculate_min_acceptable_rate(10_000, 15_000),
            Err(Error::InvalidSlippageTolerance)
        );
        assert_eq!(
            calculate_max_acceptable_rate(10_000, 15_000),
            Err(Error::InvalidSlippageTolerance)
        );
    }

    // --- Cross-currency conversion scenario tests ------------------------------

    #[test]
    fn test_cross_currency_conversion_with_slippage_protection() {
        // Scenario: Converting NGN → GHS through XLM corridor
        // Expected rate: 1 NGN = 0.05 GHS (50 basis points)
        let expected_rate = 500_000; // 0.05 in 7 decimals
        let slippage_tolerance_bps = 200; // 2%

        // Calculate acceptable bounds
        let min_rate = calculate_min_acceptable_rate(expected_rate, slippage_tolerance_bps)
            .unwrap();
        let max_rate = calculate_max_acceptable_rate(expected_rate, slippage_tolerance_bps)
            .unwrap();

        // min_rate = 500_000 * 9_800 / 10_000 = 490_000
        assert_eq!(min_rate, 490_000);
        // max_rate = 500_000 * 10_200 / 10_000 = 510_000
        assert_eq!(max_rate, 510_000);

        // Valid conversion within bounds
        let actual_rate_1 = 495_000; // 1% below expected
        assert_eq!(
            enforce_slippage_tolerance(expected_rate, actual_rate_1, slippage_tolerance_bps),
            Ok(())
        );

        let actual_rate_2 = 505_000; // 1% above expected
        assert_eq!(
            enforce_slippage_tolerance(expected_rate, actual_rate_2, slippage_tolerance_bps),
            Ok(())
        );

        // Invalid conversion outside bounds
        let toxic_rate_low = 480_000; // 4% below expected
        assert_eq!(
            enforce_slippage_tolerance(expected_rate, toxic_rate_low, slippage_tolerance_bps),
            Err(Error::SlippageToleranceExceeded)
        );

        let toxic_rate_high = 520_000; // 4% above expected
        assert_eq!(
            enforce_slippage_tolerance(expected_rate, toxic_rate_high, slippage_tolerance_bps),
            Err(Error::SlippageToleranceExceeded)
        );
    }

    #[test]
    fn test_volatile_market_corridor_protection() {
        // High-volatility corridor requires tighter slippage bounds
        let expected_rate = 1_000_000_000; // 1:1 conversion in 9 decimals
        let tight_slippage = 50; // 0.5% for volatile markets

        // Just within tolerance
        let borderline_rate = 1_005_000_000; // 0.5% above
        assert_eq!(
            enforce_slippage_tolerance(expected_rate, borderline_rate, tight_slippage),
            Ok(())
        );

        // Just outside tolerance
        let excessive_rate = 1_006_000_000; // 0.6% above
        assert_eq!(
            enforce_slippage_tolerance(expected_rate, excessive_rate, tight_slippage),
            Err(Error::SlippageToleranceExceeded)
        );
    }

    #[test]
    fn test_slippage_with_large_values() {
        // Test with realistic large values (e.g., high-value conversions)
        let large_expected = 1_000_000_000_000; // Large amount
        let slippage_bps = 100; // 1%

        let min = calculate_min_acceptable_rate(large_expected, slippage_bps).unwrap();
        let max = calculate_max_acceptable_rate(large_expected, slippage_bps).unwrap();

        assert_eq!(min, 990_000_000_000);
        assert_eq!(max, 1_010_000_000_000);

        // Verify enforcement works with large values
        assert_eq!(
            enforce_slippage_tolerance(large_expected, 995_000_000_000, slippage_bps),
            Ok(())
        );
    }

    #[test]
    fn test_slippage_with_small_values() {
        // Test with small values (e.g., micro-transactions)
        let small_expected = 100; // Small amount
        let slippage_bps = 500; // 5%

        let min = calculate_min_acceptable_rate(small_expected, slippage_bps).unwrap();
        let max = calculate_max_acceptable_rate(small_expected, slippage_bps).unwrap();

        assert_eq!(min, 95);
        assert_eq!(max, 105);

        // Verify enforcement works with small values
        assert_eq!(
            enforce_slippage_tolerance(small_expected, 96, slippage_bps),
            Ok(())
        );
        assert_eq!(
            enforce_slippage_tolerance(small_expected, 90, slippage_bps),
            Err(Error::SlippageToleranceExceeded)
        );
    }
}

