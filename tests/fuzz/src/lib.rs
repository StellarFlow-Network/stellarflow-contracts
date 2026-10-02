//! Property-based fuzz harness for the AMM math engine.
//!
//! Implements the invariant-based fuzz testing suite specified in GitHub issues
//! [#625](https://github.com/StellarFlow-Network/stellarflow-contracts/issues/625) and
//! [#950](https://github.com/StellarFlow-Network/stellarflow-contracts/issues/950) —
//! "Build Invariant-Based Fuzz Testing Suite for AMM Math Engine".
//!
//! # Why a standalone crate?
//!
//! The AMM math layer (`src/amm/invariant.rs`, `src/amm/slippage.rs`) is pure:
//! none of the core invariant or arithmetic functions touch `soroban_sdk::Env`, so they
//! can be exercised from any host. We deliberately pull them in with
//! `#[path = "..."]` instead of depending on the main
//! `stellarflow-contracts` crate, so this harness builds and tests cleanly.
//!
//! # How to run
//!
//! ```text
//! cd tests/fuzz
//! cargo test --release
//! ```
//!
//! Run the dedicated 100,000 swap invariant verification:
//! ```text
//! cargo test --release prop_k_monotonicity_100k_swaps -- --nocapture
//! ```
//!
//! # Invariants covered
//!
//! 1. **No-Panic Boundary Tolerance** — every input combination (including
//!    adversarial extreme numerical boundaries) returns `Ok` or `Err`, never panics or overflows.
//! 2. **k-Monotonicity ($k_{after} \ge k_{before}$)** — for every generated swap whose output is
//!    successfully computed, `assert_invariant_stable` succeeds across 100,000+ randomized swap inputs.
//!    Pool reserves never lose value to rounding ($k_{after} \ge k_{before}$).
//! 3. **Dynamic Fee Calculation Safety** — verifies no integer underflow or overflow can occur during
//!    dynamic fee calculations (`calculate_and_deduct_fee`, `split_pool_fee`, corridor usage fee share,
//!    decayed fees, and volatility fee mappings).
//! 4. **Floor Rounding** — when `compute_swap_out` returns an output
//!    `y` for inputs `(x, r_in, r_out)`, it holds that
//!    `y * (r_in + x) <= r_out * x` (the textbook definition of
//!    floor-rounding towards zero).
//! 5. **Mint / Burn Roundtrip** — for any deposit
//!    `(a, b)` into a pool with reserves `(r_a, r_b)` and `total_shares`,
//!    burning the LP shares `S = compute_lp_shares(a, b, ...)` returns
//!    `(out_a, out_b) = compute_remove_liquidity(S, ...)` where
//!    `out_a <= a` and `out_b <= b`. Pool always keeps at least as much
//!    as it minted representation for.
//! 6. **Slippage Enforcement** — `enforce_slippage(amount_out, min)` is
//!    identity on success (`Ok(amount_out)` when `amount_out >= min`)
//!    and monotone in `min`.

// Stub the host crate's `ContractError` so that `use crate::ContractError;`
// in the included AMM source resolves cleanly without depending on the
// main `stellarflow-contracts` library.
#[allow(dead_code, non_camel_case_types)]
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum ContractError {
    InvalidInput,
    Overflow,
    DivisionByZero,
    SlippageExceeded,
    MathOverflow,
    InvariantViolation,
}

#[path = "../../../src/amm/invariant.rs"]
pub mod invariant;

#[path = "../../../src/amm/slippage.rs"]
pub mod slippage;

/// Pure fee math module mirroring the arithmetic logic of `src/fees.rs`
/// for host-side property testing without requiring Soroban SDK storage/host.
pub mod fee_math {
    use super::ContractError;

    pub const INTERIOR_SCALE: u128 = 100_000_000_000_000; // 10^14
    pub const FIXED_POINT_SCALE: u128 = 10_000_000;       // 10^7
    pub const MIN_DYNAMIC_FEE: u64 = 5_000;
    pub const DYNAMIC_FEE_SCALE: u64 = 10_000_000;

    pub fn calculate_and_deduct_fee(amount: u128, fee_bps: u32) -> Result<(u128, u128), ContractError> {
        let fee_amount = amount
            .checked_mul(fee_bps as u128)
            .ok_or(ContractError::Overflow)?
            .checked_div(10000)
            .ok_or(ContractError::DivisionByZero)?;

        let amount_after_fees = amount
            .checked_sub(fee_amount)
            .ok_or(ContractError::MathOverflow)?;

        Ok((amount_after_fees, fee_amount))
    }

    pub fn split_pool_fee(fee_amount: u128) -> (u128, u128) {
        let treasury_share = fee_amount / 5;
        let lp_share = fee_amount - treasury_share;
        (lp_share, treasury_share)
    }

    pub fn normalize_to_fixed_point_footprint(interior_value: u128) -> Result<u64, ContractError> {
        let normalized = interior_value
            .checked_div(INTERIOR_SCALE)
            .ok_or(ContractError::DivisionByZero)?;
        u64::try_from(normalized).map_err(|_| ContractError::Overflow)
    }

    pub fn compute_corridor_usage_fee_share(
        total_fee: u64,
        relayer_usage: u64,
        total_usage: u64,
    ) -> Result<u64, ContractError> {
        if total_usage == 0 {
            return Err(ContractError::DivisionByZero);
        }
        if total_fee == 0 || relayer_usage == 0 {
            return Ok(0);
        }

        let interior_numerator = u128::from(total_fee)
            .checked_mul(u128::from(relayer_usage))
            .ok_or(ContractError::Overflow)?
            .checked_mul(INTERIOR_SCALE)
            .ok_or(ContractError::Overflow)?;

        let interior_quotient = interior_numerator / u128::from(total_usage);
        normalize_to_fixed_point_footprint(interior_quotient)
    }

    pub fn calculate_decayed_fee(
        base_fee: u64,
        peak_fee: u64,
        lambda: u64,
        elapsed_seconds: u64,
    ) -> u64 {
        let base_fee = base_fee.max(MIN_DYNAMIC_FEE);
        if peak_fee <= base_fee || lambda == 0 || elapsed_seconds == 0 {
            return peak_fee.max(base_fee);
        }

        // Integer-safe exponential approximation bounded within [base_fee, peak_fee]
        let decay_term = (lambda as u128)
            .saturating_mul(elapsed_seconds as u128)
            / (DYNAMIC_FEE_SCALE as u128);
        if decay_term >= 50 {
            return base_fee;
        }

        let diff = (peak_fee - base_fee) as f64;
        let exp_decay = libm::exp(-((lambda as f64 / DYNAMIC_FEE_SCALE as f64) * elapsed_seconds as f64));
        if exp_decay.is_nan() || exp_decay.is_infinite() || exp_decay <= 0.0 {
            return base_fee;
        }
        let variable_fee = (diff * exp_decay) as u64;
        base_fee.saturating_add(variable_fee).max(MIN_DYNAMIC_FEE)
    }

    pub fn fee_for_volatility(
        vol_bps: u64,
        low_vol_bps: u64,
        high_vol_bps: u64,
        base_fee_bps: u32,
        max_fee_bps: u32,
    ) -> u32 {
        let low = low_vol_bps as u128;
        let high = high_vol_bps as u128;
        let v = vol_bps as u128;
        let base = base_fee_bps as u128;
        let max = max_fee_bps as u128;

        if high <= low {
            return base_fee_bps;
        }
        if v <= low {
            return base_fee_bps;
        }
        if v >= high {
            return max_fee_bps;
        }
        let span = max.saturating_sub(base);
        let ratio = (v.saturating_sub(low)) * span / (high - low);
        ((base + ratio) as u32).clamp(base_fee_bps, max_fee_bps)
    }
}

use proptest::prelude::*;

/// Strategy that draws u128 values from a heavy-weight boundary
/// distribution plus genuinely random draws, so the harness spends
/// most of its case budget on extreme numerical boundaries.
fn extreme_u128() -> impl Strategy<Value = u128> {
    prop_oneof![
        // 0 and 1 are the most adversarial small-magnitude cases.
        Just(0u128),
        Just(1u128),
        Just(0u128),
        Just(1u128),
        // 2, 1_000, 10_000_000 are mid-range boundary values.
        Just(2u128),
        Just(1_000u128),
        Just(10_000_000u128),
        // Near-maximum cases — the canonical u128 stress points.
        Just(u128::MAX),
        Just(u128::MAX - 1),
        Just(u128::MAX / 2),
        Just(u128::MAX / 4),
        // Truly random u128 draw.
        any::<u128>(),
    ]
}

/// Strategy for dynamic fee in basis points (0 to 10,000 bps + extreme u32 values).
fn dynamic_fee_bps() -> impl Strategy<Value = u32> {
    prop_oneof![
        Just(0u32),
        Just(1u32),
        Just(5u32),    // 0.05%
        Just(30u32),   // 0.30%
        Just(100u32),  // 1.00%
        Just(150u32),  // 1.50%
        Just(500u32),  // 5.00%
        Just(1_000u32), // 10.00%
        Just(10_000u32), // 100.00%
        Just(u32::MAX),
        0u32..=10_000u32,
        any::<u32>(),
    ]
}

/// Strategy constrained to small magnitudes so explicit floor comparisons fit u128.
fn small_u128() -> impl Strategy<Value = u128> {
    1u128..=1_000_000u128
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(10_000))]

    // ── Property 1: No-Panic Boundary Tolerance ──────────────────────────
    // Every function in the AMM layer must return Ok or Err for arbitrary
    // input, including the most adversarial boundary combinations.

    #[test]
    fn prop_no_panic_compute_swap_out(
        amount_in  in extreme_u128(),
        reserve_in in extreme_u128(),
        reserve_out in extreme_u128(),
    ) {
        let _ = invariant::compute_swap_out(amount_in, reserve_in, reserve_out);
    }

    #[test]
    fn prop_no_panic_compute_lp_shares(
        amount_a    in extreme_u128(),
        amount_b    in extreme_u128(),
        reserve_a   in extreme_u128(),
        reserve_b   in extreme_u128(),
        total_shares in extreme_u128(),
    ) {
        let _ = invariant::compute_lp_shares(
            amount_a,
            amount_b,
            reserve_a,
            reserve_b,
            total_shares,
        );
    }

    #[test]
    fn prop_no_panic_compute_remove_liquidity(
        shares       in extreme_u128(),
        total_shares in extreme_u128(),
        reserve_a    in extreme_u128(),
        reserve_b    in extreme_u128(),
    ) {
        let _ = invariant::compute_remove_liquidity(
            shares,
            total_shares,
            reserve_a,
            reserve_b,
        );
    }

    #[test]
    fn prop_no_panic_assert_invariant_stable(
        reserve_in_before in extreme_u128(),
        reserve_out_before in extreme_u128(),
        amount_in in extreme_u128(),
        amount_out in extreme_u128(),
    ) {
        let _ = invariant::assert_invariant_stable(
            reserve_in_before,
            reserve_out_before,
            amount_in,
            amount_out,
        );
    }

    // ── Property 2: k-Monotonicity ───────────────────────────────────────
    // The constant-product invariant k = r_in * r_out must never decrease
    // across an accepted swap (k_after >= k_before). assert_invariant_stable
    // is the contract's canonical check.

    #[test]
    fn prop_k_monotonicity(
        reserve_in  in extreme_u128(),
        reserve_out in extreme_u128(),
        amount_in   in extreme_u128(),
    ) {
        if let Ok(amount_out) =
            invariant::compute_swap_out(amount_in, reserve_in, reserve_out)
        {
            prop_assert!(
                invariant::assert_invariant_stable(
                    reserve_in,
                    reserve_out,
                    amount_in,
                    amount_out,
                )
                .is_ok(),
                "AMM k invariant regressed: \
                 reserve_in={} reserve_out={} amount_in={} amount_out={}",
                reserve_in, reserve_out, amount_in, amount_out,
            );
        }
    }

    // ── Property 3: Dynamic Fee Safety & Invariants ──────────────────────
    // Verify no integer underflow or overflow conditions can occur during dynamic fee calculations,
    // and that fees are non-negative, amount_after_fees + fee_amount == amount,
    // and fee splits preserve total fee amount without loss.

    #[test]
    fn prop_dynamic_fee_calculation_no_overflow(
        amount in extreme_u128(),
        fee_bps in dynamic_fee_bps(),
    ) {
        if fee_bps <= 10_000 {
            if let Ok((amount_after_fees, fee_amount)) = fee_math::calculate_and_deduct_fee(amount, fee_bps) {
                // Assert no underflow/overflow and exact conservation:
                prop_assert!(
                    amount_after_fees <= amount,
                    "amount_after_fees {} exceeds initial amount {}",
                    amount_after_fees, amount
                );
                prop_assert!(
                    fee_amount <= amount,
                    "fee_amount {} exceeds initial amount {}",
                    fee_amount, amount
                );
                prop_assert_eq!(
                    amount_after_fees.checked_add(fee_amount),
                    Some(amount),
                    "amount_after_fees + fee_amount != amount"
                );

                // Verify fee split between LP (80%) and Treasury (20%)
                let (lp_share, treasury_share) = fee_math::split_pool_fee(fee_amount);
                prop_assert_eq!(
                    lp_share.checked_add(treasury_share),
                    Some(fee_amount),
                    "lp_share + treasury_share != fee_amount"
                );
                prop_assert!(
                    lp_share >= treasury_share * 3,
                    "LP share should be ~80% and treasury ~20%"
                );
            }
        } else {
            // For fee_bps > 10,000 or overflow scenarios, calculate_and_deduct_fee must safely return Err or succeed without panicking.
            let _ = fee_math::calculate_and_deduct_fee(amount, fee_bps);
        }
    }

    #[test]
    fn prop_dynamic_fee_corridor_and_decay_no_panic(
        total_fee in any::<u64>(),
        usage_a in any::<u64>(),
        usage_b in any::<u64>(),
        base_fee in any::<u64>(),
        peak_fee in any::<u64>(),
        lambda in any::<u64>(),
        elapsed in any::<u64>(),
        vol_bps in any::<u64>(),
    ) {
        let _ = fee_math::compute_corridor_usage_fee_share(total_fee, usage_a, usage_b);
        let decayed = fee_math::calculate_decayed_fee(base_fee, peak_fee, lambda, elapsed);
        prop_assert!(decayed >= fee_math::MIN_DYNAMIC_FEE);

        let fee = fee_math::fee_for_volatility(vol_bps, 100, 1000, 30, 150);
        prop_assert!(fee >= 30 && fee <= 150);
    }

    // ── Property 4: Constant Product Invariant Under Dynamic Fees ────────
    // When dynamic fee is deducted from trade amount before constant-product swap,
    // k_after >= k_before must strictly hold.

    #[test]
    fn prop_swap_with_dynamic_fee_k_monotonicity(
        reserve_in in extreme_u128(),
        reserve_out in extreme_u128(),
        amount_in in extreme_u128(),
        fee_bps in 1u32..=1_000u32, // 0.01% to 10.00%
    ) {
        if amount_in > 0 && reserve_in > 0 && reserve_out > 0 {
            if let Ok((net_amount_in, _fee)) = fee_math::calculate_and_deduct_fee(amount_in, fee_bps) {
                if net_amount_in > 0 {
                    if let Ok(amount_out) = invariant::compute_swap_out(net_amount_in, reserve_in, reserve_out) {
                        // Full amount_in is deposited into reserve_in (including fee), while amount_out is withdrawn from reserve_out
                        let k_stable = invariant::assert_invariant_stable(
                            reserve_in,
                            reserve_out,
                            amount_in,
                            amount_out,
                        );
                        prop_assert!(
                            k_stable.is_ok(),
                            "Invariant violated with dynamic fee: r_in={} r_out={} amt_in={} fee_bps={} amt_out={}",
                            reserve_in, reserve_out, amount_in, fee_bps, amount_out
                        );
                    }
                }
            }
        }
    }

    // ── Property 5: Floor Rounding ──────────────────────────────────────
    // The contract must use floor division so the pool's k can never
    // grow in the pool's favour. Algebraically:
    //   y = compute_swap_out(x, r_in, r_out) => y * (r_in + x) <= r_out * x

    #[test]
    fn prop_swap_out_floor_rounding(
        amount_in  in small_u128(),
        reserve_in in small_u128(),
        reserve_out in small_u128(),
    ) {
        if let Ok(amount_out) =
            invariant::compute_swap_out(amount_in, reserve_in, reserve_out)
        {
            let denom = reserve_in + amount_in;
            let y_times_d = amount_out
                .checked_mul(denom)
                .expect("amount_out * denom fits in u128 within small_u128 range");
            let r_times_x = reserve_out
                .checked_mul(amount_in)
                .expect("reserve_out * amount_in fits in u128 within small_u128 range");

            prop_assert!(
                y_times_d <= r_times_x,
                "floor rounding violated: \
                 amount_out={} reserve_in={} reserve_out={} amount_in={} \
                 => y*d={} > r*x={}",
                amount_out, reserve_in, reserve_out, amount_in,
                y_times_d, r_times_x,
            );
        }
    }

    // ── Property 6: Mint / Burn Roundtrip ────────────────────────────────
    // For any successful mint, the corresponding burn must return at most
    // (a, b): the pool never prints free money and rounding favours LPs.

    #[test]
    fn prop_mint_burn_roundtrip(
        amount_a     in extreme_u128(),
        amount_b     in extreme_u128(),
        reserve_a    in extreme_u128(),
        reserve_b    in extreme_u128(),
        total_shares in extreme_u128(),
    ) {
        let minted = invariant::compute_lp_shares(
            amount_a, amount_b, reserve_a, reserve_b, total_shares,
        );
        if let Ok(shares) = minted {
            let removed = invariant::compute_remove_liquidity(
                shares, total_shares, reserve_a, reserve_b,
            );
            if let Ok((out_a, out_b)) = removed {
                prop_assert!(
                    out_a <= amount_a,
                    "mint/burn roundtrip printed money: out_a={} > amount_a={}",
                    out_a, amount_a,
                );
                prop_assert!(
                    out_b <= amount_b,
                    "mint/burn roundtrip printed money: out_b={} > amount_b={}",
                    out_b, amount_b,
                );
            }
        }
    }

    // ── Property 7: Slippage Enforcement ─────────────────────────────────
    // enforce_slippage must be identity on Ok and reject by exactly one
    // error variant. We assert the complete input/output mapping.

    #[test]
    fn prop_slippage_enforcement(
        amount_out in extreme_u128(),
        min        in extreme_u128(),
    ) {
        let expected = if amount_out >= min {
            Ok(amount_out)
        } else {
            Err(ContractError::SlippageExceeded)
        };
        prop_assert_eq!(
            slippage::enforce_slippage(amount_out, min),
            expected,
            "slippage enforcement inconsistent: \
             amount_out={} min={} expected={:?}",
            amount_out, min, expected,
        );
    }
}

// ── Property 8: Dedicated 100,000 Randomized Swap Invariant Test ────────
// Direct verification of k_after >= k_before across 100,000 randomized swap inputs
// fulfilling Issue #950 requirements.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(100_000))]

    #[test]
    fn prop_k_monotonicity_100k_swaps(
        reserve_in  in extreme_u128(),
        reserve_out in extreme_u128(),
        amount_in   in extreme_u128(),
    ) {
        if let Ok(amount_out) = invariant::compute_swap_out(amount_in, reserve_in, reserve_out) {
            prop_assert!(
                invariant::assert_invariant_stable(
                    reserve_in,
                    reserve_out,
                    amount_in,
                    amount_out,
                )
                .is_ok(),
                "100k swap run invariant failure: r_in={} r_out={} amt_in={} amt_out={}",
                reserve_in, reserve_out, amount_in, amount_out
            );
        }
    }
}

