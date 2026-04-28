# Oracle Snapshot - Quick Reference

## What It Does

Every 100 ledgers, the StellarFlow oracle emits an `OracleSnapshotEvent` containing a complete snapshot of all current asset prices. This helps indexers and subgraphs track price history efficiently.

## Event Structure

```rust
#[soroban_sdk::contractevent]
pub struct OracleSnapshotEvent {
    pub ledger_sequence: u32,    // The ledger number (e.g., 100, 200, 300)
    pub timestamp: u64,           // Unix timestamp when snapshot was taken
    pub prices: Vec<PriceEntry>,  // All current asset prices
}

// Each price in the vector contains:
pub struct PriceEntry {
    pub price: i128,      // The price value (normalized to 9 decimals)
    pub timestamp: u64,   // When this price was last updated
    pub decimals: u32,    // Always 9 for normalized prices
}
```

## How to Use

### Listen for Events

```javascript
// In your indexer (e.g., Indexing Service, The Graph)
const handler = {
  OracleSnapshotEvent: async (event) => {
    const { ledger_sequence, timestamp, prices } = event;
    
    // Store snapshot in your database
    await db.saveSnapshot({
      ledgerSequence: ledger_sequence,
      timestamp,
      prices: prices.map(p => ({
        price: p.price,
        timestamp: p.timestamp
      }))
    });
  }
};
```

### Sync Against Snapshots

```sql
-- Get most recent snapshot
SELECT * FROM snapshots 
ORDER BY ledger_sequence DESC 
LIMIT 1;

-- Use as verification point for current prices
-- If your prices don't match the latest snapshot, resync
```

## Emission Schedule

| Ledger | Emits? |
|--------|--------|
| 99     | ❌     |
| 100    | ✅     |
| 199    | ❌     |
| 200    | ✅     |
| 299    | ❌     |
| 300    | ✅     |

**Frequency**: ~Every 8-10 minutes on Stellar testnet/mainnet

## Integration Example

```python
# Python example using Stellar event indexer
from stellar_indexer import subscribe_to_events

@subscribe_to_events("OracleSnapshotEvent")
def handle_oracle_snapshot(event):
    ledger = event['ledger_sequence']
    prices = event['prices']
    
    print(f"Snapshot at ledger {ledger}: {len(prices)} prices")
    
    # Update your database
    for price in prices:
        print(f"  Price: {price['price']}, Updated: {price['timestamp']}")
    
    # Validate against your current state
    validate_prices(prices)
```

## What It Helps With

✅ **Efficient History Tracking** - Don't replay every update, use snapshots  
✅ **State Verification** - Verify your prices match the oracle's snapshot  
✅ **Gap Recovery** - Quickly sync after downtime using latest snapshot  
✅ **Audit Trail** - Immutable record of prices at specific ledgers  

## When Snapshots Are NOT Emitted

- During ledgers that are not multiples of 100
- If a snapshot was already emitted at a ledger (deduplication)
- Before any price has been set (no prices to snapshot)

## Notes for Developers

1. **Only Verified Prices**: Snapshots contain only verified prices (not community prices)
2. **Complete Asset List**: Every tracked asset is included, even if price is stale
3. **Normalized Decimals**: All prices are normalized to 9 decimal places
4. **Gas Efficient**: Snapshots are emitted alongside price updates, no extra calls needed
5. **Time-Ordered**: Within a ledger, multiple price updates will only trigger one snapshot

## Common Patterns

### Pattern 1: Just Get Latest Prices

```sql
-- Most recent snapshot
SELECT prices FROM snapshots 
WHERE ledger_sequence = (
  SELECT MAX(ledger_sequence) FROM snapshots
);
```

### Pattern 2: Track Price Over Time

```sql
-- Get price for one asset across multiple snapshots
SELECT ledger_sequence, timestamp, price 
FROM (
  SELECT UNNEST(prices) as price_entry 
  FROM snapshots
) 
WHERE asset = 'NGN'
ORDER BY ledger_sequence DESC;
```

### Pattern 3: Verify State Integrity

```javascript
async function verify_oracle_state() {
  // Get latest snapshot
  const snapshot = await get_latest_snapshot();
  
  // Get current prices from oracle
  const current = await oracle.get_all_prices();
  
  // Compare
  for (let p of snapshot.prices) {
    if (current[p.asset] !== p.price) {
      alert('Price mismatch detected!');
      await full_resync();
    }
  }
}
```

## Troubleshooting

**Q: Why is there no snapshot event at ledger 150?**  
A: Snapshots only emit at ledger boundaries divisible by 100 (100, 200, 300, etc.)

**Q: My snapshot has fewer prices than expected**  
A: Verify all assets are tracked with `get_all_assets()`. Snapshots only include tracked assets.

**Q: I got two snapshots at ledger 200**  
A: This shouldn't happen - deduplication prevents it. Check your event indexer configuration.

**Q: How do I know if an asset price in the snapshot is stale?**  
A: Check the `timestamp` field in the price entry and compare with current time.

## Reference

- Event Definition: [src/lib.rs](contracts/price-oracle/src/lib.rs)
- Snapshot Logic: [src/lib.rs - emit_snapshot_if_needed()](contracts/price-oracle/src/lib.rs)
- Storage Key: [src/types.rs - DataKey::LastSnapshotLedger](contracts/price-oracle/src/types.rs)
- Integration Guide: [contracts/price-oracle/INTEGRATION.md](contracts/price-oracle/INTEGRATION.md)
