# Telemetry Storage Refactoring Summary

## Overview

This document describes the refactoring of high-frequency telemetry tracking to use Soroban **Temporary Storage** instead of persistent storage, significantly reducing long-term ledger rent burdens.

## Problem Statement

Previously, incoming telemetry tracking (heartbeat timestamps) was stored in a way that could accumulate ledger rent costs. High-frequency, short-lived price feeds should not burden persistent storage nodes long-term.

## Solution Implemented

### 1. Temporary Storage Usage

The heartbeat tracking system now uses **Soroban Temporary Storage** with proper TTL (Time-To-Live) configuration:

```rust
const HEARTBEAT_TTL_LEDGERS: u32 = 17_280; // ~24 hours at 5s/ledger
const HEARTBEAT_TTL_THRESHOLD: u32 = 5_000; // Extend when < 5000 ledgers remain
```

### 2. Key Changes in `src/lib.rs`

#### Updated `_record_heartbeat` Function

```rust
fn _record_heartbeat(env: &Env, asset: Symbol) {
    let mut timestamps: Map<Symbol, u64> = env.storage().temporary().get(&HEARTBEAT_KEY).unwrap_or_else(|| Map::new(env));
    timestamps.set(asset, env.ledger().timestamp());
    env.storage().temporary().set(&HEARTBEAT_KEY, &timestamps);
    
    // Set TTL to ensure entries expire naturally after validation window
    env.storage().temporary().extend_ttl(
        &HEARTBEAT_KEY,
        HEARTBEAT_TTL_THRESHOLD,
        HEARTBEAT_TTL_LEDGERS,
    );
}
```

**Key Features:**
- Uses `env.storage().temporary()` for all telemetry data
- Automatically extends TTL when entries are accessed and have < 5000 ledgers remaining
- Entries expire after ~24 hours (17,280 ledgers at 5 seconds per ledger)
- No manual cleanup required - ledger automatically purges expired entries

### 3. Automatic Expiration

Temporary storage entries expire naturally from ledger state once their validation time window closes:

- **Initial TTL:** 17,280 ledgers (~24 hours)
- **Auto-extension:** When accessed with < 5,000 ledgers remaining, TTL resets
- **Natural cleanup:** Soroban automatically removes expired entries from ledger state

### 4. Functions Using Temporary Storage

The following public functions interact with temporary telemetry storage:

- `update_heartbeat()` - Records timestamp for an asset
- `get_last_update_timestamp()` - Retrieves last update time
- `is_data_fresh()` - Checks if data is within the configured interval
- `finalize_consensus()` - Cleans up temporary cache and telemetry

## Benefits

### 1. **Reduced Ledger Rent**
- No long-term storage costs for high-frequency telemetry data
- Old entries automatically expire without manual intervention

### 2. **Improved Performance**
- Temporary storage is optimized for short-lived data
- No accumulation of historical telemetry data

### 3. **Automatic Cleanup**
- Soroban's ledger state automatically purges expired entries
- No need for manual maintenance or archival processes

### 4. **Cost Efficiency**
- Temporary storage has significantly lower rent costs
- Predictable costs based on TTL configuration

## Storage Type Comparison

| Storage Type | Use Case | Rent Cost | Expiration |
|--------------|----------|-----------|------------|
| **Instance** | Contract configuration (admin, intervals) | Medium | Manual/explicit |
| **Persistent** | Long-term data (stakes, node profiles) | High | Manual/explicit |
| **Temporary** | High-frequency telemetry, caches | Low | Automatic via TTL |

## Configuration

### Heartbeat Interval
Default: 5 minutes (300 seconds)
```rust
const DEFAULT_HEARTBEAT_INTERVAL: u64 = 5 * 60;
```

Can be customized via:
```rust
pub fn set_heartbeat_interval(env: Env, interval: u64, admin: Address)
```

### TTL Parameters

```rust
// Entries live for ~24 hours before automatic expiration
const HEARTBEAT_TTL_LEDGERS: u32 = 17_280; 

// Auto-extend TTL when < 5000 ledgers remain
const HEARTBEAT_TTL_THRESHOLD: u32 = 5_000;
```

## Usage Example

```rust
// Record a heartbeat (automatically sets TTL)
TimeLockedUpgradeContract::_record_heartbeat(&env, symbol_short!("VALUE"));

// Check if data is fresh
let is_fresh = TimeLockedUpgradeContract::is_data_fresh(env.clone(), symbol_short!("NGN"));

// Get last update timestamp (returns Option<u64>)
let last_update = TimeLockedUpgradeContract::get_last_update_timestamp(env.clone(), symbol_short!("NGN"));

// Data automatically expires after TTL window - no manual cleanup needed
```

## Testing

The test suite in `src/test.rs` validates the refactored behavior:

- `test_heartbeat_fresh_data()` - Verifies immediate freshness after update
- `test_heartbeat_stale_data()` - Validates expiration after interval
- `test_heartbeat_never_updated()` - Handles missing entries gracefully
- `test_heartbeat_custom_interval()` - Tests configurable intervals
- `test_stake_updates_heartbeat()` - Verifies stake operations record telemetry
- `test_set_value_updates_heartbeat()` - Verifies value updates record telemetry

## Migration Notes

### No Breaking Changes
This refactoring maintains the same public API. Existing integrations continue to work without modification.

### Storage Behavior Changes
- Old persistent heartbeat data (if any) will remain until manually removed
- New heartbeat data uses temporary storage exclusively
- Automatic expiration ensures no long-term accumulation

## Technical Details

### Storage Keys

```rust
const HEARTBEAT_KEY: Symbol = symbol_short!("HBEAT");
const HB_INTERVAL_KEY: Symbol = symbol_short!("HBINTV");
const CONSENSUS_CACHE_KEY: Symbol = symbol_short!("CACHE");
```

### Data Structure

```rust
// Map of asset symbols to their last update timestamps
Map<Symbol, u64>
```

## Best Practices

1. **Use temporary storage for:**
   - High-frequency price feeds
   - Telemetry and heartbeat data
   - Consensus cache data
   - Any data with natural expiration windows

2. **TTL Configuration:**
   - Set TTL longer than your validation window
   - Use threshold-based extension for active data
   - Balance between availability and cost

3. **Monitoring:**
   - Track `is_data_fresh()` for data availability
   - Monitor heartbeat intervals for optimal freshness
   - Adjust TTL based on actual usage patterns

## Future Enhancements

Potential improvements for consideration:

1. **Dynamic TTL:** Adjust TTL based on asset activity levels
2. **Metrics:** Add telemetry for storage usage patterns
3. **Archival:** Optional off-chain archival for historical analysis
4. **Multi-tier Storage:** Automatic migration from temporary to persistent for important events

## Conclusion

This refactoring successfully moves high-frequency telemetry tracking to Soroban's temporary storage, eliminating long-term ledger rent burdens while maintaining full functionality. Old entries now expire naturally from ledger state registers once their validation time window closes.

---

**Implementation Date:** June 2026  
**Contract:** TimeLockedUpgradeContract  
**Modified Files:** `src/lib.rs`
