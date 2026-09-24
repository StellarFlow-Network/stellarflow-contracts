# StellarFlow Architecture Specification

## 1. System Topology

StellarFlow is composed of **1 core router contract** and **7 satellite contracts** deployed on Soroban (Stellar's smart-contract platform). All contracts target Soroban SDK v20.0.0 and use `#![no_std]`.

### 1.1 Contract Inventory

| Contract | Crate | Role |
|---|---|---|
| `TimeLockedUpgradeContract` | `stellarflow-contracts` (root) | Core router: staking, admin, governance, fees, upgrades, telemetry validation, slashing, recovery |
| `PriceOracle` | `contracts/price-oracle` | On-chain price feed: multi-source median, twap, asset registry, circuit-breaker, callback subscriptions |
| `GasTank` | `contracts/gas-tank` | Prepaid gas metering: deposit/withdraw/allowance/reimburse for relayer tx costs |
| `LiquidityLockContract` | `contracts/liquidity-lock` | Time-locked streaming: linear vesting over 3000 ledgers |
| `RewardSplitter` | `contracts/reward-splitter` | Proportional distribution: recipient registry with basis-point shares, multi-stage cooldown governance |
| `AnalyticsEngine` | `contracts/analytics-engine` | EMA smoothing: single rolling metric per asset id |
| `LedgerTimeHelper` | `contracts/ledger-time-helper` | Library: exposes `current_ledger_timestamp()` and `current_ledger_sequence()` |
| `HelloWorld` | `contracts/hello-world` | Placeholder/demo contract |

### 1.2 Deployment Topology

```
                        ┌──────────────────────────────┐
                        │   TimeLockedUpgradeContract   │
                        │     (Core Router)             │
                        │  - staking / unstaking        │
                        │  - admin & multi-sig          │
                        │  - governance & upgrades      │
                        │  - fees & corridor weights    │
                        │  - telemetry validation       │
                        │  - slashing                   │
                        │  - dead-man's-switch recovery │
                        │  - price variance config      │
                        └──────────┬───────────────────┘
                                   │
              ┌────────────────────┼────────────────────┐
              │                    │                    │
              ▼                    ▼                    ▼
   ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐
   │   PriceOracle     │  │    GasTank       │  │ LiquidityLock    │
   │  - price feeds    │  │  - gas deposits  │  │ - token streams  │
   │  - median calc    │  │  - allowances    │  │ - linear vesting │
   │  - circuit-breaker│  │  - reimbursements│  │                  │
   │  - subscriptions  │  │                  │  │                  │
   └──────────────────┘  └──────────────────┘  └──────────────────┘
              │                    │
              ▼                    ▼
   ┌──────────────────┐  ┌──────────────────┐
   │ AnalyticsEngine   │  │ RewardSplitter   │
   │  - EMA smoothing  │  │ - distribution   │
   │  - per-asset      │  │ - cooldown gov   │
   └──────────────────┘  └──────────────────┘
```

## 2. Cross-Contract Call Topology

### 2.1 Core Router → Satellite Calls

| Caller | Callee | Trigger | Data Flow |
|---|---|---|---|
| `TimeLockedUpgradeContract` | `GasTank::reimburse` | Telemetry submission acceptance | Oracle address triggers gas reimbursement for relayers |
| `PriceOracle` | `GasTank::reimburse` | Price submission acceptance | Oracle contract calls `reimburse` to pay relayer gas |
| `PriceOracle` | Subscriber contracts' `on_price_update` | Price update for tracked asset | Oracle pushes new price data to all subscribed downstream contracts |
| `PriceOracle` | SEP-41 token `transfer` | Staking/slashing/rewards | Token movements between providers, vault, and insurance reserve |
| `LiquidityLockContract` | SEP-41 token `transfer` | Stream creation / claim | Token transfers from admin to contract, contract to recipient |
| `GasTank` | SEP-41 token `transfer` | Deposit/withdraw/reimburse | Token movements between consumers, contract, and relayers |
| `RewardSplitter` | SEP-41 token `transfer` | Distribution | Token transfers from contract to each recipient |

### 2.2 PriceOracle Client Interface

The `StellarFlowClient` generated client trait exposes the primary cross-contract entrypoint for downstream Soroban applications:

```
get_price(asset, verified)          → PriceData
get_last_price(asset)               → i128
get_prices(assets)                  → Vec<Option<PriceEntry>>
get_index_price(components)         → i128
get_price_with_status(asset)        → PriceDataWithStatus
get_price_safe(asset)               → Option<PriceData>
get_twap(asset)                     → Option<i128>
```

### 2.3 Subscription Callback Pattern

```
PriceOracle                              Subscriber Contract
     │                                           │
     ├─ subscribe_to_price_updates(cb_addr) ────►│
     │                                           │
     │  ... on price update ...                  │
     │                                           │
     ├─ on_price_update(asset, price) ──────────►│
     │                                           │
     ├─ unsubscribe(cb_addr) ───────────────────►│
```

## 3. State Management Layouts

### 3.1 Storage Class Usage

| Storage Class | Use Case | Key Examples |
|---|---|---|
| **Instance** | Persistent contract config & admin data | `DATA_KEY` (ContractData), `TOTAL_STAKED_KEY`, `HB_INTERVAL_KEY`, `SIGNERS` count, `STAKE_REGISTRY_KEY`, admin change proposals, ownership transfer, treasury |
| **Persistent** | Long-lived per-entity records | `NodeProfile`, `FeedStake`, `AssetMetrics`, `CorridorFeePool`, `CorridorWeightProfile`, nonce state, slashing history, feed stake values |
| **Temporary** | Ephemeral round-state & ballots | Heartbeat timestamps, consensus participant cache, voting ballots (`BallotKey`), revocation proposals, emergency revocation proposals |

### 3.2 Core Router State Layout

```
Instance Storage:
  DATA_KEY                → ContractData { admin: Address, value: u64, max_fee_ceiling: u64 }
  TREASURY_KEY            → Address                  (immutable after init)
  TOTAL_STAKED_KEY        → u64                      (global staked sum)
  HB_INTERVAL_KEY         → u64                      (heartbeat interval seconds)
  SIGNERS_KEY             → u32                      (signer count)
  PAUSED_KEY              → bool                     (emergency pause flag)
  PNDOWN                  → PendingOwner             (ownership transfer nominee)
  PADMIN                  → AdminChangeProposal      (two-phase admin change)
  PVARCFG                 → PriceVarianceConfig      (sealed variance settings)
  CURR_WASM               → BytesN<32>               (current wasm hash)
  PREV_WASM               → BytesN<32>               (previous wasm hash for rollback)
  UPG_TIME                → u64                      (upgrade timestamp)
  RKEY                    → Address                  (recovery key)
  LASTACT                 → u64                      (last admin activity timestamp)
  CAPITAL                 → u64                      (platform capital)
  SEQCTR                  → u64                      (sequence counter)
  BLKTRK                  → Map<Address, u32>        (per-node block tracker)
  SLASHED                 → Map<Address, u64>        (per-node slashed total)
  STAKES                  → Map<Address, u64>        (global stake registry - legacy)
  SchemaVersion           → u32                      (migration version)

  Tuple-Keyed Instance:
    StakeKey::StakeByNode(Address)           → u64
    SignerKey::SignerByAddress(Address)      → bool
    RevokedSignerKey::RevokedByAddress(Addr) → bool
    ConsensusStorageKey::ConsensusSeq(Sym)   → u32
    ConsensusStorageKey::EpochSeqArchive(Sym)→ u32
    NonceKey::State(Address)                 → NonceState
    AdminNonceKey::Action(Addr, AdminAction) → u64

Persistent Storage:
  NodeProfileKey::ProfileByNode(Address)     → NodeProfile
  StakingStorageKey::TierConfig              → StakingTierConfig
  StakingStorageKey::AssetMetrics(AssetId)   → AssetFeedMetrics
  StakingStorageKey::FeedStake(Addr,AssetId) → FeedStakeValue
  FeesStorageKey::CorridorPool(AssetId)      → CorridorFeePool
  CorridorWeightKey::Profile(AssetId)        → CorridorWeightProfile
  SlashingStorageKey::FaultHistory(Addr,Sym) → TrackingFaultHistory

Temporary Storage:
  BallotKey::Proposal(Symbol)                → VotingBallot
  CONSENSUS_CACHE_KEY                        → Vec<Address>
  HEARTBEAT_KEY                              → Map<AssetId, u64>
  EMREV_T (revocation temp)                  → EmergencyRevocationProposal
```

### 3.3 PriceOracle State Layout

```
Instance Storage:
  DataKey::Initialized         → bool
  DataKey::Destroyed           → bool
  DataKey::BaseCurrencyPairs   → Vec<Symbol>
  DataKey::AdminUpdateTimestamp→ u64

Persistent Storage:
  DataKey::VerifiedPrice(asset)             → PriceData
  DataKey::CommunityPrice(asset)            → PriceData
  DataKey::VerifiedProviders                → Vec<Address>
  DataKey::ProviderStake(addr)              → i128
  DataKey::SlashToken                       → Address
  DataKey::InsuranceReserve                 → Address
  DataKey::FeeToken                         → Address
  DataKey::QueryFee                         → i128
  DataKey::FeeVault                         → i128
  DataKey::RewardBalance(addr)              → i128
  DataKey::TrackedAsset(asset)              → ()
  DataKey::PriceFloorEntry(asset)           → i128
  DataKey::ProviderLastSeenLedger(addr)     → u32
  DataKey::PriceSubscription                → Vec<Address>
  DataKey::CouncilAddress                   → Address
  DataKey::CircuitBreakerCoordinators       → Vec<Address>
  DataKey::AdminWeights                     → Map<Address, u32>
  DataKey::DelegateMap                      → Map<Address, Address>
  DataKey::HealthTotalAssets                → u32
  DataKey::HealthLastLedger                 → u32
  DataKey::ProposalCount                    → u64
  ProposedAction(id)                        → ProposedAction
  ActionVoters(id)                          → Vec<Address>

Temporary Storage:
  DataKey::PriceBufferByAsset(asset, seq)  → PriceBuffer
  DataKey::Twap(asset)                      → Vec<(u64, i128)>
  DataKey::RecentEvents                     → Vec<RecentEvent>
  DataKey::IsLocked                         → bool (reentrancy guard)
```

### 3.4 Satellite Contract State Layouts

**GasTank:**
- Instance: `Token` (Address), `Oracle` (Address)
- Persistent: `Balance(consumer)` → i128, `Allowance(consumer, relayer)` → i128, `RelayerFunders(relayer)` → Vec<Address>

**LiquidityLock:**
- Instance: `Admin` (Address), `Token` (Address), `Stream(recipient)` → StreamData

**RewardSplitter:**
- Instance: `Admin`, `Token`, `Recipients` (Vec<Recipient>), `TotalShares` (u32), `DefaultAdmin`, `DefaultToken`, `CooldownStage(n)`, `CooldownAction(id)`

**AnalyticsEngine:**
- Instance: `Alpha` (i128)
- Persistent: `EmaRecord(AssetId)` → EmaRecord

## 4. Authorization Vectors

### 4.1 Authorization Matrix

| Operation | Auth Requirement | Mechanism | Multi-Sig? |
|---|---|---|---|
| Initialize contract | Caller set as admin | `caller.require_auth()` | No |
| Stake / register | Node address auth | `node.require_auth()` | No |
| Unstake | Node address auth | `node.require_auth()` | No |
| Upgrade proposal | Admin auth + nonce | `admin.require_auth()`, `consume_nonce()` | No |
| Upgrade execution | Admin auth + nonce + timelock (48h) | `admin.require_auth()`, delay check | No |
| Set heartbeat interval | Admin auth | `admin.require_auth()` | No |
| Upsert node profile | Admin auth | `admin.require_auth()` | No |
| Set staking tier config | Admin auth + multi-sig | `admin.require_auth()`, `require_multisig()` (≥2 signers) | Yes |
| Set asset feed metrics | Admin auth + multi-sig | `admin.require_auth()`, `require_multisig()` | Yes |
| Ownership transfer | Phase 1: admin; Phase 2: nominee | `propose_ownership_transfer`, `claim_ownership` | No |
| Admin key change | Phase 1: admin; Phase 2A: cosigner OR Phase 2B: 24h timelock | `propose_admin_change`, `countersign_admin_change` / `execute_admin_change_by_timelock` | Conditional |
| Emergency revocation | Proposal: signer/admin; Vote: threshold = signers/2 + 1 | `propose_emergency_revocation`, `vote_emergency_revocation` | Yes |
| Emergency pause | Admin auth + nonce | `set_paused` with per-action nonce | No |
| Set price variance config | Admin auth | `admin.require_auth()` | No |
| Register signer | Admin auth | `admin.require_auth()` | No |
| Remove signer | Admin auth | `admin.require_auth()` | No |
| Update prices bundle | Node auth | `node.require_auth()` | No |
| Submit telemetry | Node auth (not revoked) | `node.require_auth()`, `assert_not_revoked()` | No |
| Report ingestion dropout | Admin auth | `admin.require_auth()` | No |
| Apply ingestion penalty | Admin auth | `admin.require_auth()` | No |
| Set recovery key | Admin auth | `admin.require_auth()` | No |
| Recover admin | Recovery key auth (180d inactivity) | `recovery_key.require_auth()`, inactivity check | No |
| Rollback upgrade | Admin auth + nonce + 72h window | `admin.require_auth()`, rollback window check | No |

### 4.2 Multi-Sig Architecture

The `require_multisig()` function enforces:
- Minimum **2 valid signers** (of any registered signer count)
- Signers are deduplicated
- Only registered signers (or the admin) count toward the threshold
- Only active (non-revoked) signers participate
- Iteration short-circuits once threshold is met

### 4.3 Revocation Chain

```
RevokedSignerKey::RevokedByAddress(target)
    │
    ├── Guards every sensitive function via assert_not_revoked()
    ├── Blocks staking, telemetry submission, profile updates
    ├── Removes from active signer set
    └── Cannot be reversed
```

### 4.4 Two-Phase Admin Change

```
Phase 1: Admin proposes
    └── AdminChangeProposal stored to instance
            │
    ┌───────┴────────┐
    ▼                ▼
Path A:          Path B:
Cosigner        24h Timelock
approves        elapses
    │                │
    └───────┬────────┘
            ▼
     Admin key updated
```

## 5. Threat Vector Mitigation Matrix

### 5.1 Threat Model

| Threat | Vector | Severity | Mitigation |
|---|---|---|---|
| **Compromised admin key** | Single-actor admin takeover | Critical | Two-phase admin change (24h timelock or cosigner), emergency revocation by multi-sig coordinator group, dead-man's-switch recovery (180d) |
| **Replay attack** | Re-broadcast captured signed transaction | High | Nonce consumption (`consume_nonce`), signature expiry timestamps, per-action nonce isolation for admin operations |
| **Flash loan manipulation** | Temporary capital injection to manipulate price feeds | High | Minimum reserve balance check (100k XLM), minimum 24h trading volume (10k XLM), multi-source median with quorum ≥3 validators |
| **Stale price ingestion** | Relayer submits outdated price data | Medium | `verify_payload_freshness` (max 60s age), heartbeat interval tracking, `is_data_fresh` query |
| **Compromised relayer node** | Malicious price submission | Medium | Bond capacity check (premium pools), tiered staking requirements, exponential slashing for repeated dropouts, deviation-based slashing tiers |
| **Upgrade abuse** | Malicious WASM deployment | High | 48h timelock on upgrade execution, post-upgrade health check (6 diagnostics), rollback within 72h window, pre-upgrade WASM hash preserved |
| **Admin key lockout** | Lost/compromised key with no recovery path | High | Dead-man's-switch recovery key (180d inactivity threshold), two-phase admin change with cosigner fallback |
| **Storage rent expiry** | Validator feed stake purged after inactivity | Medium | `update_feed_stake_activity` on profile updates, `check_and_prune_feed_stake` with 30d TTL threshold, automatic TTL extension on active entries |
| **Reentrancy** | Cross-contract call reentrancy | Medium | Reentrancy lock on `set_price` path in PriceOracle, reentrancy guard on swap operations |
| **Consensus manipulation** | Validator collusion or Sybil attack | Medium | Stake-weighted median, minimum quorum of 3 validators, 16-validator cap, per-asset epoch sequence isolation |
| **Governance paralysis** | Malicious proposal flood | Low | Single active proposal limit, temporary storage auto-purge via TTL, explicit purge function |
| **Emergency revocation stall** | Target proposes own revocation to block | Medium | Proposer cannot be target (`proposer == target` guard), target cannot vote on own revocation |
| **Nonce reuse across actions** | Cross-action replay | Medium | Per-action isolated nonce counters (`AdminNonceKey::Action(caller, action)`) |
| **Parameter injection** | Unauthorized config mutation | Medium | Admin-only state gating with `require_auth()`, compile-time admin storage isolation via `AdminStorageKey` enum |
| **Oracle price manipulation** | Circuit-breaker bypass | Medium | 1h safety-check bypass grace period, requiries admin auth, automatic expiry, separate disable fn |
| **Budget exhaustion** | Out-of-gas during median calc | Medium | `MAX_MEDIAN_ENTRIES = 11` cap on buffer, weight-based truncation, `MAX_BUNDLE_ASSETS = 20` on price bundles |

### 5.2 Error Event Flow

```
Unauthorized action
    │
    ├── Admin check fails        → ContractError::NotAdmin (panic, full revert)
    ├── Multi-sig threshold      → ContractError::ThresholdNotReached
    ├── Revoked address          → ContractError::RevokedAddress
    ├── Invalid nonce            → ContractError::InvalidNonce
    ├── Expired signature        → ContractError::SignatureExpired
    └── Uninitialized contract   → ContractError::NotInitialized
```

### 5.3 Security Boundaries

```
External (User / Relayer)
    │
    │  node.require_auth()
    ▼
┌─────────────────────────────────────┐
│         Auth Gate                   │
│  - require_auth()                   │
│  - assert_not_revoked()             │
│  - consume_nonce() / per-action     │
└──────────┬──────────────────────────┘
           │
           ▼
┌─────────────────────────────────────┐
│      Validation Pipeline            │
│  - verify_payload_freshness()       │
│  - validate_reserve_balance()       │
│  - validate_trading_volume()        │
│  - check_bond_capacity()            │
│  - check_liquidity_depth()          │
└──────────┬──────────────────────────┘
           │
           ▼
┌─────────────────────────────────────┐
│      State Mutation                 │
│  - Instance / Persistent / Temp     │
│  - Event emission                   │
│  - TTL management                   │
└─────────────────────────────────────┘
```

## 6. Upgrade & Migration Architecture

### 6.1 Timelocked Upgrade Flow

```
1. propose_upgrade()  ──► StagedUpgrade stored (48h countdown starts)
2. ... 48h window ...
3. execute_upgrade()  ──► 6 diagnostic health checks run post-deployment
                       │
                       ├── Diagnostic 1: admin preserved
                       ├── Diagnostic 2: DATA_KEY accessible
                       ├── Diagnostic 3: treasury address intact
                       ├── Diagnostic 4: TOTAL_STAKED_KEY readable
                       ├── Diagnostic 5: signers map accessible
                       └── Diagnostic 6: heartbeat interval accessible
                       │
                       └── On failure: ContractError::UpgradeHealthCheckFailed
```

### 6.2 Rollback Window

```
Upgrade executed
    │
    ▼
┌─ 72-hour rollback window ──────────────────┐
│  rollback_upgrade() → previous WASM hash   │
└─────────────────────────────────────────────┘
    │
    ▼
Window expires → rollback permanently disabled
```

### 6.3 Schema Migration

```
ensure_schema_version()
    │
    ├── Version ≥ 2 → Ok (no migration needed)
    ├── Version 1   → migrate_from_version(env, 1)
    └── Version 0   → migrate_from_version(env, 0)

    Migration steps:
    ├── Node profiles: instance Map → per-entry Persistent keys
    ├── Signers: instance Map → per-entry Instance keys
    ├── Stake registry: instance Map → per-entry Instance keys
    ├── Total staked: renamed key
    └── Heartbeats: instance Map → per-entry Temporary keys
```

## 7. Gas Optimization Patterns

| Pattern | Location | Mechanism |
|---|---|---|
| **Tuple-keyed storage** | `storage.rs` | Enum-based composite keys avoid map deserialization overhead |
| **Interior fee scaling** | `fees.rs` | 10^14 interior scale preserves precision during division before normalizing to 10^7 |
| **Bundle pre-indexing** | `validation.rs` | `build_bundle_index()` precomputes symbol pointers in O(n) before linear validation pass |
| **Price buffer compaction** | PriceOracle | Linear compaction folds identical prices into `(price, count)` buckets before median sort |
| **Vector compacting** | `consensus.rs` | `compact_duplicate_price_rows()` merges equal values before weighted-sum computation |
| **Stack allocation** | `consensus.rs` | `MAX_VALIDATORS = 16` stack-allocated array for validator binary search |
| **Per-asset isolation** | `consensus.rs` | `ConsensusStorageKey::ConsensusSeq(asset)` replaces monolithic Map |
| **Weight-based truncation** | PriceOracle | When buffer > MAX_MEDIAN_ENTRIES (11), keep highest-weight providers |

## 8. Event Model

### 8.1 Core Router Events

| Event | Data | Trigger |
|---|---|---|
| `telem_ok` | `(node, pool, payload_timestamp)` | Successful telemetry submission |
| `RECOVER_CFG` | `(caller, recovery_key)` | Recovery key configured |
| `RECOVER_DONE` | `(recovery_key,)` | Admin recovery completed |

### 8.2 PriceOracle Events

| Event | Data | Trigger |
|---|---|---|
| `ContractInitialized` | `(admin, version)` | Contract deployment |
| `asset_added_event` | `(asset,)` | Asset registered |
| `PriceVarianceEvent` | `(asset, old, new, variance_bps)` | Price update with variance |
| `SlashExecutedEvent` | `(bad_relayer, amount, reserve, executor)` | Governance slash |
| `OwnershipRenouncedEvent` | `(previous_admin,)` | Admin renouncement |
| `DelegateAssignedEvent` | `(admin, delegate)` | Delegate set |
| `DelegateRevokedEvent` | `(admin, delegate)` | Delegate removed |
| `RescueTokensEvent` | `(token, recipient, amount)` | Token rescue |

### 8.3 GasTank Events

| Event | Data | Trigger |
|---|---|---|
| `deposit` | `(consumer, amount)` | Token deposit |
| `withdraw` | `(consumer, amount)` | Token withdrawal |
| `allowance` | `(consumer, relayer, amount)` | Allowance set |
| `reimburse` | `(relayer, consumer, charge)` | Gas reimbursement |
