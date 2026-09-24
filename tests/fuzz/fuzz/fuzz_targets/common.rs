//! Shared stub module for the cargo-fuzz targets under `fuzz_targets/`.
//!
//! The included AMM source files reference `crate::ContractError` from
//! `use crate::ContractError;`. Because each cargo-fuzz target is a
//! standalone binary (each one declared via `[[bin]]` in
//! `tests/fuzz/fuzz/Cargo.toml`), they don't share a crate root. Instead
//! each target file `#[path]`-includes this `common.rs` to provide a
//! matching stub for the AMM source's `use crate::ContractError;`.

#![allow(dead_code, non_camel_case_types)]

/// Local stub matching the variants the AMM modules reference.
/// Only `InvalidInput`, `Overflow`, `DivisionByZero`, and
/// `SlippageExceeded` are observed to be referenced by the fuzzed
/// functions; new variants can be added here without touching the
/// included AMM source.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum ContractError {
    InvalidInput,
    Overflow,
    DivisionByZero,
    SlippageExceeded,
}
