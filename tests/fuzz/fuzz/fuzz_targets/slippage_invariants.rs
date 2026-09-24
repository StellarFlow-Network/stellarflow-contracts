//! Coverage-guided fuzz target for slippage enforcement.
//!
//! Mirrors `prop_slippage_enforcement` in `tests/fuzz/src/lib.rs`.
//! Verifies the complete input/output mapping of `enforce_slippage`.
//!
//! Self-contained: AMM module + `ContractError` stub via `#[path]`.

#![no_main]
use libfuzzer_sys::fuzz_target;
use arbitrary::Arbitrary;

#[path = "common.rs"]
mod common;

// `slippage.rs` (included below) uses `use crate::ContractError;` — that
// resolves to *this* fuzz target's crate root, where `mod common;` brings
// `ContractError` into scope. We reference it directly as `ContractError`,
// not as `slippage::ContractError` (the enum does not live in `slippage`).
use common::ContractError;

#[path = "../../../../src/amm/slippage.rs"]
mod slippage;

#[derive(Arbitrary, Debug)]
struct SlippageInputs {
    amount_out: u128,
    min: u128,
}

fuzz_target!(|inputs: SlippageInputs| {
    let SlippageInputs { amount_out, min } = inputs;

    let expected = if amount_out >= min {
        Ok(amount_out)
    } else {
        Err(ContractError::SlippageExceeded)
    };

    let actual = slippage::enforce_slippage(amount_out, min);
    assert_eq!(
        actual, expected,
        "slippage enforcement inconsistent: \
         amount_out={} min={} expected={:?} got={:?}",
        amount_out, min, expected, actual,
    );
});
