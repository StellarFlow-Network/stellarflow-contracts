//! Coverage-guided fuzz target for swap math and dynamic fees.
//!
//! Mirrors `prop_no_panic_compute_swap_out`, `prop_k_monotonicity`, and
//! `prop_swap_with_dynamic_fee_k_monotonicity` in `tests/fuzz/src/lib.rs`.
//! libFuzzer mutates the structured `SwapInputs` to drive coverage-guided exploration of
//! boundary regions the random proptest sampler may not reach for hours.
//!
//! Self-contained: the AMM module is pulled in with `#[path = "..."]`,
//! and the host's `ContractError` is supplied via `common.rs`.

#![no_main]
use libfuzzer_sys::fuzz_target;
use arbitrary::Arbitrary;

#[path = "common.rs"]
mod common;

#[path = "../../../../src/amm/invariant.rs"]
mod invariant;

#[derive(Arbitrary, Debug)]
struct SwapInputs {
    amount_in: u128,
    reserve_in: u128,
    reserve_out: u128,
    fee_bps: u32,
}

fn calculate_and_deduct_fee(amount: u128, fee_bps: u32) -> Result<(u128, u128), common::ContractError> {
    let fee_amount = amount
        .checked_mul(fee_bps as u128)
        .ok_or(common::ContractError::Overflow)?
        .checked_div(10000)
        .ok_or(common::ContractError::DivisionByZero)?;

    let amount_after_fees = amount
        .checked_sub(fee_amount)
        .ok_or(common::ContractError::Overflow)?;

    Ok((amount_after_fees, fee_amount))
}

fuzz_target!(|inputs: SwapInputs| {
    let SwapInputs {
        amount_in,
        reserve_in,
        reserve_out,
        fee_bps,
    } = inputs;

    // Property 1: No-panic boundary tolerance (mirrors proptest).
    let _ = invariant::compute_swap_out(amount_in, reserve_in, reserve_out);

    // Property 2: k-Monotonicity. For every successful swap output the
    // contract's `assert_invariant_stable` (delegated to its internal
    // U256 arithmetic) must succeed (k_after >= k_before).
    if let Ok(amount_out) =
        invariant::compute_swap_out(amount_in, reserve_in, reserve_out)
    {
        let result = invariant::assert_invariant_stable(
            reserve_in,
            reserve_out,
            amount_in,
            amount_out,
        );
        assert!(
            result.is_ok(),
            "k-invariant violated: reserve_in={} reserve_out={} \
             amount_in={} amount_out={} => {:?}",
            reserve_in, reserve_out, amount_in, amount_out, result,
        );
    }

    // Property 3: Dynamic Fee Swap Invariant.
    // Dynamic fee deduction must not underflow/overflow and resulting swap must maintain k_after >= k_before.
    let bounded_fee_bps = fee_bps % 10_001; // 0% to 100%
    if let Ok((net_amount_in, fee_amount)) = calculate_and_deduct_fee(amount_in, bounded_fee_bps) {
        assert!(net_amount_in <= amount_in);
        assert!(fee_amount <= amount_in);
        assert_eq!(net_amount_in + fee_amount, amount_in);

        if net_amount_in > 0 && reserve_in > 0 && reserve_out > 0 {
            if let Ok(amount_out) = invariant::compute_swap_out(net_amount_in, reserve_in, reserve_out) {
                let k_res = invariant::assert_invariant_stable(
                    reserve_in,
                    reserve_out,
                    amount_in,
                    amount_out,
                );
                assert!(
                    k_res.is_ok(),
                    "k invariant violated under dynamic fee: r_in={} r_out={} amt_in={} fee_bps={} amt_out={}",
                    reserve_in, reserve_out, amount_in, bounded_fee_bps, amount_out
                );
            }
        }
    }
});

