//! Flash Loan Arbitrage Detection and Prevention
//!
//! Detects and prevents malicious arbitrage sequences that exploit temporary
//! price imbalances induced by flash loans. The guard operates post-transaction
//! and enforces three complementary invariants:
//!
//! 1. **Reserve ratio stability** – the relative ratio of pool reserves
//!    (`reserve_a / reserve_b`) must not deviate beyond a configurable
//!    percentage from its pre-transaction baseline. Large ratio swings
//!    indicate a price imbalance characteristic of flash loan manipulation.
//!
//! 2. **Pool invariant non-decrease** – the constant-product invariant
//!    `k = reserve_a * reserve_b` must be greater than or equal to its
//!    pre-transaction value. A decrease in `k` means value was extracted
//!    from the pool, which is the hallmark of an exploitative flash loan
//!    arbitrage sequence.
//!
//! 3. **Liquidity depth safety threshold** – both individual reserve
//!    balances must remain above an absolute floor
//!    ([`MIN_LIQUIDITY_DEPTH`]). Dropping below this threshold makes the
//!    pool economically trivial to manipulate and signals that an attacker
//!    has drained reserves to achieve a favorable exit price.
//!
//! ## Usage
//!
//! Call [`check_flash_loan_arbitrage`] at the end of any state-changing
//! operation that touches pool reserves (swaps, liquidity removals):
//!
//! ```ignore
//! use crate::flash_loan_guard::{PoolSnapshot, check_flash_loan_arbitrage};
//!
//! let before = PoolSnapshot { reserve_a: 1_000_000, reserve_b: 2_000_000 };
//! let after  = PoolSnapshot { reserve_a: 900_000,   reserve_b: 2_100_000 };
//! check_flash_loan_arbitrage(&before, &after)?;
//! ```
//!
//! Closes #757

use crate::ContractError;

// ─── Constants ───────────────────────────────────────────────────────────────

/// Absolute minimum reserve balance (in stroops) each side of the pool must
/// maintain after a transaction.  Pools smaller than this are trivially
/// manipulable and are rejected regardless of the ratio-shift check.
///
/// Value: 100,000 XLM × 10⁷ stroops/XLM = 1 × 10¹² stroops.
pub const MIN_LIQUIDITY_DEPTH: u128 = 1_000_000_000_000;

/// Maximum allowed deviation in the reserve ratio expressed as a fraction of
/// `MAX_RATIO_DEVIATION_BPS` basis points (1 bp = 0.01 %).
///
/// A post-transaction ratio that has shifted by more than this fraction
/// relative to the pre-transaction ratio triggers
/// [`ContractError::FlashLoanArbitrageDetected`].
///
/// Default: 5 000 bp = 50 %.  A 50 % ratio swing within a single transaction
/// is economically irrational for legitimate LPs and is a strong signal of
/// a flash-loan-induced price spike.
pub const MAX_RATIO_DEVIATION_BPS: u128 = 5_000;

/// Basis-point denominator (10 000 = 100 %).
const BPS_DENOMINATOR: u128 = 10_000;

/// Scale factor used for ratio comparison arithmetic to avoid floating-point
/// operations. Both ratios are represented as `reserve_a * RATIO_SCALE /
/// reserve_b`.
const RATIO_SCALE: u128 = 1_000_000_000_000_000_000; // 10^18

// ─── Types ───────────────────────────────────────────────────────────────────

/// A lightweight snapshot of a two-asset pool's reserve balances.
///
/// Capture one *before* executing the transaction and one *after*, then
/// pass both to [`check_flash_loan_arbitrage`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolSnapshot {
    /// Reserve balance of asset A (in stroops or the pool's base denomination).
    pub reserve_a: u128,
    /// Reserve balance of asset B (in stroops or the pool's base denomination).
    pub reserve_b: u128,
}

// ─── Core check ──────────────────────────────────────────────────────────────

/// Validate the post-transaction pool state against flash-loan-arbitrage
/// detection criteria.
///
/// # Arguments
/// * `before` – Pool reserves captured *before* the transaction executed.
/// * `after`  – Pool reserves captured *after* the transaction executed.
///
/// # Returns
/// `Ok(())` when all three invariants are satisfied.
///
/// # Errors
/// * [`ContractError::FlashLoanArbitrageDetected`] – when any invariant is
///   violated (ratio deviation, k-decrease, or depth below minimum).
/// * [`ContractError::InvalidInput`] – when either snapshot contains a
///   zero reserve (undefined pool state).
pub fn check_flash_loan_arbitrage(
    before: &PoolSnapshot,
    after: &PoolSnapshot,
) -> Result<(), ContractError> {
    // ── 0. Guard against degenerate pools ───────────────────────────────────
    if before.reserve_a == 0 || before.reserve_b == 0 {
        return Err(ContractError::InvalidInput);
    }
    if after.reserve_a == 0 || after.reserve_b == 0 {
        return Err(ContractError::InvalidInput);
    }

    // ── 1. Liquidity depth safety threshold ─────────────────────────────────
    check_liquidity_depth(after)?;

    // ── 2. Pool invariant non-decrease (k_after >= k_before) ────────────────
    check_k_nondecreasing(before, after)?;

    // ── 3. Reserve ratio deviation bound ────────────────────────────────────
    check_reserve_ratio(before, after)?;

    Ok(())
}

// ─── Sub-checks ──────────────────────────────────────────────────────────────

/// Assert that both reserves in `snapshot` are at or above
/// [`MIN_LIQUIDITY_DEPTH`].
///
/// This is the cheapest check (two comparisons) so it runs first.
pub fn check_liquidity_depth(snapshot: &PoolSnapshot) -> Result<(), ContractError> {
    if snapshot.reserve_a < MIN_LIQUIDITY_DEPTH || snapshot.reserve_b < MIN_LIQUIDITY_DEPTH {
        return Err(ContractError::FlashLoanArbitrageDetected);
    }
    Ok(())
}

/// Assert that the constant-product invariant `k = reserve_a * reserve_b`
/// has not decreased after the transaction.
///
/// Uses 256-bit intermediate arithmetic to avoid overflow when reserves are
/// near `u128::MAX / 2`.
pub fn check_k_nondecreasing(
    before: &PoolSnapshot,
    after: &PoolSnapshot,
) -> Result<(), ContractError> {
    // Use 128-bit saturating mul for overflow safety on very large reserves.
    // For pools of realistic size (< 2^63 each side) this is exact.
    let k_before = wide_mul(before.reserve_a, before.reserve_b);
    let k_after = wide_mul(after.reserve_a, after.reserve_b);

    // k_after < k_before means pool value was extracted — flash loan exploit.
    if k_after < k_before {
        return Err(ContractError::FlashLoanArbitrageDetected);
    }
    Ok(())
}

/// Assert that the reserve ratio has not shifted beyond
/// [`MAX_RATIO_DEVIATION_BPS`] basis points from the pre-transaction state.
///
/// The ratio is computed as `reserve_a * RATIO_SCALE / reserve_b` (scaled
/// integer division). The absolute deviation between before and after ratios
/// is then compared to `before_ratio * MAX_RATIO_DEVIATION_BPS / BPS_DENOMINATOR`.
pub fn check_reserve_ratio(
    before: &PoolSnapshot,
    after: &PoolSnapshot,
) -> Result<(), ContractError> {
    // Compute scaled ratios: ratio = reserve_a * RATIO_SCALE / reserve_b.
    // Division is safe because we checked reserve_b != 0 in check_flash_loan_arbitrage.
    let ratio_before = before
        .reserve_a
        .checked_mul(RATIO_SCALE)
        .ok_or(ContractError::Overflow)?
        / before.reserve_b;

    let ratio_after = after
        .reserve_a
        .checked_mul(RATIO_SCALE)
        .ok_or(ContractError::Overflow)?
        / after.reserve_b;

    // Absolute deviation (unsigned).
    let deviation = if ratio_after > ratio_before {
        ratio_after - ratio_before
    } else {
        ratio_before - ratio_after
    };

    // Allowed deviation = before_ratio * MAX_RATIO_DEVIATION_BPS / BPS_DENOMINATOR.
    let allowed = ratio_before
        .checked_mul(MAX_RATIO_DEVIATION_BPS)
        .ok_or(ContractError::Overflow)?
        / BPS_DENOMINATOR;

    if deviation > allowed {
        return Err(ContractError::FlashLoanArbitrageDetected);
    }
    Ok(())
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Compute `a * b` as a 256-bit value represented as `(lo128, hi128)`.
///
/// This mirrors the `U256::mul` implementation in `src/amm/invariant.rs` but
/// is kept as a private helper here to avoid cross-module coupling at the
/// cost of a small code duplication, which is acceptable for a security-
/// critical guard.
#[inline]
fn wide_mul(a: u128, b: u128) -> (u128, u128) {
    const MASK: u128 = (1u128 << 64) - 1;

    let a_lo = a & MASK;
    let a_hi = a >> 64;
    let b_lo = b & MASK;
    let b_hi = b >> 64;

    let p_lo_lo = a_lo * b_lo;
    let p_lo_hi = a_lo * b_hi;
    let p_hi_lo = a_hi * b_lo;
    let p_hi_hi = a_hi * b_hi;

    let (mid, carry_mid) = p_lo_hi.overflowing_add(p_hi_lo);
    let mid_lo = mid << 64;
    let mid_hi = (mid >> 64).wrapping_add(carry_mid as u128);

    let (lo, carry_lo) = p_lo_lo.overflowing_add(mid_lo);
    let hi = p_hi_hi
        .wrapping_add(mid_hi)
        .wrapping_add(carry_lo as u128);

    (lo, hi)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn deep_pool(reserve_a: u128, reserve_b: u128) -> PoolSnapshot {
        PoolSnapshot { reserve_a, reserve_b }
    }

    const DEPTH: u128 = MIN_LIQUIDITY_DEPTH;

    // ── wide_mul ─────────────────────────────────────────────────────────────

    #[test]
    fn wide_mul_basic() {
        let (lo, hi) = wide_mul(5, 7);
        assert_eq!(lo, 35);
        assert_eq!(hi, 0);
    }

    #[test]
    fn wide_mul_large() {
        // (2^64)^2 = 2^128 overflows u128, should land in hi word.
        let base: u128 = 1u128 << 64;
        let (lo, hi) = wide_mul(base, base);
        assert_eq!(lo, 0);
        assert_eq!(hi, 1);
    }

    #[test]
    fn wide_mul_max() {
        // u128::MAX * u128::MAX = 2^256 - 2^129 + 1
        // lo = 1, hi = u128::MAX - 1
        let (lo, hi) = wide_mul(u128::MAX, u128::MAX);
        assert_eq!(lo, 1);
        assert_eq!(hi, u128::MAX - 1);
    }

    // ── check_liquidity_depth ────────────────────────────────────────────────

    #[test]
    fn depth_ok_at_minimum() {
        let snap = deep_pool(DEPTH, DEPTH);
        assert!(check_liquidity_depth(&snap).is_ok());
    }

    #[test]
    fn depth_ok_above_minimum() {
        let snap = deep_pool(DEPTH * 10, DEPTH * 10);
        assert!(check_liquidity_depth(&snap).is_ok());
    }

    #[test]
    fn depth_fails_reserve_a_below_min() {
        let snap = deep_pool(DEPTH - 1, DEPTH * 10);
        assert_eq!(
            check_liquidity_depth(&snap),
            Err(ContractError::FlashLoanArbitrageDetected)
        );
    }

    #[test]
    fn depth_fails_reserve_b_below_min() {
        let snap = deep_pool(DEPTH * 10, DEPTH - 1);
        assert_eq!(
            check_liquidity_depth(&snap),
            Err(ContractError::FlashLoanArbitrageDetected)
        );
    }

    #[test]
    fn depth_fails_both_below_min() {
        let snap = deep_pool(1, 1);
        assert_eq!(
            check_liquidity_depth(&snap),
            Err(ContractError::FlashLoanArbitrageDetected)
        );
    }

    // ── check_k_nondecreasing ────────────────────────────────────────────────

    #[test]
    fn k_stable_is_ok() {
        let before = deep_pool(1_000, 1_000);
        let after = deep_pool(1_000, 1_000);
        assert!(check_k_nondecreasing(&before, &after).is_ok());
    }

    #[test]
    fn k_increased_is_ok() {
        // More reserve in — k grows (fee capture).
        let before = deep_pool(1_000, 1_000);
        let after = deep_pool(1_100, 950);
        // k_before = 1_000_000, k_after = 1_045_000  ✓
        assert!(check_k_nondecreasing(&before, &after).is_ok());
    }

    #[test]
    fn k_decreased_is_rejected() {
        let before = deep_pool(1_000, 1_000);
        let after = deep_pool(900, 900); // k drops from 1_000_000 to 810_000
        assert_eq!(
            check_k_nondecreasing(&before, &after),
            Err(ContractError::FlashLoanArbitrageDetected)
        );
    }

    #[test]
    fn k_large_reserves_ok() {
        // Typical large reserves: 10^15 each side, no overflow.
        let base = 1_000_000_000_000_000u128;
        let before = deep_pool(base, base);
        let after = deep_pool(base + 1, base - 1);
        // k_after = base^2 - 1 < base^2 = k_before → should fail.
        assert_eq!(
            check_k_nondecreasing(&before, &after),
            Err(ContractError::FlashLoanArbitrageDetected)
        );
    }

    #[test]
    fn k_large_reserves_with_fee_ok() {
        let base = 1_000_000_000_000_000u128;
        // Fee accrual: amount_in goes in, slightly less comes out.
        let before = deep_pool(base, base);
        let after = deep_pool(base + 1000, base - 999); // k grows
        assert!(check_k_nondecreasing(&before, &after).is_ok());
    }

    // ── check_reserve_ratio ──────────────────────────────────────────────────

    #[test]
    fn ratio_unchanged_is_ok() {
        let snap = deep_pool(2 * DEPTH, DEPTH);
        assert!(check_reserve_ratio(&snap, &snap).is_ok());
    }

    #[test]
    fn ratio_small_shift_is_ok() {
        // 1% shift — well within 50% tolerance.
        let before = deep_pool(100 * DEPTH, 100 * DEPTH);
        let after = deep_pool(101 * DEPTH, 100 * DEPTH);
        assert!(check_reserve_ratio(&before, &after).is_ok());
    }

    #[test]
    fn ratio_exactly_at_max_deviation_is_ok() {
        // MAX_RATIO_DEVIATION_BPS = 5000 bp = 50%.
        // ratio_before = 1*RATIO_SCALE, allowed_deviation = RATIO_SCALE/2.
        // ratio_after must be <= 1.5 * RATIO_SCALE (or >= 0.5 * RATIO_SCALE).
        let before = deep_pool(DEPTH, DEPTH);
        // 50% upward shift: after_a = 1.5 * before_a.
        // ratio_after = 1.5 * RATIO_SCALE; deviation = 0.5 * RATIO_SCALE = allowed ✓
        let after = deep_pool(DEPTH * 3 / 2, DEPTH);
        assert!(check_reserve_ratio(&before, &after).is_ok());
    }

    #[test]
    fn ratio_exceeds_max_deviation_is_rejected() {
        // 100% shift — reserve_a doubles, reserve_b unchanged.
        let before = deep_pool(DEPTH, DEPTH);
        let after = deep_pool(DEPTH * 2 + 1, DEPTH); // ratio > 1.5 (beyond 50% tolerance)
        // deviation = RATIO_SCALE > 0.5 * RATIO_SCALE → fail
        assert_eq!(
            check_reserve_ratio(&before, &after),
            Err(ContractError::FlashLoanArbitrageDetected)
        );
    }

    #[test]
    fn ratio_downward_extreme_shift_is_rejected() {
        // Reverse direction: reserve_a collapses.
        let before = deep_pool(DEPTH * 4, DEPTH);
        // ratio_before = 4 * RATIO_SCALE / 1 = 4e18
        // ratio_after = RATIO_SCALE / 1 = 1e18
        // deviation = 3e18, allowed = 4e18 * 5000 / 10000 = 2e18 → fail
        let after = deep_pool(DEPTH, DEPTH);
        assert_eq!(
            check_reserve_ratio(&before, &after),
            Err(ContractError::FlashLoanArbitrageDetected)
        );
    }

    // ── check_flash_loan_arbitrage (integration) ──────────────────────────────

    #[test]
    fn normal_swap_passes_all_checks() {
        // Legitimate small swap: put in 1%, get out slightly less than 1%.
        let r = 10_000 * DEPTH;
        let before = deep_pool(r, r);
        let after = deep_pool(r + r / 100, r - r / 101); // k slightly grows
        assert!(check_flash_loan_arbitrage(&before, &after).is_ok());
    }

    #[test]
    fn zero_reserve_before_is_invalid_input() {
        let before = PoolSnapshot { reserve_a: 0, reserve_b: DEPTH };
        let after = deep_pool(DEPTH, DEPTH);
        assert_eq!(
            check_flash_loan_arbitrage(&before, &after),
            Err(ContractError::InvalidInput)
        );
    }

    #[test]
    fn zero_reserve_after_is_invalid_input() {
        let before = deep_pool(DEPTH, DEPTH);
        let after = PoolSnapshot { reserve_a: DEPTH, reserve_b: 0 };
        assert_eq!(
            check_flash_loan_arbitrage(&before, &after),
            Err(ContractError::InvalidInput)
        );
    }

    #[test]
    fn pool_drained_below_depth_rejected() {
        let before = deep_pool(DEPTH, DEPTH);
        // Both reserves drop below minimum — depth check fires first.
        let after = PoolSnapshot { reserve_a: DEPTH / 2, reserve_b: DEPTH / 2 };
        assert_eq!(
            check_flash_loan_arbitrage(&before, &after),
            Err(ContractError::FlashLoanArbitrageDetected)
        );
    }

    #[test]
    fn flash_loan_price_spike_rejected() {
        // Attacker borrows large amount, swaps token A → B driving the ratio
        // sky-high, profits via arbitrage, repays loan in same transaction.
        let r = 100 * DEPTH;
        let before = deep_pool(r, r);
        // After flash loan swap: reserve_a tripled, reserve_b near zero (but
        // we keep reserve_b above depth to isolate the ratio test).
        let after = deep_pool(r * 3, r * 2 / 3); // k roughly preserved but ratio slammed
        // ratio_before = 1; ratio_after = 4.5 → 350% deviation (way over 50%)
        assert_eq!(
            check_flash_loan_arbitrage(&before, &after),
            Err(ContractError::FlashLoanArbitrageDetected)
        );
    }

    #[test]
    fn k_decrease_on_deep_pool_rejected() {
        // Both reserves above depth, ratio fine, but k decreased.
        let r = 10 * DEPTH;
        let before = deep_pool(r, r);
        let after = deep_pool(r - 1, r - 1); // k strictly less
        assert_eq!(
            check_flash_loan_arbitrage(&before, &after),
            Err(ContractError::FlashLoanArbitrageDetected)
        );
    }

    #[test]
    fn large_balanced_swap_with_fees_passes() {
        // 10% swap with fee accrual: k grows by a tiny amount.
        let r = 1_000_000 * DEPTH;
        let amount_in = r / 10;
        // constant-product: amount_out < amount_in (floor division)
        let amount_out = (r * amount_in) / (r + amount_in) - 1;
        let before = deep_pool(r, r);
        let after = deep_pool(r + amount_in, r - amount_out);
        assert!(check_flash_loan_arbitrage(&before, &after).is_ok());
    }
}
