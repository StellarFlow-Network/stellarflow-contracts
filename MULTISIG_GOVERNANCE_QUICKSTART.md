# Issue #595: Multi-Sig Governance - Quick Start Guide

## What Was Implemented

N-of-M weighted multi-signature governance for WASM upgrades. Prevents single-key compromise by requiring multiple signers' combined weight to reach a threshold.

## Key Features

✅ **Weight-Based Quorum** - Signers have individual weights that must sum to meet threshold  
✅ **Upgrade Abort Protection** - `execute_upgrade()` fails if collected weight < required weight  
✅ **Event Transparency** - `GovernanceUpgradeProposed` event includes weight details  
✅ **Admin Default Weight** - Admin gets weight 1 if not explicitly set  
✅ **Duplicate Prevention** - Same signer counted only once per proposal  
✅ **Backward Compatible** - Legacy count-based quorum still enforced alongside weights  
✅ **Overflow Safe** - Uses `checked_add()` for all weight calculations  

## Usage Examples

### Initialize Multi-Sig Governance

```rust
// Set up the configuration (admin only)
let config = MultiSigConfig {
    required_weight: 3,      // Need 3 total weight
    max_signer_weight: 1,    // Max 1 weight per signer
};
contract.set_multisig_config(env, admin, config)?;

// Register signers with weights
contract.set_signer_weight(env, admin, signer1, 1)?;
contract.set_signer_weight(env, admin, signer2, 1)?;
contract.set_signer_weight(env, admin, signer3, 1)?;
```

### Propose Upgrade with Multi-Sig Validation

```rust
let signers = vec![signer1, signer2, signer3];

// Proposes upgrade
// ✅ Validates all 3 signers together provide weight 3 (meets threshold)
// ✅ Emits GovernanceUpgradeProposed with weight info
// ✅ Stores proposal for 48-hour timelock
contract.propose_upgrade(
    env,
    new_wasm_hash,
    admin,        // proposer must be admin
    signers,      // will be weight-checked
    nonce,
    salt,
    salt_signature,
    sig_expires_at,
)?;
```

### Query Weight Status

```rust
// Check multi-sig config
let config = contract.get_multisig_config(env);
println!("Required weight: {}", config.required_weight);      // 3
println!("Max signer weight: {}", config.max_signer_weight);  // 1

// Check individual signer weight
let weight = contract.get_signer_weight(env, signer1);
println!("Signer1 weight: {}", weight);  // 1

// Get active proposal with weight details
if let Some(proposal) = contract.get_governance_upgrade_proposal(env) {
    println!("Signers: {:?}", proposal.signers);
    // Can look at historical events for collected weight
}
```

### Execute Upgrade (Re-validates Weight)

```rust
// Execute upgrade - re-validates weight before proceeding
// ❌ Fails if weight threshold no longer met (e.g., signer revoked)
// ✅ Succeeds if weight still valid and timelock passed
contract.execute_upgrade(
    env,
    executor,     // must be admin
    nonce,
    salt,
    signature,
    sig_expires_at,
)?;
```

## Error Cases

| Error | Cause | Solution |
|-------|-------|----------|
| `ThresholdNotReached` | Collected weight < required_weight | Add more signers or increase individual weights |
| `InvalidStakeAmount` | Set weight > max_signer_weight | Reduce weight or increase max_signer_weight in config |
| `NotAdmin` | Only admin can set config/weights | Use admin address |
| `Unauthorized` | Signer not in SIGNERS_KEY | Register signer first or use authorized signers |

## Storage Keys

| Key | Name | Contents | Type |
|-----|------|----------|------|
| `SIGWT` | SIGNER_WEIGHTS_KEY | Map<Address, u32> | Instance |
| `QWTH` | QUORUM_WEIGHT_THRESHOLD_KEY | MultiSigConfig | Instance |
| `GVNCFG` | GOVERNANCE_CONFIG_KEY | GovernanceConfig (legacy) | Instance |
| `GOVUPG` | GOVERNANCE_UPGRADE_KEY | GovernanceUpgradeProposal | Instance |

## Events Emitted

| Symbol | Event Type | Data | When |
|--------|-----------|------|------|
| `MULTISIG_CFG` | Config Updated | (admin, required_weight, max_signer_weight) | After set_multisig_config |
| `SIGNER_WT` | Weight Updated | (admin, signer, weight) | After set_signer_weight |
| `GV_UPG_PRO` | Upgrade Proposed | GovernanceUpgradeProposedEvent | After propose_upgrade succeeds |

## Data Structures

### MultiSigConfig
```rust
pub struct MultiSigConfig {
    pub required_weight: u32,      // N in N-of-M: minimum weight needed
    pub max_signer_weight: u32,    // M in N-of-M: max weight per signer
}
```

### GovernanceUpgradeProposedEvent
```rust
pub struct GovernanceUpgradeProposedEvent {
    pub new_wasm_hash: BytesN<32>,
    pub proposer: Address,
    pub signers: Vec<Address>,
    pub staged_at: u64,
    pub required_weight: u32,      // Threshold at proposal time
    pub collected_weight: u32,     // Weight actually collected
}
```

## Deployment Checklist

- [ ] Deploy contract with updated governance.rs and lib.rs
- [ ] Call `set_multisig_config()` with desired required_weight and max_signer_weight
- [ ] Register all signers with `set_signer_weight()` for each authorized signer
- [ ] Verify signer weights with `get_signer_weight()` queries
- [ ] Propose test upgrade with all signers to verify weight calculation
- [ ] Check emitted event includes correct required_weight and collected_weight
- [ ] Execute test upgrade after timelock to verify re-validation works
- [ ] Monitor for weight threshold breach errors and adjust config as needed

## Weight Calculation Logic

```
For each signer in proposal:
  1. Skip if already counted (deduplication)
  2. Check if authorized (admin or in SIGNERS_KEY)
  3. If admin: use explicitly set weight or default 1
  4. If signer: use explicitly set weight or 0
  5. Add weight to total
  
If total_weight >= required_weight: ✅ PASS
Else: ❌ FAIL with ThresholdNotReached
```

## Examples: Real Scenarios

### Scenario A: 3-of-5 Multi-Sig
- 5 signers registered with weight 1 each
- required_weight = 3
- Proposal with any 3+ signers → ✅ Pass
- Proposal with 2 signers → ❌ Fail

### Scenario B: 2-of-3 with Weighted Roles
- Admin (weight 2), Signer1 (weight 1), Signer2 (weight 1)
- required_weight = 2
- Proposal with [Admin] → ✅ Pass (weight 2)
- Proposal with [Signer1, Signer2] → ✅ Pass (weight 2)
- Proposal with [Signer1] → ❌ Fail (weight 1)

### Scenario C: Upgrade Blocked at Execution
- Proposal created with 3 signers (weight 3)
- Before execution, one signer is revoked (weight removed)
- Execute called → ❌ Fail: Revalidation finds weight 2, below threshold 3
- Security: Prevents outdated proposals from executing

## Integration Tips

1. **Store event data**: Listen for `GV_UPG_PRO` events and store collected_weight for auditing
2. **Monitor weight changes**: Track `SIGNER_WT` events to know when signers are added/removed
3. **Dual validation**: Both count-based AND weight-based checks must pass
4. **Testing**: Always test with insufficient weight first to verify rejection works
5. **Config planning**: Choose required_weight based on security vs. operability needs

## Backward Compatibility

✅ Existing contracts work without changes  
✅ Legacy `quorum_threshold` still enforced (count-based)  
✅ New weight system is additive (both checks required)  
✅ No breaking changes to existing APIs  
✅ Admin address still required for upgrades  

---

**For full technical documentation, see:** `MULTISIG_GOVERNANCE_IMPLEMENTATION.md`
