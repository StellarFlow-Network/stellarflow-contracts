//! Property-based fuzz harness for the AMM math layer.
//!
//! Implements the harness specified in GitHub issue
//! [#625](https://github.com/StellarFlow-Network/stellarflow-contracts/issues/625) —
//! "Fuzz-Testing | Invariant Swap Validation Fuzz Harness" (assigned to
//! `@Syringe7`, Impact Severity: High).
//!
//! # Why a standalone crate?
//!
//! The AMM math layer (`src/amm/invariant.rs`, `src/amm/slippage.rs`) is pure:
//! none of the functions in those modules touch `soroban_sdk::Env`, so they
//! can be exercised from any host. We deliberately pull them in with
//! `#[path = "..."]` instead of depending on the main
//! `stellarflow-contracts` crate, so this harness builds and tests even
//! while the main crate has outstanding compile-time merge artifacts (see
//! the open issues closing this PR covers). No public-API changes to the
//! AMM modules are required.
//!
//! # How to run
//!
//! ```text
//! cd tests/fuzz
//! cargo test --release
//! ```
//!
//! Each property runs the issue's mandated **10,000 cases**, mixed
//! genuinely-random u128 draws with deliberately-chosen extreme
//! boundaries (`0`, `1`, `2`, `u128::MAX`, `u128::MAX - 1`,
//! `u128::MAX / 2`, `u128::MAX / 4`, `10_000_000`). Override the case
//! count via the `PROPTEST_CASES` environment variable for longer runs.
//!
//! # Invariants covered
//!
//! 1. **No-Panic Boundary Tolerance** — every input triple (including
//!    the boundaries above) returns `Ok` or `Err`, never panics.
//! 2. **k-Monotonicity** — for every generated swap whose output is
//!    successfully computed, `assert_invariant_stable` succeeds. Pool
//!    reserves never lose value to rounding.
//! 3. **Floor Rounding** — when `compute_swap_out` returns an output
//!    `y` for inputs `(x, r_in, r_out)`, it holds that
//!    `y * (r_in + x) <= r_out * x` (the textbook definition of
//!    floor-rounding towards zero).
//! 4. **Mint / Burn Roundtrip** — for any deposit
//!    `(a, b)` into a pool with reserves `(r_a, r_b)` and `total_shares`,
//!    burning the LP shares `S = compute_lp_shares(a, b, ...)` returns
//!    `(out_a, out_b) = compute_remove_liquidity(S, ...)` where
//!    `out_a <= a` and `out_b <= b`. Pool always keeps at least as much
//!    as it minted representation for.
//! 5. **Slippage Enforcement** — `enforce_slippage(amount_out, min)` is
//!    identity on success (`Ok(amount_out)` when `amount_out >= min`)
//!    and monotone in `min`. The pair of cases covers the divergent
//!    edges of the boundary (`==` must succeed, `<` must fail).

// Stub the host crate's `ContractError` so that `use crate::ContractError;`
// in the included AMM source resolves cleanly without depending on the
// main `stellarflow-contracts` library. Only the variants that the AMM
// modules are observed to reference are listed here.
#[allow(dead_code, non_camel_case_types)]
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum ContractError {
    InvalidInput,
    Overflow,
    DivisionByZero,
    SlippageExceeded,
}

// Pull in the AMM modules through `#[path]` so the harness compiles
// even when the main contract crate has unresolved compile issues. The
// `pub` re-exports keep the API identical to the original crate for
// consumers who would treat this crate as a one-for-one substitute.
// tests/fuzz/src/lib.rs is two directories below the repo root:
//   tests/fuzz/src/  ->  tests/fuzz/  ->  tests/  ->  <repo root>
// so the path needs three `..` segments to reach src/amm/. An earlier
// version used only two, which resolved to tests/src/amm/ and failed
// to compile. The cargo-fuzz targets in tests/fuzz/fuzz/fuzz_targets/
// are one level deeper and correctly use four `..` segments.
#[path = "../../../src/amm/invariant.rs"]
pub mod invariant;

#[path = "../../../src/amm/slippage.rs"]
pub mod slippage;

use proptest::prelude::*;

/// Strategy that draws u128 values from a heavy-weight boundary
/// distribution plus genuinely random draws, so the harness spends
/// most of its 10,000-case budget on the cases the issue spec calls
/// out ("extreme numerical boundaries").
///
/// proptest 1.4's `prop_oneof!` macro accepts bare strategies only;
/// the `strategy => weight` syntax is not supported (it generates a
/// `TupleUnion` whose `Value` is not `u128`). Uniform sampling across
/// these nine boundary cases plus `any::<u128>()` still gives 90%
/// boundary over-sampling, which satisfies the issue spec. The
/// `Just(u128::MAX / k)` near-maximum bounds are the canonical
/// "near-maximum but arithmetic still succeeds" stress points for
/// `u128` products, so they are weighted by repetition: the smaller
/// boundary values are listed twice to over-sample them relative to
/// the larger boundary values, approximating the original weight
/// intent without using `=> weight` syntax.
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

/// Strategy constrained to small magnitudes so the explicit
/// `amount_out * denominator <= reserve_out * amount_in` floor-rounding
/// comparison never overflows `u128`. Used only for the explicit
/// floor-division property; the boundary-stress properties above use
/// `extreme_u128` and rely on the producer's own `U256`-based
/// `assert_invariant_stable` for soundness (so they don't need a
/// direct arithmetic comparison).
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
    // across an accepted swap. assert_invariant_stable is the contract's
    // canonical check, so we delegate to it on every generation.

    #[test]
    fn prop_k_monotonicity(
        reserve_in  in extreme_u128(),
        reserve_out in extreme_u128(),
        amount_in   in extreme_u128(),
    ) {
        // Cases where compute_swap_out returns Err are naturally skipped:
        // we only need to verify the invariant on successful swaps, not
        // on rejected inputs. assert_invariant_stable is the producer's
        // canonical U256-based check, so it stays sound for the
        // extreme cases that do produce an output.
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

    // ── Property 3: Floor Rounding ──────────────────────────────────────
    // The contract must use floor division so the pool's k can never
    // grow in the pool's favour.  Algebraically:
    //
    //   y = compute_swap_out(x, r_in, r_out)
    //       =>  y * (r_in + x)  <=  r_out * x
    //
    // i.e. y is at most floor(r_out * x / (r_in + x)).

    #[test]
    fn prop_swap_out_floor_rounding(
        amount_in  in small_u128(),
        reserve_in in small_u128(),
        reserve_out in small_u128(),
    ) {
        // Inputs are bounded via `small_u128()` so both products below
        // fit comfortably in u128 and the explicit check is always
        // reachable. The structural k-monotonicity check at Property 2
        // covers the extreme input ranges via the producer's U256 path.
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

    // ── Property 4: Mint / Burn Roundtrip ────────────────────────────────
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
        // Boundary inputs from extreme_u128 exhaustively probe zero,
        // ones, max-u128, and near-max values. Err returns from the
        // mint or burn path are skipped naturally.
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

    // ── Property 5: Slippage Enforcement ─────────────────────────────────
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
