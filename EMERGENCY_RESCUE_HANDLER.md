# Timelocked Protocol Treasury Emergency Rescue Handler (#783)

## Overview

This document describes the implementation of the **Timelocked Protocol Treasury Emergency Rescue Handler** (Issue #783), which enables governance to safely recover mis-sent non-protocol tokens stuck in contract addresses after a mandatory timelock delay, while strictly protecting primary pool and vault reserve assets.

## Acceptance Criteria Verification ✓

All acceptance criteria specified in Issue #783 are implemented and fully verified:

1. **Governance Proposal Queueing**:
   - Governance/admin proposes a token rescue action using `queue_token_rescue()`.
   - Action enters a mandatory 48-hour timelock delay window (`RESCUE_TIMELOCK_DELAY = 172,800` seconds).
   - Unique monotonically increasing `proposal_id` assigned and status set to `Pending`.

2. **Protected Assets Safeguard**:
   - Admin registers primary pool tokens and vault reserve assets using `register_protected_asset()`.
   - Both `queue_token_rescue()` and `execute_token_rescue()` enforce `is_protected_asset()` check.
   - Any attempt to rescue a protected asset reverts immediately with `ContractError::ProtectedAssetNotRescueable`.

3. **Timelocked Treasury Execution**:
   - Execution (`execute_token_rescue()`) is blocked until current ledger timestamp exceeds `execute_at` deadline.
   - Upon timelock expiration, tokens are transferred directly to the designated protocol treasury address via `soroban_sdk::token::Client`.
   - Lifecycle status updated to `Executed` and event `rsc_exec` emitted.

---

## Architecture & Interface

### Core Module (`src/rescue.rs`)

**Data Structures:**
```rust
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RescueProposalStatus {
    Pending,
    Executed,
    Cancelled,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RescueProposal {
    pub proposal_id: u64,
    pub token: Address,
    pub amount: i128,
    pub recipient: Address,
    pub proposer: Address,
    pub staged_at: u64,
    pub execute_at: u64,
    pub status: RescueProposalStatus,
}
```

**Storage Keys:**
- `RSCPROP`: Prefix for proposal entries `(RESCUE_PROPOSAL_KEY, proposal_id)`
- `RSCCNT`: Monotonically increasing proposal counter `u64`
- `PRTASST`: Protected assets lookup map `Map<Address, bool>`

**Public Interface:**
```rust
pub fn register_protected_asset(env: Env, caller: Address, asset: Address) -> Result<(), ContractError>;
pub fn is_protected_asset(env: Env, asset: Address) -> bool;
pub fn queue_token_rescue(env: Env, proposer: Address, token: Address, amount: i128, recipient: Address) -> Result<u64, ContractError>;
pub fn execute_token_rescue(env: Env, executor: Address, proposal_id: u64) -> Result<(), ContractError>;
pub fn cancel_token_rescue(env: Env, canceller: Address, proposal_id: u64) -> Result<(), ContractError>;
pub fn get_rescue_proposal(env: Env, proposal_id: u64) -> Option<RescueProposal>;
```

---

## Error Codes

The following error variants were added to `ContractError`:
- `ProtectedAssetNotRescueable = 75`: Attempted emergency rescue on a protected pool/vault reserve asset.
- `RescueProposalNotFound = 76`: Specified rescue proposal ID does not exist.
- `RescueProposalNotPending = 77`: Proposal is not in `Pending` status (already executed or cancelled).
- `RescueTimelockNotExpired = 78`: Execution attempted before 48-hour timelock expired.

---

## Events

Standardized event topics emitted:
- `prt_asset`: Emitted when an admin registers a protected asset `(caller, asset)`.
- `rsc_queue`: Emitted when a rescue action is queued `(proposer, token, amount, recipient, execute_at)`.
- `rsc_exec`: Emitted upon execution `(executor, token, amount, recipient)`.
- `rsc_canc`: Emitted upon cancellation `(canceller,)`.

---

## Testing & Verification

1. **Unit Tests (`src/rescue.rs`)**:
   - Protection registration authorization and validation.
   - Non-protected vs protected asset queueing bounds.
   - Premature execution rejection.
   - Timelock cancellation.

2. **Integration Tests (`tests/rescue_integration_test.rs`)**:
   - Full end-to-end token transfer to treasury address using Soroban asset contract client.
   - Verification of contract balance updates post-rescue.
