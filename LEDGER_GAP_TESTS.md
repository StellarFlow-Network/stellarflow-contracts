# Ledger Gap Enforcement Tests

This document outlines the test cases for Issue #369: Ledger-Sync | Enforcing Absolute Chronological Gaps Between Node Ingestions.

## Test Cases

### 1. `test_ledger_gap_new_provider_allowed`
- Tests that a new provider (first submission) can submit without any ledger gap restrictions
- Provider submits at ledger 100
- Should succeed

### 2. `test_ledger_gap_insufficient_gap_rejected`
- Tests that a provider cannot submit if less than 3 blocks have passed
- Provider submits at ledger 100
- Same provider tries to submit at ledger 102 (gap of 2 blocks)
- Should fail with `ContractError::LedgerGapTooSmall`

### 3. `test_ledger_gap_exactly_3_blocks_allowed`
- Tests that a provider can submit after exactly 3 blocks have passed
- Provider submits at ledger 100
- Same provider submits at ledger 103 (gap of exactly 3 blocks)
- Should succeed

### 4. `test_ledger_gap_more_than_3_blocks_allowed`
- Tests that a provider can submit with a larger gap
- Provider submits at ledger 100
- Same provider submits at ledger 150 (gap of 50 blocks)
- Should succeed

### 5. `test_ledger_gap_multiple_providers_independent`
- Tests that ledger gap tracking is independent for each provider
- Provider A submits at ledger 100
- Provider B submits at ledger 101 (no gap restriction for new provider)
- Provider A tries to submit at ledger 102 (fails, gap too small)
- Provider B can submit again at ledger 101 (succeeds, first submission for B)
- Provider B tries to submit at ledger 103 (fails, gap too small for B)
- Provider B submits at ledger 104 (succeeds, gap is exactly 3 for B)

## Implementation Details

- Minimum ledger gap: 3 blocks
- Stored in `DataKey::ProviderLastSeenLedger` (already existed)
- New error code: `ContractError::LedgerGapTooSmall = 53`
- Validation occurs in `update_price()` function
- Helper function: `enforce_ledger_gap()`
