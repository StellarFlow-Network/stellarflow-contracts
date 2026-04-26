# StellarFlow Cross-Contract Callback Interface

## Overview

The Cross-Contract Callback Interface enables downstream Soroban contracts (e.g., Lending protocols, DEXs) to subscribe to real-time price updates from the StellarFlow Oracle without polling. This implements the standardized callback pattern for Soroban smart contracts.

## Architecture

### Design Goals

1. **Event-Driven**: Contracts react to price changes immediately without polling
2. **Standardized**: All subscriber contracts implement the same `on_price_update` interface
3. **Efficient**: Gas-optimized callback invocation
4. **Resilient**: One failed callback doesn't block price updates
5. **Scalable**: Support for multiple concurrent subscribers

## Implementation Details

### Core Components

#### 1. **Types** (`types.rs`)

**`PriceUpdatePayload`**: The data structure passed to subscriber callbacks

```rust
#[contracttype]
pub struct PriceUpdatePayload {
    pub asset: Symbol,              // Asset symbol (e.g., NGN, KES, GHS)
    pub price: i128,                // Price value (normalized to 9 decimals)
    pub timestamp: u64,             // Update timestamp
    pub provider: Address,          // Provider who submitted the price
    pub decimals: u32,              // Always 9 for normalized prices
    pub confidence_score: u32,      // Confidence (0-100)
}
```

**`DataKey::PriceUpdateSubscribers`**: Storage key for the subscriber list

#### 2. **Callbacks Module** (`callbacks.rs`)

Core subscription management functions:

- **`subscribe(env, callback_contract)`**: Register a contract for callbacks
- **`unsubscribe(env, callback_contract)`**: Unregister a contract
- **`get_subscribers(env)`**: Retrieve all subscribers
- **`notify_subscribers(env, payload)`**: Invoke callbacks on all subscribers
- **`try_invoke_callback(env, callback_contract, payload)`**: Invoke a single callback

#### 3. **Contract Interface** (`lib.rs`)

Public entry points for subscription management:

```rust
pub fn subscribe_to_price_updates(env: Env, callback_contract: Address) -> Result<(), String>
pub fn unsubscribe_from_price_updates(env: Env, callback_contract: Address) -> Result<(), String>
pub fn get_price_update_subscribers(env: Env) -> soroban_sdk::Vec<Address>
```

#### 4. **Integration Points**

Price updates trigger callbacks in:
- **`update_price()`**: When authorized providers update prices
- **`set_price()`**: When admins set prices

## Usage Guide

### For Subscriber Contracts

#### Step 1: Implement the Callback Interface

Your contract must implement the `on_price_update` function:

```rust
#[contract]
pub struct MyLendingProtocol;

#[contractimpl]
impl MyLendingProtocol {
    /// Called by StellarFlow Oracle whenever a price is updated
    pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
        // Extract price information
        let asset = payload.asset;
        let price = payload.price;
        let timestamp = payload.timestamp;
        
        // Update internal state or trigger protocol actions
        // e.g., check for liquidation conditions, rebalance pools, etc.
        
        // Example: Liquidate undercollateralized positions
        if let Some(positions) = get_positions_for_asset(&env, asset.clone()) {
            for position in positions.iter() {
                if is_undercollateralized(&env, &position, price) {
                    trigger_liquidation(&env, &position);
                }
            }
        }
    }
}
```

#### Step 2: Subscribe to Price Updates

Call the oracle's subscription function:

```rust
// In your contract initialization or management function
pub fn subscribe_to_oracle(env: Env, oracle_address: Address) -> Result<(), String> {
    let oracle_client = PriceOracleClient::new(&env, &oracle_address);
    let my_contract = env.current_contract_address();
    
    oracle_client.subscribe_to_price_updates(&my_contract)
}
```

#### Step 3: Handle Callback Invocations

The oracle will automatically invoke your `on_price_update` function when prices change. Keep implementations lightweight:

```rust
pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
    // Store the latest price
    env.storage().instance().set(
        &DataKey::LastKnownPrice(payload.asset.clone()),
        &payload.price,
    );
    
    // Emit an event for monitoring
    env.events().publish((
        Symbol::new(&env, "price_received"),
    ), (
        payload.asset,
        payload.price,
        payload.timestamp,
    ));
}
```

### For Oracle Administrators

#### Subscribe a Contract

```rust
let lending_protocol = Address::from_contract_id(&env, "lending_contract_id");
oracle_client.subscribe_to_price_updates(&lending_protocol)?;
```

#### Unsubscribe a Contract

```rust
oracle_client.unsubscribe_from_price_updates(&lending_protocol)?;
```

#### View All Subscribers

```rust
let subscribers = oracle_client.get_price_update_subscribers();
for subscriber in subscribers.iter() {
    println!("Subscriber: {}", subscriber);
}
```

## Error Handling

### Subscription Errors

- **"Contract is already subscribed"**: Attempt to subscribe an already-registered contract
- **"Contract not found in subscribers"**: Attempt to unsubscribe a non-subscribed contract

### Callback Invocation

Callbacks are invoked with best-effort semantics:
- If a callback fails, an error is logged internally but processing continues
- Other subscribers receive their callbacks normally
- The price update itself always succeeds

## Gas Considerations

### Cost Breakdown

1. **Subscription**: O(n) where n = current subscriber count
   - Small fixed cost for append/remove operations

2. **Callback Invocation**: O(n * m) where:
   - n = subscriber count
   - m = average callback implementation complexity

### Recommendations

- **Max Subscribers**: No hard limit, but recommend ≤ 10 for production
- **Callback Complexity**: Keep implementations simple and non-blocking
- **Monitor Gas Usage**: Track callback invocation costs in tests

### Gas Optimization Tips

1. **Batch Updates**: Update your protocol state in batches rather than per-callback
2. **Lazy Evaluation**: Only process relevant assets
3. **Storage Efficiency**: Use instance storage for frequently-accessed data

```rust
pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
    // Only process assets relevant to your protocol
    if !is_tracked_asset(&env, &payload.asset) {
        return; // Early exit saves gas
    }
    
    // Use instance storage for quick access
    let instance = env.storage().instance();
    instance.set(&DataKey::LastPrice(payload.asset), &payload.price);
}
```

## Security Considerations

### Contract Validation

1. **Verify Oracle**: Always verify the oracle contract address before implementing callbacks
   ```rust
   const ORACLE_ADDRESS: &str = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4";
   
   pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
       let caller = env.invoker();
       assert_eq!(caller, Address::from_contract_id(&env, ORACLE_ADDRESS));
       // ... process update
   }
   ```

2. **Validate Payload**: Check payload integrity
   ```rust
   pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
       // Verify timestamp is reasonable
       let now = env.ledger().timestamp();
       assert!(payload.timestamp <= now);
       assert!(payload.timestamp > now - 300); // Not older than 5 mins
       
       // Verify price is positive
       assert!(payload.price > 0);
   }
   ```

### Callback Guarantees

⚠️ **Important**: The oracle provides **non-blocking** callback semantics:
- Callbacks execute in the same transaction as the price update
- If your callback panics, the transaction fails but price update already occurred
- Design callbacks to be robust against invalid inputs

## Testing

### Unit Tests

The callback module includes built-in tests:

```rust
#[test]
fn test_subscribe_to_price_updates() {
    let (env, _contract_id, client) = setup();
    let callback_contract = Address::generate(&env);
    
    let result = client.subscribe_to_price_updates(&callback_contract);
    assert_eq!(result, Ok(()));
}

#[test]
fn test_update_price_with_subscribers() {
    let env = Env::default();
    env.mock_all_auths();
    
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    
    let subscriber = Address::generate(&env);
    client.subscribe_to_price_updates(&subscriber).unwrap();
    
    // Price update should succeed even with subscribers
    client.set_price(&symbol_short!("NGN"), &1_500_000, &6, &3600);
}
```

### Integration Tests

Create a mock subscriber contract to test callbacks:

```rust
#[contract]
pub struct MockSubscriber;

#[contractimpl]
impl MockSubscriber {
    pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
        // Record that callback was received
        env.storage().instance().set(
            &Symbol::new(&env, "last_callback"),
            &payload,
        );
    }
}

#[test]
fn test_callback_receives_correct_data() {
    let env = Env::default();
    env.mock_all_auths();
    
    let oracle_id = env.register(PriceOracle, ());
    let subscriber_id = env.register(MockSubscriber, ());
    
    let oracle_client = PriceOracleClient::new(&env, &oracle_id);
    let subscriber = Address::from_contract_id(&env, &subscriber_id);
    
    // Subscribe
    oracle_client.subscribe_to_price_updates(&subscriber).unwrap();
    
    // Update price
    oracle_client.set_price(&symbol_short!("NGN"), &1_500_000, &6, &3600);
    
    // Verify callback received data
    env.as_contract(&subscriber_id, || {
        let last_callback: PriceUpdatePayload = env.storage()
            .instance()
            .get(&Symbol::new(&env, "last_callback"))
            .unwrap();
        
        assert_eq!(last_callback.asset, symbol_short!("NGN"));
        assert_eq!(last_callback.price, 1_500_000);
    });
}
```

## API Reference

### Oracle Functions

#### `subscribe_to_price_updates`

```rust
pub fn subscribe_to_price_updates(
    env: Env,
    callback_contract: Address
) -> Result<(), String>
```

**Parameters:**
- `callback_contract`: Address of the contract to subscribe

**Returns:**
- `Ok(())`: Subscription successful
- `Err(String)`: Subscription failed (contract already subscribed)

**Errors:**
- "Contract is already subscribed"

---

#### `unsubscribe_from_price_updates`

```rust
pub fn unsubscribe_from_price_updates(
    env: Env,
    callback_contract: Address
) -> Result<(), String>
```

**Parameters:**
- `callback_contract`: Address of the contract to unsubscribe

**Returns:**
- `Ok(())`: Unsubscription successful
- `Err(String)`: Unsubscription failed (contract not found)

**Errors:**
- "Contract not found in subscribers"

---

#### `get_price_update_subscribers`

```rust
pub fn get_price_update_subscribers(env: Env) -> soroban_sdk::Vec<Address>
```

**Returns:** Vector of all currently subscribed contract addresses

---

### Subscriber Interface

#### `on_price_update` (Required)

```rust
pub fn on_price_update(env: Env, payload: PriceUpdatePayload)
```

**Parameters:**
- `payload`: Contains asset, price, timestamp, provider, and confidence data

**Execution Context:**
- Invoked synchronously after price update
- In same transaction as price update
- May fail without affecting price storage

## Examples

### Example 1: Lending Protocol

```rust
#[contract]
pub struct LendingPool;

#[contractimpl]
impl LendingPool {
    pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
        // Check for liquidation opportunities
        let asset = payload.asset;
        let new_price = payload.price;
        
        // Get all borrowing positions using this asset as collateral
        let positions = get_borrowed_positions(&env, &asset);
        
        for position in positions.iter() {
            let collateral_value = position.collateral_amount * new_price;
            let required_collateral = calculate_required_collateral(&env, &position);
            
            if collateral_value < required_collateral {
                // Trigger liquidation
                env.events().publish((
                    Symbol::new(&env, "liquidation_triggered"),
                ), (
                    position.borrower.clone(),
                    asset.clone(),
                    new_price,
                ));
            }
        }
    }
}
```

### Example 2: DEX Protocol

```rust
#[contract]
pub struct AutomatedMarketMaker;

#[contractimpl]
impl AutomatedMarketMaker {
    pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
        // Rebalance liquidity pools based on price change
        let asset = payload.asset;
        let new_price = payload.price;
        
        // Get pool data
        if let Some(mut pool) = get_pool(&env, &asset) {
            // Calculate price impact
            let old_price = pool.last_known_price;
            let price_change_pct = ((new_price - old_price) * 10000) / old_price;
            
            // Rebalance if price moved more than 2%
            if price_change_pct.abs() > 200 {
                rebalance_pool(&env, &mut pool, new_price);
            }
            
            pool.last_known_price = new_price;
            pool.last_update = env.ledger().timestamp();
            set_pool(&env, &asset, &pool);
        }
    }
}
```

## Migration Guide

### From Polling to Callbacks

**Before (Polling):**
```rust
// Contracts had to check price periodically
#[contractimpl]
impl MyProtocol {
    pub fn check_prices(env: Env) {
        let oracle = PriceOracleClient::new(&env, &oracle_address);
        let ngn_price = oracle.get_price(&symbol_short!("NGN"), &true).unwrap();
        update_internal_price(&env, ngn_price);
    }
}
```

**After (Callbacks):**
```rust
// Oracle pushes updates automatically
#[contractimpl]
impl MyProtocol {
    pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
        update_internal_price(&env, payload.price);
    }
}
```

## Changelog

### Version 1.0.0 (Current)

- ✅ Standard callback interface (`on_price_update`)
- ✅ Subscription management functions
- ✅ Multi-subscriber support
- ✅ Integration with `update_price` and `set_price`
- ✅ Comprehensive test suite
- ✅ Gas-optimized implementations

## Future Enhancements

- **Filtering**: Subscribe to specific assets only
- **Pagination**: Support for large subscriber lists
- **Batching**: Group multiple price updates into single callback
- **Priority**: VIP subscribers with guaranteed execution
- **Rate Limiting**: Maximum callback frequency per subscriber

## Support

For questions or issues:
1. Check the [Integration Guide](INTEGRATION.md)
2. Review test cases in [test.rs](contracts/price-oracle/src/test.rs)
3. Consult the [main README](README.md)
