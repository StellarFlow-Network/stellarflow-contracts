# REPAIRS

Tracked-out-of-band repairs for the `stellarflow-contracts` workspace. This
file documents structural/compile-time repairs that were applied directly to
the working tree while the five issue workstreams (#722, #724, #727, #749,
#138) were pending. It also records exemptions that are **deliberately out of
scope** so the next developer does not re-open them by accident.

## Workspace status

`cargo test --workspace` is green (no failures, no compile errors).

## Crates excluded from the workspace (with documented reasons)

These crates were removed from the `members` list in the root `Cargo.toml`.
They are kept in the repo but are not built by any workspace command. Each was
broken before the repair effort and none is a deliverable of the issue trackers:

| Crate / dir            | Why it is excluded                                        |
| ---------------------- | --------------------------------------------------------- |
| `contracts/gas-tank`   | `src/lib.rs` is a one-line AI placeholder ("✅ Task completed…"); never implemented. Excluded per team decision. |
| `contracts/price-oracle` | ~686 compile errors from soroban-sdk 20 API drift (pre-existing). Excluded per team decision. Issue #722 (TWAP) is instead delivered in the root crate. |
| `tests/benchmarks`     | Depends on `contracts/price-oracle`; cannot build while that crate is excluded. |
| `tests/fuzz`           | Pre-existing compile break unaddressed by this effort; excluded per team decision. |

> Note: `Cargo.lock` still contains the excluded crates (they remain on disk),
> so lock-file churn is expected and is not itself a regression signal.

## Root crate repairs (needed to restore `cargo test --workspace`)

- Added a `testutils` feature to the root `Cargo.toml` (`default = ["testutils"]`).
  `#[contractimpl]` (soroban-sdk-macros 20.x) gates the generated
  `ContractFunctionSet` impl behind `cfg(any(test, feature = "testutils"))`, so
  without the feature the integration-test targets in `tests/` could not
  `env.register_contract` the contract.
- Fixed `DEFAULT_HEARTBEAT_INTERVAL` (`pub(crate)` → `pub`) and `mod nonce`
  (`pub(crate)` → `pub`) so `tests/unit.rs` can use them.
- Fixed `tests/mocks/token_mocks.rs` `approve` call for the SDK-20 token client
  signature (scalar args passed by reference; `expiration_ledger` added).
- Updated legacy tests in `tests/unit.rs` to current contract invariants:
  - `StakingTierConfig`: `tier1..4_min` → `regional_min_stake`,
    `standard_min_stake`, `premier_min_stake`.
  - `get_latest_rate` client result no longer wrapped in `Ok(..)`.
  - Corridor-fee amounts raised above `MIN_TRANSFER_AMOUNT` (10_000).
  - Emergency-revocation tests now register additional signers so the quorum
    threshold exceeds 1 (a lone proposer previously triggered immediate
    execution, correctly removing the proposal).
  - Bundle-processing test timestamp fixed (`0 - 30` overflow).
- Restored doctests: three stale ` ```rust ` examples in `src/validation.rs`
  changed to ` ```ignore ` (matching the existing convention; they reference
  nonexistent symbols).
- `.gitignore` now ignores `test_snapshots/` — the SDK-20 host regenerates
  these JSON artifacts on every test run; 281 files were removed from git
  tracking.

## Contract-level repairs already merged into the tree

Sub-crate suites fixed for soroban-sdk `=20.0.0`:

- `linear-vesting` (6) — claim abort fixed via `advance_ledgers` keeping
  `sequence_number` + sane TTLs.
- `relayer-allowance` (8) — `symbol_short!` >10-char symbols → `Symbol::new`;
  added `Overflow` error; sane TTLs.
- `analytics-engine` (3) — EWMA weight is `alpha` in basis points (linear),
  not sqrt-scaled.
- `reward-splitter` (27) — `Result`-returning contract (no in-contract
  panics); body callable only via `try_*`; tests use `mock_all_auths`,
  real-token setup via `StellarAssetClient` mint + transfer, and
  `register_contract(&None, ..)`.
- Root crate lib suite (426) + `tests/unit.rs` (38) + `tests/integration.rs`
  (3) + docs (7 ignored) — green.