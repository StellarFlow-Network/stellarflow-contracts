//! End-to-end dynamic volatility stress suite for StellarFlow AMM pools.
//!
//! Issue: `StellarFlow-Network/stellarflow-contracts#1025`.
//!
//! # What this crate does
//!
//! Drives the **real** `contracts/amm` pool through deterministic, high-volatility
//! market sequences in a Soroban host environment and asserts the on-chain safety
//! properties the pool is supposed to hold while that happens:
//!
//! * the effective constant-product invariant `k_eff` never decreases across a
//!   swap sequence (`k_after >= k_before`),
//! * reserves, LP share supply and token balances never go negative, and the
//!   pool is never drained (no total-loss state),
//! * realised swap output is never better than the production quote, and price
//!   impact is monotonically worse as the same-size trade is repeated,
//! * the pool's real reserves stay fully backed by the tokens it custodies, and
//! * the virtual core floor still holds after a crash, so a later depositor is
//!   not handed a mispriced pool.
//!
//! # Explicitly NOT covered: the circuit breaker
//!
//! Issue #1025 also asks that an on-chain spot-price circuit breaker be
//! exercised under stress. **That criterion is not satisfied by this crate, and
//! cannot be from here.** The breaker lives in the root crate at
//! `src/amm/circuit_breaker.rs`, and that crate does not compile: the root
//! package carries unresolved breakage in `src/config.rs`,
//! `src/auth/mod.rs`, `src/bridge/timelock.rs`, `src/events/mod.rs`,
//! `src/errors/mod.rs`, and `src/storage.rs` is missing the
//! `PERSISTENT_TTL_THRESHOLD` constant the breaker references. Until the root
//! crate builds, the breaker cannot be compiled at all, let alone asserted
//! against.
//!
//! An earlier revision of this crate papered over that by re-exporting the
//! breaker through `#[path]` and feeding it a hand-written `host_stubs` surface.
//! That has been removed: a stubbed breaker would assert against a re-creation
//! of the risk control rather than the real one, which is exactly the kind of
//! claim this suite must not make. `src/breaker_gap.rs` records the gap and
//! asserts the properties that *are* exercisable instead.
//!
//! Fixing the root crate is a separate issue; wiring the breaker into this
//! suite is left for that work.
//!
//! # Why this is a separate crate
//!
//! The empty `[workspace]` table in `Cargo.toml` keeps this package out of the
//! broken root workspace, and its only non-dev dependency on the real code is
//! `amm-contract` by path, so everything asserted here is production logic. This
//! mirrors the isolation strategy the sibling `tests/fuzz` harness documents in
//! its own README.

#![allow(dead_code)]

pub mod breaker_gap;
pub mod pool;
pub mod rng;
pub mod scenario;
pub mod stress;

pub use pool::PoolHarness;
pub use rng::DeterministicRng;
pub use scenario::{assert_record_holds, scenario_selloff_then_rebalance, Scenario, GENESIS};
