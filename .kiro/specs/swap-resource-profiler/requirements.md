# Requirements Document

## Introduction

The Swap Resource Profiler is a Soroban smart contract component within the StellarFlow workspace that measures and enforces CPU instruction consumption and memory footprint for every swap transaction. Because Soroban enforces hard per-transaction resource budgets (CPU instructions and memory bytes), any contract entrypoint that unexpectedly exceeds those limits will be aborted on-chain, causing a gas budget breach. This feature provides per-entrypoint profiling, structured log emission, and a hard budget-guard that rejects operations projected to exceed safe thresholds before they can breach the network limit.

## Glossary

- **Profiler**: The swap-resource-profiler contract module responsible for capturing and validating resource usage data.
- **Resource_Snapshot**: A structured record containing `cpu_instructions` (u64) and `memory_bytes` (u64) captured at a single measurement point.
- **Entrypoint**: A named contract function exposed in a `#[contractimpl]` block (e.g., `swap`, `multi_hop_swap`, `deposit`).
- **Budget_Guard**: The Profiler sub-component that compares a Resource_Snapshot against configured thresholds and rejects over-budget operations.
- **Cpu_Limit**: The maximum allowed CPU instruction count per transaction, expressed as a u64 in Soroban instruction units.
- **Memory_Limit**: The maximum allowed memory footprint per transaction, expressed as a u64 in bytes.
- **Profile_Record**: A persistent on-chain log entry containing the entrypoint name, Resource_Snapshot, ledger sequence, and a budget-breach flag.
- **Safety_Margin_Bps**: A governance-configurable percentage expressed in basis points (0–9999) subtracted from the network hard limit to define the effective threshold, providing a buffer before the hard cap is reached.
- **Swap_Contract**: The StellarFlow swap contract whose entrypoints are subject to resource profiling.
- **Admin**: The authorized address permitted to configure thresholds and retrieve aggregated profile data.

## Requirements

### Requirement 1: Resource Snapshot Capture

**User Story:** As a smart contract developer, I want CPU instruction usage and memory consumption to be captured after each swap entrypoint executes, so that I have accurate per-call resource data for analysis and alerting.

#### Acceptance Criteria

1. WHEN a swap entrypoint completes execution, THE Profiler SHALL capture a Resource_Snapshot containing the CPU instruction count and memory byte count consumed during that invocation (delta values representing budget units consumed during that call, not cumulative totals), including cases where either value is zero.
2. THE Profiler SHALL read CPU instruction consumption from `env.budget().cpu_instruction_count()` and memory consumption from `env.budget().memory_bytes_used()` as provided by the Soroban SDK, where these values are read immediately after the entrypoint returns and before any subsequent operations alter the budget state.
3. WHEN a Resource_Snapshot is captured, THE Profiler SHALL record the name of the Entrypoint being profiled alongside the snapshot values, where the entrypoint name is stored as a string exactly matching the function identifier (e.g., `"swap"`, `"multi_hop_swap"`, `"deposit"`, `"withdraw"`).
4. THE Profiler SHALL capture a Resource_Snapshot for each of the following Swap_Contract entrypoints: `swap`, `multi_hop_swap`, `deposit`, and `withdraw`.

---

### Requirement 2: On-Chain Profile Logging

**User Story:** As a smart contract developer, I want each resource snapshot to be persisted as a structured on-chain log entry, so that I can query historical resource usage across ledgers.

#### Acceptance Criteria

1. WHEN a Resource_Snapshot is captured, THE Profiler SHALL store a Profile_Record in contract persistent storage keyed by entrypoint name and ledger sequence number; IF a record already exists for the same entrypoint name and ledger sequence number, THEN THE Profiler SHALL overwrite it with the new Profile_Record.
2. WHEN a Resource_Snapshot is captured, THE Profiler SHALL emit a Soroban contract event containing the entrypoint name, `cpu_instructions`, `memory_bytes`, and a boolean `budget_breached` flag after each measurement, regardless of whether persistent record storage succeeds.
3. WHEN persistent storage for Profile_Records exceeds 50 entries per entrypoint, THE Profiler SHALL overwrite the entry with the lowest ledger sequence number for that entrypoint using a circular buffer strategy.
4. THE Profiler SHALL record the current ledger sequence number (via `env.ledger().sequence()`) in every Profile_Record so that callers can correlate records with on-chain time.
5. IF persistent storage of the Profile_Record fails, THEN THE Profiler SHALL still emit the contract event as specified in criterion 2 and SHALL return a `StorageFailure` error to the caller.

---

### Requirement 3: Budget Threshold Configuration

**User Story:** As an Admin, I want to configure CPU and memory thresholds per entrypoint with a configurable safety margin, so that budget guards can be tuned without redeploying the contract.

#### Acceptance Criteria

1. IF `set_cpu_limit` or `set_memory_limit` is called by an address that is not the stored Admin, THEN THE Profiler SHALL return an `Unauthorized` error without modifying stored state.
2. IF `set_cpu_limit` is called with a `Cpu_Limit` value of 0 or greater than 100,000,000, THEN THE Profiler SHALL return an `InvalidLimit` error without modifying stored state.
3. IF `set_memory_limit` is called with a `Memory_Limit` value of 0 or greater than 40,960,000, THEN THE Profiler SHALL return an `InvalidLimit` error without modifying stored state.
4. THE Admin SHALL be able to set a `Safety_Margin_Bps` value in the range 0–9999 inclusive via a `set_safety_margin` entrypoint.
5. IF `set_safety_margin` is called with a value outside the range 0–9999, THEN THE Profiler SHALL return an `InvalidSafetyMargin` error without modifying stored state.
6. THE Profiler SHALL apply the `Safety_Margin_Bps` to compute an effective threshold using floor-truncating integer arithmetic: `effective_threshold = limit * (10_000 - safety_margin_bps) / 10_000`.
7. WHEN no explicit limit has been configured for an entrypoint, THE Profiler SHALL apply Soroban's default network limits as the baseline: 100,000,000 CPU instructions and 40,960,000 memory bytes.
8. THE Profiler SHALL expose a `get_effective_threshold` read-only entrypoint that accepts an entrypoint name and returns the stored limit, the current `Safety_Margin_Bps`, and the computed effective threshold for that entrypoint.

---

### Requirement 4: Budget Guard Enforcement

**User Story:** As a smart contract operator, I want swap operations that exceed their configured resource budget to be rejected before they breach the Soroban network limit, so that transactions do not fail on-chain due to gas budget exhaustion.

#### Acceptance Criteria

1. WHEN a Resource_Snapshot is captured and `cpu_instructions` is strictly greater than the configured effective CPU threshold for that Entrypoint, THEN THE Budget_Guard SHALL return a `CpuBudgetExceeded` error, set the `budget_breached` flag to `true` in the Profile_Record, and persist that Profile_Record to storage.
2. WHEN a Resource_Snapshot is captured and `memory_bytes` is strictly greater than the configured effective memory threshold for that Entrypoint, THEN THE Budget_Guard SHALL return a `MemoryBudgetExceeded` error, set the `budget_breached` flag to `true` in the Profile_Record, and persist that Profile_Record to storage.
3. IF a Resource_Snapshot has `cpu_instructions` less than or equal to the effective CPU threshold AND `memory_bytes` less than or equal to the effective memory threshold, THEN THE Budget_Guard SHALL set the `budget_breached` flag to `false` in the Profile_Record and allow execution to complete normally.
4. WHEN THE Budget_Guard rejects an operation with `CpuBudgetExceeded` or `MemoryBudgetExceeded`, THE Profiler SHALL emit the contract event with the `cpu_instructions` and `memory_bytes` values from the captured Resource_Snapshot and `budget_breached = true` before returning the error.
5. IF a Resource_Snapshot has `cpu_instructions` strictly greater than the effective CPU threshold AND `memory_bytes` strictly greater than the effective memory threshold, THEN THE Budget_Guard SHALL return a `CpuBudgetExceeded` error (CPU takes precedence) and set `budget_breached = true`.

---

### Requirement 5: Profile Data Retrieval

**User Story:** As an Admin, I want to query the most recent resource profile records for any entrypoint, so that I can monitor resource trends and investigate near-limit invocations.

#### Acceptance Criteria

1. WHEN `get_latest_profiles` is called with an entrypoint name and a count `n` (1–50 inclusive), THE Profiler SHALL return up to `n` of the most recent Profile_Records for that entrypoint in descending ledger sequence order; IF fewer than `n` records exist, THE Profiler SHALL return all available records.
2. IF `get_latest_profiles` is called with `n` outside the range 1–50, THEN THE Profiler SHALL return an `InvalidQueryCount` error.
3. IF `get_latest_profiles` is called for an entrypoint that has no stored records, THEN THE Profiler SHALL return an empty list without error.
4. WHEN `get_peak_usage` is called with an entrypoint name, THE Profiler SHALL return the single Profile_Record with the highest `cpu_instructions` value observed for that entrypoint across all stored records; IF multiple records share the highest `cpu_instructions` value, THE Profiler SHALL return the record with the greatest ledger sequence number.
5. IF `get_peak_usage` is called for an entrypoint with no stored records, THEN THE Profiler SHALL return a `NoRecordsFound` error.

---

### Requirement 6: Initialization and Authorization

**User Story:** As a deployer, I want the Profiler to require explicit initialization before accepting profile data, so that misconfigured deployments are rejected at the gate.

#### Acceptance Criteria

1. WHEN `initialize` is called with a valid Admin address on an uninitialized Profiler, THE Profiler SHALL store the Admin address in instance storage and transition to an initialized state.
2. IF `initialize` is called with a zero address or an otherwise invalid Admin address, THEN THE Profiler SHALL return an `InvalidAdmin` error without modifying stored state.
3. IF `initialize` is called on an already-initialized Profiler, THEN THE Profiler SHALL return an `AlreadyInitialized` error without modifying stored state.
4. IF any of the admin-only entrypoints (`set_cpu_limit`, `set_memory_limit`, `set_safety_margin`) are called by an address that is not the stored Admin, THEN THE Profiler SHALL return an `Unauthorized` error.
5. IF any of the profiling or query entrypoints (`record_profile`, `get_latest_profiles`, `get_peak_usage`, `get_utilization_pct`) are called before `initialize` has been invoked, THEN THE Profiler SHALL return a `NotInitialized` error.

---

### Requirement 7: Soroban Network Limit Compliance Reporting

**User Story:** As a smart contract operator, I want the Profiler to compute and expose the percentage of the Soroban network hard limit consumed per entrypoint call, so that I can verify operations are staying safely within network-imposed resource bounds.

#### Acceptance Criteria

1. WHEN `get_utilization_pct` is called with an entrypoint name, THE Profiler SHALL return a `UtilizationReport` containing `cpu_utilization_bps` and `memory_utilization_bps` computed from the Profile_Record with the highest recorded ledger sequence number for that entrypoint.
2. THE Profiler SHALL compute `cpu_utilization_bps` as `(cpu_instructions * 10_000) / cpu_network_hard_limit` where `cpu_network_hard_limit` is 100,000,000.
3. THE Profiler SHALL compute `memory_utilization_bps` as `(memory_bytes * 10_000) / memory_network_hard_limit` where `memory_network_hard_limit` is 40,960,000.
4. IF `get_utilization_pct` is called for an entrypoint with no recorded Profile_Records, THEN THE Profiler SHALL return a `NoRecordsFound` error; IF the entrypoint name is empty or invalid, THEN THE Profiler SHALL return an `InvalidEntrypoint` error.
5. IF `cpu_utilization_bps` or `memory_utilization_bps` is strictly greater than 9,000 (i.e., greater than 90% of the network hard limit), THEN THE Profiler SHALL include `near_limit_warning = true` in the returned `UtilizationReport`.
6. IF both `cpu_utilization_bps` and `memory_utilization_bps` are less than or equal to 9,000, THEN THE Profiler SHALL include `near_limit_warning = false` in the returned `UtilizationReport`.
