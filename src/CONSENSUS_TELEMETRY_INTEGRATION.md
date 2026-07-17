# Consensus Telemetry Integration Guide

## Overview

This document describes how the consensus module (`src/consensus.rs`) integrates with the temporary storage-based telemetry tracking system.

## Architecture

```
┌─────────────────────────────────────────────────────┐
│          TimeLockedUpgradeContract                  │
│                  (src/lib.rs)                       │
├─────────────────────────────────────────────────────┤
│                                                     │
│  ┌──────────────────────────────────────────────┐  │
│  │     Consensus Module (src/consensus.rs)      │  │
│  │  - Weighted averaging                        │  │
│  │  - Quorum calculations                       │  │
│  │  - Stake-based weighting                     │  │
│  └──────────────────────────────────────────────┘  │
│                      ↓                              │
│  ┌──────────────────────────────────────────────┐  │
│  │   Telemetry Tracking (_record_heartbeat)    │  │
│  │  - Temporary storage with TTL                │  │
│  │  - Automatic expiration                      │  │
│  │  - Asset-specific timestamps                 │  │
│  └──────────────────────────────────────────────┘  │
│                      ↓                              │
│  ┌──────────────────────────────────────────────┐  │
│  │       Soroban Temporary Storage              │  │
│  │  Key: HEARTBEAT_KEY ("HBEAT")                │  │
│  │  Value: Map<Symbol, u64>                     │  │
│  │  TTL: 17,280 ledgers (~24 hours)             │  │
│  └──────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────┘
```

## Consensus Operations That Record Telemetry

### 1. Stake Registration

When nodes stake and register, a heartbeat is recorded:

```rust
pub fn stake_and_register(env: Env, node: Address, amount: u64) -> Result<StakeRecord, ContractError> {
    // ... stake logic ...
    
    // Record telemetry in temporary storage
    Self::_record_heartbeat(&env, symbol_short!("STAKE"));
    
    Ok(StakeRecord { node, amount, registered_at: env.ledger().timestamp() })
}
```

**Storage Impact:**
- ✅ Temporary storage (low rent)
- ✅ Auto-expires after 24 hours
- ✅ No long-term accumulation

### 2. Value Updates

When consensus values are set, telemetry is tracked:

```rust
pub fn set_value(env: Env, new_value: u64, caller: Address, ...) -> Result<(), ContractError> {
    // ... validation and authorization ...
    
    data.value = new_value;
    env.storage().instance().set(&DATA_KEY, &data);
    
    // High-frequency tracking in temporary storage
    Self::_record_heartbeat(&env, symbol_short!("VALUE"));
    
    Ok(())
}
```

**Storage Impact:**
- ✅ Contract data in instance storage (configuration)
- ✅ Telemetry in temporary storage (short-lived)
- ✅ Optimal cost structure

### 3. Manual Heartbeat Updates

Administrators can manually update telemetry:

```rust
pub fn update_heartbeat(env: Env, asset: Symbol, updater: Address) -> Result<(), ContractError> {
    let data = Self::get_data(env.clone())?;
    if data.admin != updater { return Err(ContractError::NotAdmin); }
    updater.require_auth();
    
    // Record timestamp in temporary storage
    Self::_record_heartbeat(&env, asset);
    
    Ok(())
}
```

## Consensus Module Functions

The `consensus.rs` module provides pure computational functions that **do not directly access storage**:

### Weighted Consensus Functions

```rust
// Calculate stake-weighted average
pub fn compute_weighted_average(entries: &Vec<WeightedEntry>) -> Result<u64, ContractError>

// Calculate weighted sum and total weight
pub fn compute_weighted_sum(entries: &Vec<WeightedEntry>) -> Result<(u64, u64), ContractError>

// Calculate minimum weight for quorum
pub fn compute_quorum_threshold(total_weight: u64, quorum_bps: u64) -> Result<u64, ContractError>

// Calculate weight share in basis points
pub fn entry_weight_share_bps(entry_weight: u64, total_weight: u64) -> Result<u64, ContractError>
```

### Integration Pattern

```rust
use crate::consensus::{WeightedEntry, compute_weighted_average};

// 1. Collect provider submissions with stakes
let mut entries = Vec::new(&env);
entries.push_back(WeightedEntry { value: 1000, weight: stake1 });
entries.push_back(WeightedEntry { value: 1020, weight: stake2 });

// 2. Compute consensus value
let consensus_value = compute_weighted_average(&entries)?;

// 3. Update contract state
data.value = consensus_value;
env.storage().instance().set(&DATA_KEY, &data);

// 4. Record telemetry in temporary storage
Self::_record_heartbeat(&env, symbol_short!("VALUE"));
```

## Telemetry Query Functions

### Check Data Freshness

```rust
pub fn is_data_fresh(env: Env, asset: Symbol) -> bool {
    let timestamps: Map<Symbol, u64> = env.storage()
        .temporary()
        .get(&HEARTBEAT_KEY)
        .unwrap_or_else(|| Map::new(&env));
    
    if let Some(last_update) = timestamps.get(asset) {
        env.ledger().timestamp().saturating_sub(last_update) <= Self::_get_interval(&env)
    } else { 
        false 
    }
}
```

**Usage:**
```rust
// Check if NGN price feed is fresh
if TimeLockedUpgradeContract::is_data_fresh(env.clone(), symbol_short!("NGN")) {
    // Safe to use current price
    let price = get_price(env, symbol_short!("NGN"));
} else {
    // Price is stale - handle appropriately
    return Err(ContractError::StaleData);
}
```

### Get Last Update Timestamp

```rust
pub fn get_last_update_timestamp(env: Env, asset: Symbol) -> Option<u64> {
    let timestamps: Map<Symbol, u64> = env.storage()
        .temporary()
        .get(&HEARTBEAT_KEY)
        .unwrap_or_else(|| Map::new(&env));
    
    timestamps.get(asset)
}
```

**Usage:**
```rust
match TimeLockedUpgradeContract::get_last_update_timestamp(env.clone(), symbol_short!("KES")) {
    Some(timestamp) => {
        let age = env.ledger().timestamp() - timestamp;
        // Use timestamp for staleness calculations
    },
    None => {
        // Never updated - handle missing data
    }
}
```

## Temporary Storage Lifecycle

### Write Path
```
1. Operation occurs (stake, set_value, etc.)
   ↓
2. _record_heartbeat() called
   ↓
3. Load existing Map<Symbol, u64> from temporary storage (or create new)
   ↓
4. Update timestamp for asset symbol
   ↓
5. Write back to temporary storage
   ↓
6. Extend TTL (17,280 ledgers, threshold 5,000)
```

### Read Path
```
1. Query function called (is_data_fresh, get_last_update_timestamp)
   ↓
2. Load Map<Symbol, u64> from temporary storage
   ↓
3. Return timestamp for requested asset (or None)
   ↓
4. No TTL extension on reads (only on writes)
```

### Expiration Path
```
1. Entry reaches TTL expiration (17,280 ledgers pass)
   ↓
2. Soroban automatically purges from ledger state
   ↓
3. Next read returns None (no entry found)
   ↓
4. is_data_fresh() returns false
```

## Best Practices for Consensus Operations

### 1. Always Record Telemetry After State Changes

```rust
// ✅ GOOD: Record telemetry after updating state
data.value = new_value;
env.storage().instance().set(&DATA_KEY, &data);
Self::_record_heartbeat(&env, symbol_short!("VALUE"));

// ❌ BAD: Forget to record telemetry
data.value = new_value;
env.storage().instance().set(&DATA_KEY, &data);
// Missing heartbeat!
```

### 2. Use Appropriate Asset Symbols

```rust
// ✅ GOOD: Use specific, meaningful symbols
Self::_record_heartbeat(&env, symbol_short!("NGN"));
Self::_record_heartbeat(&env, symbol_short!("KES"));
Self::_record_heartbeat(&env, symbol_short!("STAKE"));

// ❌ BAD: Use generic or ambiguous symbols
Self::_record_heartbeat(&env, symbol_short!("DATA"));
Self::_record_heartbeat(&env, symbol_short!("UPDATE"));
```

### 3. Check Freshness Before Using Data

```rust
// ✅ GOOD: Validate freshness before use
if Self::is_data_fresh(env.clone(), asset.clone()) {
    let value = Self::get_value(env.clone())?;
    // Use value safely
} else {
    return Err(ContractError::StaleData);
}

// ❌ BAD: Use data without freshness check
let value = Self::get_value(env.clone())?;
// Might be stale!
```

### 4. Configure Appropriate Intervals

```rust
// ✅ GOOD: Match interval to update frequency
// For 5-minute updates, use 5-minute interval
Self::set_heartbeat_interval(env.clone(), 5 * 60, admin);

// ❌ BAD: Interval much longer than updates
// Updates every 5 minutes but interval is 1 hour
Self::set_heartbeat_interval(env.clone(), 60 * 60, admin);
```

## Error Handling

```rust
// Check if interval is valid
pub fn set_heartbeat_interval(env: Env, interval: u64, admin: Address) -> Result<(), ContractError> {
    if interval == 0 { 
        return Err(ContractError::InvalidHeartbeatInterval); 
    }
    // ... rest of logic
}

// Handle missing data gracefully
pub fn get_last_update_timestamp(env: Env, asset: Symbol) -> Option<u64> {
    let timestamps = env.storage()
        .temporary()
        .get(&HEARTBEAT_KEY)
        .unwrap_or_else(|| Map::new(&env));
    
    timestamps.get(asset)  // Returns Option<u64>
}
```

## Performance Considerations

### Storage Costs

| Operation | Storage Type | Relative Cost |
|-----------|--------------|---------------|
| Write heartbeat | Temporary | 1x (baseline) |
| Write to instance | Instance | 3-5x |
| Write to persistent | Persistent | 10-20x |

### Optimization Tips

1. **Batch operations** when possible to minimize storage writes
2. **Use asset-specific symbols** to track granular telemetry
3. **Configure TTL** based on actual data lifetime needs
4. **Clean up** during finalization with `finalize_consensus()`

## Testing Checklist

- [ ] Verify heartbeats are recorded for all state-changing operations
- [ ] Confirm freshness checks work correctly
- [ ] Test staleness detection after interval expiration
- [ ] Validate custom interval configuration
- [ ] Ensure proper behavior with missing data (never updated)
- [ ] Check TTL extension on write operations
- [ ] Verify automatic expiration after TTL window

## Conclusion

The integration between consensus operations and temporary storage-based telemetry provides:

1. **Low-cost tracking** of high-frequency price feeds
2. **Automatic cleanup** via TTL-based expiration
3. **Flexible freshness checks** for data validation
4. **No long-term rent burden** on persistent storage

All consensus-related state changes should record telemetry using `_record_heartbeat()` to maintain accurate freshness tracking.

---

**Module:** `src/consensus.rs`  
**Integration Point:** `src/lib.rs::_record_heartbeat()`  
**Storage Type:** Temporary with TTL
