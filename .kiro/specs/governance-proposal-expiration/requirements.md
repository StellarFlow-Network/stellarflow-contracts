# Requirements Document

## Introduction

This feature adds a governance proposal expiration and storage cleanup routine to `src/governance.rs` in the stellarflow-contracts workspace. The routine automatically transitions proposals to a "defunct" status when they fail to achieve passage within a 20,000-ledger window, and permanently frees every associated storage slot upon that transition. All ledger-sequence reads are delegated to the centralised `ledger-time-helper` crate. The expiration check operates in strict O(1) complexity — it works on a single, caller-supplied proposal ID and never iterates the full proposal map.

---

## Glossary

- **Governance_Module**: The Soroban smart contract module implemented in `src/governance.rs` that manages governance proposals and their lifecycle.
- **Ledger_Time_Helper**: The centralised helper crate located at `contracts/ledger-time-helper` that exposes `current_ledger_sequence()` — the authoritative source for on-chain ledger sequence numbers used by all time-based checks.
- **Proposal**: A governance action record stored in the contract's persistent storage, identified by a unique `proposal_id: u64`.
- **ProposalStatus**: An enum describing the lifecycle state of a Proposal. Valid variants are `Active`, `Passed`, `Rejected`, `Defunct`.
- **Defunct**: The terminal ProposalStatus assigned to a Proposal that has not reached `Passed` status within the 20,000-ledger expiration window.
- **Expiration_Window**: The fixed constant of 20,000 ledgers elapsed since a Proposal's `created_at_ledger` field, after which the Proposal is eligible to be marked `Defunct`.
- **Storage_Key**: A `DataKey` enum variant used as the key to a Soroban persistent storage slot. Each Proposal occupies at least a `Proposal(u64)` slot and a `ProposalVotes(u64)` slot.
- **Cleanup**: The act of calling `env.storage().persistent().remove()` on every Storage_Key associated with a given Proposal ID, leaving no dangling references or orphaned data in contract storage.
- **Unit_Test**: A native Rust `#[test]` function using the Soroban mock environment (`Env::default()`) that exercises the expiration and cleanup logic in isolation.
- **Boundary_Condition**: The exact ledger at which elapsed ledgers equals 20,000 (i.e., `current_ledger == created_at_ledger + 20_000`).

---

## Requirements

### Requirement 1: Proposal Data Model

**User Story:** As a smart contract developer, I want a well-typed Proposal struct and ProposalStatus enum stored under deterministic keys, so that the expiration routine can locate and remove every storage slot for a given proposal in O(1) time.

#### Acceptance Criteria

1. THE Governance_Module SHALL define a `ProposalStatus` enum with exactly four variants: `Active`, `Passed`, `Rejected`, and `Defunct`, annotated with `#[contracttype]`.
2. THE Governance_Module SHALL define a `Proposal` struct annotated with `#[contracttype]` containing at minimum the fields: `proposal_id: u64`, `status: ProposalStatus`, `created_at_ledger: u32`, and `vote_count: u32`.
3. THE Governance_Module SHALL define a `DataKey` enum annotated with `#[contracttype]` that includes at minimum the variants `Proposal(u64)` and `ProposalVotes(u64)`.
4. WHEN a Proposal is created, THE Governance_Module SHALL write exactly one `DataKey::Proposal(proposal_id)` slot and exactly one `DataKey::ProposalVotes(proposal_id)` slot to persistent storage, and no additional per-proposal storage slots.

---

### Requirement 2: Ledger Sequence Access via Ledger_Time_Helper

**User Story:** As a smart contract developer, I want all ledger-sequence reads to go through the centralised Ledger_Time_Helper, so that time-based logic is not scattered across modules and can be mocked uniformly in tests.

#### Acceptance Criteria

1. WHEN the Governance_Module requires the current ledger sequence number, THE Governance_Module SHALL obtain it exclusively by calling `ledger_time_helper::current_ledger_sequence(env)` — direct calls to `env.ledger().sequence()` or any other ledger-reading primitive inside `src/governance.rs` are not permitted.
2. WHEN the `ledger-time-helper` crate is updated to change its sequence-reading logic, THE Governance_Module SHALL inherit that change without any modification to `src/governance.rs`.

> **Note on the current helper:** The `contracts/ledger-time-helper/src/lib.rs` presently exports `current_ledger_timestamp` (Unix seconds). A companion function `current_ledger_sequence(env: &Env) -> u32` returning `env.ledger().sequence()` must be added to that crate as part of this feature's implementation. The governance module then calls only that exported symbol.

---

### Requirement 3: Expiration Check Routine

**User Story:** As a smart contract developer, I want a single-proposal expiration function that marks a proposal defunct when its 20,000-ledger window has elapsed, so that stale proposals are automatically retired without expensive on-chain iteration.

#### Acceptance Criteria

1. THE Governance_Module SHALL expose a public function `expire_proposal(env: Env, proposal_id: u64) -> Result<ProposalStatus, GovernanceError>` that checks and, if appropriate, transitions a single Proposal to `Defunct`.
2. WHEN `expire_proposal` is called and the Proposal identified by `proposal_id` does not exist in storage, THEN THE Governance_Module SHALL return `GovernanceError::ProposalNotFound`.
3. WHEN `expire_proposal` is called and the Proposal's `status` is already `Passed`, `Rejected`, or `Defunct`, THEN THE Governance_Module SHALL return the current `ProposalStatus` unchanged without modifying any storage.
4. WHEN `expire_proposal` is called and the Proposal's `status` is `Active` and the elapsed ledger count (`current_ledger_sequence(env) - proposal.created_at_ledger`) is strictly less than 20,000, THEN THE Governance_Module SHALL return `ProposalStatus::Active` without modifying any storage.
5. WHEN `expire_proposal` is called and the Proposal's `status` is `Active` and the elapsed ledger count is greater than or equal to 20,000, THEN THE Governance_Module SHALL transition the Proposal to `ProposalStatus::Defunct` and execute storage Cleanup for that `proposal_id`.
6. THE `expire_proposal` function SHALL execute in O(1) storage operations — it SHALL perform at most two storage reads, one optional storage write, and exactly two storage removals (for a defunct transition), and SHALL NOT iterate over any collection of proposals.

---

### Requirement 4: Storage Cleanup on Defunct Transition

**User Story:** As a smart contract developer, I want every storage slot for a defunct proposal to be permanently removed upon expiration, so that the contract does not accumulate orphaned state that consumes on-chain storage fees.

#### Acceptance Criteria

1. WHEN a Proposal is transitioned to `Defunct`, THE Governance_Module SHALL call `env.storage().persistent().remove(&DataKey::Proposal(proposal_id))`.
2. WHEN a Proposal is transitioned to `Defunct`, THE Governance_Module SHALL call `env.storage().persistent().remove(&DataKey::ProposalVotes(proposal_id))`.
3. WHEN a Proposal is transitioned to `Defunct`, THE Governance_Module SHALL NOT leave any storage slot with a key derived from `proposal_id` in a non-removed state.
4. AFTER a Proposal is transitioned to `Defunct`, WHEN `env.storage().persistent().get::<DataKey, Proposal>(&DataKey::Proposal(proposal_id))` is called, THE Governance_Module's storage SHALL return `None`.
5. AFTER a Proposal is transitioned to `Defunct`, WHEN `env.storage().persistent().get::<DataKey, u32>(&DataKey::ProposalVotes(proposal_id))` is called, THE Governance_Module's storage SHALL return `None`.

---

### Requirement 5: Expiration Boundary Semantics

**User Story:** As a smart contract developer, I want the expiration boundary to be defined as an inclusive 20,000-ledger threshold, so that the exact boundary ledger triggers expiration and tests can verify the transition deterministically.

#### Acceptance Criteria

1. THE Governance_Module SHALL define the expiration window as the constant `EXPIRATION_WINDOW_LEDGERS: u32 = 20_000`.
2. WHEN the elapsed ledger count equals exactly 20,000 (i.e., `current_ledger == created_at_ledger + EXPIRATION_WINDOW_LEDGERS`), THE Governance_Module SHALL treat the Proposal as expired and transition it to `Defunct`.
3. WHEN the elapsed ledger count equals exactly 19,999, THE Governance_Module SHALL treat the Proposal as still `Active` and perform no transition.
4. THE Governance_Module SHALL compute elapsed ledgers as `current_ledger_sequence(env).saturating_sub(proposal.created_at_ledger)` to prevent underflow when ledger values are unusual.

---

### Requirement 6: Error Type

**User Story:** As a smart contract developer, I want a typed error enum for the governance module so that callers can handle failure modes programmatically.

#### Acceptance Criteria

1. THE Governance_Module SHALL define a `GovernanceError` enum annotated with `#[contracterror]` containing at minimum the variant `ProposalNotFound = 1`.
2. IF a caller invokes `expire_proposal` with a `proposal_id` for which no `DataKey::Proposal(proposal_id)` entry exists in storage, THEN THE Governance_Module SHALL return `Err(GovernanceError::ProposalNotFound)`.

---

### Requirement 7: Unit Test — Boundary Condition and Storage Verification

**User Story:** As a smart contract developer, I want a comprehensive unit test that mocks the ledger sequence at exactly the 20,000-ledger boundary, so that I can verify the defunct transition and confirm storage slots are freed without relying on real ledger state.

#### Acceptance Criteria

1. THE Unit_Test SHALL use `soroban_sdk::Env::default()` as the mock environment.
2. THE Unit_Test SHALL set the ledger sequence to `created_at_ledger + EXPIRATION_WINDOW_LEDGERS` using `env.ledger().set(LedgerInfo { sequence_number: ..., ..Default::default() })` or the equivalent Soroban testutils setter, to simulate the exact Boundary_Condition.
3. THE Unit_Test SHALL call `expire_proposal` at the Boundary_Condition ledger and assert that the returned status is `ProposalStatus::Defunct`.
4. THE Unit_Test SHALL assert that `env.storage().persistent().get::<DataKey, Proposal>(&DataKey::Proposal(proposal_id))` returns `None` after the call.
5. THE Unit_Test SHALL assert that `env.storage().persistent().get::<DataKey, u32>(&DataKey::ProposalVotes(proposal_id))` returns `None` after the call.
6. THE Unit_Test SHALL include a negative case: set the ledger sequence to `created_at_ledger + EXPIRATION_WINDOW_LEDGERS - 1` (19,999 ledgers elapsed) and assert that `expire_proposal` returns `ProposalStatus::Active` and both storage slots remain present.
7. THE Unit_Test SHALL include a non-existent-proposal case: call `expire_proposal` with an unused `proposal_id` and assert the result is `Err(GovernanceError::ProposalNotFound)`.
8. THE Unit_Test SHALL always include an already-defunct case: call `expire_proposal` twice in succession at the boundary ledger and assert the second call returns `Err(GovernanceError::ProposalNotFound)` without panicking, confirming idempotent cleanup behaviour regardless of test setup conditions.
