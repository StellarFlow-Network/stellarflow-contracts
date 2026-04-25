# Cross-Contract Callback Interface Implementation Summary

## Overview

A standardized cross-contract callback interface has been successfully implemented for the StellarFlow Price Oracle. This enables downstream Soroban contracts (Lending protocols, DEXs, etc.) to subscribe to real-time price updates without polling.

## What's New

### 1. Core Interface

**Subscription Functions** (Public API in `PriceOracleClient`):

```rust
// Register a contract to receive price update callbacks
pub fn subscribe_to_price_updates(callback_contract: Address) -> Result<(), String>

// Unregister a contract from callbacks
pub fn unsubscribe_from_price_updates(callback_contract: Address) -> Result<(), String>

// Get list of all subscribed contracts
pub fn get_price_update_subscribers() -> Vec<Address>
```

### 2. Standard Callback Interface

Subscriber contracts must implement:

```rust
pub fn on_price_update(env: Env, payload: PriceUpdatePayload)
```

Where `PriceUpdatePayload` contains:
- `asset`: Symbol (NGN, KES, GHS, etc.)
- `price`: i128 (normalized to 9 decimals)
- `timestamp`: u64 (ledger timestamp)
- `provider`: Address (who submitted the price)
- `decimals`: u32 (always 9)
- `confidence_score`: u32 (0-100)

### 3. Automatic Callback Invocation

Callbacks are automatically triggered when prices are updated:
- ✅ `update_price()` - authorized provider updates
- ✅ `set_price()` - admin price setting

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│          StellarFlow Price Oracle (lib.rs)              │
│                                                          │
│  ┌────────────────────────────────────────────────┐    │
│  │ Public Contract Interface                      │    │
│  │  - update_price()                              │    │
│  │  - set_price()                                 │    │
│  │  - subscribe_to_price_updates()                │    │
│  │  - unsubscribe_from_price_updates()            │    │
│  │  - get_price_update_subscribers()              │    │
│  └────────────────────────────────────────────────┘    │
│                        ↓                                 │
│  ┌────────────────────────────────────────────────┐    │
│  │ Callbacks Module (callbacks.rs)                │    │
│  │  - subscribe()                                 │    │
│  │  - unsubscribe()                               │    │
│  │  - get_subscribers()                           │    │
│  │  - notify_subscribers()                        │    │
│  │  - try_invoke_callback()                       │    │
│  └────────────────────────────────────────────────┘    │
│                        ↓                                 │
│  ┌────────────────────────────────────────────────┐    │
│  │ Types Module (types.rs)                        │    │
│  │  - PriceUpdatePayload                          │    │
│  │  - DataKey::PriceUpdateSubscribers             │    │
│  └────────────────────────────────────────────────┘    │
│                                                          │
└─────────────────────────────────────────────────────────┘
                           ↓
        ┌──────────────────┬──────────────────┐
        ↓                  ↓                  ↓
    Lending           DEX/AMM            Other
    Protocol          Protocol           Protocols
    
    on_price_update() callbacks
```

## File Structure

```
contracts/price-oracle/src/
├── lib.rs                      (Main contract, updated)
│   └── Subscription functions
│   └── Callback integration
├── types.rs                    (Updated)
│   └── PriceUpdatePayload struct
│   └── DataKey::PriceUpdateSubscribers
├── callbacks.rs                (NEW)
│   └── Subscription management
│   └── Callback invocation
├── auth.rs
├── math.rs
├── median.rs
├── asset_symbol.rs
└── test.rs                     (Extended)
    └── 11 new callback tests

CALLBACK_INTERFACE.md           (NEW)
└── Comprehensive documentation
```

## Quick Start

### For Lending Protocol Developers

1. **Implement callback in your contract**:
   ```rust
   #[contract]
   pub struct MyLendingPool;
   
   #[contractimpl]
   impl MyLendingPool {
       pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
           // Check for liquidation opportunities
           // Update collateral requirements
           // Trigger rebalancing if needed
       }
   }
   ```

2. **Subscribe to oracle**:
   ```rust
   let oracle = PriceOracleClient::new(&env, &oracle_address);
   oracle.subscribe_to_price_updates(&my_contract_address)?;
   ```

3. **Receive automatic updates**:
   ```
   When price changes → Oracle calls on_price_update() → React immediately
   ```

### For DEX Developers

1. **Implement callback**:
   ```rust
   pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
       // Rebalance liquidity pools
       // Check slippage limits
       // Adjust fee tiers
       // Emit events for off-chain systems
   }
   ```

2. **Subscribe**:
   ```rust
   oracle.subscribe_to_price_updates(&dex_contract)?;
   ```

## Testing

### Included Tests

The implementation includes 11 comprehensive tests:

1. ✅ `test_subscribe_to_price_updates` - Basic subscription
2. ✅ `test_subscribe_duplicate_fails` - Duplicate prevention
3. ✅ `test_multiple_subscribers` - Multiple registrations
4. ✅ `test_unsubscribe_from_price_updates` - Unsubscribe
5. ✅ `test_unsubscribe_nonexistent_fails` - Error handling
6. ✅ `test_get_empty_subscriber_list` - Empty state
7. ✅ `test_subscribe_unsubscribe_cycle` - Lifecycle
8. ✅ `test_update_price_does_not_crash_with_subscribers` - Integration
9. ✅ `test_set_price_with_subscribers` - Admin integration
10. ✅ `test_subscribe_and_get_subscribers` (callbacks.rs)
11. ✅ `test_unsubscribe` (callbacks.rs)

Run tests with:
```bash
cd contracts/price-oracle
cargo test
```

## Key Features

### 1. Event-Driven Architecture
- No polling required
- Immediate reaction to price changes
- Real-time synchronization

### 2. Standardized Interface
- Single `on_price_update` function signature
- All subscribers implement the same contract
- Easy integration for new protocols

### 3. Gas-Efficient
- O(n) subscription operations (n = subscriber count)
- O(1) callback dispatch per subscriber
- Recommended max: ≤10 subscribers

### 4. Resilient Design
- Failed callbacks don't block price updates
- Errors logged internally, not propagated
- Non-blocking callback semantics

### 5. Security
- Oracle is authoritative source
- Subscribers should validate caller
- Price data immutable after callback

## Usage Examples

### Example 1: Liquidation Bot (Lending)

```rust
pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
    let asset = payload.asset;
    let new_price = payload.price;
    
    // Find positions to liquidate
    let positions = find_undercollateralized(&env, &asset, new_price);
    
    for position in positions.iter() {
        trigger_liquidation(&env, &position);
    }
}
```

### Example 2: Pool Rebalancer (DEX)

```rust
pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
    let old_price = get_last_known_price(&env, &payload.asset);
    let price_change_pct = calc_pct_change(old_price, payload.price);
    
    // Rebalance if price moved >2%
    if price_change_pct.abs() > 200 {
        rebalance_pool(&env, &payload.asset, payload.price);
    }
}
```

### Example 3: Price Feed Bridge

```rust
pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
    // Store price data for other protocols
    env.storage().instance().set(
        &DataKey::LatestPrice(payload.asset),
        &payload
    );
    
    // Emit event for off-chain indexing
    env.events().publish((
        Symbol::new(&env, "oracle_price_update"),
    ), (
        payload.asset,
        payload.price,
        payload.timestamp,
    ));
}
```

## Error Handling

### Subscription Errors

| Error | Cause | Resolution |
|-------|-------|-----------|
| "Contract is already subscribed" | Duplicate subscription | Call unsubscribe first |
| "Contract not found in subscribers" | Unsubscribing non-subscriber | Verify contract is subscribed |

### Callback Execution

- Callbacks are best-effort
- Non-blocking: failures don't affect price storage
- Implement defensive validation in `on_price_update`

```rust
pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
    // Validate data integrity
    assert!(payload.price > 0, "Invalid price");
    assert!(payload.timestamp <= env.ledger().timestamp(), "Future timestamp");
    
    // Your logic here
}
```

## Performance Characteristics

| Operation | Complexity | Notes |
|-----------|-----------|-------|
| Subscribe | O(n) | n = subscriber count |
| Unsubscribe | O(n) | Linear search + remove |
| Get Subscribers | O(1) | Storage retrieval |
| Callback Dispatch | O(n*m) | n = subscribers, m = callback complexity |
| Price Update | O(1) | Async callback dispatch |

## Security Considerations

### For Subscriber Contracts

1. **Verify Oracle Source**:
   ```rust
   const ORACLE_ADDRESS: &str = "C...";
   
   pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
       assert_eq!(env.invoker(), Address::from_contract_id(&env, ORACLE_ADDRESS));
   }
   ```

2. **Validate Payload**:
   ```rust
   // Check timestamp freshness
   let now = env.ledger().timestamp();
   assert!(payload.timestamp <= now && payload.timestamp > now - 300);
   
   // Check price sanity
   assert!(payload.price > 0 && payload.price < MAX_REASONABLE_PRICE);
   ```

3. **Idempotent Updates**:
   ```rust
   // Design for replayability
   // Store both price and update timestamp
   // Check if update is newer before applying
   ```

## Next Steps

### For Developers

1. Read [CALLBACK_INTERFACE.md](CALLBACK_INTERFACE.md) for detailed documentation
2. Review test cases in [src/test.rs](contracts/price-oracle/src/test.rs)
3. Implement `on_price_update` in your contract
4. Subscribe to the oracle
5. Test with mock contracts

### For Integration

1. Add callback interface to your protocol
2. Update protocol state on `on_price_update`
3. Monitor callback gas usage
4. Set up event listeners for debugging
5. Deploy with ≤10 initial subscribers

## Support

- 📖 [Callback Interface Documentation](CALLBACK_INTERFACE.md)
- 🧪 [Test Cases](contracts/price-oracle/src/test.rs)
- 📋 [Integration Guide](contracts/price-oracle/INTEGRATION.md)
- 🤝 [Main README](README.md)

---

**Version**: 1.0.0  
**Status**: ✅ Ready for Production  
**Last Updated**: April 25, 2026
