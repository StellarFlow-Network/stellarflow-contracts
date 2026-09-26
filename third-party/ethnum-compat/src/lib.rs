//! Compatibility shim for `ethnum`, pinned by `soroban-env-common 20.0.0` to
//! `=1.5.0`.
//!
//! ethnum 1.5.0 builds the errors returned by its integer conversions with
//! `mem::transmute`, including `unsafe { mem::transmute(()) }` to produce a
//! `TryFromIntError`. That relied on `TryFromIntError` being zero-sized. On
//! current stable Rust it is one byte, so the transmute is a hard
//! `E0512` error and the whole workspace fails to build.
//!
//! Upstream fixed the constructors in ethnum 1.5.3, but `soroban-env-common`
//! requires exactly `1.5.0`, so the fix can never be selected through normal
//! version resolution. This crate advertises the pinned `1.5.0` version and
//! re-exports the fixed upstream implementation unchanged, which lets the
//! workspace build while keeping the public `ethnum` API identical.
//!
//! It is selected via `[patch.crates-io]` in the workspace root `Cargo.toml`.
//! Once `soroban-env-common` allows an `ethnum` version `>= 1.5.3`, the patch
//! becomes a no-op and both this directory and the patch entry can be deleted.

#![no_std]

pub use ethnum_upstream::*;
