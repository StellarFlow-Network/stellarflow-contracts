# Issue #595: Multi-Sig Governance - Implementation Summary

## Overview

Implemented weighted N-of-M multi-signature governance for WASM code upgrades, preventing single-key compromise exploits. The system tracks threshold weight and registered administrative signers in persistent storage, aborts upgrades if collected signature weight is below threshold quorum, and emits detailed governance events upon proposal registration.

## Implementation Details

### 1. Storage Layer - New Keys in `governance.rs`

#### Storage Keys Added:
```rust
pub(crate) const SIGNER_WEIGHTS_KEY: Symbol = symbol_short!("SIGWT");
pub(crate) const QUORUM_WEIGHT_THRESHOLD_KEY: Symbol = symbol_short!("QWTH");
pub(crate) const PROPOSAL_WEIGHT_KEY: Symbol = symbol_short!("PROPWT");
```

#### New Data Structures:

**MultiSigConfig**
- `required_weight: u32` - Total weight required for quorum (N in N-of-M)
- `max_signer_weight: u32` - Maximum weight any single signer can hold
- Default: `required_weight: 3`, `max_signer_weight: 1`

```rust
#[contracttype]
#[derive(Clone)]
pub struct MultiSigConfig {
    pub required_weight: u32,
    pub max_signer_weight: u32,
}
```

**GovernanceUpgradeProposedEvent**
- Emitted when a governance upgrade proposal is registered
- Includes collected weight and required weight for transparency
- Provides full audit trail for multi-sig operations

```rust
#[contracttype]
#[derive(Clone)]
pub struct GovernanceUpgradeProposedEvent {
    pub new_wasm_hash: BytesN<32>,
    pub proposer: Address,
    pub signers: Vec<Address>,
    pub staged_at: u64,
    pub required_weight: u32,
    pub collected_weight: u32,
}
```

### 2. Governance Logic - Enhanced Verification

#### Weight Management Functions in `governance.rs`:

**get_multisig_config(env: &Env) -> MultiSigConfig**
- Retrieves current multi-sig weight configuration
- Returns default if not set

**set_multisig_config(env: &Env, config: &MultiSigConfig)**
- Updates multi-sig weight configuration
- Persists to instance storage

**get_signer_weight(env: &Env, signer: &Address) -> u32**
- Returns weight for a specific signer (0 if not registered)
- Allows querying individual signer weights

**set_signer_weight(env: &Env, signer: &Address, weight: u32)**
- Registers or updates a signer's weight
- Setting weight to 0 removes the signer

#### Enhanced Quorum Verification in `verify_upgrade_quorum()`

The function now performs dual validation:

1. **Legacy Count-Based Check** - Maintains backward compatibility
   - Validates that minimum number of authorized signers is met
   - Uses existing `quorum_threshold` from `GovernanceConfig`

2. **Weight-Based Check** - New N-of-M implementation
   - Calculates total collected weight from all signers in proposal
   - Admin automatically receives weight 1 if not explicitly set
   - Deduplicates signers (same signer cannot vote twice)
   - Validates that collected weight meets `required_weight` threshold
   - Returns `ThresholdNotReached` if either check fails

```rust
pub fn verify_upgrade_quorum(env: &Env, signers: &Vec<Address>) -> Result<(), ContractError> {
    // Dual validation ensures backward compatibility + new N-of-M support
    // Both checks must pass
}
```

#### Weight Calculation Helper in `calculate_collected_weight()`

```rust
pub fn calculate_collected_weight(
    env: &Env, 
    signers: &Vec<Address>, 
    data: &ContractData
) -> Result<u32, ContractError>
```

- Sums weights of all valid signers in proposal
- Admin gets default weight 1 if not explicitly registered
- Non-admin signers get registered weight (0 if not registered)
- Only counts authorized signers (admin or in SIGNERS_KEY)
- Prevents weight overflow with `checked_add`

### 3. Contract Integration - New Public Methods in `lib.rs`

#### Configuration Management:

**get_multisig_config(env: Env) -> MultiSigConfig**
- Query current multi-sig weight configuration
- Public read access, no authorization required

**set_multisig_config(env: Env, admin: Address, config: MultiSigConfig) -> Result<(), ContractError>**
- Update multi-sig weight configuration
- Requires admin authorization
- Emits `MULTISIG_CFG` event with (admin, required_weight, max_signer_weight)
- Extends TTL after update

#### Signer Weight Management:

**get_signer_weight(env: Env, signer: Address) -> u32**
- Query weight for a specific signer
- Public read access
- Returns 0 if signer not registered

**set_signer_weight(env: Env, admin: Address, signer: Address, weight: u32) -> Result<(), ContractError>**
- Register or update a signer's weight
- Requires admin authorization
- Validates weight doesn't exceed `max_signer_weight`
- Emits `SIGNER_WT` event with (admin, signer, weight)
- Extends TTL after update
- Setting weight to 0 removes the signer

### 4. Upgrade Proposal Enhancement

#### Updated `propose_upgrade()` Flow:

1. Validates signature expiration
2. Checks proposer is admin
3. Consumes nonce for replay protection
4. **Calls `verify_upgrade_quorum()` - now checks both count and weight**
5. Calculates collected weight using `calculate_collected_weight()`
6. Retrieves multi-sig config for event emission
7. Stores proposal in instance storage
8. **Emits enhanced `GovernanceUpgradeProposedEvent` with weight details**
9. Stages upgrade in pending queue with timelock

#### Event Emission:

```rust
// OLD: Single event with basic info
env.events().publish(
    (symbol_short!("GV_UPG_PROPOSED"),),
    (new_wasm_hash, proposer, signers, timestamp),
);

// NEW: Detailed event with weight information
env.events().publish(
    (symbol_short!("GV_UPG_PRO"),),
    GovernanceUpgradeProposedEvent {
        new_wasm_hash,
        proposer,
        signers,
        staged_at,
        required_weight,      // NEW: Threshold for transparency
        collected_weight,     // NEW: Actual weight collected
    },
);
```

#### Execute Upgrade Verification:

The `execute_upgrade()` function calls `verify_upgrade_quorum()` which now:
- Revalidates weight-based quorum at execution time
- Ensures weight threshold still met when upgrade executes
- Aborts if weight is insufficient, preventing unauthorized upgrades

### 5. Safety Features

#### Duplicate Signer Prevention
- Each signer counted only once per proposal
- Uses temporary `seen_signers` Map to track processed addresses
- Prevents weight multiplication from duplicate signatures

#### Overflow Protection
- All weight additions use `checked_add()` 
- Returns `Overflow` error if weight sum exceeds u32::MAX
- Prevents arithmetic overflow attacks

#### Authorization Checks
- Only admin can set multi-sig config
- Only admin can register/update signer weights
- All configuration changes require `require_auth()`
- Admin authorization is mandatory for state changes

#### Backward Compatibility
- Legacy count-based quorum still enforced
- Both count and weight checks must pass
- Existing contracts continue to work without modification
- New weight system is additive, not replacive

### 6. Events Emitted

| Event | Symbol | Data | Purpose |
|-------|--------|------|---------|
| Multi-Sig Config Updated | `MULTISIG_CFG` | (admin, required_weight, max_signer_weight) | Track config changes |
| Signer Weight Updated | `SIGNER_WT` | (admin, signer, weight) | Audit signer registration |
| Governance Upgrade Proposed | `GV_UPG_PRO` | GovernanceUpgradeProposedEvent | Notify of new proposal with weight details |

## Testing Scenarios

### Scenario 1: Basic Weight Validation
- Register 3 signers with weights [1, 1, 1]
- Set required_weight to 3
- Propose upgrade with all 3 signers → ✅ Success
- Propose upgrade with 2 signers → ❌ ThresholdNotReached

### Scenario 2: Admin Weight Handling
- Admin has no explicit weight (defaults to 1)
- Register 2 other signers with weights [1, 1]
- Set required_weight to 2
- Propose upgrade with admin + 1 other → ✅ Success (1+1=2)
- Propose upgrade with admin only → ❌ ThresholdNotReached (1<2)

### Scenario 3: Duplicate Signer Prevention
- Register 2 signers with weights [2, 1]
- Set required_weight to 3
- Propose upgrade with [signer1, signer1] → ❌ ThresholdNotReached (only 2, not 4)
- Confirms deduplication working

### Scenario 4: Max Weight Enforcement
- Set max_signer_weight to 1
- Attempt to set signer weight to 2 → ❌ InvalidStakeAmount
- Set signer weight to 1 → ✅ Success

### Scenario 5: Backward Compatibility
- Keep legacy quorum_threshold of 2
- Register 2 signers with weights [1, 1]
- Set required_weight to 3
- Propose upgrade with 2 signers → ❌ Fails (weight check fails: 2 < 3)
- Both checks properly enforced

### Scenario 6: Execute with Weight Verification
- Propose upgrade successfully with sufficient weight
- Time passes, reaches timelock threshold
- Execute upgrade → ✅ Weight revalidated, upgrade proceeds
- Confirm both proposal and execution check weight

## Files Modified

### `/workspaces/stellarflow-contracts/src/governance.rs`
- Added 3 new storage keys for weight management
- Added `MultiSigConfig` struct with defaults
- Added weight getter/setter functions
- Enhanced `verify_upgrade_quorum()` with dual validation
- Added `calculate_collected_weight()` helper
- Added `GovernanceUpgradeProposedEvent` struct

### `/workspaces/stellarflow-contracts/src/lib.rs`
- Added `get_multisig_config()` public method
- Added `set_multisig_config()` public method with admin check
- Added `get_signer_weight()` public method
- Added `set_signer_weight()` public method with admin check and validation
- Updated `propose_upgrade()` to emit enhanced event with weight information
- `execute_upgrade()` now uses updated `verify_upgrade_quorum()` with weight checks

## API Reference

### Query Methods (Read-Only)
```rust
// Get multi-sig configuration
pub fn get_multisig_config(env: Env) -> MultiSigConfig

// Get specific signer weight
pub fn get_signer_weight(env: Env, signer: Address) -> u32

// Get active governance proposal
pub fn get_governance_upgrade_proposal(env: Env) -> Option<GovernanceUpgradeProposal>
```

### Admin Methods (Require Authorization)
```rust
// Update multi-sig weight configuration
pub fn set_multisig_config(
    env: Env,
    admin: Address,
    config: MultiSigConfig
) -> Result<(), ContractError>

// Register or update signer weight
pub fn set_signer_weight(
    env: Env,
    admin: Address,
    signer: Address,
    weight: u32
) -> Result<(), ContractError>

// Propose governance upgrade with weight validation
pub fn propose_upgrade(
    env: Env,
    new_wasm_hash: BytesN<32>,
    proposer: Address,
    signers: Vec<Address>,
    nonce: u64,
    salt: Bytes,
    salt_signature: BytesN<32>,
    sig_expires_at: u64
) -> Result<(), ContractError>

// Execute staged upgrade (re-validates weight)
pub fn execute_upgrade(
    env: Env,
    executor: Address,
    nonce: u64,
    salt: Bytes,
    signature: BytesN<32>,
    sig_expires_at: u64
) -> Result<(), ContractError>
```

## Security Considerations

1. **Weight Overflow Prevention**: All weight arithmetic uses `checked_add()` to prevent overflow attacks
2. **Duplicate Signer Deduplication**: Same signer cannot accumulate weight multiple times in one proposal
3. **Admin Default Weight**: Admin always has minimum weight 1 even if not explicitly registered
4. **Replay Protection**: Existing nonce mechanism prevents replay attacks on proposals
5. **Dual Validation**: Both legacy count-based and new weight-based checks must pass
6. **TTL Management**: All configuration changes extend TTL to prevent eviction
7. **Authorization Requirement**: All state-changing operations require `require_auth()`

## Migration Path

For existing deployments:

1. **Initialize weights (optional)**:
   ```rust
   set_signer_weight(env, admin, admin, 1)
   set_signer_weight(env, admin, signer2, 1)
   set_signer_weight(env, admin, signer3, 1)
   ```

2. **Set multi-sig config**:
   ```rust
   set_multisig_config(env, admin, MultiSigConfig {
       required_weight: 3,
       max_signer_weight: 1,
   })
   ```

3. **Existing proposals still work** with legacy quorum_threshold

4. **New proposals use weight-based quorum** if configured

## Compliance with Requirements

✅ **Track threshold weight and registered administrative signers in storage**
- MultiSigConfig stored at QUORUM_WEIGHT_THRESHOLD_KEY
- Signer weights stored at SIGNER_WEIGHTS_KEY

✅ **Abort upgrade() invocation if collected signature weight is below threshold quorum**
- verify_upgrade_quorum() validates collected weight ≥ required_weight
- execute_upgrade() re-validates weight before proceeding
- Returns ThresholdNotReached error if insufficient

✅ **Emit GovernanceUpgradeProposed event upon proposal registration**
- GovernanceUpgradeProposedEvent struct includes weight information
- Emitted with symbol "GV_UPG_PRO" upon successful proposal
- Event includes: hash, proposer, signers, timestamp, required_weight, collected_weight

## Future Enhancements

1. **Weight Tiers**: Support different weight tiers for different signer roles
2. **Time-Locked Weight Changes**: Prevent weight manipulation during active proposals
3. **Weighted Voting**: Extend weight system to governance voting ballots
4. **Emergency Weight Recovery**: Faster weight adjustment for compromised keys
5. **Weight Expiry**: Optional weight expiration and re-registration requirements
