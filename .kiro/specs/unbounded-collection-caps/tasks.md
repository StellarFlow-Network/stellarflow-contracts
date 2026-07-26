# Implementation Plan

## Overview

This task list follows the exploratory bugfix workflow for the `unbounded-collection-caps` defect. The four affected entrypoints (`get_prices`, `get_prices_with_status`, `get_index_price` in price-oracle and `add_recipient` in reward-splitter) accept caller-supplied `Vec` inputs with no upper-length guard. The fix adds `MAX_PATH_HOPS = 5` and `MAX_RECIPIENTS = 50` constants and guards at each entrypoint boundary, returning `OutOfBounds` immediately when the cap is exceeded.

Tasks are ordered: exploration first (confirm the bug on unfixed code), preservation baseline (record correct behavior), implementation (apply fix), then boundary and integration validation.

## Tasks

- [ ] 1. Write bug condition exploration tests (BEFORE implementing the fix)
  - **Property 1: Bug Condition** - Oversized Vec Inputs Are Not Rejected on Unfixed Code
  - **CRITICAL**: These tests MUST FAIL on unfixed code — failure confirms the guards are absent
  - **DO NOT attempt to fix the test or the code when it fails**
  - **NOTE**: The tests encode expected post-fix behavior; they will validate the fix once it is applied
  - **GOAL**: Surface counterexamples that demonstrate the bug exists in all four entrypoints
  - **Scoped PBT Approach**: Use concrete minimal oversized inputs (length = 6 for oracle; count = 51 for splitter) to ensure deterministic reproduction
  - In `contracts/price-oracle/src/test.rs`, add `test_get_prices_rejects_oversized_input`:
    - Build a `Vec<Symbol>` of length 6 (e.g., six `symbol_short!("X")` entries) and call `client.get_prices(&six_symbols)`
    - Assert the call returns `Err(ContractError::OutOfBounds)` — on unfixed code it will NOT, confirming the guard is absent
    - Document counterexample: `get_prices` with 6 symbols iterates all 6 and returns a 6-entry `Vec` instead of `Err(OutOfBounds)`
  - In `contracts/price-oracle/src/test.rs`, add `test_get_prices_with_status_rejects_oversized_input`:
    - Build a `Vec<Symbol>` of length 6 and call `client.get_prices_with_status(&six_symbols)`
    - Assert `Err(ContractError::OutOfBounds)` — fails on unfixed code, confirming absent guard
    - Document counterexample: iterates all 6 without early exit
  - In `contracts/price-oracle/src/test.rs`, add `test_get_index_price_rejects_oversized_components`:
    - Build a `Vec<AssetWeight>` of length 6 and call `client.get_index_price(&six_components)`
    - Assert `Err(ContractError::OutOfBounds)` — fails on unfixed code, confirming absent guard in `calculate_index_price`
    - Document counterexample: `calculate_index_price` iterates all 6 components instead of returning early
  - In `contracts/reward-splitter/src/test.rs`, add `test_add_recipient_rejects_at_cap`:
    - Initialize a `RewardSplitter` and add 50 recipients each with share 200 (50 × 200 = 10 000 bp)
    - Call `add_recipient` for a 51st recipient — this would exceed both the count cap AND the share cap
    - To isolate the count cap specifically: add 50 recipients each with share 1, then attempt to add a 51st
    - Assert `Err(Error::OutOfBounds)` — on unfixed code the call succeeds (appending recipient 51), confirming the count guard is absent
    - Document counterexample: 51st recipient is stored; `get_recipients` returns list of length 51
  - Run all four tests on unfixed code; record each failure with the actual return value observed
  - Mark task complete when all four tests are written, run, and each failure is documented
  - _Requirements: 1.1, 1.2, 1.3, 1.4_

- [ ] 2. Write preservation property tests (BEFORE implementing the fix)
  - **Property 2: Preservation** - Valid-Length Inputs Produce Unchanged Results
  - **IMPORTANT**: Follow the observation-first methodology — run unfixed code first, record outputs, then write property assertions
  - **Observation phase (run on UNFIXED code, record results):**
    - `get_prices` with 0 symbols → observe empty `Vec` returned
    - `get_prices` with 1 symbol (registered, non-stale) → observe `Vec` of length 1 containing `Some(PriceEntry)`
    - `get_prices` with 5 symbols (all registered, non-stale) → observe `Vec` of length 5 with `Some` entries
    - `get_prices_with_status` with 5 symbols → observe `Vec` of length 5 with `PriceEntryWithStatus` entries and correct `is_stale` flags
    - `get_index_price` with 1–5 `AssetWeight` components → observe correct weighted-average result
    - `add_recipient` with 0–49 existing recipients → observe success, list grows, `TotalShares` increments
    - `distribute` with up to 50 recipients → observe proportional transfers
  - **Property-based test for oracle preservation** — add `test_get_prices_valid_lengths_preserved` in `contracts/price-oracle/src/test.rs`:
    - For lengths 0, 1, 2, 3, 4, 5 (the full valid domain): call `get_prices` and assert result length equals input length
    - For length 0: assert result is an empty `Vec` (Requirement 3.6)
    - For lengths 1–5: assert each position is `Some` or `None` consistent with whether the asset is registered and non-stale (Requirement 3.1)
    - Verify these tests PASS on unfixed code before proceeding
  - **Property-based test for `get_prices_with_status` preservation** — add `test_get_prices_with_status_valid_lengths_preserved`:
    - Cover lengths 0–5; assert result vector length matches input length
    - Assert `is_stale` flags are correct for non-stale vs stale prices (Requirement 3.2)
    - Verify PASSES on unfixed code
  - **Property-based test for `get_index_price` preservation** — add `test_get_index_price_valid_lengths_preserved`:
    - Cover 1–5 components with non-zero weights and registered assets
    - Assert result equals the manual weighted-average calculation (Requirement 3.3)
    - Verify PASSES on unfixed code
  - **Property-based test for splitter preservation** — add `test_add_recipient_under_cap_preserved` in `contracts/reward-splitter/src/test.rs`:
    - For 0–49 existing recipients: call `add_recipient` and assert it succeeds, recipient list grows by 1, `TotalShares` increments correctly (Requirement 3.4)
    - Verify PASSES on unfixed code
  - **`distribute` preservation test** — add `test_distribute_up_to_cap_preserved`:
    - Add 50 recipients (each with share 200 bp); call `distribute` with a known amount
    - Assert each recipient receives `amount * 200 / 10000` tokens (Requirement 3.5)
    - Verify PASSES on unfixed code
  - Run all preservation tests on unfixed code; confirm every test PASSES before proceeding to implementation
  - Mark task complete when all tests are written, run on unfixed code, and confirmed passing
  - _Requirements: 3.1, 3.2, 3.3, 3.4, 3.5, 3.6_

- [ ] 3. Implement the collection-length cap fix

  - [ ] 3.1 Add `OutOfBounds` error variant to `ContractError` in price-oracle
    - Open `contracts/price-oracle/src/lib.rs`
    - Append to the `ContractError` enum (after `InvalidWeightThreshold = 62`):
      ```rust
      /// Input collection length exceeds the maximum allowed cap.
      OutOfBounds = 63,
      ```
    - Verify the file compiles (`cargo check -p price-oracle`)
    - _Bug_Condition: isBugCondition_Oracle(X) — X.len() > MAX_PATH_HOPS_
    - _Expected_Behavior: Err(ContractError::OutOfBounds) returned immediately, no iteration_
    - _Preservation: Inputs with len ≤ 5 must continue to return correct results unchanged_
    - _Requirements: 2.1, 2.2, 2.3_

  - [ ] 3.2 Add `MAX_PATH_HOPS` constant in price-oracle
    - In `contracts/price-oracle/src/lib.rs`, near `MAX_CLEAR_ASSETS` and `MAX_MEDIAN_ENTRIES`, add:
      ```rust
      /// Maximum number of assets (or index components) accepted in a single batch read.
      /// Prevents callers from exhausting the Soroban CPU budget via oversized Vec inputs.
      const MAX_PATH_HOPS: u32 = 5;
      ```
    - _Requirements: 2.1, 2.2, 2.3_

  - [ ] 3.3 Add length guard to `get_prices` in price-oracle
    - In `contracts/price-oracle/src/lib.rs`, inside `get_prices`, immediately after the `_is_halted` check and before `let now = env.ledger().timestamp()`, insert:
      ```rust
      if assets.len() > MAX_PATH_HOPS {
          panic_with_error!(&env, ContractError::OutOfBounds);
      }
      ```
    - The halt check must remain first (see integration test: halt fires before length check)
    - Verify the function body is otherwise unchanged
    - _Bug_Condition: assets.len() > 5_
    - _Expected_Behavior: panic_with_error OutOfBounds before any iteration_
    - _Requirements: 2.1_

  - [ ] 3.4 Add length guard to `get_prices_with_status` in price-oracle
    - In `contracts/price-oracle/src/lib.rs`, inside `get_prices_with_status`, as the very first statement (the function has no halt check currently), insert:
      ```rust
      if assets.len() > MAX_PATH_HOPS {
          panic_with_error!(&env, ContractError::OutOfBounds);
      }
      ```
    - Verify the rest of the function body is unchanged
    - _Bug_Condition: assets.len() > 5_
    - _Expected_Behavior: panic_with_error OutOfBounds before any iteration_
    - _Requirements: 2.2_

  - [ ] 3.5 Add length guard to `get_index_price` in price-oracle
    - In `contracts/price-oracle/src/lib.rs`, inside `get_index_price`, after the `_is_halted` check and before the call to `validation::calculate_index_price`, insert:
      ```rust
      if components.len() > MAX_PATH_HOPS {
          return Err(ContractError::OutOfBounds);
      }
      ```
    - Note: this uses `return Err(...)` (not `panic_with_error!`) to match the `Result<i128, ContractError>` return type
    - Verify the call to `validation::calculate_index_price` is otherwise unchanged
    - _Bug_Condition: components.len() > 5_
    - _Expected_Behavior: Err(ContractError::OutOfBounds) before entering calculate_index_price_
    - _Requirements: 2.3_

  - [ ] 3.6 Add `OutOfBounds` error variant to `Error` in reward-splitter
    - Open `contracts/reward-splitter/src/lib.rs`
    - Append to the `Error` enum (after `InvalidActionType = 14`):
      ```rust
      /// Recipient list has reached the maximum allowed cap.
      OutOfBounds = 15,
      ```
    - Verify the file compiles (`cargo check -p reward-splitter`)
    - _Bug_Condition: isBugCondition_Splitter(stored_recipients) — stored_recipients.len() >= MAX_RECIPIENTS_
    - _Expected_Behavior: Err(Error::OutOfBounds) returned immediately, storage unchanged_
    - _Preservation: add_recipient with < 50 recipients must continue to succeed unchanged_
    - _Requirements: 2.4_

  - [ ] 3.7 Add `MAX_RECIPIENTS` constant in reward-splitter
    - In `contracts/reward-splitter/src/lib.rs`, near the existing stage constants, add:
      ```rust
      /// Maximum number of recipients allowed in the stored list.
      /// Prevents the distribute function from exceeding the Soroban CPU budget.
      const MAX_RECIPIENTS: u32 = 50;
      ```
    - _Requirements: 2.4_

  - [ ] 3.8 Add recipient count guard to `add_recipient` in reward-splitter
    - In `contracts/reward-splitter/src/lib.rs`, inside `add_recipient`, after reading `recipients` from storage and before reading `total_shares`, insert:
      ```rust
      if recipients.len() >= MAX_RECIPIENTS {
          panic_with_error!(&env, Error::OutOfBounds);
      }
      ```
    - The guard must fire before any write to storage (before `TotalShares` is read or modified)
    - Verify the rest of `add_recipient` is unchanged
    - _Bug_Condition: recipients.len() >= 50_
    - _Expected_Behavior: panic_with_error OutOfBounds before any storage write_
    - _Preservation: Preservation Requirements 3.4 — adds with count < 50 unchanged_
    - _Requirements: 2.4_

  - [ ] 3.9 Verify bug condition exploration test now passes (re-run task 1 tests)
    - **Property 1: Expected Behavior** - Oversized Vec Inputs Return OutOfBounds on Fixed Code
    - **IMPORTANT**: Re-run the SAME four tests written in task 1 — do NOT write new tests
    - The tests from task 1 encode the expected behavior; passing now confirms the fix is correct
    - Run: `cargo test -p price-oracle test_get_prices_rejects_oversized_input`
    - Run: `cargo test -p price-oracle test_get_prices_with_status_rejects_oversized_input`
    - Run: `cargo test -p price-oracle test_get_index_price_rejects_oversized_components`
    - Run: `cargo test -p reward-splitter test_add_recipient_rejects_at_cap`
    - **EXPECTED OUTCOME**: All four tests PASS (confirms bugs are fixed)
    - _Requirements: 2.1, 2.2, 2.3, 2.4_

  - [ ] 3.10 Verify preservation tests still pass (re-run task 2 tests)
    - **Property 2: Preservation** - Valid-Length Inputs Produce Unchanged Results on Fixed Code
    - **IMPORTANT**: Re-run the SAME tests written in task 2 — do NOT write new tests
    - Run: `cargo test -p price-oracle test_get_prices_valid_lengths_preserved`
    - Run: `cargo test -p price-oracle test_get_prices_with_status_valid_lengths_preserved`
    - Run: `cargo test -p price-oracle test_get_index_price_valid_lengths_preserved`
    - Run: `cargo test -p reward-splitter test_add_recipient_under_cap_preserved`
    - Run: `cargo test -p reward-splitter test_distribute_up_to_cap_preserved`
    - **EXPECTED OUTCOME**: All five tests PASS (confirms no regressions)
    - _Requirements: 3.1, 3.2, 3.3, 3.4, 3.5, 3.6_

- [ ] 4. Write boundary unit tests
  - Add exact-boundary unit tests in `contracts/price-oracle/src/test.rs`:
    - `test_get_prices_exactly_five_symbols`: call `get_prices` with exactly 5 symbols; assert result is a `Vec` of length 5 (boundary — must PASS)
    - `test_get_prices_exactly_six_symbols_returns_out_of_bounds`: call `get_prices` with exactly 6 symbols; assert `Err(ContractError::OutOfBounds)` (boundary — must PASS after fix)
    - `test_get_prices_empty_vec_still_passes`: call `get_prices` with 0 symbols; assert empty `Vec` returned (edge case — must PASS)
    - `test_get_prices_with_status_exactly_five_symbols`: call with 5 symbols; assert `Vec` of length 5 (must PASS)
    - `test_get_prices_with_status_exactly_six_returns_out_of_bounds`: call with 6 symbols; assert `Err(ContractError::OutOfBounds)` (must PASS after fix)
    - `test_get_index_price_exactly_five_components`: call with 5 valid `AssetWeight` entries; assert correct weighted-average result (must PASS)
    - `test_get_index_price_exactly_six_components_returns_out_of_bounds`: call with 6 components; assert `Err(ContractError::OutOfBounds)` (must PASS after fix)
  - Add exact-boundary unit tests in `contracts/reward-splitter/src/test.rs`:
    - `test_add_recipient_exactly_49_existing_succeeds`: add 49 recipients, then add a 50th; assert success and recipient count is 50 (boundary — must PASS)
    - `test_add_recipient_exactly_50_existing_returns_out_of_bounds`: add 50 recipients (each with share 1), then attempt to add a 51st; assert `Err(Error::OutOfBounds)` (boundary — must PASS after fix)
    - `test_distribute_with_50_recipients_transfers_correctly`: add 50 recipients each with share 200 bp (total = 10 000); call `distribute(10_000)`; assert each recipient receives 200 tokens (preservation — must PASS)
  - Run all boundary tests: `cargo test -p price-oracle && cargo test -p reward-splitter`
  - All tests must PASS
  - _Requirements: 2.1, 2.2, 2.3, 2.4, 3.1, 3.2, 3.3, 3.4, 3.5, 3.6_

- [ ] 5. Write integration tests
  - Add integration tests covering cross-component and priority scenarios in `contracts/price-oracle/src/test.rs`:
    - `test_integration_get_prices_five_registered_assets`: initialize oracle, register 5 assets with prices, call `get_prices` with all 5 — verify a `Vec` of 5 `Some(PriceEntry)` entries is returned
    - `test_integration_get_prices_six_symbols_returns_out_of_bounds`: initialize oracle, register 5 assets, call `get_prices` with 6 symbols (5 registered + 1 unknown) — verify `Err(ContractError::OutOfBounds)` is returned rather than a 6-element result with a `None` at position 6
    - `test_integration_halt_fires_before_length_check`: initialize oracle, set emergency halt via `set_emergency_halt`, call `get_prices` with 6 symbols — verify `ContractError::EmergencyHalted` is returned (not `OutOfBounds`), confirming halt takes priority
  - Add integration tests in `contracts/reward-splitter/src/test.rs`:
    - `test_integration_add_50_recipients_then_distribute`: initialize splitter with a mock token, add 50 recipients each with share 200 bp, fund the contract, call `distribute(10_000)` — verify all 50 recipients receive 200 tokens each
    - `test_integration_add_recipient_past_50_returns_out_of_bounds`: initialize splitter, add 50 recipients each with share 1 bp (total = 50 bp, well under the 10 000 bp share cap), attempt to add a 51st — verify `Err(Error::OutOfBounds)` is returned and the stored recipient list still has exactly 50 entries
  - Run: `cargo test -p price-oracle && cargo test -p reward-splitter`
  - All integration tests must PASS
  - _Requirements: 2.1, 2.2, 2.3, 2.4, 3.1, 3.4, 3.5_

- [ ] 6. Checkpoint — Ensure all tests pass
  - Run the full test suite for both contracts:
    ```
    cargo test -p price-oracle
    cargo test -p reward-splitter
    ```
  - Confirm zero failures and zero errors
  - Confirm no pre-existing tests were broken by the new error variant numbers (existing `ContractError` and `Error` variants are unaffected since `OutOfBounds` is appended at the end)
  - If any test fails, diagnose and fix before marking this task complete
  - Ask the user if any questions arise about expected behavior at the boundaries

## Task Dependency Graph

```json
{
  "waves": [
    { "wave": 1, "tasks": ["1"] },
    { "wave": 2, "tasks": ["2"] },
    { "wave": 3, "tasks": ["3.1", "3.2", "3.6", "3.7"] },
    { "wave": 4, "tasks": ["3.3", "3.4", "3.5", "3.8"] },
    { "wave": 5, "tasks": ["3.9", "3.10"] },
    { "wave": 6, "tasks": ["4"] },
    { "wave": 7, "tasks": ["5"] },
    { "wave": 8, "tasks": ["6"] }
  ]
}
```

## Notes

- Both `OutOfBounds` variants are appended at the end of their respective enums (`= 63` for `ContractError`, `= 15` for `Error`), so no existing variant discriminants are shifted.
- The halt check in `get_prices` must remain before the length check (task 3.3 note and integration test `test_integration_halt_fires_before_length_check`).
- `get_index_price` uses `return Err(...)` for the guard rather than `panic_with_error!` because the function returns `Result<i128, ContractError>`.
- `get_prices_with_status` currently has no halt check — the length guard is inserted as the first statement.
- The preservation tests (task 2) must be confirmed passing on unfixed code before implementation begins. If they fail on unfixed code, the test setup is wrong and must be corrected before proceeding.
- For the splitter recipient-cap isolation test, use shares of 1 bp each (50 × 1 = 50 bp total) so the `TotalSharesExceeded` guard (which fires at 10 000 bp) never interferes, keeping the count guard as the only active constraint.
