//! Virtual reserve arithmetic for the constant-product AMM (Issue #906).
//!
//! A vanilla constant-product pool is unusable the moment it is created: with
//! real reserves `x = y = 0` the invariant `k = x * y` is `0`, so *any* trade
//! would drain the pool completely, and most AMMs respond by refusing to quote
//! against an empty pool at all.
//!
//! The alternative — and the one implemented here — is to give the curve a
//! **virtual** reserve buffer that counts towards pricing but is never held,
//! never withdrawable and never transferred. The pair quotes against the
//! *effective* balances
//!
//! ```text
//! x_eff = x + v_x
//! y_eff = y + v_y
//! ```
//!
//! so a brand new pair has `k_eff = v_x * v_y > 0` and therefore a
//! well-defined, non-zero starting curve instead of a degenerate one. This is
//! what "non-zero starting liquidity for newly initialized trading pairs"
//! means: the pair inherits a curve out of its own parameters rather than out
//! of someone's wallet.
//!
//! # Invariant
//!
//! Every state transition is checked against
//!
//! ```text
//! k_eff = x_eff * y_eff
//! ```
//!
//! which must not decrease — exactly the classic constant-product rule, but
//! measured on effective balances.
//!
//! # The virtual buffer is never paid out
//!
//! `v_x` and `v_y` are constants, so a real reserve that has fallen onto or
//! below its virtual counterpart means the pair is trading on a curve its LPs
//! no longer back. Two guards keep that from happening:
//!
//! * [`assert_withdrawal_allowed`] refuses any LP exit that would push a real
//!   reserve below its virtual counterpart. This is the "prevent liquidity
//!   withdrawal from dipping into core virtual reserve thresholds" rule: the
//!   core is the pool's irreducible floor, so the curve a pair advertises at
//!   creation is the curve it is still quoting from when the last real
//!   deposit leaves.
//! * [`EffectiveReserves::swap_a_to_b`] / [`EffectiveReserves::swap_b_to_a`]
//!   refuse to quote an output larger than the *real* reserve on that leg, so
//!   the core can shape a price but can never fund a payout on its own.
//!
//! All amounts are non-negative token quantities. Arithmetic is checked
//! throughout and failures are reported as explicit errors rather than
//! wrapping.

/// A 256-bit unsigned product, split into `(hi, lo)` machine words.
///
/// `x_eff * y_eff` is a product of two `i128` values and routinely exceeds
/// `u128`, so it cannot be compared losslessly in a single word.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct U256 {
    /// High 128 bits of the product.
    pub hi: u128,
    /// Low 128 bits of the product.
    pub lo: u128,
}

impl U256 {
    /// Build a `(hi, lo)` pair from two 128-bit words.
    pub const fn new(hi: u128, lo: u128) -> Self {
        U256 { hi, lo }
    }

    /// `true` when `self >= other`, i.e. the invariant did not decrease.
    pub fn ge(&self, other: &U256) -> bool {
        if self.hi != other.hi {
            self.hi > other.hi
        } else {
            self.lo >= other.lo
        }
    }
}

/// Errors raised by the virtual reserve module.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VirtualReserveError {
    /// A real reserve was negative.
    NegativeReserve,
    /// A virtual reserve buffer or a trade amount was not strictly positive.
    NonPositiveAmount,
    /// A withdrawal, or a payout, would have pushed a real reserve onto or
    /// below its virtual buffer.
    VirtualFloorBreached,
    /// A caller-supplied minimum-output guard rejected the trade.
    SlippageExceeded,
    /// The effective invariant decreased across a state transition.
    InvariantViolation,
    /// A product or sum exceeded the representable range.
    Overflow,
}

/// An `(x_eff, y_eff)` snapshot of a pool's effective balances.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EffectiveReserves {
    /// Real reserve of token A, excluding the virtual buffer.
    pub x: i128,
    /// Real reserve of token A, counting the virtual buffer.
    pub x_eff: i128,
    /// Virtual buffer on token A.
    pub v_x: i128,
    /// Real reserve of token B, excluding the virtual buffer.
    pub y: i128,
    /// Real reserve of token B, counting the virtual buffer.
    pub y_eff: i128,
    /// Virtual buffer on token B.
    pub v_y: i128,
}

impl EffectiveReserves {
    /// Compute the effective reserves for a pool.
    ///
    /// `x_eff = x + v_x` and `y_eff = y + v_y`, as required by issue #906.
    /// Both virtual buffers must be strictly positive: a buffer of zero
    /// would silently degrade the pair back to a vanilla pool, and a negative
    /// buffer would let a swap quote against a balance the pool does not have.
    pub fn new(x: i128, y: i128, v_x: i128, v_y: i128) -> Result<Self, VirtualReserveError> {
        if x < 0 || y < 0 {
            return Err(VirtualReserveError::NegativeReserve);
        }
        if v_x <= 0 || v_y <= 0 {
            return Err(VirtualReserveError::NonPositiveAmount);
        }
        let x_eff = x.checked_add(v_x).ok_or(VirtualReserveError::Overflow)?;
        let y_eff = y.checked_add(v_y).ok_or(VirtualReserveError::Overflow)?;
        Ok(EffectiveReserves { x, x_eff, v_x, y, y_eff, v_y })
    }

    /// `k_eff = x_eff * y_eff`, the invariant the pair must never decrease.
    pub fn k_eff(&self) -> U256 {
        wide_product(self.x_eff, self.y_eff)
    }

    /// Apply a token-A in / token-B out swap.
    ///
    /// The quote uses the effective balances:
    ///
    /// ```text
    /// amount_out = y_eff * amount_in / (x_eff + amount_in)
    /// ```
    ///
    /// with floor division, so rounding always favours the pool. Returns the
    /// post-swap effective reserves, the amount of B handed to the trader, and
    /// the pre-swap `k_eff` so the caller can assert the invariant without
    /// recomputing it.
    pub fn swap_a_to_b(
        &self,
        amount_in: i128,
    ) -> Result<(EffectiveReserves, i128, U256), VirtualReserveError> {
        if amount_in <= 0 {
            return Err(VirtualReserveError::NonPositiveAmount);
        }

        let denominator = self.x_eff.checked_add(amount_in).ok_or(VirtualReserveError::Overflow)?;
        let amount_out = wide_mul_div(self.y_eff, amount_in, denominator)?;

        // The virtual buffer shapes the price but is never spendable: an output
        // larger than the real reserve is not payable.
        if amount_out > self.y {
            return Err(VirtualReserveError::VirtualFloorBreached);
        }

        let x = self.x.checked_add(amount_in).ok_or(VirtualReserveError::Overflow)?;
        let y = self.y.checked_sub(amount_out).ok_or(VirtualReserveError::NegativeReserve)?;

        let after = EffectiveReserves { x, y, ..*self };
        Ok((after, amount_out, self.k_eff()))
    }

    /// Apply a token-B in / token-A out swap, the mirror of
    /// [`Self::swap_a_to_b`].
    pub fn swap_b_to_a(
        &self,
        amount_in: i128,
    ) -> Result<(EffectiveReserves, i128, U256), VirtualReserveError> {
        if amount_in <= 0 {
            return Err(VirtualReserveError::NonPositiveAmount);
        }

        let denominator = self.y_eff.checked_add(amount_in).ok_or(VirtualReserveError::Overflow)?;
        let amount_out = wide_mul_div(self.x_eff, amount_in, denominator)?;

        if amount_out > self.x {
            return Err(VirtualReserveError::VirtualFloorBreached);
        }

        let x = self.x.checked_sub(amount_out).ok_or(VirtualReserveError::NegativeReserve)?;
        let y = self.y.checked_add(amount_in).ok_or(VirtualReserveError::Overflow)?;

        let after = EffectiveReserves { x, y, ..*self };
        Ok((after, amount_out, self.k_eff()))
    }
}

/// Multiply two non-negative `i128` values into a lossless 256-bit product.
///
/// Implemented as a four-product `u64 x u64` decomposition with explicit carry
/// propagation, mirroring `mul_high` in the contract root. `u128::widening_mul`
/// is the idiomatic call but is still unstable (rust-lang/rust#85532).
fn wide_product(a: i128, b: i128) -> U256 {
    const MASK: u128 = u64::MAX as u128;

    let a = a as u128;
    let b = b as u128;

    let a_lo = a & MASK;
    let a_hi = a >> 64;
    let b_lo = b & MASK;
    let b_hi = b >> 64;

    let p_ll = a_lo * b_lo;
    let p_lh = a_lo * b_hi;
    let p_hl = a_hi * b_lo;
    let p_hh = a_hi * b_hi;

    let (mid, carry_mid) = p_lh.overflowing_add(p_hl);
    // Carry out of the low word (bits 0..127) into bit 128.
    let low_carry = ((p_ll >> 64) + (mid & MASK)) >> 64;

    let lo = p_ll.wrapping_add(mid << 64);
    let hi = p_hh
        .wrapping_add(mid >> 64)
        .wrapping_add((carry_mid as u128) << 64)
        .wrapping_add(low_carry);

    U256 { hi, lo }
}

/// Divide a [`U256`] by a positive `u128` divisor via bitwise long division.
///
/// Returns `None` when the quotient would not fit in 128 bits, which can only
/// happen for a divisor of `1` combined with a non-zero high word.
fn div_256_by_u128(value: &U256, divisor: u128) -> Option<u128> {
    if divisor == 0 || value.hi >= divisor {
        return None;
    }

    let mut remainder: u128 = value.hi;
    let mut quotient: u128 = 0;

    for bit in (0..128).rev() {
        remainder = (remainder << 1) | ((value.lo >> bit) & 1);
        if remainder >= divisor {
            remainder -= divisor;
            quotient |= 1u128 << bit;
        }
    }

    Some(quotient)
}

/// Compute `a * b / d` at 256-bit precision, flooring.
///
/// Every quote routes through here so a unit of rounding error always stays
/// with the pool rather than with the trader.
fn wide_mul_div(a: i128, b: i128, d: i128) -> Result<i128, VirtualReserveError> {
    if d <= 0 {
        return Err(VirtualReserveError::NonPositiveAmount);
    }
    let product = wide_product(a, b);
    let quotient = div_256_by_u128(&product, d as u128).ok_or(VirtualReserveError::Overflow)?;
    if quotient > i128::MAX as u128 {
        return Err(VirtualReserveError::Overflow);
    }
    Ok(quotient as i128)
}

/// Assert that a state transition preserved the effective invariant.
pub fn assert_k_eff_not_decreased(before: &U256, after: &U256) -> Result<(), VirtualReserveError> {
    if after.ge(before) {
        Ok(())
    } else {
        Err(VirtualReserveError::InvariantViolation)
    }
}

/// LP shares minted for the *first* deposit into a pair with virtual reserves.
///
/// A vanilla pool mints `min(x, y)` shares, so a new pair seeded with a dust
/// deposit would have a share supply of dust and a correspondingly violent
/// first price move. Counting the virtual buffer gives
///
/// ```text
/// shares = min(x_deposit + v_x, y_deposit + v_y)
/// ```
///
/// which starts the pair from the curve it is actually quoting on and bounds
/// what the first depositor can mint out of a small seed.
pub fn initial_lp_shares(
    x_deposit: i128,
    y_deposit: i128,
    v_x: i128,
    v_y: i128,
) -> Result<i128, VirtualReserveError> {
    if x_deposit <= 0 || y_deposit <= 0 {
        return Err(VirtualReserveError::NonPositiveAmount);
    }
    let x = x_deposit.checked_add(v_x).ok_or(VirtualReserveError::Overflow)?;
    let y = y_deposit.checked_add(v_y).ok_or(VirtualReserveError::Overflow)?;
    Ok(x.min(y))
}

/// The largest amount of one side that may still be withdrawn without eating
/// into the virtual buffer. Returns `0` once the reserve sits on the buffer.
pub fn withdrawable(reserve: i128, virtual_reserve: i128) -> i128 {
    if reserve <= virtual_reserve {
        0
    } else {
        reserve - virtual_reserve
    }
}

/// Guard a withdrawal against dipping into the virtual reserve thresholds.
///
/// The virtual buffer is a promise about the curve, not an asset: an LP may
/// withdraw their own deposit, but never the liquidity the pair advertises at
/// creation. Returns the realisable amount, which equals `amount` on success.
pub fn assert_withdrawal_allowed(
    reserve: i128,
    virtual_reserve: i128,
    amount: i128,
) -> Result<i128, VirtualReserveError> {
    if amount <= 0 {
        return Err(VirtualReserveError::NonPositiveAmount);
    }
    if amount > withdrawable(reserve, virtual_reserve) {
        return Err(VirtualReserveError::VirtualFloorBreached);
    }
    Ok(amount)
}

/// Enforce the virtual reserve floor on **both** sides of a withdrawal.
///
/// Both legs must clear their own threshold, so a balanced exit can never
/// drain one side onto its buffer while the other side still looks healthy.
pub fn assert_withdrawal_allowed_both_sides(
    reserves: &EffectiveReserves,
    amount_x: i128,
    amount_y: i128,
) -> Result<(), VirtualReserveError> {
    assert_withdrawal_allowed(reserves.x, reserves.v_x, amount_x)?;
    assert_withdrawal_allowed(reserves.y, reserves.v_y, amount_y)?;
    Ok(())
}

/// Guard a swap quote against a caller-supplied minimum output.
pub fn assert_min_amount_out(
    amount_out: i128,
    min_amount_out: i128,
) -> Result<(), VirtualReserveError> {
    if amount_out < min_amount_out {
        return Err(VirtualReserveError::SlippageExceeded);
    }
    Ok(())
}

/// Validate a virtual reserve configuration at pair initialisation.
///
/// Both buffers must be strictly positive; a zero buffer would silently
/// degrade the pair to a vanilla pool and a negative one would let the curve
/// quote against balances the pair does not hold.
pub fn validate_virtual_reserves(
    x: i128,
    y: i128,
    v_x: i128,
    v_y: i128,
) -> Result<(), VirtualReserveError> {
    if x < 0 || y < 0 {
        return Err(VirtualReserveError::NegativeReserve);
    }
    if v_x <= 0 || v_y <= 0 {
        return Err(VirtualReserveError::NonPositiveAmount);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A representative 1 : 1 pair with a 1,000-unit bootstrap buffer.
    fn pair(x: i128, y: i128) -> EffectiveReserves {
        EffectiveReserves::new(x, y, 1_000, 1_000).unwrap()
    }

    #[test]
    fn effective_reserves_add_the_virtual_buffer() {
        let r = pair(500, 500);
        assert_eq!(r.x, 500);
        assert_eq!(r.x_eff, 1_500);
        assert_eq!(r.y, 500);
        assert_eq!(r.y_eff, 1_500);
    }

    #[test]
    fn empty_pair_has_a_non_zero_starting_curve() {
        // The headline property of issue #906: a brand new pair quotes against
        // k_eff = v_x * v_y > 0 rather than the degenerate k = 0.
        let r = EffectiveReserves::new(0, 0, 1_000, 1_000).unwrap();
        assert_eq!(r.k_eff(), U256::new(0, 1_000_000));
    }

    #[test]
    fn effective_reserves_reject_degenerate_buffers() {
        assert_eq!(
            EffectiveReserves::new(0, 0, 0, 1_000).err(),
            Some(VirtualReserveError::NonPositiveAmount)
        );
        assert_eq!(
            EffectiveReserves::new(0, 0, 1_000, -1).err(),
            Some(VirtualReserveError::NonPositiveAmount)
        );
        assert_eq!(
            EffectiveReserves::new(-1, 0, 1_000, 1_000).err(),
            Some(VirtualReserveError::NegativeReserve)
        );
    }

    #[test]
    fn virtual_core_adds_depth_to_the_first_trade() {
        // A vanilla pool holding (1000, 1000) would quote
        //   out = 1000 * 1000 / 2000 = 500.
        // Counting the virtual core the same pair quotes on (2000, 2000):
        //   out = 2000 * 1000 / 3000 = 666.
        // The buffer is what gives a freshly initialised pair a non-zero,
        // sane starting curve instead of a degenerate one.
        let r = pair(1_000, 1_000);
        let (_, out, _) = r.swap_a_to_b(1_000).unwrap();
        assert_eq!(out, 666);
        assert!(out > 500);
        assert!(out < 1_000, "the trade must return less than it put in");
    }

    #[test]
    fn first_deposit_still_needs_real_backing_on_both_legs() {
        // Real reserves of 1 cannot fund a 334-unit payout, so the core is
        // never spendable — it only ever shapes the price.
        let r = EffectiveReserves::new(1, 1, 1_000, 1_000).unwrap();
        assert_eq!(
            r.swap_a_to_b(500).err(),
            Some(VirtualReserveError::VirtualFloorBreached)
        );
    }

    #[test]
    fn swap_respects_the_virtual_floor() {
        // The pool holds no real B, so the buffer may not fund a payout.
        let r = EffectiveReserves::new(5_000, 0, 10_000, 10_000).unwrap();
        assert_eq!(
            r.swap_a_to_b(1_000).err(),
            Some(VirtualReserveError::VirtualFloorBreached)
        );
        let r = EffectiveReserves::new(0, 5_000, 10_000, 10_000).unwrap();
        assert_eq!(
            r.swap_b_to_a(1_000).err(),
            Some(VirtualReserveError::VirtualFloorBreached)
        );
    }

    #[test]
    fn swap_preserves_the_effective_invariant() {
        let r = pair(1_000_000, 2_000_000);
        let (after, out, k_before) = r.swap_a_to_b(10_000).unwrap();
        assert!(out > 0);
        assert_k_eff_not_decreased(&k_before, &after.k_eff()).unwrap();
    }

    #[test]
    fn reverse_swap_preserves_the_effective_invariant() {
        let r = pair(1_000_000, 2_000_000);
        let (after, out, k_before) = r.swap_b_to_a(10_000).unwrap();
        assert!(out > 0);
        assert_k_eff_not_decreased(&k_before, &after.k_eff()).unwrap();
    }

    #[test]
    fn swap_rejects_non_positive_input() {
        let r = pair(100, 100);
        assert_eq!(
            r.swap_a_to_b(0).err(),
            Some(VirtualReserveError::NonPositiveAmount)
        );
        assert_eq!(
            r.swap_b_to_a(-5).err(),
            Some(VirtualReserveError::NonPositiveAmount)
        );
    }

    #[test]
    fn invariant_decrease_is_detected() {
        let before = wide_product(1_000, 1_000);
        let after = wide_product(999, 1_000);
        assert_eq!(
            assert_k_eff_not_decreased(&before, &after),
            Err(VirtualReserveError::InvariantViolation)
        );
    }

    #[test]
    fn wide_product_exceeds_u128_and_still_compares() {
        // (2^127 - 1)^2 = 2^254 - 2^128 + 1, i.e. lo = 1, hi = 2^126 - 1.
        let p = wide_product(i128::MAX, i128::MAX);
        assert_eq!(p.lo, 1);
        assert_eq!(p.hi, (1u128 << 126) - 1);
        assert!(p.ge(&wide_product(i128::MAX, i128::MAX - 1)));
        assert!(!wide_product(i128::MAX, i128::MAX - 1).ge(&p));
    }

    #[test]
    fn wide_div_keeps_full_precision() {
        // 2^254 / 2^127 == 2^127, i.e. a 129-bit intermediate that a naive
        // u128 multiply would have lost.
        assert_eq!(
            wide_mul_div(i128::MAX, i128::MAX, i128::MAX).unwrap(),
            i128::MAX
        );
        assert_eq!(wide_mul_div(1_500, 1_000, 1_500).unwrap(), 1_000);
    }

    #[test]
    fn wide_div_rejects_a_zero_or_negative_divisor() {
        assert_eq!(
            wide_mul_div(10, 10, 0).err(),
            Some(VirtualReserveError::NonPositiveAmount)
        );
    }

    #[test]
    fn initial_shares_bootstrap_from_the_buffer() {
        // A vanilla pool would mint min(100, 100) = 100 shares out of an empty
        // pair. Counting the buffer starts the supply at the curve being quoted.
        assert_eq!(initial_lp_shares(100, 100, 1_000, 1_000).unwrap(), 1_100);
        assert_eq!(initial_lp_shares(100, 5_000, 1_000, 1_000).unwrap(), 1_100);
    }

    #[test]
    fn initial_shares_reject_empty_deposits() {
        assert_eq!(
            initial_lp_shares(0, 100, 1_000, 1_000).err(),
            Some(VirtualReserveError::NonPositiveAmount)
        );
    }

    #[test]
    fn withdrawal_cannot_eat_the_virtual_buffer() {
        let r = pair(1_100, 1_100);
        // 100 is the whole real deposit: allowed.
        assert_eq!(assert_withdrawal_allowed(r.x, r.v_x, 100), Ok(100));
        // 101 would take the reserve onto the buffer.
        assert_eq!(
            assert_withdrawal_allowed(r.x, r.v_x, 101),
            Err(VirtualReserveError::VirtualFloorBreached)
        );
    }

    #[test]
    fn withdrawal_is_blocked_once_reserves_sit_on_the_buffer() {
        assert_eq!(withdrawable(1_000, 1_000), 0);
        assert_eq!(
            assert_withdrawal_allowed(1_000, 1_000, 1),
            Err(VirtualReserveError::VirtualFloorBreached)
        );
    }

    #[test]
    fn withdrawal_rejects_non_positive_amounts() {
        assert_eq!(
            assert_withdrawal_allowed(2_000, 1_000, 0).err(),
            Some(VirtualReserveError::NonPositiveAmount)
        );
    }

    #[test]
    fn both_sides_must_clear_their_own_threshold() {
        let r = pair(1_100, 5_000);
        assert!(assert_withdrawal_allowed_both_sides(&r, 100, 100).is_ok());
        // A sits on its floor even though B is comfortably above its own.
        assert_eq!(
            assert_withdrawal_allowed_both_sides(&r, 101, 100),
            Err(VirtualReserveError::VirtualFloorBreached)
        );
        assert_eq!(
            assert_withdrawal_allowed_both_sides(&r, 100, 4_001),
            Err(VirtualReserveError::VirtualFloorBreached)
        );
    }

    #[test]
    fn min_amount_out_guard() {
        assert!(assert_min_amount_out(100, 100).is_ok());
        assert_eq!(
            assert_min_amount_out(99, 100),
            Err(VirtualReserveError::SlippageExceeded)
        );
    }

    #[test]
    fn initial_configuration_is_validated() {
        assert!(validate_virtual_reserves(0, 0, 1, 1).is_ok());
        assert_eq!(
            validate_virtual_reserves(1_000, 1_000, 0, 1_000),
            Err(VirtualReserveError::NonPositiveAmount)
        );
        assert_eq!(
            validate_virtual_reserves(-1, 1_000, 1, 1),
            Err(VirtualReserveError::NegativeReserve)
        );
    }

    #[test]
    fn high_volume_swap_keeps_the_invariant() {
        let r = EffectiveReserves::new(
            1_000_000_000_000_000_000,
            2_000_000_000_000_000_000,
            1_000,
            1_000,
        )
        .unwrap();
        let (after, out, k_before) = r.swap_a_to_b(100_000_000_000_000_000).unwrap();
        assert!(out > 0);
        assert_k_eff_not_decreased(&k_before, &after.k_eff()).unwrap();
    }
}
