# Property-Based Fuzz Harness for AMM Math Invariants

**Closes:** [#625](https://github.com/StellarFlow-Network/stellarflow-contracts/issues/625) — *Fuzz-Testing | Invariant Swap Validation Fuzz Harness*

This crate contains a property-based ("fuzz-style") harness for the AMM
math layer of `stellarflow-contracts`. It directly satisfies the issue
spec:

| Spec requirement | Implementation |
| --- | --- |
| *Run cargo-fuzz target through 10,000 iterations without unexpected panics* | Every property runs exactly `10_000` cases via `ProptestConfig::with_cases(10_000)`. |
| *Assert pool math invariants hold under extreme numerical boundaries* | Strategy `extreme_u128()` over-samples boundary values (`0`, `1`, `2`, `1_000`, `10_000_000`, `u128::MAX`, `u128::MAX-1`, `u128::MAX/2`, `u128::MAX/4`) compared to uniform draws. Used by **all five properties**, except `prop_swap_out_floor_rounding` (which uses the structurally safe `small_u128()` so its explicit arithmetic comparison never overflows `u128`). |

## Why proptest and not `cargo-fuzz`?

`cargo-fuzz` requires a nightly toolchain and pulls in `libfuzzer-sys`
plus a dedicated fuzz binary that the rest of the project's CI does not
exercise. `proptest` integrates with the standard `cargo test`
workflow on **stable** Rust, supports deterministic test runs, and gives
us shrinking for free when a property fails. The 10,000-iteration
requirement maps one-to-one to `ProptestConfig::with_cases(10_000)`.

If downstream contributors want coverage-guided fuzzing on nightly
Rust, `cargo-fuzz` can be added as a follow-up — see the "Future
work" section below.

## Why is this a standalone crate?

The included AMM modules (`src/amm/invariant.rs`, `src/amm/slippage.rs`)
are pure functions — none of them take a `soroban_sdk::Env`. The
harness `#[path]`-includes those two files directly, defines a local
stub `ContractError`, and never touches `stellarflow-contracts`'s
`src/lib.rs`. That makes the fuzz harness independent of the main
crate's compile state, so it can build and run even while the main
crate carries outstanding merge-time artifacts.

| Path | Purpose |
| --- | --- |
| `Cargo.toml` | Standalone `stellarflow-contracts-fuzz` package. |
| `src/lib.rs` | Stub `ContractError` + `#[path]`-included AMM modules + `proptest!` block. |
| `README.md` | This file. |

## How to run

```bash
cd tests/fuzz
cargo test --release
```

The first run takes a few seconds; subsequent runs are cache-warm and
finish in well under a second.

To bump the case count for longer-running nightly runs, set
`PROPTEST_CASES`:

```bash
PROPTEST_CASES=1_000_000 cargo test --release
```

## Properties covered

See the `proptest!` block in `src/lib.rs` for full source. In summary:

1. **No-Panic Boundary Tolerance** (`prop_no_panic_*`) — every
   AMM function returns `Ok` or `Err` for any input draw, including
   `u128::MAX` extremes. Satisfies the issue's
   *"10,000 iterations without unexpected panics"* clause.
2. **k-Monotonicity** (`prop_k_monotonicity`) — for every successful
   swap, the contract's `assert_invariant_stable` re-check passes:
   the constant-product invariant k never decreases.
3. **Floor Rounding** (`prop_swap_out_floor_rounding`) — verifies the
   textbook floor-division identity.
4. **Mint / Burn Roundtrip** (`prop_mint_burn_roundtrip`) — burning the
   shares minted by a deposit returns no more than the deposit, never
   printing free money.
5. **Slippage Enforcement** (`prop_slippage_enforcement`) — the slippage
   guard is identity on `Ok` and rejects by exactly one error variant
   on `Err`.

## Future work

A coverage-guided `cargo-fuzz` target with `libfuzzer-sys` can be added
as a follow-up for nightly-Rust users who want compiler-explorer-grade
mutation feedback. The same properties map cleanly to a
`fuzz_target = "..."` macro under `tests/fuzz/fuzz_targets/`. Pull
request welcome.
