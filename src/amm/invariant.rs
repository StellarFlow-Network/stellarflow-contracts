use crate::ContractError;

/// 256-bit unsigned integer represented as two machine words.
///
/// Used internally to hold intermediate products of two `u128` values before
/// division, preventing precision loss in the constant-product invariant.
struct U256(u128, u128);

impl U256 {
    fn zero() -> Self {
        U256(0, 0)
    }

    /// Multiply two `u128` values, returning the full 256-bit product.
    fn mul(a: u128, b: u128) -> Self {
        let a_lo = a as u64;
        let a_hi = (a >> 64) as u64;
        let b_lo = b as u64;
        let b_hi = (b >> 64) as u64;

        let lo = (a_lo as u128) * (b_lo as u128);
        let cross1 = (a_hi as u128) * (b_lo as u128);
        let cross2 = (a_lo as u128) * (b_hi as u128);
        let hi = (a_hi as u128) * (b_hi as u128);

        let mid = cross1 + cross2;
        let mid_lo = mid << 64;
        let mid_hi = mid >> 64;

        let (lo, carry1) = lo.overflowing_add(mid_lo);
        let hi = hi + mid_hi + (carry1 as u128);

        U256(lo, hi)
    }

    /// Divide a U256 by a u128 divisor, returning the (quotient, remainder).
    /// Returns `None` when `divisor` is zero or the quotient exceeds u128.
    fn div_mod(&self, divisor: u128) -> Option<(u128, u128)> {
        if divisor == 0 {
            return None;
        }
        let d = divisor;
        let hi = self.1;
        let lo = self.0;

        if hi >= d {
            return None;
        }

        let mut r = hi;
        let mut q = 0u128;

        for i in (0..128).rev() {
            r = (r << 1) | ((lo >> i) & 1);
            if r >= d {
                r -= d;
                q |= 1u128 << i;
            }
        }

        Some((q, r))
    }
}

/// Compute `numerator * denominator / divisor` using full 256-bit intermediate
/// precision. All rounding truncates toward zero (floor), which always favors
/// pool reserves.
fn mul_div(numerator: u128, denominator: u128, divisor: u128) -> Result<u128, ContractError> {
    if divisor == 0 {
        return Err(ContractError::DivisionByZero);
    }
    let product = U256::mul(numerator, denominator);
    let (quot, _rem) = product.div_mod(divisor).ok_or(ContractError::Overflow)?;
    Ok(quot)
}

/// Compute the output amount for a constant-product swap.
///
/// Formula: `out = reserve_out * amount_in / (reserve_in + amount_in)`
///
/// The result is rounded down (floor division) so that the pool never loses
/// value — the invariant `k` is guaranteed to be non-decreasing.
pub fn compute_swap_out(
    amount_in: u128,
    reserve_in: u128,
    reserve_out: u128,
) -> Result<u128, ContractError> {
    if amount_in == 0 || reserve_in == 0 || reserve_out == 0 {
        return Err(ContractError::InvalidInput);
    }
    let denominator = reserve_in
        .checked_add(amount_in)
        .ok_or(ContractError::Overflow)?;
    mul_div(reserve_out, amount_in, denominator)
}

/// Compute the amount of LP shares to mint for a liquidity deposit.
///
/// Formula: `shares = min(a * total_shares / reserve_a, b * total_shares / reserve_b)`
///
/// Rounded down to favor existing LPs.
pub fn compute_lp_shares(
    amount_a: u128,
    amount_b: u128,
    reserve_a: u128,
    reserve_b: u128,
    total_shares: u128,
) -> Result<u128, ContractError> {
    if amount_a == 0 || amount_b == 0 || total_shares == 0 {
        return Err(ContractError::InvalidInput);
    }
    if reserve_a == 0 || reserve_b == 0 {
        return Err(ContractError::InvalidInput);
    }
    let shares_a = mul_div(amount_a, total_shares, reserve_a)?;
    let shares_b = mul_div(amount_b, total_shares, reserve_b)?;
    Ok(shares_a.min(shares_b))
}

/// Compute the amounts returned when burning `shares` LP tokens.
///
/// Formula: `amount_a = shares * reserve_a / total_shares`
///          `amount_b = shares * reserve_b / total_shares`
///
/// Rounded down to favor the pool.
pub fn compute_remove_liquidity(
    shares: u128,
    total_shares: u128,
    reserve_a: u128,
    reserve_b: u128,
) -> Result<(u128, u128), ContractError> {
    if shares == 0 || total_shares == 0 {
        return Err(ContractError::InvalidInput);
    }
    if shares > total_shares {
        return Err(ContractError::InvalidInput);
    }
    let amount_a = mul_div(shares, reserve_a, total_shares)?;
    let amount_b = mul_div(shares, reserve_b, total_shares)?;
    Ok((amount_a, amount_b))
}

/// Verify that `k_new >= k_old` for a swap, ensuring rounding favors reserves.
pub fn assert_invariant_stable(
    reserve_in_before: u128,
    reserve_out_before: u128,
    amount_in: u128,
    amount_out: u128,
) -> Result<(), ContractError> {
    let k_before = U256::mul(reserve_in_before, reserve_out_before);
    let reserve_in_after = reserve_in_before
        .checked_add(amount_in)
        .ok_or(ContractError::Overflow)?;
    let reserve_out_after = reserve_out_before
        .checked_sub(amount_out)
        .ok_or(ContractError::Overflow)?;
    let k_after = U256::mul(reserve_in_after, reserve_out_after);

    if k_after.1 < k_before.1 || (k_after.1 == k_before.1 && k_after.0 < k_before.0) {
        return Err(ContractError::Overflow);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mul_div_basic() {
        let result = mul_div(100, 200, 50).unwrap();
        assert_eq!(result, 400);
    }

    #[test]
    fn test_mul_div_floor_rounding() {
        let result = mul_div(10, 3, 7).unwrap();
        assert_eq!(result, 4);
    }

    #[test]
    fn test_mul_div_zero_divisor() {
        assert_eq!(mul_div(100, 200, 0), Err(ContractError::DivisionByZero));
    }

    #[test]
    fn test_swap_out_basic() {
        let out = compute_swap_out(10, 100, 200).unwrap();
        assert_eq!(out, 18);
    }

    #[test]
    fn test_swap_out_floor_reserves_favored() {
        let out = compute_swap_out(1, 3, 10).unwrap();
        assert_eq!(out, 2);
    }

    #[test]
    fn test_invariant_stable_after_swap() {
        let reserve_in = 100u128;
        let reserve_out = 200u128;
        let amount_in = 10u128;
        let amount_out = compute_swap_out(amount_in, reserve_in, reserve_out).unwrap();
        assert!(amount_out < reserve_out);
        assert_invariant_stable(reserve_in, reserve_out, amount_in, amount_out).unwrap();
    }

    #[test]
    fn test_invariant_increases_with_floor_rounding() {
        let reserve_in = 1000u128;
        let reserve_out = 2000u128;
        let amount_in = 1u128;
        let amount_out = compute_swap_out(amount_in, reserve_in, reserve_out).unwrap();
        let k_before = U256::mul(reserve_in, reserve_out);
        let k_after = U256::mul(reserve_in + amount_in, reserve_out - amount_out);
        assert!(
            k_after.1 > k_before.1 || (k_after.1 == k_before.1 && k_after.0 >= k_before.0),
            "k must not decrease"
        );
    }

    #[test]
    fn test_lp_shares_basic() {
        let shares = compute_lp_shares(50, 100, 100, 200, 1000).unwrap();
        assert_eq!(shares, 500);
    }

    #[test]
    fn test_lp_shares_floor() {
        let shares = compute_lp_shares(10, 20, 100, 200, 1000).unwrap();
        assert_eq!(shares, 100);
    }

    #[test]
    fn test_lp_shares_min_rule() {
        let shares = compute_lp_shares(10, 50, 100, 200, 1000).unwrap();
        assert_eq!(shares, 100);
    }

    #[test]
    fn test_remove_liquidity_basic() {
        let (a, b) = compute_remove_liquidity(500, 1000, 100, 200).unwrap();
        assert_eq!(a, 50);
        assert_eq!(b, 100);
    }

    #[test]
    fn test_remove_liquidity_floor() {
        let (a, b) = compute_remove_liquidity(333, 1000, 100, 200).unwrap();
        assert!(a <= 33);
        assert!(b <= 66);
    }

    #[test]
    fn test_swap_out_zero_input_rejected() {
        assert_eq!(
            compute_swap_out(0, 100, 200),
            Err(ContractError::InvalidInput)
        );
    }

    #[test]
    fn test_lp_shares_zero_input_rejected() {
        assert_eq!(
            compute_lp_shares(0, 100, 100, 200, 1000),
            Err(ContractError::InvalidInput)
        );
    }

    #[test]
    fn test_remove_liquidity_excessive_shares_rejected() {
        assert_eq!(
            compute_remove_liquidity(2000, 1000, 100, 200),
            Err(ContractError::InvalidInput)
        );
    }

    #[test]
    fn test_u256_mul_max_bounds() {
        let a = u128::MAX;
        let b = u128::MAX;
        let result = U256::mul(a, b);
        assert!(result.1 > 0);
    }

    #[test]
    fn test_u256_mul_basic() {
        let result = U256::mul(5, 7);
        assert_eq!(result.0, 35);
        assert_eq!(result.1, 0);
    }

    #[test]
    fn test_u256_div_mod_basic() {
        let u = U256(100, 0);
        let (q, r) = u.div_mod(7).unwrap();
        assert_eq!(q, 14);
        assert_eq!(r, 2);
    }

    #[test]
    fn test_u256_div_mod_zero() {
        let u = U256(100, 0);
        assert!(u.div_mod(0).is_none());
    }

    #[test]
    fn test_u256_div_mod_hi_nonzero() {
        let u = U256(0, 1);
        let (q, r) = u.div_mod(2).unwrap();
        assert_eq!(q, 1u128 << 127);
        assert_eq!(r, 0);
    }

    #[test]
    fn test_invariant_max_bounds() {
        let reserve_in = u128::MAX / 2;
        let reserve_out = u128::MAX / 2;
        let amount_in = 1;
        let amount_out = compute_swap_out(amount_in, reserve_in, reserve_out).unwrap();
        assert_eq!(amount_out, 0);
        assert_invariant_stable(reserve_in, reserve_out, amount_in, amount_out).unwrap();
    }

    #[test]
    fn test_invariant_high_volume() {
        let reserve_in = 1_000_000_000_000_000_000u128;
        let reserve_out = 2_000_000_000_000_000_000u128;
        let amount_in = 100_000_000_000_000_000u128;
        let amount_out = compute_swap_out(amount_in, reserve_in, reserve_out).unwrap();
        assert!(amount_out > 0);
        assert_invariant_stable(reserve_in, reserve_out, amount_in, amount_out).unwrap();
    }
}
