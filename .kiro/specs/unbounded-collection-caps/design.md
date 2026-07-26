# Unbounded Collection Caps Bugfix Design

## Overview

Two contracts in the StellarFlow suite accept caller-supplied `Vec` inputs with no upper-length guard. Because every heap allocation in the Soroban execution model consumes CPU instructions, a malicious or buggy caller can push a transaction past the network hard limits (100 M instructions / 40 MB memory), causing an on-chain budget abort and denying service to all other users.

**Affected sites:**

| Contract | Entrypoint | Input | Bug |
|---|---|---|---|
| `price-oracle` | `get_prices` | `Vec<Symbol>` | no length guard before full iteration |
| `price-oracle` | `get_prices_with_status` | `Vec<Symbol>` | no length guard before full iteration |
| `price-oracle` | `get_index_price` | `Vec<AssetWeight>` | no length guard before loop in `calculate_index_price` |
| `reward-splitter` | `add_recipient` | stored `Vec<Recipient>` | no cap on total list size, so `distribute` grows without bound |

The fix adds a single length-guard check at the entrypoint boundary in each affected function. No other logic changes. When the guard trips it returns the contract's canonical range error immediately — before any iteration, storage read, or storage write.

---

## Glossary

- **Bug_Condition (C)**: The condition that triggers the bug — a caller-supplied (or accumulated) collection whose length exceeds the defined safe maximum.
- **Property (P)**: The desired behavior when the bug condition holds — the function SHALL return the appropriate `OutOfBounds` error immediately, without iterating or modifying state.
- **Preservation**: All existing behavior for inputs that do NOT satisfy C must remain byte-for-byte identical after the fix.
- **`MAX_PATH_HOPS`**: New constant (`5`) to be introduced in `contracts/price-oracle/src/lib.rs` (or a dedicated constants module). Caps the length of `Vec<Symbol>` / `Vec<AssetWeight>` inputs for the three oracle batch reads.
- **`MAX_RECIPIENTS`**: New constant (`50`) to be introduced in `contracts/reward-splitter/src/lib.rs`. Caps the stored recipient list length guarded by `add_recipient`.
- **`ContractError::OutOfBounds`**: New error variant to be added to the `ContractError` enum in `contracts/price-oracle/src/lib.rs`. Returned when a collection input exceeds `MAX_PATH_HOPS`.
- **`Error::OutOfBounds`**: New error variant to be added to the `Error` enum in `contracts/reward-splitter/src/lib.rs`. Returned when the recipient list already holds `MAX_RECIPIENTS` entries.
- **`get_prices`**: The batch price read in `PriceOracle` at `lib.rs:1818` that iterates `assets` without a length check.
- **`get_prices_with_status`**: The batch price-with-freshness read in `PriceOracle` at `lib.rs:1855` that iterates `assets` without a length check.
- **`get_index_price`**: The weighted-average index calculation in `PriceOracle` at `lib.rs:1217` that delegates to `validation::calculate_index_price` without a length check.
- **`add_recipient`**: The recipient-registration function in `RewardSplitter` at `reward-splitter/src/lib.rs:130` that appends to the stored list without bounding its length.
- **`distribute`**: The payout function in `RewardSplitter` that iterates all stored recipients; its cost is proportional to list length.

---

## Bug Details

### Bug Condition

**Oracle (price-oracle) — `get_prices`, `get_prices_with_status`, `get_index_price`:**

The bug manifests when any of the three functions receives a `Vec` whose length exceeds `MAX_PATH_HOPS` (5). The function iterates every element before performing any early-exit validation, consuming CPU instructions proportional to the caller-supplied length.

```
FUNCTION isBugCondition_Oracle(X)
  INPUT:  X of type Vec<Symbol> or Vec<AssetWeight>
  OUTPUT: boolean

  RETURN X.len() > MAX_PATH_HOPS   // MAX_PATH_HOPS = 5
END FUNCTION
```

**Splitter (reward-splitter) — `add_recipient`:**

The bug manifests when `add_recipient` is called and the stored recipient list already contains `MAX_RECIPIENTS` (50) entries. Appending beyond this point causes `distribute` (which iterates all recipients) to exceed safe CPU budgets over time.

```
FUNCTION isBugCondition_Splitter(stored_recipients)
  INPUT:  stored_recipients of type Vec<Recipient>
  OUTPUT: boolean

  RETURN stored_recipients.len() >= MAX_RECIPIENTS   // MAX_RECIPIENTS = 50
END FUNCTION
```

### Examples

**Oracle examples:**

- `get_prices` called with a `Vec<Symbol>` of length 6 → currently iterates all 6 elements and returns 6 price lookups; **after fix** returns `Err(ContractError::OutOfBounds)` before the loop.
- `get_prices` called with a `Vec<Symbol>` of length 1 000 000 → currently panics with a Soroban budget abort; **after fix** returns `Err(ContractError::OutOfBounds)` immediately.
- `get_prices_with_status` called with a `Vec<Symbol>` of length 6 → same as above.
- `get_index_price` called with a `Vec<AssetWeight>` of length 6 → currently enters `calculate_index_price` and iterates all components; **after fix** returns `Err(ContractError::OutOfBounds)` before the loop.
- `get_prices` called with a `Vec<Symbol>` of length 5 → **unchanged**, returns `Vec<Option<PriceEntry>>` of length 5.
- `get_prices` called with an empty `Vec<Symbol>` → **unchanged**, returns an empty `Vec`.

**Splitter examples:**

- `add_recipient` called when 50 recipients are already stored → currently appends recipient 51; **after fix** returns `Err(Error::OutOfBounds)` before modifying storage.
- `add_recipient` called when 49 recipients are stored → **unchanged**, appends the new recipient, increments total shares.
- `distribute` called with 50 recipients → **unchanged**, iterates all 50 and transfers proportional shares.

---

## Expected Behavior

### Preservation Requirements

**Unchanged behaviors:**

- `get_prices` called with 1–5 symbols SHALL continue to return `Vec<Option<PriceEntry>>` in input order, with `None` for missing or stale entries (Requirement 3.1).
- `get_prices_with_status` called with 1–5 symbols SHALL continue to return `Vec<Option<PriceEntryWithStatus>>` with correct freshness flags (Requirement 3.2).
- `get_index_price` called with 1–5 `AssetWeight` components SHALL continue to return the correct weighted-average index price (Requirement 3.3).
- `add_recipient` called when total recipients after addition would be ≤ 50 SHALL continue to append the recipient, update `TotalShares`, and persist the updated list (Requirement 3.4).
- `distribute` called with a valid amount and a list of up to 50 recipients SHALL continue to transfer proportional shares to each recipient (Requirement 3.5).
- `get_prices` called with an empty `Vec<Symbol>` SHALL continue to return an empty result vector without error (Requirement 3.6).

**Scope:**

All inputs that do NOT satisfy `isBugCondition_Oracle` or `isBugCondition_Splitter` are completely unaffected by this fix. This includes:
- Every invocation of `get_price`, `get_last_price`, `get_price_with_status`, `get_price_safe`, `get_twap`, and all write paths — no changes to those code paths.
- All governance, admin, and slashing functions — completely out of scope.
- The `remove_recipient`, `update_recipient_share`, and all other `RewardSplitter` functions — unchanged.

---

## Hypothesized Root Cause

Both defects share the same root cause category: **missing input-length validation at the public entrypoint boundary**. The functions were written assuming callers pass reasonably small collections; no defensive cap was ever added.

**Specific analysis:**

1. **`get_prices` / `get_prices_with_status` — no length guard before the loop:**  
   Both functions begin iterating `assets.iter()` immediately after the emergency-halt check. There is no `if assets.len() > N { return Err(...); }` guard anywhere in the call path (`lib.rs:1818` and `lib.rs:1855`).

2. **`get_index_price` — no length guard before delegating to `calculate_index_price`:**  
   `lib.rs:1217` passes `components` directly to `validation::calculate_index_price`. That helper checks `components.is_empty()` but not an upper bound. A large `components` vec causes proportional CPU consumption inside the loop at `validation.rs:calculate_index_price`.

3. **`add_recipient` — no recipient count cap:**  
   `reward-splitter/src/lib.rs:130` reads the existing recipient list and appends unconditionally, bounded only by the `TotalSharesExceeded` check (which limits the total basis-point sum to 10 000, not the number of recipients). An admin could add up to 10 000 individual recipients each with 1 bp share before hitting the share cap, making `distribute` prohibitively expensive.

4. **No `OutOfBounds` error variant exists in either contract:**  
   `ContractError` in `price-oracle/src/lib.rs` has 62 variants but none for collection length. `Error` in `reward-splitter/src/lib.rs` has 14 variants but none for collection length. New variants must be added before the guard can return them.

---

## Correctness Properties

Property 1: Bug Condition — Oracle Entrypoints Return OutOfBounds for Oversized Inputs

_For any_ `Vec<Symbol>` or `Vec<AssetWeight>` input X where `isBugCondition_Oracle(X)` returns true (i.e., `X.len() > MAX_PATH_HOPS`), the fixed `get_prices`, `get_prices_with_status`, and `get_index_price` functions SHALL return `Err(ContractError::OutOfBounds)` immediately, without iterating any element, accessing persistent storage, or triggering an on-chain budget abort.

**Validates: Requirements 2.1, 2.2, 2.3**

Property 2: Bug Condition — Splitter add_recipient Returns OutOfBounds at Cap

_For any_ contract state where `isBugCondition_Splitter(stored_recipients)` returns true (i.e., the stored recipient list length is ≥ `MAX_RECIPIENTS`), the fixed `add_recipient` function SHALL return `Err(Error::OutOfBounds)` immediately, without appending to the list or updating `TotalShares` in storage.

**Validates: Requirements 2.4**

Property 3: Preservation — Oracle Batch Reads Unchanged for Valid Inputs

_For any_ `Vec<Symbol>` or `Vec<AssetWeight>` input X where `isBugCondition_Oracle(X)` returns false (i.e., `X.len() <= MAX_PATH_HOPS`), the fixed `get_prices`, `get_prices_with_status`, and `get_index_price` functions SHALL produce exactly the same result as the original functions, including empty-input behavior (Requirement 3.6).

**Validates: Requirements 3.1, 3.2, 3.3, 3.6**

Property 4: Preservation — Splitter add_recipient and distribute Unchanged for Valid Inputs

_For any_ contract state where `isBugCondition_Splitter(stored_recipients)` returns false (i.e., recipient count < `MAX_RECIPIENTS`), the fixed `add_recipient` and `distribute` functions SHALL produce exactly the same result as the original functions, preserving recipient storage, share accounting, and token transfer behavior.

**Validates: Requirements 3.4, 3.5**

---

## Fix Implementation

### Changes Required

#### File: `contracts/price-oracle/src/lib.rs`

**1. Add `OutOfBounds` error variant to `ContractError`:**

Add the following entry to the `ContractError` enum (after the last existing variant, assigning the next available integer, currently `63`):

```rust
/// Input collection length exceeds the maximum allowed cap.
OutOfBounds = 63,
```

**2. Add `MAX_PATH_HOPS` constant near the top of the file** (alongside other module-level constants like `MAX_CLEAR_ASSETS` and `MAX_MEDIAN_ENTRIES`):

```rust
/// Maximum number of assets (or index components) accepted in a single batch read.
/// Prevents callers from exhausting the Soroban CPU budget via oversized Vec inputs.
const MAX_PATH_HOPS: u32 = 5;
```

**3. Guard `get_prices`** — insert length check as the first executable statement after the emergency-halt guard (around `lib.rs:1826`):

```rust
pub fn get_prices(
    env: Env,
    assets: soroban_sdk::Vec<Symbol>,
) -> soroban_sdk::Vec<Option<crate::types::PriceEntry>> {
    if crate::auth::_is_halted(&env) {
        panic_with_error!(&env, ContractError::EmergencyHalted);
    }
    // NEW: reject oversized input before any iteration
    if assets.len() > MAX_PATH_HOPS {
        panic_with_error!(&env, ContractError::OutOfBounds);
    }
    // ... existing loop unchanged ...
}
```

**4. Guard `get_prices_with_status`** — same pattern, inserted after the function opens (around `lib.rs:1855`):

```rust
pub fn get_prices_with_status(
    env: Env,
    assets: soroban_sdk::Vec<Symbol>,
) -> soroban_sdk::Vec<Option<PriceEntryWithStatus>> {
    // NEW: reject oversized input before any iteration
    if assets.len() > MAX_PATH_HOPS {
        panic_with_error!(&env, ContractError::OutOfBounds);
    }
    // ... existing loop unchanged ...
}
```

**5. Guard `get_index_price`** — insert length check before delegating to `calculate_index_price` (around `lib.rs:1217`):

```rust
pub fn get_index_price(
    env: Env,
    components: soroban_sdk::Vec<crate::types::AssetWeight>,
) -> Result<i128, ContractError> {
    if crate::auth::_is_halted(&env) {
        panic_with_error!(&env, ContractError::EmergencyHalted);
    }
    // NEW: reject oversized input before entering the calculation loop
    if components.len() > MAX_PATH_HOPS {
        return Err(ContractError::OutOfBounds);
    }
    validation::calculate_index_price(&env, &components)
}
```

#### File: `contracts/reward-splitter/src/lib.rs`

**1. Add `OutOfBounds` error variant to `Error`:**

```rust
/// Recipient list has reached the maximum allowed cap.
OutOfBounds = 15,
```

**2. Add `MAX_RECIPIENTS` constant:**

```rust
/// Maximum number of recipients allowed in the stored list.
/// Prevents the distribute function from exceeding the Soroban CPU budget.
const MAX_RECIPIENTS: u32 = 50;
```

**3. Guard `add_recipient`** — insert length check after reading the current list, before any modification:

```rust
pub fn add_recipient(env: Env, admin: Address, recipient: Address, share: u32) {
    Self::require_admin(&env, &admin);

    if share == 0 || share > 10000 {
        panic_with_error!(&env, Error::InvalidShare);
    }

    let mut recipients: Vec<Recipient> = env
        .storage()
        .instance()
        .get(&DataKey::Recipients)
        .unwrap_or_else(|| Vec::new(&env));

    // NEW: enforce recipient list cap before appending
    if recipients.len() >= MAX_RECIPIENTS {
        panic_with_error!(&env, Error::OutOfBounds);
    }

    // ... rest of existing logic unchanged ...
}
```

---

## Testing Strategy

### Validation Approach

The testing strategy follows a two-phase approach: first, surface counterexamples that demonstrate the bug on the **unfixed** code, then verify the fix works correctly and preserves all existing behavior.

### Exploratory Bug Condition Checking

**Goal**: Surface counterexamples that demonstrate the bug BEFORE implementing the fix. Confirm or refute the root cause analysis.

**Test Plan**: Write tests that construct oversized `Vec` inputs and call each affected entrypoint on the unfixed code. Assert that the result is NOT `Err(OutOfBounds)` — confirming the guard is absent — and that the call succeeds (or panics with a budget error rather than an `OutOfBounds` error).

**Test Cases:**

1. **`get_prices` oversized input (oracle)**: Call `get_prices` with 6 symbols on unfixed code. Expected: function iterates all 6 — no `OutOfBounds` returned (will fail once fixed code is in place). (will fail on unfixed code to return OutOfBounds)
2. **`get_prices_with_status` oversized input (oracle)**: Call `get_prices_with_status` with 6 symbols on unfixed code. Same expectation. (will fail on unfixed code)
3. **`get_index_price` oversized components (oracle)**: Call `get_index_price` with 6 `AssetWeight` components on unfixed code. Same expectation. (will fail on unfixed code)
4. **`add_recipient` past cap (splitter)**: Add 50 recipients then call `add_recipient` once more on unfixed code. Expected: recipient is appended (list has 51 entries), no `OutOfBounds` returned. (will fail on unfixed code)

**Expected Counterexamples:**

- None of the above calls return `Err(OutOfBounds)` on unfixed code — confirming the validation is absent.
- `get_prices` / `get_prices_with_status` / `get_index_price` return results (or budget panics) for lengths > 5.
- `add_recipient` succeeds for the 51st recipient.

### Fix Checking

**Goal**: Verify that for all inputs where the bug condition holds, the fixed functions produce the expected `OutOfBounds` error.

```
FOR ALL X WHERE isBugCondition_Oracle(X) DO
  result_get_prices        ← get_prices_fixed(X)
  result_get_prices_status ← get_prices_with_status_fixed(X)
  result_index_price       ← get_index_price_fixed(X)
  ASSERT result_get_prices        = Err(ContractError::OutOfBounds)
  ASSERT result_get_prices_status = Err(ContractError::OutOfBounds)
  ASSERT result_index_price       = Err(ContractError::OutOfBounds)
END FOR

FOR ALL state WHERE isBugCondition_Splitter(state.recipients) DO
  result ← add_recipient_fixed(admin, new_address, share)
  ASSERT result = Err(Error::OutOfBounds)
END FOR
```

### Preservation Checking

**Goal**: Verify that for all inputs where the bug condition does NOT hold, the fixed functions produce the same result as the original functions.

```
FOR ALL X WHERE NOT isBugCondition_Oracle(X) DO
  ASSERT get_prices_original(X)             = get_prices_fixed(X)
  ASSERT get_prices_with_status_original(X) = get_prices_with_status_fixed(X)
  ASSERT get_index_price_original(X)        = get_index_price_fixed(X)
END FOR

FOR ALL state WHERE NOT isBugCondition_Splitter(state.recipients) DO
  ASSERT add_recipient_original(state, ...) = add_recipient_fixed(state, ...)
END FOR
```

**Testing Approach**: Property-based testing is recommended for preservation checking because:
- It generates many random `Vec` lengths in the range `[0, MAX_PATH_HOPS]` automatically.
- It catches ordering and content regressions that hand-written examples might miss.
- It provides strong guarantees that behavior is unchanged for all valid-length inputs.

**Test Cases:**

1. **`get_prices` valid lengths (oracle preservation)**: Observe behavior for lengths 0–5 on unfixed code; write property tests asserting the same results on fixed code.
2. **`get_prices_with_status` valid lengths (oracle preservation)**: Same pattern.
3. **`get_index_price` valid lengths (oracle preservation)**: Same pattern.
4. **`add_recipient` under-cap (splitter preservation)**: Observe that adds 1–49 succeed on unfixed code; write property tests asserting the same results on fixed code.
5. **`distribute` up to cap (splitter preservation)**: Observe correct proportional transfers on unfixed code; assert same behavior on fixed code.

### Unit Tests

- Test that `get_prices` with exactly 5 symbols returns 5 entries (boundary — must pass).
- Test that `get_prices` with exactly 6 symbols returns `Err(ContractError::OutOfBounds)` (boundary — must pass after fix).
- Test that `get_prices` with 0 symbols returns an empty Vec (edge case — must still pass).
- Test that `get_prices_with_status` with 5 symbols returns 5 entries (boundary — must pass).
- Test that `get_prices_with_status` with 6 symbols returns `Err(ContractError::OutOfBounds)` (boundary — must pass after fix).
- Test that `get_index_price` with 5 components returns the correct weighted average (boundary — must pass).
- Test that `get_index_price` with 6 components returns `Err(ContractError::OutOfBounds)` (boundary — must pass after fix).
- Test that `add_recipient` with 50 existing recipients returns `Err(Error::OutOfBounds)` (boundary — must pass after fix).
- Test that `add_recipient` with 49 existing recipients succeeds (boundary — must pass).
- Test that `distribute` with 50 recipients still transfers correctly (preservation — must pass).

### Property-Based Tests

- Generate random `Vec<Symbol>` of length 0–5 and verify `get_prices` returns a vector of the same length with correct `None`/`Some` entries (preservation property).
- Generate random `Vec<Symbol>` of length 6–100 and verify `get_prices` returns `Err(ContractError::OutOfBounds)` (fix property).
- Generate random `Vec<AssetWeight>` of length 1–5 and verify `get_index_price` returns a value consistent with the weighted average formula (preservation property).
- Generate random `Vec<AssetWeight>` of length 6–100 and verify `get_index_price` returns `Err(ContractError::OutOfBounds)` (fix property).
- Generate random recipient lists of length 0–49 and verify `add_recipient` succeeds and increments `TotalShares` (preservation property).
- Generate contract states with 50 recipients and verify every `add_recipient` call returns `Err(Error::OutOfBounds)` (fix property).

### Integration Tests

- End-to-end: initialize oracle, register 5 assets, call `get_prices` with all 5 — verify full result vector.
- End-to-end: initialize oracle, register 5 assets, call `get_prices` with 6 symbols (5 registered + 1 unknown) — verify `Err(ContractError::OutOfBounds)` rather than a 6-element result with a `None`.
- End-to-end: initialize splitter, add 50 recipients each with 200 bp, attempt to add a 51st — verify `Err(Error::OutOfBounds)`.
- End-to-end: initialize splitter, add 50 recipients each with 200 bp, call `distribute` — verify all 50 receive correct amounts.
- Regression: verify that the emergency-halt check in `get_prices` still fires before the length check (halt takes priority over bounds).
