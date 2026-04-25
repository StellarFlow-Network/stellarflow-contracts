use soroban_sdk::{Env, String};
extern crate alloc;

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
pub fn format_price(env: &Env, price: i128, decimals: u32) -> String {
    // --- 1. Convert the absolute value to ASCII digits in a fixed buffer ------
    // i128::MAX is 39 digits; 1 sign + 39 digits + 1 dot + 1 NUL = 42 bytes is safe.
    const BUF: usize = 42;
    let mut digits = [0u8; BUF]; // ASCII digit buffer (filled right-to-left)
    let mut len = 0usize;

    let negative = price < 0;
    // Use u128 so we can safely negate i128::MIN without overflow.
    let mut remaining: u128 = if negative {
        (price as i128).unsigned_abs()
    } else {
        price as u128
    };

    // Edge case: price == 0
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
    // digits[BUF-len .. BUF] now holds the ASCII digits, most-significant first.

    // --- 2. Build the output byte slice into a second fixed buffer ------------
    // Max output length: 1 (sign) + 39 (digits) + 1 (dot) = 41 bytes.
    let mut out = [0u8; 41];
    let mut pos = 0usize;

    let decimals = decimals as usize;

    if negative {
        out[pos] = b'-';
        pos += 1;
    }

    if decimals == 0 {
        // No decimal point needed — copy digits straight through.
        let src = &digits[BUF - len..BUF];
        out[pos..pos + len].copy_from_slice(src);
        pos += len;
    } else if len <= decimals {
        // The integer part is zero; we need leading "0." and possibly leading
        // fractional zeros.  e.g. price=50, decimals=3 → "0.050"
        out[pos] = b'0';
        pos += 1;
        out[pos] = b'.';
        pos += 1;

        // Pad with zeros until we reach the actual digits.
        let leading_zeros = decimals - len;
        for _ in 0..leading_zeros {
            out[pos] = b'0';
            pos += 1;
        }

        let src = &digits[BUF - len..BUF];
        out[pos..pos + len].copy_from_slice(src);
        pos += len;
    } else {
        // Normal case: integer part has (len - decimals) digits.
        let int_len = len - decimals;
        let src = &digits[BUF - len..BUF];

        out[pos..pos + int_len].copy_from_slice(&src[..int_len]);
        pos += int_len;

        out[pos] = b'.';
        pos += 1;

        out[pos..pos + decimals].copy_from_slice(&src[int_len..]);
        pos += decimals;
    }

    // --- 3. Wrap in a Soroban String ------------------------------------------
<<<<<<< Aggregator
    // `from_bytes` expects a byte slice, so we pass the slice directly.
=======
    // `from_bytes` expects a byte slice, not a soroban_sdk::Bytes.
>>>>>>> main
    String::from_bytes(env, &out[..pos])
}

pub fn normalize_to_seven(value: i128, input_decimals: u32) -> i128 {
    if input_decimals < 7 {
        let diff = 7 - input_decimals;
        let multiplier = 10_i128.checked_pow(diff).expect("Overflow on multiplier pow");
        value.checked_mul(multiplier).expect("Overflow on multiplication")
    } else if input_decimals > 7 {
        let diff = input_decimals - 7;
        let divisor = 10_i128.checked_pow(diff).expect("Overflow on divisor pow");
        value.checked_div(divisor).expect("Overflow on division")
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::Env;
<<<<<<< Aggregator
    extern crate alloc;
=======
>>>>>>> main
    use alloc::string::ToString;

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

    // --- normalize_to_seven tests ---------------------------------------------

    #[test]
    fn test_normalize_to_seven_scale_up() {
        assert_eq!(normalize_to_seven(150, 2), 15_000_000);
    }

    #[test]
    fn test_normalize_to_seven_scale_down() {
        assert_eq!(normalize_to_seven(100_000_000, 9), 1_000_000);
    }

    #[test]
    fn test_normalize_to_seven_no_scale() {
        assert_eq!(normalize_to_seven(1234567, 7), 1234567);
    }
}
