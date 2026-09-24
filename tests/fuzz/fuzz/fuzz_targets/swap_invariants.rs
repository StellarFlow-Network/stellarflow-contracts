//! Coverage-guided fuzz target for swap math.
//!
//! Mirrors the `prop_no_panic_compute_swap_out` and `prop_k_monotonicity`
//! properties in `tests/fuzz/src/lib.rs`. libFuzzer mutates the
//! structured `SwapInputs` to drive coverage-guided exploration of
//! boundary regions the random proptest sampler may not reach for hours.
//!
//! Self-contained: the AMM module is pulled in with `#[path = "..."]`,
//! and the host's `ContractError` is supplied via `common.rs` so the
//! fuzz crate does not need to depend on the proptest crate.

#![no_main]
use libfuzzer_sys::fuzz_target;
use arbitrary::Arbitrary;

#[path = "common.rs"]
mod common;

// `use crate::ContractError;` inside the included `invariant.rs` resolves
// to *this* fuzz target's crate root, where `mod common;` declares
// `ContractError`. We do not need to `use` it into this scope — neither
// this target nor its assertions reference `ContractError` by name.

#[path = "../../../../src/amm/invariant.rs"]
mod invariant;

#[derive(Arbitrary, Debug)]
struct SwapInputs {
    amount_in: u128,
    reserve_in: u128,
    reserve_out: u128,
}

fuzz_target!(|inputs: SwapInputs| {
    let SwapInputs {
        amount_in,
        reserve_in,
        reserve_out,
    } = inputs;

    // Property 1: No-panic boundary tolerance (mirrors proptest).
    let _ = invariant::compute_swap_out(amount_in, reserve_in, reserve_out);

    // Property 2: k-Monotonicity. For every successful swap output the
    // contract's `assert_invariant_stable` (delegated to its internal
    // U256 arithmetic) must succeed. This is the real invariant; the
    // trivial `amount_out <= reserve_out` bound the previous draft
    // asserted is structurally implied by `compute_swap_out`'s
    // floor-division implementation and adds zero coverage value.
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
});
