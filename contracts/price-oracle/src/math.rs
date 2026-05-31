use soroban_sdk::{Env, String};

use crate::Error;

const INTERNAL_SCALE_9: i128 = 1_000_000_000; // 10^9
const LEDGER_SCALE_7: i128 = 10_000_000; // 10^7

/// Divide two 7-decimal fixed-point values using a 9-decimal internal scale.
///
/// This implements issue #346's precision workaround:
/// - upscale by 10^9 before the division
/// - downscale precisely back to 10^7 (with rounding) for ledger storage/return
///
/// Concretely:
/// `result_7 = round( (numerator_7 / denominator_7) * 10^7 )`
///
/// # Notes
/// - `numerator_7` and `denominator_7` are assumed to be scaled by 10^7.
/// - Returns `Error::PriceMathOverflow` on overflow or divide-by-zero.
pub fn div_7_by_7_to_7(numerator_7: i128, denominator_7: i128) -> Result<i128, Error> {
    if denominator_7 == 0 {
        return Err(Error::PriceMathOverflow);
    }

    // numerator_7 / denominator_7 yields a dimensionless ratio.
    // To return 7-decimal fixed-point, multiply by 10^7.
    //
    // We keep extra precision by using an internal 9-decimal multiplier first:
    // ratio_scaled_9 = numerator_7 * 10^9 / denominator_7
    // result_7 = round( ratio_scaled_9 / 10^2 )
    //
    // (10^9 -> 10^7 is a /100 downscale).
    let ratio_scaled_9 = numerator_7
        .checked_mul(INTERNAL_SCALE_9)
        .ok_or(Error::PriceMathOverflow)?
        .checked_div(denominator_7)
        .ok_or(Error::PriceMathOverflow)?;

    round_divide_by_100(ratio_scaled_9)
}

/// Convert a 9-decimal fixed-point value to 7 decimals with rounding-to-nearest
/// (ties away from zero).
pub fn downscale_9_to_7_round(value_9: i128) -> Result<i128, Error> {
    if value_9 == 0 {
        return Ok(0);
    }
    round_divide_by_100(value_9)
}

fn round_divide_by_100(value: i128) -> Result<i128, Error> {
    const DIVISOR: i128 = INTERNAL_SCALE_9 / LEDGER_SCALE_7; // 100
    const HALF: i128 = DIVISOR / 2; // 50

    let adjusted = if value >= 0 {
        value.checked_add(HALF).ok_or(Error::PriceMathOverflow)?
    } else {
        value.checked_sub(HALF).ok_or(Error::PriceMathOverflow)?
    };

    adjusted.checked_div(DIVISOR).ok_or(Error::PriceMathOverflow)
}

pub fn format_price(env: &Env, price: i128, decimals: u32) -> String {
    const BUF: usize = 42;
    let mut digits = [0u8; BUF];
    let mut len = 0usize;
    let negative = price < 0;
    let mut remaining: u128 = if negative {
        (price as i128).unsigned_abs()
    } else {
        price as u128
    };

    if remaining == 0 {
        digits[BUF - 1] = b'0';
        len = 1;
    } else {
        while remaining > 0 {
            len += 1;
            digits[BUF - len] = b'0' + (remaining % 10) as u8;
            remaining /= 10;
        }
    }

    let mut out = [0u8; 41];
    let mut pos = 0usize;
    let decimals = decimals as usize;

    if negative {
        out[pos] = b'-';
        pos += 1;
    }

    if decimals == 0 {
        let src = &digits[BUF - len..BUF];
        out[pos..pos + len].copy_from_slice(src);
        pos += len;
    } else if len <= decimals {
        out[pos] = b'0';
        pos += 1;
        out[pos] = b'.';
        pos += 1;

        let leading_zeros = decimals - len;
        for _ in 0..leading_zeros {
            out[pos] = b'0';
            pos += 1;
        }

        let src = &digits[BUF - len..BUF];
        out[pos..pos + len].copy_from_slice(src);
        pos += len;
    } else {
        let int_len = len - decimals;
        let src = &digits[BUF - len..BUF];

        out[pos..pos + int_len].copy_from_slice(&src[..int_len]);
        pos += int_len;

        out[pos] = b'.';
        pos += 1;

        out[pos..pos + decimals].copy_from_slice(&src[int_len..]);
        pos += decimals;
    }

    String::from_bytes(env, &out[..pos])
}

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

pub fn normalize_to_seven(value: i128, input_decimals: u32) -> Result<i128, Error> {
    if input_decimals < 7 {
        let diff = 7 - input_decimals;
        let multiplier = 10_i128.checked_pow(diff).ok_or(Error::PriceMathOverflow)?;
        value.checked_mul(multiplier).ok_or(Error::PriceMathOverflow)
    } else if input_decimals > 7 {
        let diff = input_decimals - 7;
        let divisor = 10_i128.checked_pow(diff).ok_or(Error::PriceMathOverflow)?;
        // When scaling down, round to the nearest representable 7-decimal value.
        // This avoids truncating small fractional values to zero (issue #346).
        let half = divisor / 2;
        let adjusted = if value >= 0 {
            value.checked_add(half).ok_or(Error::PriceMathOverflow)?
        } else {
            value.checked_sub(half).ok_or(Error::PriceMathOverflow)?
        };
        adjusted.checked_div(divisor).ok_or(Error::PriceMathOverflow)
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
/// # Examples
/// ```text
/// normalize_to_nine(1_000_000_0, 7)  => 1_000_000_000  (XLM, 7 dec → 9 dec)
/// normalize_to_nine(100,         2)  => 10_000_000_000  (NGN, 2 dec → 9 dec)
/// normalize_to_nine(1_000_000_000, 9) => 1_000_000_000  (already 9 dec, no-op)
/// normalize_to_nine(1_000_000_000_00, 11) => 1_000_000_000 (scale down)
/// ```
pub fn normalize_to_nine(value: i128, native_decimals: u32) -> Result<i128, Error> {
    const TARGET: u32 = 9;
    if native_decimals < TARGET {
        let diff = TARGET - native_decimals;
        let multiplier = 10_i128.checked_pow(diff).ok_or(Error::PriceMathOverflow)?;
        value.checked_mul(multiplier).ok_or(Error::PriceMathOverflow)
    } else if native_decimals > TARGET {
        let diff = native_decimals - TARGET;
        let divisor = 10_i128.checked_pow(diff).ok_or(Error::PriceMathOverflow)?;
        value.checked_div(divisor).ok_or(Error::PriceMathOverflow)
    } else {
        Ok(value)
    }
}

/// Calculate the inverse of a price (e.g., NGN/XLM → XLM/NGN).
///
/// Uses a fixed-point scale factor of `10^decimals` so that the result
/// preserves the same decimal precision as the input.
///
/// Formula: `(10^decimals * 10^decimals) / price`
///
/// # Returns
/// `Some(inverse)` on success, or `None` when `price` is zero (divide-by-zero).
///
/// # Examples
/// ```text
/// calculate_inverse_price(2_000, 3)  => Some(500_000)   // 1/2.000 = 0.500 (scaled)
/// calculate_inverse_price(0,     7)  => None             // divide-by-zero guard
/// ```
pub fn calculate_inverse_price(price: i128, decimals: u32) -> Option<i128> {
    if price == 0 {
        return None;
    }
    let scale = 10_i128.checked_pow(decimals)?;
    let numerator = scale.checked_mul(scale)?;
    numerator.checked_div(price)
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate alloc;
    use alloc::string::ToString;
    use soroban_sdk::Env;

    // --- format_price tests ---------------------------------------------------

    #[test]
    fn test_format_price_normal() {
        let env = Env::default();
        // 75050 with 2 decimals → "750.50"
        let s = format_price(&env, 75050, 2);
        assert_eq!(s.to_string(), "750.50");
    }

    #[test]
    fn test_format_price_small_value() {
        let env = Env::default();
        // 50 with 3 decimals → "0.050"
        let s = format_price(&env, 50, 3);
        assert_eq!(s.to_string(), "0.050");
    }

    #[test]
    fn test_format_price_no_decimals() {
        let env = Env::default();
        // 12345 with 0 decimals → "12345"
        let s = format_price(&env, 12345, 0);
        assert_eq!(s.to_string(), "12345");
    }

    #[test]
    fn test_format_price_zero() {
        let env = Env::default();
        // 0 with 2 decimals → "0.00"
        let s = format_price(&env, 0, 2);
        assert_eq!(s.to_string(), "0.00");
    }

    #[test]
    fn test_format_price_exact_decimal_boundary() {
        let env = Env::default();
        // 1 with 1 decimal → "0.1"
        let s = format_price(&env, 1, 1);
        assert_eq!(s.to_string(), "0.1");
    }

    #[test]
    fn test_format_price_negative() {
        let env = Env::default();
        // -75050 with 2 decimals → "-750.50"
        let s = format_price(&env, -75050, 2);
        assert_eq!(s.to_string(), "-750.50");
    }

    // --- #346 precision helpers -----------------------------------------------

    #[test]
    fn test_downscale_9_to_7_round_small_nonzero() {
        // 0.000000015 (9dp) should round up to 0.0000000? (7dp) => 0
        assert_eq!(downscale_9_to_7_round(15), Ok(0));

        // 0.000000050 (9dp) is exactly half of 1e-7; ties round away from zero.
        assert_eq!(downscale_9_to_7_round(50), Ok(1));
        assert_eq!(downscale_9_to_7_round(-50), Ok(-1));
    }

    #[test]
    fn test_div_7_by_7_to_7_low_value_ratio() {
        // (1 stroop / 3 stroops) in 7dp should be ~0.3333333 (rounded)
        let one = 1_i128;
        let three = 3_i128;
        assert_eq!(div_7_by_7_to_7(one, three), Ok(3_333_333));
    }

    // --- normalize_to_seven tests ---------------------------------------------

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
        assert_eq!(normalize_to_nine(100, 2), Ok(1_000_000_000));
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
}
