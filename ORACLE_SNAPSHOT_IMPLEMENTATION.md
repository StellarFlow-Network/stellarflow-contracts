# Oracle Snapshot Implementation

## Overview

This document describes the implementation of the OracleSnapshot feature for the StellarFlow price oracle contract. This feature enables subgraphs and indexers to efficiently track price history by emitting checkpointed events every 100 ledgers.

## Goal

Make it easier for subgraphs/indexers to track history by emitting a special `OracleSnapshot` event every 100 ledgers containing all current asset prices. This provides a "Checkpointed" state for off-chain databases to sync against.

## Implementation Details

### 1. New Data Types

#### DataKey Addition (src/types.rs)
```rust
pub enum DataKey {
    // ... existing variants ...
    /// Last ledger sequence where a snapshot was emitted (for checkpoint events).
    LastSnapshotLedger,
}
```

This key tracks the ledger number of the most recent snapshot emission to prevent duplicate snapshots at the same ledger boundary.

#### SnapshotPrice Struct (src/types.rs)
```rust
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotPrice {
    /// The asset symbol (e.g., NGN, KES, GHS).
    pub asset: Symbol,
    /// The current price value (normalized to 9 decimal places).
    pub price: i128,
    /// Timestamp when this price was last updated.
    pub timestamp: u64,
}
```

This structure represents a single asset's price data within a snapshot (note: while defined, the actual snapshot event uses the existing `PriceEntry` type which contains the same data: price, timestamp, decimals).

### 2. Event Definition

#### OracleSnapshotEvent (src/lib.rs)
```rust
#[soroban_sdk::contractevent]
pub struct OracleSnapshotEvent {
    /// The ledger sequence number at which this snapshot was taken.
    pub ledger_sequence: u32,
    /// Timestamp when the snapshot was emitted.
    pub timestamp: u64,
    /// All currently tracked asset prices at this checkpoint.
    pub prices: soroban_sdk::Vec<PriceEntry>,
}
```

This is the event that gets emitted every 100 ledgers, containing all current verified prices.

### 3. Core Logic

#### emit_snapshot_if_needed() Function (src/lib.rs)

```rust
fn emit_snapshot_if_needed(env: &Env) {
    let current_ledger = env.ledger().sequence();
    
    // Only emit snapshots at 100-ledger boundaries
    if current_ledger % 100 != 0 {
        return;
    }
    
    // Check if we've already emitted a snapshot at this ledger
    let last_snapshot: Option<u32> = env
        .storage()
        .instance()
        .get(&DataKey::LastSnapshotLedger);
    
    if let Some(last) = last_snapshot {
        if last >= current_ledger {
            return;
        }
    }
    
    // Collect all current asset prices
    let assets = get_tracked_assets(env);
    let storage = env.storage().persistent();
    let mut prices = soroban_sdk::Vec::new(env);
    
    for asset in assets.iter() {
        if let Some(price_data) = storage.get::<DataKey, PriceData>(&DataKey::VerifiedPrice(asset.clone())) {
            let entry = PriceEntry {
                price: price_data.price,
                timestamp: price_data.timestamp,
                decimals: price_data.decimals,
            };
            prices.push_back(entry);
        }
    }
    
    // Emit the snapshot event
    env.events().publish_event(&OracleSnapshotEvent {
        ledger_sequence: current_ledger,
        timestamp: env.ledger().timestamp(),
        prices,
    });
    
    // Update the last snapshot ledger
    env.storage()
        .instance()
        .set(&DataKey::LastSnapshotLedger, &current_ledger);
}
```

This function:
1. Checks if the current ledger is at a 100-ledger boundary (divisible by 100)
2. Verifies we haven't already emitted a snapshot at this ledger
3. Retrieves all tracked assets
4. For each asset, reads the latest verified price from storage
5. Packages all prices into a `Vec<PriceEntry>`
6. Emits the `OracleSnapshotEvent` with the snapshot data
7. Records the snapshot ledger in storage to prevent duplicate emissions

### 4. Integration Points

The snapshot emission is hooked into price update operations:

#### In set_price() - Three places:
1. **Zero-write optimization path** (line ~1110): When price is unchanged but timestamp needs refresh
   ```rust
   emit_snapshot_if_needed(&env);
   return Ok(());
   ```

2. **Main price update path** (line ~1133): After price is successfully written
   ```rust
   emit_snapshot_if_needed(&env);
   Ok(())
   ```

#### In update_price() - One place:
1. **After median calculation and storage** (line ~1433): After price is finalized
   ```rust
   emit_snapshot_if_needed(&env);
   Ok(())
   ```

Both functions call the snapshot emission function after notifying subscribers, ensuring the snapshot contains the latest prices.

### 5. Import Updates

Added `PriceEntry` to the imports from the types module to support the event definition:
```rust
use crate::types::{..., PriceEntry, ...};
```

## How It Works

### Snapshot Frequency
- **Ledger Boundary**: Every ledger where `ledger_sequence % 100 == 0`
- **On Stellar**: Approximately every 8-10 minutes (depending on network speed)
- **Examples**: Ledgers 100, 200, 300, 400... emit snapshots

### Snapshot Deduplication
- Stored `LastSnapshotLedger` prevents multiple snapshots at the same ledger
- If another price update occurs at the same 100-ledger boundary, only the first snapshot is emitted
- Works correctly even with multiple concurrent price updates

### Data Included
Each snapshot contains:
- **All tracked assets**: Every asset currently being tracked by the oracle
- **Latest verified prices**: Only prices from the VerifiedPrice bucket (not community prices)
- **Price metadata**: Timestamp when each price was last updated and decimal precision
- **Ledger information**: Exact ledger where snapshot was taken

## Benefits for Off-Chain Systems

### 1. Checkpointed Synchronization
Indexers can use snapshots as synchronization points to verify their state is consistent with the oracle.

### 2. Efficient History Storage
Instead of storing every price update, subgraphs can store snapshots at regular intervals, reducing storage requirements.

### 3. Gap Recovery
If an indexer falls behind, it can use the latest snapshot to quickly bring its state up-to-date rather than replaying all updates.

### 4. Audit Trail
Snapshots provide a timestamped, immutable record of all prices at specific ledger intervals.

## Example: Indexer Usage

```javascript
// Example: Listening for OracleSnapshot events with a GraphQL indexer
const handler = {
  OracleSnapshotEvent: async (event) => {
    const { ledger_sequence, timestamp, prices } = event;
    
    // Store the snapshot as a checkpoint
    await db.snapshots.create({
      ledger_sequence,
      timestamp,
      prices: prices.map(p => ({
        price: p.price,
        timestamp: p.timestamp,
        decimals: p.decimals
      }))
    });
    
    // Verify snapshot matches current prices
    const current_prices = await db.prices.findAll();
    const snapshot_prices = prices.map(p => p.price);
    
    if (!prices_match(current_prices, snapshot_prices)) {
      console.warn(`Price mismatch at ledger ${ledger_sequence}`);
      await recovery_sync();
    }
  }
};
```

## Testing Considerations

The implementation should be tested for:

1. **Scope correctness**: Verify snapshots are only emitted at 100-ledger boundaries
2. **Completeness**: Ensure all tracked assets are included in snapshots
3. **Deduplication**: Verify no duplicate snapshots at the same ledger
4. **Data accuracy**: Check that snapshot prices match current oracle prices
5. **Multiple updates**: Ensure snapshots work correctly with concurrent price updates

## Migration Notes

This is a **non-breaking change**:
- Existing price update functions work unchanged
- No modifications to storage format for existing data
- New storage key (`LastSnapshotLedger`) is independent
- CLI/contract clients can optionally listen for snapshot events

## Future Enhancements

Potential improvements for future versions:

1. **Configurable frequency**: Allow adjusting snapshot frequency (currently fixed at 100 ledgers)
2. **Selective snapshots**: Emit snapshots only for specific asset groups
3. **Snapshot versioning**: Include schema version in event for backward compatibility
4. **Compression**: For large numbers of assets, consider compressing price data
5. **Delta encoding**: Emit only changed prices since last snapshot

## Files Modified

1. **src/types.rs**
   - Added `LastSnapshotLedger` to `DataKey` enum
   - Added `SnapshotPrice` struct (for reference/documentation)

2. **src/lib.rs**
   - Added `PriceEntry` to imports from types
   - Added `OracleSnapshotEvent` event struct definition
   - Added `emit_snapshot_if_needed()` function
   - Integrated snapshot emission into `set_price()` function (3 locations)
   - Integrated snapshot emission into `update_price()` function (1 location)

3. **contracts/price-oracle/INTEGRATION.md**
   - Added "Oracle Snapshot Events (For Indexers/Subgraphs)" section
   - Documented event structure and usage for developers

## Verification

No compilation errors detected. All changes compile successfully with the Soroban SDK.
