//! Coverage-guided fuzz target for LP math.
//!
//! Mirrors `prop_no_panic_compute_lp_shares`,
//! `prop_no_panic_compute_remove_liquidity`, and
//! `prop_mint_burn_roundtrip` in `tests/fuzz/src/lib.rs`.
//!
//! Self-contained: AMM module + `ContractError` stub via `#[path]`.

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
struct LpInputs {
    amount_a: u128,
    amount_b: u128,
    reserve_a: u128,
    reserve_b: u128,
    total_shares: u128,
}

fuzz_target!(|inputs: LpInputs| {
    let LpInputs {
        amount_a,
        amount_b,
        reserve_a,
        reserve_b,
        total_shares,
    } = inputs;

    // No-panic tolerance for both LP operations.
    let _ = invariant::compute_lp_shares(
        amount_a,
        amount_b,
        reserve_a,
        reserve_b,
        total_shares,
    );
    let _ = invariant::compute_remove_liquidity(
        amount_a,
        total_shares,
        reserve_a,
        reserve_b,
    );

    // Mint / burn roundtrip — burning the shares minted by a deposit
    // must return at most the deposit, never printing free money.
    if let Ok(shares) = invariant::compute_lp_shares(
        amount_a,
        amount_b,
        reserve_a,
        reserve_b,
        total_shares,
    ) {
        if let Ok((out_a, out_b)) = invariant::compute_remove_liquidity(
            shares,
            total_shares,
            reserve_a,
            reserve_b,
        ) {
            assert!(
                out_a <= amount_a,
                "LP roundtrip printed money: out_a {} > amount_a {}",
                out_a, amount_a,
            );
            assert!(
                out_b <= amount_b,
                "LP roundtrip printed money: out_b {} > amount_b {}",
                out_b, amount_b,
            );
        }
    }
});
