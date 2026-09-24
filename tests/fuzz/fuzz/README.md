# Coverage-Guided Fuzz Targets for AMM Math

**Follow-up to:** [#625](https://github.com/StellarFlow-Network/stellarflow-contracts/issues/625)

This subcrate adds **coverage-guided** fuzzing on top of the
property-based harness in `tests/fuzz/`. Where `proptest` is excellent
for random sampling with shrinking, `cargo fuzz` + `libFuzzer` adds
structured-mutation feedback that lets it explore unreachable code
paths the random sampler might not hit for hours at a time.

The two harnesses complement each other:

| Harness | Strength | When to use |
| --- | --- | --- |
| `proptest` (in `tests/fuzz/`) | Stable Rust, deterministic runs, integrates with `cargo test`. | Per-PR CI on stable. |
| `cargo fuzz` (this crate) | Coverage-guided mutation, persistent corpus, automatic regression saving. | Nightly CI / long-running local sessions. |

## Targets

| Target | Property mirrored | Inputs |
| --- | --- | --- |
| `swap_invariants` | `prop_no_panic_compute_swap_out` + `prop_k_monotonicity` | `amount_in`, `reserve_in`, `reserve_out` |
| `lp_invariants` | `prop_no_panic_compute_lp_shares` + `prop_no_panic_compute_remove_liquidity` + `prop_mint_burn_roundtrip` | `amount_a`, `amount_b`, `reserve_a`, `reserve_b`, `total_shares` |
| `slippage_invariants` | `prop_slippage_enforcement` | `amount_out`, `min` |

Each target file is fully self-contained — `#[path = "..."]`
includes the relevant AMM source plus a local `ContractError` stub, so
this crate does not need to depend on the proptest crate.

## Requirements

- **Nightly Rust.** `libfuzzer-sys` requires a nightly toolchain.
  Install: `rustup toolchain install nightly`.
- **`cargo-fuzz` subcommand.**
  ```sh
  cargo install cargo-fuzz
  ```

## How to run

From this directory:

```bash
# Quick session — each target for 60 seconds.
cargo +nightly fuzz run swap_invariants      -- -max_total_time=60
cargo +nightly fuzz run lp_invariants       -- -max_total_time=60
cargo +nightly fuzz run slippage_invariants -- -max_total_time=60
```

Or run all targets in background sessions for a daily smoke job:

```bash
PROPTEST_CASES=1_000_000 cargo +nightly fuzz run swap_invariants -- -max_total_time=86400
```

The generated artifacts (corpus, crash repros) live in
`tests/fuzz/fuzz/corpus/<target>/` and `tests/fuzz/fuzz/artifacts/<target>/`
respectively. Both are gitignored.

## Triage

If a target reports a failure, libFuzzer writes a reproducing artifact
under `artifacts/`. To reproduce locally:

```bash
cargo +nightly fuzz run swap_invariants artifacts/swap_invariants/crash-<sha>
```

To minimize the failing input (shrinks it to the smallest reproducer):

```bash
cargo +nightly fuzz tmin swap_invariants artifacts/swap_invariants/crash-<sha>
```

Save minimized reproducers to `tests/fuzz/regressions/<target>/`
(commit them — they're how we ensure the bug doesn't reappear).

## Files

| Path | Purpose |
| --- | --- |
| `Cargo.toml` | Crate manifest; pulls `libfuzzer-sys` and `arbitrary`. |
| `.gitignore` | Excludes `target/`, `corpus/`, `artifacts/`. |
| `fuzz_targets/swap_invariants.rs` | `swap_invariants` fuzz target. |
| `fuzz_targets/lp_invariants.rs` | `lp_invariants` fuzz target. |
| `fuzz_targets/slippage_invariants.rs` | `slippage_invariants` fuzz target. |
| `README.md` | This file. |

## Why is this subcrate excluded from the root workspace?

`tests/fuzz/Cargo.toml` (the proptest crate) is a stable-Rust workspace
member. This subcrate requires **nightly** because of `libfuzzer-sys`.
Mixing them in `[workspace]` members would force stable CI to compile
nightly-only deps on every `cargo test`, which we don't want.

The `Cargo.toml` at the repo root explicitly excludes
`tests/fuzz/fuzz/` so `cargo test --workspace` stays stable-Rust-clean.
Cargo-fuzz finds this subcrate from its own discovery (it doesn't care
about workspace membership).

## Future work

- Persist the corpus in object storage (e.g., an s3 bucket) so progress
  is shared across CI runners.
- Add a `tests/fuzz/regressions/` directory with checked-in repros
  for any historical panics.
- Wire a coverage-tracker (e.g., `cargo fuzz coverage`) into nightly
  CI for the AMM math layer.
