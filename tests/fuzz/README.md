# Invariant-Based Fuzz Testing Suite for AMM Math Engine

**Closes:** 
- [#625](https://github.com/StellarFlow-Network/stellarflow-contracts/issues/625) — *Fuzz-Testing | Invariant Swap Validation Fuzz Harness*
- [#950](https://github.com/StellarFlow-Network/stellarflow-contracts/issues/950) — *🧪 Build Invariant-Based Fuzz Testing Suite for AMM Math Engine*

This crate contains an invariant-based, property-driven fuzz harness for the AMM
math layer and dynamic fee arithmetic of `stellarflow-contracts`. It directly satisfies the issue
specs:

| Spec requirement | Implementation |
| --- | --- |
| *Implement property-based fuzz tests verifying constant product invariants under randomized input parameters* | Proptest strategies (`extreme_u128`, `dynamic_fee_bps`, `small_u128`) exhaustively stress random draws and adversarial numerical boundaries across all AMM operations. |
| *Assert $k_{after} \ge k_{before}$ across 100,000 randomized swap inputs* | `prop_k_monotonicity_100k_swaps` runs `100_000` iterations asserting constant-product invariant preservation ($k_{after} \ge k_{before}$) using exact 256-bit arithmetic. |
| *Verify no integer underflow or overflow conditions can occur during dynamic fee calculations* | `prop_dynamic_fee_calculation_no_overflow`, `prop_dynamic_fee_corridor_and_decay_no_panic`, and `prop_swap_with_dynamic_fee_k_monotonicity` verify overflow/underflow resistance and exact conservation across arbitrary values. |
| *Run fuzz suite through 10,000 iterations without unexpected panics* | Properties run `10_000` iterations per property by default via `ProptestConfig::with_cases(10_000)`. |

## Why proptest and not only `cargo-fuzz`?

`proptest` integrates with the standard `cargo test` workflow on **stable** Rust,
supports deterministic test runs, and provides automated test shrinking when a property
fails. A companion nightly `cargo-fuzz` suite is also provided under `tests/fuzz/fuzz/` for
coverage-guided mutation fuzzing.

## How to run

```bash
cd tests/fuzz
cargo test --release
```

To run the dedicated 100,000-case swap invariant property test:

```bash
cargo test --release prop_k_monotonicity_100k_swaps -- --nocapture
```

To customize the case count for longer-running runs, set `PROPTEST_CASES`:

```bash
PROPTEST_CASES=1_000_000 cargo test --release
```

## Properties covered

See the `proptest!` blocks in `src/lib.rs` for full source:

1. **No-Panic Boundary Tolerance** (`prop_no_panic_*`) — every
   AMM function returns `Ok` or `Err` for any input draw, including
   `u128::MAX` extremes.
2. **k-Monotonicity** (`prop_k_monotonicity` & `prop_k_monotonicity_100k_swaps`) — for every successful
   swap, `assert_invariant_stable` verifies $k_{after} \ge k_{before}$ under exact 256-bit arithmetic across 100,000 randomized inputs.
3. **Dynamic Fee Calculation Safety** (`prop_dynamic_fee_calculation_no_overflow`, `prop_dynamic_fee_corridor_and_decay_no_panic`) — dynamic fee deductions, corridor splits, exponential decay, and volatility fee mappings never overflow or underflow.
4. **Constant Product Invariant under Dynamic Fees** (`prop_swap_with_dynamic_fee_k_monotonicity`) — verifies $k_{after} \ge k_{before}$ holds across randomized swap inputs with dynamic fees applied.
5. **Floor Rounding** (`prop_swap_out_floor_rounding`) — verifies the
   textbook floor-division identity ($y \cdot (r_{in} + x) \le r_{out} \cdot x$).
6. **Mint / Burn Roundtrip** (`prop_mint_burn_roundtrip`) — burning the
   shares minted by a deposit returns no more than the deposit, never
   printing free money.
7. **Slippage Enforcement** (`prop_slippage_enforcement`) — the slippage
   guard is identity on `Ok` and rejects by `ContractError::SlippageExceeded` on `Err`.

