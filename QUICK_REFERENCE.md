# 🚀 StellarFlow Callback Interface - Quick Reference

## TL;DR - Start Here

### For Subscriber Contracts (3 steps)

#### 1️⃣ Implement Callback
```rust
pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
    // Your price update logic here
    // payload contains: asset, price, timestamp, provider, decimals, confidence_score
}
```

#### 2️⃣ Subscribe to Oracle
```rust
let oracle = PriceOracleClient::new(&env, &oracle_address);
oracle.subscribe_to_price_updates(&my_contract_address)?
```

#### 3️⃣ That's It!
Callbacks now fire automatically when prices update. No polling needed!

---

## API Reference

### Subscribe
```rust
subscribe_to_price_updates(callback_contract: Address) -> Result<(), String>
```
**Returns**: `Ok(())` on success, error if already subscribed

### Unsubscribe
```rust
unsubscribe_from_price_updates(callback_contract: Address) -> Result<(), String>
```
**Returns**: `Ok(())` on success, error if not found

### Get Subscribers
```rust
get_price_update_subscribers() -> Vec<Address>
```
**Returns**: List of all subscribed contracts

### Callback (implement in your contract)
```rust
pub fn on_price_update(env: Env, payload: PriceUpdatePayload)
```

---

## Quick Examples

### Example 1: Lending Protocol - Check Liquidations
```rust
pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
    let positions = get_positions_for_asset(&env, &payload.asset);
    for position in positions.iter() {
        if is_undercollateralized(&env, &position, payload.price) {
            liquidate(&env, &position);
        }
    }
}
```

### Example 2: DEX - Rebalance Pools
```rust
pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
    let old_price = get_last_price(&env, &payload.asset);
    let pct_change = ((payload.price - old_price) * 10000) / old_price;
    
    if pct_change.abs() > 200 {  // >2% change
        rebalance_pool(&env, &payload.asset, payload.price);
    }
}
```

### Example 3: Store Latest Price
```rust
pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
    env.storage().instance().set(
        &DataKey::LatestPrice(payload.asset),
        &payload.price,
    );
}
```

---

## PriceUpdatePayload Structure

```rust
pub struct PriceUpdatePayload {
    pub asset: Symbol,              // "NGN", "KES", "GHS", etc.
    pub price: i128,                // Price value (9 decimals)
    pub timestamp: u64,             // When it was updated
    pub provider: Address,          // Who submitted it
    pub decimals: u32,              // Always 9
    pub confidence_score: u32,      // 0-100
}
```

---

## Common Patterns

### Pattern 1: Validate Caller (Security)
```rust
const ORACLE_ADDRESS: &str = "C...";

pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
    let caller = env.invoker();
    assert_eq!(caller, Address::from_contract_id(&env, ORACLE_ADDRESS));
    // Process update...
}
```

### Pattern 2: Check Freshness
```rust
pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
    let now = env.ledger().timestamp();
    assert!(payload.timestamp <= now && payload.timestamp > now - 300);
    // Process update...
}
```

### Pattern 3: Idempotent Updates
```rust
pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
    let current_version = get_price_version(&env, &payload.asset);
    
    // Only update if newer
    if payload.timestamp > current_version.timestamp {
        update_price(&env, payload);
    }
}
```

### Pattern 4: Store + Emit Event
```rust
pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
    store_price(&env, &payload);
    
    env.events().publish(
        (Symbol::new(&env, "price_update_received"),),
        (payload.asset, payload.price, payload.timestamp),
    );
}
```

---

## Troubleshooting

| Issue | Cause | Solution |
|-------|-------|----------|
| Callback not called | Not subscribed | Call `subscribe_to_price_updates` first |
| "Already subscribed" error | Duplicate subscription | Check if already subscribed first |
| "Not found" error | Not subscribed | Call `subscribe_to_price_updates` |
| Callback panics | Invalid data | Add validation in `on_price_update` |
| Gas limit exceeded | Too complex callback | Simplify callback logic |

---

## Performance Tips

### ✅ DO
- Keep callbacks simple and fast
- Use early exits (return on first check)
- Store frequently-accessed data
- Validate inputs
- Emit events for monitoring

### ❌ DON'T
- Don't do complex computations in callback
- Don't call other contracts in callback
- Don't store too much data per call
- Don't ignore validation errors
- Don't subscribe too many contracts (max ~10)

---

## Testing Your Callback

```rust
#[test]
fn test_my_callback() {
    let env = Env::default();
    let contract = env.register_contract(None, MyProtocol);
    
    // Simulate callback
    let payload = PriceUpdatePayload {
        asset: symbol_short!("NGN"),
        price: 1_500_000,
        timestamp: 1_000_000,
        provider: Address::generate(&env),
        decimals: 9,
        confidence_score: 95,
    };
    
    // Call callback directly
    MyProtocolClient::new(&env, &contract).on_price_update(&payload);
    
    // Verify state changed correctly
    // assert_eq!(get_updated_state(&env), expected_state);
}
```

---

## Integration Checklist

- [ ] Read [CALLBACK_INTERFACE.md](CALLBACK_INTERFACE.md)
- [ ] Implement `on_price_update` in your contract
- [ ] Add validation in callback (caller, timestamp, price)
- [ ] Test callback with various inputs
- [ ] Call `subscribe_to_price_updates` to register
- [ ] Monitor callback gas usage
- [ ] Set up event listeners for debugging
- [ ] Deploy and verify callbacks fire

---

## Key Concepts

### Event-Driven vs Polling
```
Before (Polling):
Your Contract → repeatedly calls oracle.get_price()

After (Callbacks):
Oracle → automatically calls your contract.on_price_update()
```

### Non-Blocking Semantics
```
If callback fails:
✅ Price still updates
✅ Other subscribers still get callbacks
✅ Transaction succeeds
```

### Data Flow
```
Price Update
    ↓
validate & store price
    ↓
publish PriceUpdatedEvent
    ↓
notify_subscribers()
    ↓
for each subscriber:
    call on_price_update(payload)
    (ignore errors)
    ↓
Done
```

---

## Asset Symbols

Common assets in StellarFlow:

| Symbol | Asset | Decimals |
|--------|-------|----------|
| NGN | Nigerian Naira | 2 |
| KES | Kenyan Shilling | 2 |
| GHS | Ghanaian Cedi | 2 |
| XLM | Stellar Lumens | 7 |
| USDT | Tether USD | 6 |

All prices normalized to 9 decimals internally.

---

## Common Mistakes to Avoid

❌ **Mistake 1**: Not validating caller
```rust
// BAD
pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
    // Anyone could call this!
}

// GOOD
pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
    assert_eq!(env.invoker(), ORACLE_ADDRESS);
}
```

❌ **Mistake 2**: Ignoring timestamp
```rust
// BAD
pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
    update_price(&env, payload.price);  // Old price?
}

// GOOD
pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
    if payload.timestamp > last_update_time {
        update_price(&env, payload.price);
    }
}
```

❌ **Mistake 3**: Doing too much in callback
```rust
// BAD
pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
    for position in get_all_positions() {           // Expensive!
        for history in get_all_history() {          // Expensive!
            calculate_complex_math();               // Expensive!
        }
    }
}

// GOOD
pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
    check_liquidations_for_asset(&env, &payload.asset);  // Focused
}
```

---

## Resources

| Resource | Link |
|----------|------|
| Full Guide | [CALLBACK_INTERFACE.md](CALLBACK_INTERFACE.md) |
| Implementation | [Summary](CALLBACK_IMPLEMENTATION_SUMMARY.md) |
| Example | [Lending Integration](EXAMPLE_LENDING_INTEGRATION.rs) |
| Tests | [test.rs](contracts/price-oracle/src/test.rs) |
| Main Readme | [README.md](README.md) |

---

## One-Minute Cheatsheet

```rust
// 1. Import what you need
use price_oracle::types::PriceUpdatePayload;

// 2. Implement callback
#[contractimpl]
impl MyProtocol {
    pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
        // Validate caller
        assert_eq!(env.invoker(), ORACLE_ADDRESS);
        
        // Process price update
        let asset = payload.asset;
        let price = payload.price;
        let timestamp = payload.timestamp;
        
        // Do your thing...
    }
}

// 3. Subscribe
let oracle = PriceOracleClient::new(&env, &oracle_address);
oracle.subscribe_to_price_updates(&my_contract)?

// 4. Done! Callbacks fire automatically
```

---

## Questions?

1. **How do I subscribe?** → Call `subscribe_to_price_updates`
2. **How are callbacks triggered?** → Automatically on price update
3. **What if my callback fails?** → Price still updates, other callbacks run
4. **Can I unsubscribe?** → Yes, call `unsubscribe_from_price_updates`
5. **How many subscribers?** → Recommend max 10 for production
6. **What's the gas cost?** → ~5,000 per callback + your logic
7. **Can I filter assets?** → No, subscribe to all (future: per-asset filtering)
8. **Do I still need polling?** → No! Callbacks are real-time

---

**Last Updated**: April 25, 2026  
**Version**: 1.0.0  
**Status**: ✅ Ready to Use
