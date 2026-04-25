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
