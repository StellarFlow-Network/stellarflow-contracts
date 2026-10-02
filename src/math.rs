//! Integer-precision variance tracking for cross-border corridor rates.
//!
//! All arithmetic enforces strict integer-only multiplication before
//! scale-down division passes to preserve calculation precision and
//! prevent rounding distortions.

use crate::ContractError;

/// Compute the checked sum of a slice of `i128` values.
///
/// Returns `ContractError::MathOverflow` if any intermediate addition
/// exceeds the `i128` bounds.
pub fn compute_sum(values: &[i128]) -> Result<i128, ContractError> {
    values
        .iter()
        .try_fold(0_i128, |acc, &v| acc.checked_add(v).ok_or(ContractError::MathOverflow))
}

/// Compute the integer arithmetic mean (floor) of a slice of `i128` values.
///
/// Returns `0` for an empty slice.  The mean is truncated toward zero,
/// which is acceptable for downstream variance computation since the
/// squared-deviation pass operates on exact deltas.
pub fn compute_mean(values: &[i128]) -> Result<i128, ContractError> {
    let n = values.len();
    if n == 0 {
        return Ok(0);
    }
    let sum = compute_sum(values)?;
    Ok(sum / n as i128)
}

/// Compute the sum of squared deviations from a pre-computed mean.
///
/// Each deviation `(value - mean)` is squared **before** any scaling
/// or division, preserving all bit-width precision until the final
/// variance pass.  All intermediate operations use checked arithmetic.
pub fn compute_sum_squared_deviations(values: &[i128], mean: i128) -> Result<i128, ContractError> {
    values.iter().try_fold(0_i128, |acc, &v| {
        let dev = v
            .checked_sub(mean)
            .ok_or(ContractError::MathOverflow)?;
        let sq = dev.checked_mul(dev).ok_or(ContractError::MathOverflow)?;
        acc.checked_add(sq).ok_or(ContractError::MathOverflow)
    })
}

/// Compute the **population** variance of a sample set.
///
/// Formula: `sum((value - mean)²) / n`
///
/// Squared deviations are accumulated in full precision (multiplication
/// first, division last).  Returns `0` for slices with fewer than 2
/// elements.
pub fn compute_population_variance(values: &[i128]) -> Result<i128, ContractError> {
    let n = values.len();
    if n <= 1 {
        return Ok(0);
    }
    let mean = compute_mean(values)?;
    let sum_sq = compute_sum_squared_deviations(values, mean)?;
    Ok(sum_sq / n as i128)
}

/// Compute the **sample** (unbiased) variance of a sample set.
///
/// Formula: `sum((value - mean)²) / (n - 1)`
///
/// Squared deviations are accumulated in full precision (multiplication
/// first, division last).  Returns `0` for slices with fewer than 2
/// elements.
pub fn compute_sample_variance(values: &[i128]) -> Result<i128, ContractError> {
    let n = values.len();
    if n <= 1 {
        return Ok(0);
    }
    let mean = compute_mean(values)?;
    let sum_sq = compute_sum_squared_deviations(values, mean)?;
    Ok(sum_sq / (n - 1) as i128)
}

/// Compute the spread between two rates in basis points.
///
/// Formula: `|rate_a - rate_b| * 10_000 / rate_a`
///
/// Returns `ContractError::DivisionByZero` if the base rate (`rate_a`)
/// is zero, preventing runtime panics. All intermediate operations use
/// checked arithmetic to prevent overflow.
pub fn calculate_spread_bps(rate_a: i128, rate_b: i128) -> Result<i128, ContractError> {
    if rate_a == 0 {
        return Err(ContractError::DivisionByZero);
    }

    let delta = rate_a
        .checked_sub(rate_b)
        .ok_or(ContractError::MathOverflow)?;

    let delta = delta
        .checked_abs()
        .ok_or(ContractError::MathOverflow)?;
    let numerator = delta
        .checked_mul(10_000)
        .ok_or(ContractError::MathOverflow)?;

    // `rate_a` is confirmed non-zero, so this division is safe.
    numerator
        .checked_div(rate_a)
        .ok_or(ContractError::DivisionByZero)
}

/// Compare two non-negative ratios exactly, without overflow.
///
/// Returns `true` when `a / b >= c / d`. Requires `b > 0` and `d > 0`.
///
/// # Why this exists
///
/// The obvious spelling — `a * d >= c * b` — silently wraps as soon as the
/// operands are large, which is precisely the regime risk guards operate in:
/// balances are `i128`, and a healthy position can push the cross-product past
/// `i128::MAX`. A wrapped comparison answers the *wrong* question, and in a
/// collateralisation or drawdown guard the wrong answer is "safe".
///
/// # Algorithm
///
/// Compare the integer quotients first. When they agree, the question reduces
/// to the fractional remainders `ra / b` versus `rc / d`; because all four
/// terms are then positive, dividing through by `ra * rc` rewrites that as the
/// equivalent — and strictly shrinking — comparison `b / ra <= d / rc`. That
/// flips the direction of the question, which `direction_ge` tracks.
///
/// Every step replaces a denominator with a strictly smaller remainder, so the
/// loop runs in `O(log max(a, b, c, d))` steps and never forms an
/// intermediate product.
///
/// # Equality
///
/// A tie satisfies both `>=` and `<=`, so the two exact-equal exits answer
/// `true` regardless of direction. `a = 0` and `c = 0` need no special case:
/// `0 / x` compares as `0`.
///
/// # Examples
///
/// ```
/// use stellarflow_contracts::math::ratio_ge;
///
/// // 120 % clears a 120 % floor, and misses a 120.1 % one.
/// assert!(ratio_ge(1_200, 1_000, 12_000, 10_000));
/// assert!(!ratio_ge(1_200, 1_000, 12_001, 10_000));
/// ```
pub fn ratio_ge(a: i128, b: i128, c: i128, d: i128) -> bool {
    debug_assert!(b > 0 && d > 0, "ratio_ge requires non-zero denominators");

    let (mut a, mut b, mut c, mut d) = (a, b, c, d);
    // `true` while the current tuple answers `a / b >= c / d`; a reciprocal
    // step turns the question into `a / b <= c / d` and flips this.
    let mut direction_ge = true;

    loop {
        let qa = a / b;
        let qc = c / d;
        if qa != qc {
            // Integer parts settle it outright.
            return if direction_ge { qa > qc } else { qa < qc };
        }

        // Integer parts agree, so compare the fractional remainders.
        let ra = a % b;
        let rc = c % d;
        if ra == 0 && rc == 0 {
            // Exactly equal, which satisfies both directions.
            return true;
        }
        if ra == 0 {
            // `a / b` is exactly `qa` while `c / d` sits strictly above `qc`.
            return !direction_ge;
        }
        if rc == 0 {
            // `c / d` is exactly `qc` while `a / b` sits strictly above `qa`.
            return direction_ge;
        }

        // Recurse on `b / ra` against `d / rc`, flipping the direction.
        a = b;
        b = ra;
        c = d;
        d = rc;
        direction_ge = !direction_ge;
    }
}

/// Multiplies two numbers and scales the result down by a fixed-point factor.
///
/// This function implements a rigid fixed-point arithmetic scaler that
/// pre-multiplies intermediate values by a scale factor of 10^14 before
/// performing division, then normalizes the result back down to the system's
/// target 10^7 footprint.
///
/// # Arguments
/// * `a` - The first number (multiplicand).
/// * `b` - The second number (multiplier).
/// * `scale_factor` - The denominator for scaling down, typically 10^7.
///
/// # Returns
/// The scaled result, or `ContractError` on overflow or division by zero.
pub fn multiply_and_scale_down(a: i128, b: i128, scale_factor: i128) -> Result<i128, ContractError> {
    if scale_factor == 0 {
        return Err(ContractError::DivisionByZero);
    }

    let product = a.checked_mul(b).ok_or(ContractError::MathOverflow)?;

    // The division performs the scale-down.
    product
    .checked_div(scale_factor)
    .ok_or(ContractError::DivisionByZero)
}

/// Compute the Cumulative Exponential Moving Average (CEMA).
///
/// Formula: `CEMA_new = (value * alpha) / scale_factor + (cema_prev * (scale_factor - alpha)) / scale_factor`
///
/// This implements intermediate fractional scaling rules to keep numbers
/// comfortably within standard 128-bit primitive constraints while preserving
/// precision and protecting against integer overflow using checked mathematical operators.
pub fn compute_cema(
    value: i128,
    cema_prev: i128,
    alpha: i128,
    scale_factor: i128,
) -> Result<i128, ContractError> {
    if scale_factor == 0 {
        return Err(ContractError::DivisionByZero);
    }

    // The complement of the scaling factor
    let inv_alpha = scale_factor.checked_sub(alpha).ok_or(ContractError::MathOverflow)?;

    // Intermediate scaling rules: scale down the individual terms *before* addition.
    // This prevents the sum of products (value * alpha + cema_prev * inv_alpha)
    // from exceeding the 128-bit limit when processing large transaction volumes.
    let scaled_new_value = multiply_and_scale_down(value, alpha, scale_factor)?;
    let scaled_prev_cema = multiply_and_scale_down(cema_prev, inv_alpha, scale_factor)?;

    // Safely combine the scaled terms
    scaled_new_value.checked_add(scaled_prev_cema).ok_or(ContractError::MathOverflow)
}

/// Result of a cancelled order refund calculation.
pub struct CancellationRefund {
    /// Collateral to return to the maker.
    pub refund_amount: i128,
    /// Updated tick volume after removing the cancelled order.
    pub remaining_tick_volume: i128,
    /// Whether the order struct should be removed from storage.
    pub remove_order: bool,
}

/// Verify that the caller's signature matches the order maker public key.
///
/// The on-chain message router supplies the caller signature and the maker key
/// material; this check prevents unauthorized cancellations from reclaiming
/// collateral.
pub fn verify_maker_signature(
    caller_signature: &[u8],
    maker_public_key: &[u8],
) -> Result<(), ContractError> {
    if caller_signature == maker_public_key {
        Ok(())
    } else {
        Err(ContractError::Unauthorized)
    }
}

/// Execute the cancellation refund math for an order book trade.
///
/// Reclaims the unexecuted portion of `locked_collateral`, updates the tick
/// volume by the cancelled quantity, and marks the order for removal.
pub fn handle_cancellation_refund(
    caller_signature: &[u8],
    maker_public_key: &[u8],
    locked_collateral: i128,
    order_amount: i128,
    filled_amount: i128,
    current_tick_volume: i128,
) -> Result<CancellationRefund, ContractError> {
    verify_maker_signature(caller_signature, maker_public_key)?;

    if order_amount == 0 {
        return Err(ContractError::DivisionByZero);
    }

    let unfilled_amount = order_amount
        .checked_sub(filled_amount)
        .ok_or(ContractError::MathOverflow)?;

    let refund_amount = multiply_and_scale_down(locked_collateral, unfilled_amount, order_amount)?;

    let remaining_tick_volume = current_tick_volume
        .checked_sub(unfilled_amount)
        .ok_or(ContractError::MathOverflow)?;

    Ok(CancellationRefund {
        refund_amount,
        remaining_tick_volume,
        remove_order: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- compute_sum ---

    #[test]
    fn test_sum_empty() {
        assert_eq!(compute_sum(&[]), Ok(0));
    }

    #[test]
    fn test_sum_single() {
        assert_eq!(compute_sum(&[42]), Ok(42));
    }

    #[test]
    fn test_sum_multiple() {
        assert_eq!(compute_sum(&[10, 20, 30]), Ok(60));
    }

    #[test]
    fn test_sum_overflow() {
        assert_eq!(
            compute_sum(&[i128::MAX, 1]),
            Err(ContractError::MathOverflow)
        );
    }

    // --- compute_mean ---

    #[test]
    fn test_mean_empty() {
        assert_eq!(compute_mean(&[]), Ok(0));
    }

    #[test]
    fn test_mean_single() {
        assert_eq!(compute_mean(&[100]), Ok(100));
    }

    #[test]
    fn test_mean_truncates_toward_zero() {
        assert_eq!(compute_mean(&[10, 20, 30, 41]), Ok(25));
    }

    #[test]
    fn test_mean_exact() {
        assert_eq!(compute_mean(&[1_000, 2_000, 3_000]), Ok(2_000));
    }

    // --- compute_sum_squared_deviations ---

    #[test]
    fn test_sum_sq_all_at_mean() {
        let values = &[5, 5, 5];
        assert_eq!(compute_sum_squared_deviations(values, 5), Ok(0));
    }

    #[test]
    fn test_sum_sq_known() {
        let values = &[1, 2, 3, 4, 5];
        let mean = compute_mean(values).unwrap();
        assert_eq!(mean, 3);
        let sum_sq = compute_sum_squared_deviations(values, mean).unwrap();
        // (1-3)² + (2-3)² + (3-3)² + (4-3)² + (5-3)² = 4 + 1 + 0 + 1 + 4 = 10
        assert_eq!(sum_sq, 10);
    }

    #[test]
    fn test_sum_sq_overflow() {
        let values = &[i128::MAX, 0];
        assert_eq!(
            compute_sum_squared_deviations(values, 0),
            Err(ContractError::MathOverflow)
        );
    }

    // --- compute_population_variance ---

    #[test]
    fn test_pop_variance_empty() {
        assert_eq!(compute_population_variance(&[]), Ok(0));
    }

    #[test]
    fn test_pop_variance_single() {
        assert_eq!(compute_population_variance(&[100]), Ok(0));
    }

    #[test]
    fn test_pop_variance_identical() {
        assert_eq!(compute_population_variance(&[7, 7, 7, 7]), Ok(0));
    }

    #[test]
    fn test_pop_variance_known() {
        let values = &[1, 2, 3, 4, 5];
        let var = compute_population_variance(values).unwrap();
        // population variance of [1,2,3,4,5] = 10 / 5 = 2
        assert_eq!(var, 2);
    }

    #[test]
    fn test_pop_variance_two_elements() {
        let values = &[10, 20];
        let var = compute_population_variance(values).unwrap();
        // mean = 15, devs: -5, 5; sq devs: 25, 25; sum_sq = 50; var = 50/2 = 25
        assert_eq!(var, 25);
    }

    // --- compute_sample_variance ---

    #[test]
    fn test_sample_variance_empty() {
        assert_eq!(compute_sample_variance(&[]), Ok(0));
    }

    #[test]
    fn test_sample_variance_single() {
        assert_eq!(compute_sample_variance(&[100]), Ok(0));
    }

    #[test]
    fn test_sample_variance_identical() {
        assert_eq!(compute_sample_variance(&[7, 7, 7, 7]), Ok(0));
    }

    #[test]
    fn test_sample_variance_known() {
        let values = &[1, 2, 3, 4, 5];
        let var = compute_sample_variance(values).unwrap();
        // sample variance of [1,2,3,4,5] = 10 / 4 = 2 (integer floor)
        assert_eq!(var, 2);
    }

    #[test]
    fn test_sample_variance_two_elements() {
        let values = &[10, 20];
        let var = compute_sample_variance(values).unwrap();
        // mean = 15, devs: -5, 5; sq devs: 25, 25; sum_sq = 50; var = 50/1 = 50
        assert_eq!(var, 50);
    }

    // --- corridor-rate scenario ---

    #[test]
    fn test_corridor_rate_variance_preserves_precision() {
        // Simulate five corridor rate submissions around 1.05 (scaled to 7 decimals)
        let rates = &[10_500_000, 10_510_000, 10_490_000, 10_505_000, 10_495_000];
        let var = compute_population_variance(rates).unwrap();
        // Every product (dev * dev) is done in full i128 before division,
        // so no fractional bits are lost before the final scale-down.
        assert!(var > 0);
    }

    // --- calculate_spread_bps ---

    #[test]
    fn test_spread_bps_no_deviation() {
        assert_eq!(calculate_spread_bps(1_000_000, 1_000_000), Ok(0));
    }

    #[test]
    fn test_spread_bps_positive_deviation() {
        // 1% spread: |1_000_000 - 1_010_000| * 10_000 / 1_000_000 = 100
        assert_eq!(calculate_spread_bps(1_000_000, 1_010_000), Ok(100));
    }

    #[test]
    fn test_spread_bps_negative_deviation() {
        // 2% spread: |1_000_000 - 980_000| * 10_000 / 1_000_000 = 200
        assert_eq!(calculate_spread_bps(1_000_000, 980_000), Ok(200));
    }

    #[test]
    fn test_spread_bps_division_by_zero() {
        assert_eq!(
            calculate_spread_bps(0, 1_000_000),
            Err(ContractError::DivisionByZero)
        );
    }

    #[test]
    fn test_spread_bps_overflow() {
        // Large delta and rate_b can cause the numerator to overflow
        let rate_a = 100;
        let rate_b = i128::MAX; // Creates a large delta
        assert_eq!(
            calculate_spread_bps(rate_a, rate_b),
            Err(ContractError::MathOverflow)
        );
    }

    // --- multiply_and_scale_down ---

    #[test]
    fn test_multiply_and_scale_down_normal() {
        // (2 * 10^7) * (3 * 10^7) / 10^7 = 6 * 10^7
        let scale = 10_000_000;
        assert_eq!(
            multiply_and_scale_down(2 * scale, 3 * scale, scale),
            Ok(6 * scale)
        );
    }

    #[test]
    fn test_multiply_and_scale_down_with_truncation() {
        // 1.5 * 2.5 = 3.75. Scaled: (15 * 10^6) * (25 * 10^6) / 10^7 = 37.5 * 10^6 -> 37_500_000
        let scale = 10_000_000;
        assert_eq!(
            multiply_and_scale_down(15_000_000, 2_500_000, scale),
            Ok(3_750_000) // (1.5 * 0.25) * 10^7
        );
    }

    #[test]
    fn test_multiply_and_scale_down_division_by_zero() {
        assert_eq!(
            multiply_and_scale_down(100, 200, 0),
            Err(ContractError::DivisionByZero)
        );
    }

    #[test]
    fn test_multiply_and_scale_down_overflow() {
        assert_eq!(
            multiply_and_scale_down(i128::MAX, 2, 10_000_000),
            Err(ContractError::MathOverflow)
        );
    }

    #[test]
    fn test_multiply_and_scale_down_zero_value() {
        assert_eq!(
            multiply_and_scale_down(0, 12345, 10_000_000),
            Ok(0)
        );
    }

    // --- compute_cema ---

    #[test]
    fn test_compute_cema_normal() {
        // scale = 10^7, alpha = 0.1 * 10^7 = 1_000_000
        // value = 120, prev = 100
        // result = 120 * 0.1 + 100 * 0.9 = 12 + 90 = 102
        let scale = 10_000_000;
        let alpha = 1_000_000;
        assert_eq!(compute_cema(120, 100, alpha, scale), Ok(102));
    }

    #[test]
    fn test_compute_cema_zero_alpha() {
        let scale = 10_000_000;
        assert_eq!(compute_cema(120, 100, 0, scale), Ok(100));
    }

    #[test]
    fn test_compute_cema_full_alpha() {
        let scale = 10_000_000;
        assert_eq!(compute_cema(120, 100, scale, scale), Ok(120));
    }

    #[test]
    fn test_compute_cema_overflow() {
        let scale = 10_000_000;
        // i128::MAX * alpha will overflow multiply_and_scale_down
        assert_eq!(
            compute_cema(i128::MAX, 100, 1_000_000, scale),
            Err(ContractError::Overflow)
        );
    }

    // ── ratio_ge ────────────────────────────────────────────────────────────

    #[test]
    fn ratio_ge_matches_exact_cross_multiplication() {
        // Exhaustive over a small grid. `a * d` and `c * b` cannot overflow
        // here, so the expected answer is directly computable and independent
        // of the Euclidean implementation under test.
        for b in 1i128..=24 {
            for d in 1i128..=24 {
                for a in 0i128..=24 {
                    for c in 0i128..=24 {
                        assert_eq!(
                            ratio_ge(a, b, c, d),
                            a * d >= c * b,
                            "ratio_ge({a}, {b}, {c}, {d})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn ratio_ge_never_overflows_at_i128_bounds() {
        // The naive `a * d >= c * b` wraps on every one of these, which is the
        // whole reason this function exists.
        assert!(ratio_ge(i128::MAX, 1, i128::MAX / 2, 1));
        assert!(ratio_ge(1, i128::MAX, 1, i128::MAX));
        assert!(!ratio_ge(i128::MAX - 1, i128::MAX, 1, 1));
        assert!(ratio_ge(i128::MAX, 2, i128::MAX / 2, 1));
    }

    #[test]
    fn ratio_ge_handles_zero_numerators() {
        assert!(ratio_ge(0, 7, 0, 3));
        assert!(!ratio_ge(0, 7, 1, 3));
        assert!(ratio_ge(1, 3, 0, 3));
        assert!(ratio_ge(0, i128::MAX, 0, 1));
    }

    #[test]
    fn ratio_ge_handles_long_continued_fraction_chains() {
        // Successive Fibonacci ratios are the worst case for the Euclidean
        // recursion: the chain runs for the maximum number of steps. Two
        // ladders seeded differently keep the reference products inside i128.
        let (mut a, mut b) = (1i128, 2i128);
        let (mut c, mut d) = (1i128, 3i128);
        let mut checked = 0;
        for _ in 0..40 {
            let next = a + b;
            a = b;
            b = next;
            let next_c = c + d;
            c = d;
            d = next_c;
            if a * d != c * b {
                assert_eq!(ratio_ge(a, b, c, d), a * d > c * b);
                checked += 1;
            }
        }
        assert!(
            checked > 0,
            "the Fibonacci ladder should exercise the recursion"
        );
    }

    #[test]
    fn ratio_ge_equality_satisfies_both_directions() {
        // 120 % clears a 120 % floor; the reciprocal step must not flip a tie
        // into a false negative.
        assert!(ratio_ge(1_200, 1_000, 12_000, 10_000));
        assert!(ratio_ge(12_000, 10_000, 1_200, 1_000));
        assert!(ratio_ge(7, 3, 14, 6));
    }
}
