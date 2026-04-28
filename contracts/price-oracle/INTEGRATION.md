# StellarFlow Oracle Integration Guide

## Overview

The StellarFlow Oracle provides a standardized Rust trait (`StellarFlowTrait`) that allows other Soroban contracts to query price data using a clean, gas-optimized interface.

## Using the Oracle in Your Contract

### 1. Add the Oracle as a Dependency

Add the StellarFlow Oracle to your `Cargo.toml`:

```toml
[dependencies]
stellarflow-oracle = { path = "../price-oracle" }
```

### 2. Import the Client

The `#[contractclient]` attribute automatically generates a `StellarFlowClient` that you can use:

```rust
use soroban_sdk::{contract, contractimpl, Env, Symbol, Address};
use stellarflow_oracle::StellarFlowClient;

#[contract]
pub struct MyContract;

#[contractimpl]
impl MyContract {
    pub fn get_ngn_price(env: Env, oracle_address: Address) -> i128 {
        let oracle = StellarFlowClient::new(&env, &oracle_address);
        
        // Get the latest NGN price
        let price = oracle.get_last_price(&Symbol::new(&env, "NGN"))
            .expect("Failed to get NGN price");
        
        price
    }
}
```

### 3. Available Methods

The `StellarFlowClient` provides the following methods:

#### `get_price(asset: Symbol, verified: bool) -> Result<PriceData, Error>`
Get the full price data for a specific asset. When `verified` is `true`, reads from the verified price bucket.

#### `get_last_price(asset: Symbol) -> Result<i128, Error>`
Get just the price value as an i128. **This is the most gas-efficient method** when you only need the price.

#### `get_price_safe(asset: Symbol) -> Option<PriceData>`
Get the price data without throwing an error if the asset is not found or stale.

#### `get_prices(assets: Vec<Symbol>) -> Vec<Option<PriceEntry>>`
Get prices for multiple assets in a single call.

#### `get_all_assets() -> Vec<Symbol>`
Get all currently tracked asset symbols.

#### `get_asset_count() -> u32`
Get the total number of tracked assets.

### 4. Example: DeFi Lending Protocol

```rust
use soroban_sdk::{contract, contractimpl, Env, Symbol, Address};
use stellarflow_oracle::StellarFlowClient;

#[contract]
pub struct LendingProtocol;

#[contractimpl]
impl LendingProtocol {
    pub fn calculate_collateral_value(
        env: Env,
        oracle_address: Address,
        collateral_asset: Symbol,
        collateral_amount: i128,
    ) -> i128 {
        let oracle = StellarFlowClient::new(&env, &oracle_address);
        
        // Get the current price of the collateral asset
        let price = oracle.get_last_price(&collateral_asset)
            .expect("Collateral asset not found");
        
        // Calculate total collateral value
        collateral_amount * price / 1_000_000 // Assuming 6 decimals
    }
    
    pub fn check_liquidation(
        env: Env,
        oracle_address: Address,
        debt_asset: Symbol,
        collateral_asset: Symbol,
        debt_amount: i128,
        collateral_amount: i128,
    ) -> bool {
        let oracle = StellarFlowClient::new(&env, &oracle_address);
        
        // Get both prices in a single call for efficiency
        let assets = soroban_sdk::vec![&env, debt_asset, collateral_asset];
        let prices = oracle.get_prices(&assets);
        
        let debt_price = prices.get(0).unwrap().unwrap().price;
        let collateral_price = prices.get(1).unwrap().unwrap().price;
        
        let debt_value = debt_amount * debt_price;
        let collateral_value = collateral_amount * collateral_price;
        
        // Liquidate if collateral value < 150% of debt value
        collateral_value < (debt_value * 150 / 100)
    }
}
```

## Gas Optimization Tips

1. **Use `get_last_price`** when you only need the price value (not timestamp, decimals, etc.)
2. **Use `get_prices`** for batch queries instead of multiple `get_price` calls
3. **Use `get_price_safe`** when you want to handle missing prices gracefully without error handling overhead

## Error Handling

The Oracle returns the following errors:

- `Error::AssetNotFound` - Asset does not exist or price is stale
- `Error::Unauthorized` - Caller is not authorized (for admin functions)
- `Error::InvalidAssetSymbol` - Asset symbol is not in the approved list

## Oracle Snapshot Events (For Indexers/Subgraphs)

The StellarFlow Oracle emits special `OracleSnapshot` events every 100 ledgers to help subgraphs and indexers track price history efficiently.

### What is an OracleSnapshot Event?

Every 100 ledgers (approximately every 8-10 minutes on Stellar), the oracle emits a checkpointed snapshot containing:
- **Ledger Sequence**: The ledger number where the snapshot was taken
- **Timestamp**: When the snapshot was emitted
- **All Current Prices**: A complete list of current verified prices for all tracked assets

### Why Use Snapshots?

**Checkpointed State**: Snapshots provide periodic "known-good" states that indexers can use as synchronization points instead of replaying every single price update.

**Efficient History Tracking**: Off-chain databases can use snapshots to verify their state is consistent with the oracle, filling in any gaps that may have occurred.

**Reduced Data Load**: Rather than processing every price update, indexers can sync against snapshots for faster, more reliable state management.

### Listening to OracleSnapshot Events

You can listen to these events using any Stellar event indexer:

```rust
// Example: listening for OracleSnapshot events
// In your indexer code:

#[soroban_sdk::contractevent]
pub struct OracleSnapshotEvent {
    pub ledger_sequence: u32,
    pub timestamp: u64,
    pub prices: Vec<PriceEntry>,
}

// Process the snapshot event
fn handle_snapshot(event: OracleSnapshotEvent) {
    println!("Snapshot at ledger {}: {} prices", 
             event.ledger_sequence, 
             event.prices.len());
    
    // Store or validate all current prices against your database
    for price_entry in event.prices {
        validate_and_store_price(&price_entry);
    }
}
```

### Snapshot Frequency

- **Every 100 ledgers**: Snapshots are emitted at ledger boundaries divisible by 100
- **Ledger 100, 200, 300**, etc.
- **On Stellar**: Approximately every 8-10 minutes (depends on network speed)

### Example: Using Snapshots for Sync Points

```sql
-- In your off-chain database
CREATE TABLE price_snapshots (
    ledger_sequence INTEGER PRIMARY KEY,
    timestamp BIGINT,
    data JSONB  -- All prices at this snapshot
);

-- When receiving a snapshot event:
INSERT INTO price_snapshots (ledger_sequence, timestamp, data)
VALUES ($1, $2, $3);

-- To verify current state:
SELECT * FROM price_snapshots 
WHERE ledger_sequence <= current_ledger 
ORDER BY ledger_sequence DESC 
LIMIT 1;  -- Get most recent snapshot
```

## Supported Assets

The Oracle currently supports the following African fiat currencies:
- NGN (Nigerian Naira)
- KES (Kenyan Shilling)
- GHS (Ghanaian Cedi)

Check the current list with `get_all_assets()`.

## Contract Address

Deploy your own Oracle instance or use the official StellarFlow Oracle address:
- **Testnet**: `[TBD]`
- **Mainnet**: `[TBD]`

## Support

For issues or questions, please open an issue on the [StellarFlow GitHub repository](https://github.com/dev-fatima-24/stellarflow-contracts).
