# Proxy Pattern Quick Start Guide

## What is the Proxy Pattern?

The Proxy Pattern separates **storage** from **logic** in smart contracts:
- **Storage Contract**: Immutable, handles all data persistence
- **Logic Contract**: Upgradeable, contains business logic

This makes audits easier because storage is frozen and logic can be verified independently.

## Project Structure

```
contracts/
├── price-oracle/              # Logic contract (upgradeable)
│   └── src/
│       ├── lib.rs            # Main contract logic
│       ├── auth.rs           # Authorization
│       ├── callbacks.rs       # Callback system
│       ├── types.rs          # Data types
│       └── test.rs           # Tests
│
└── price-oracle-storage/      # Storage contract (immutable)
    └── src/
        ├── lib.rs            # Storage operations
        └── types.rs          # Storage keys & types
```

## Building the Storage Contract

```bash
cd contracts/price-oracle-storage
cargo build --target wasm32-unknown-unknown --release
```

## Storage Contract API

### Admin Operations
```rust
storage_client.set_admin(&admin);
let admin = storage_client.get_admin()?;
let is_admin = storage_client.is_admin(&address);
```

### Price Operations
```rust
// Verified prices (used by internal logic)
storage_client.set_verified_price(&asset, &price_data);
let price = storage_client.get_verified_price(&asset)?;

// Community prices (user-submitted)
storage_client.set_community_price(&asset, &price_data);
let price = storage_client.get_community_price(&asset)?;
```

### Asset Management
```rust
storage_client.add_asset(&asset)?;
let assets = storage_client.get_all_assets();
let count = storage_client.get_asset_count();
```

### Subscriber Management
```rust
storage_client.subscribe(&callback_contract)?;
storage_client.unsubscribe(&callback_contract)?;
let subscribers = storage_client.get_subscribers();
```

### Initialization
```rust
storage_client.initialize(&admin)?;
let initialized = storage_client.is_initialized();
```

## Using Storage Contract in Logic Contract

### 1. Import the Storage Client
```rust
use price_oracle_storage::PriceOracleStorageClient;
```

### 2. Create a Client Instance
```rust
let storage_address = Address::from_contract_id(&env, &storage_contract_id);
let storage_client = PriceOracleStorageClient::new(&env, &storage_address);
```

### 3. Call Storage Operations
```rust
// Get admin
let admin = storage_client.get_admin()?;

// Update price
storage_client.set_verified_price(&asset, &price_data);

// Get subscribers
let subscribers = storage_client.get_subscribers();
```

## Error Handling

Storage operations return `Result<T, Error>`:

```rust
pub enum Error {
    NotFound = 1,           // Key not found in storage
    AlreadyExists = 2,      // Duplicate entry
    Unauthorized = 3,       // Access denied
    InvalidInput = 4,       // Invalid input data
}
```

Example:
```rust
match storage_client.get_admin() {
    Ok(admin) => { /* use admin */ },
    Err(Error::NotFound) => { /* handle missing admin */ },
    Err(e) => { /* handle other errors */ },
}
```

## Storage Types

### Instance Storage (Fast, Limited)
- Admin address
- Initialization flag
- Pause state

### Persistent Storage (Slower, Unlimited)
- Asset list
- Asset metadata
- Subscriber list

### Temporary Storage (Cheapest, TTL-based)
- Verified prices
- Community prices
- TWAP buffers

## Testing

### Unit Tests for Storage Contract
```bash
cd contracts/price-oracle-storage
cargo test
```

### Integration Tests
```bash
cd contracts/price-oracle
cargo test
```

## Deployment Steps

1. **Deploy Storage Contract**
   ```bash
   soroban contract deploy --wasm contracts/price-oracle-storage/target/wasm32-unknown-unknown/release/price_oracle_storage.wasm
   ```

2. **Initialize Storage**
   ```bash
   soroban contract invoke --id <storage_id> -- initialize --admin <admin_address>
   ```

3. **Deploy Logic Contract**
   ```bash
   soroban contract deploy --wasm contracts/price-oracle/target/wasm32-unknown-unknown/release/price_oracle.wasm
   ```

4. **Configure Logic Contract**
   - Set storage contract address
   - Initialize with admin

## Key Differences from Original

| Aspect | Original | With Proxy |
|--------|----------|-----------|
| Storage | Direct access | Cross-contract calls |
| Upgrades | Full contract | Logic contract only |
| Auditing | Entire contract | Storage & logic separately |
| Gas Cost | Lower | Slightly higher (cross-contract calls) |
| Flexibility | Limited | Higher (can upgrade logic) |

## Common Patterns

### Reading a Price
```rust
let storage_client = PriceOracleStorageClient::new(&env, &storage_address);
match storage_client.get_verified_price(&asset) {
    Ok(price) => { /* use price */ },
    Err(Error::NotFound) => { /* price not available */ },
    Err(e) => { /* handle error */ },
}
```

### Updating a Price
```rust
let price_data = PriceData {
    price: 100_000_000,
    timestamp: env.ledger().timestamp(),
    provider: caller.clone(),
    decimals: 9,
};
storage_client.set_verified_price(&asset, &price_data);
```

### Managing Subscribers
```rust
// Subscribe
storage_client.subscribe(&callback_contract)?;

// Get all subscribers
let subscribers = storage_client.get_subscribers();

// Unsubscribe
storage_client.unsubscribe(&callback_contract)?;
```

## Troubleshooting

### "Storage contract not initialized"
- Call `initialize()` with admin address first

### "Cross-contract call failed"
- Verify storage contract address is correct
- Check storage contract is deployed
- Verify caller has necessary permissions

### "Price not found"
- Asset may not be added yet
- Price may have expired (temporary storage TTL)
- Check asset symbol is correct

## Next Steps

1. Review `PROXY_PATTERN_IMPLEMENTATION.md` for detailed architecture
2. Build and test the storage contract
3. Refactor logic contract to use storage client
4. Run integration tests
5. Deploy and verify on testnet

## Resources

- Storage Contract: `contracts/price-oracle-storage/`
- Documentation: `PROXY_PATTERN_IMPLEMENTATION.md`
- Original Contract: `contracts/price-oracle/`
- Tests: `contracts/price-oracle/src/test.rs`
