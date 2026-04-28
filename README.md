# StellarFlow Contracts - Time-Locked Upgrade Implementation

This repository contains smart contracts for the StellarFlow Network with a time-locked upgrade mechanism to prevent "flash-upgrades" by enforcing a 48-hour delay between contract upgrade proposals and execution.

## Features

- **Time-Locked Upgrades**: 48-hour mandatory delay between upgrade proposal and execution
- **Pending State Management**: Secure storage of new WASM hash in pending state
- **Timestamp Validation**: Uses `ledger().timestamp()` for accurate time validation
- **Admin-Only Operations**: Only contract administrators can propose and execute upgrades
- **Upgrade Cancellation**: Ability to cancel pending upgrades before execution
- **Timelock Monitoring**: Functions to check remaining timelock time
- **Batch Price Updates**: Efficient multi-asset price updates in a single transaction
- **Fee Optimization**: Reduces submission fees by batching 5+ asset price updates

## Architecture

### Core Components

1. **PendingUpgrade Struct**: Stores information about pending upgrades
   - `new_wasm_hash`: The hash of the new contract code
   - `proposed_at`: Timestamp when the upgrade was proposed
   - `proposer`: Address of who proposed the upgrade

2. **ContractData Struct**: Stores contract state
   - `admin`: Administrator address with upgrade permissions
   - `value`: Sample storage value for testing

3. **AssetPrice Struct**: Stores individual asset price information
   - `asset_code`: Symbol representing the asset (e.g., "BTC", "ETH")
   - `price`: Current price of the asset
   - `timestamp`: When the price was last updated

4. **PriceUpdate Struct**: Input structure for price updates
   - `asset_code`: Symbol representing the asset
   - `price`: New price to set

### Key Functions

#### Upgrade Management
- `initialize()`: Sets up the contract with an admin address
- `propose_upgrade()`: Initiates the 48-hour timelock period
- `execute_upgrade()`: Executes the upgrade after timelock expires
- `cancel_upgrade()`: Cancels a pending upgrade
- `get_pending_upgrade()`: Retrieves pending upgrade information
- `get_upgrade_timelock_remaining()`: Returns remaining timelock time

#### Price Management
- `update_prices_batch()`: Updates prices for 5+ assets in a single transaction
- `get_price()`: Retrieves the current price for a specific asset
- `get_all_prices()`: Returns all current asset prices

## Security Features

### Flash Upgrade Prevention

The contract prevents flash upgrades through:

1. **48-Hour Timelock**: Mandatory delay between proposal and execution
2. **Pending State Storage**: New WASM hash stored in pending state until timelock expires
3. **Timestamp Validation**: Uses Stellar ledger timestamp for accurate time measurement
4. **Authorization Checks**: Only admin can propose/execute upgrades

### Access Control

- **Admin-Only Operations**: Critical functions require admin authorization
- **Proposal Tracking**: All proposals are tracked with proposer identity
- **Cancellation Rights**: Admin can cancel pending upgrades

## Usage Example

### Upgrade Management
```rust
// Initialize contract
contract.initialize(&admin_address);

// Propose upgrade (starts 48-hour timelock)
let new_wasm_hash = BytesN::from_array(&env, &[1u8; 32]);
contract.propose_upgrade(&new_wasm_hash, &admin_address);

// Check timelock status
let remaining = contract.get_upgrade_timelock_remaining();
println!("Time remaining: {} seconds", remaining.unwrap());

// After 48 hours, execute upgrade
contract.execute_upgrade(&admin_address);
```

### Batch Price Updates
```rust
// Create price updates for 5+ assets
let mut price_updates = Vec::new(&env);
price_updates.push_back(PriceUpdate {
    asset_code: Symbol::short("BTC"),
    price: 50000,
});
price_updates.push_back(PriceUpdate {
    asset_code: Symbol::short("ETH"),
    price: 3000,
});
price_updates.push_back(PriceUpdate {
    asset_code: Symbol::short("USDC"),
    price: 1000,
});
price_updates.push_back(PriceUpdate {
    asset_code: Symbol::short("USDT"),
    price: 1000,
});
price_updates.push_back(PriceUpdate {
    asset_code: Symbol::short("XLM"),
    price: 100,
});

// Update all prices in a single transaction (saves on fees)
contract.update_prices_batch(&price_updates, &admin_address);

// Retrieve individual price
let btc_price = contract.get_price(&Symbol::short("BTC"));
println!("BTC price: {}", btc_price.unwrap().price);

// Get all prices
let all_prices = contract.get_all_prices();
println!("Total assets tracked: {}", all_prices.len());
```

## Testing

The contract includes comprehensive tests covering:

- Basic functionality and initialization
- Upgrade proposal and execution flow
- Timelock enforcement and countdown
- Unauthorized operation prevention
- Upgrade cancellation
- Batch price update functionality
- Minimum asset requirement enforcement (5+ assets)
- Price timestamp tracking
- Authorization for price updates

Run tests with:

```bash
cargo test
```

## Technical Requirements Met

✅ **Ledger Timestamp Validation**: Uses `ledger().timestamp()` for time validation  
✅ **Pending State Storage**: New WASM hash stored in pending state before commitment  
✅ **48-Hour Delay**: Enforced delay between proposal and execution  
✅ **Flash Upgrade Prevention**: Complete protection against immediate upgrades  
✅ **Batch Price Updates**: `update_prices_batch(Vec)` function implemented  
✅ **Multi-Asset Efficiency**: Supports 5+ asset updates in single transaction  
✅ **Fee Optimization**: Reduces submission fees through batching  

## Build and Deploy

```bash
# Build the contract
cargo build --target wasm32-unknown-unknown --release

# Deploy to Stellar network
stellar contract deploy --wasm target/wasm32-unknown-unknown/release/stellarflow_contracts.wasm
```

## License

This project is part of the StellarFlow Network ecosystem.
