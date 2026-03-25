#![no_std]
use soroban_sdk::{contract, contracterror, contractimpl, contracttype, symbol_short, Env, Symbol};
use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, symbol_short, Address, Env, Symbol,
};

/// Error types for the price oracle contract
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    /// Asset does not exist in the price oracle
    AssetNotFound = 1,
    /// Unauthorized caller - not a whitelisted provider
    Unauthorized = 2,
}

/// Price data structure containing price information for an asset
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceData {
    /// The asset symbol (e.g., "XLM", "BTC")
    pub asset: Symbol,
    /// The price value (stored as scaled integer, e.g., 1000000 = 1.00 USD)
    pub price: i128,
    /// Timestamp when the price was last updated
    pub timestamp: u64,
}

/// Event emitted when a price is updated
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceUpdated {
    pub source: Address,
    pub asset: Symbol,
    pub price: i128,
    pub timestamp: u64,
}

/// Storage key for the price data map
const PRICE_DATA_KEY: Symbol = symbol_short!("PRICES");

#[contract]
pub struct PriceOracle;

#[contractimpl]
impl PriceOracle {
    /// Get the price data for a specific asset
    ///
    /// # Arguments
    /// * `env` - The contract environment
    /// * `asset` - The asset symbol to look up
    ///
    /// # Returns
    /// * `Ok(PriceData)` - The price data for the asset
    /// * `Err(Error::AssetNotFound)` - If the asset doesn't exist
    pub fn get_price(env: Env, asset: Symbol) -> Result<PriceData, Error> {
        // Get the persistent storage instance
        let storage = env.storage().persistent();

        // Try to retrieve the price data map
        let prices: soroban_sdk::Map<Symbol, PriceData> = storage
            .get(&PRICE_DATA_KEY)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));

        // Try to get the price for the specified asset
        match prices.get(asset) {
            Some(price_data) => Ok(price_data),
            None => Err(Error::AssetNotFound),
        }
    }

    /// Returns None instead of an error when asset is not found — safe for frontend callers.
    pub fn get_price_safe(env: Env, asset: Symbol) -> Option<PriceData> {
        let prices: soroban_sdk::Map<Symbol, PriceData> = env
            .storage()
            .persistent()
            .get(&PRICE_DATA_KEY)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));
        prices.get(asset)
    }

    /// Returns a Vec of all currently tracked asset symbols.
    pub fn get_all_assets(env: Env) -> soroban_sdk::Vec<Symbol> {
        let prices: soroban_sdk::Map<Symbol, PriceData> = env
            .storage()
            .persistent()
            .get(&PRICE_DATA_KEY)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));
        prices.keys()
    }

    /// Set the price data for a specific asset (admin function)
    ///
    /// # Arguments
    /// * `env` - The contract environment
    /// * `asset` - The asset symbol
    /// * `val` - The price value to store
    pub fn set_price(env: Env, asset: Symbol, val: i128) {
        // Get the persistent storage instance
        let storage = env.storage().persistent();

        // Get existing prices or create new map
        let mut prices: soroban_sdk::Map<Symbol, PriceData> = storage
            .get(&PRICE_DATA_KEY)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));

        let price_data = PriceData {
            asset: asset.clone(),
            price: val,
            timestamp: env.ledger().timestamp(),
        };

        // Set the price for the asset
        prices.set(asset, price_data);

        // Store the updated map
        storage.set(&PRICE_DATA_KEY, &prices);
    }

    /// Update the price for a specific asset (authorized backend relayer function)
    ///
    /// # Arguments
    /// * `env` - The contract environment
    /// * `source` - The address of the authorized backend relayer
    /// * `asset` - The asset symbol to update
    /// * `price` - The new price (as i128)
    pub fn update_price(env: Env, source: Address, asset: Symbol, price: i128) {
        // Check if the source is a whitelisted provider
        if !crate::auth::_is_provider(&env, &source) {
            panic!("Unauthorised: caller is not a whitelisted provider");
        }

        // Require authentication from the source address
        source.require_auth();

        // Get the storage instance
        let storage = env.storage().instance();

        // Get existing prices or create new map
        let mut prices: soroban_sdk::Map<Symbol, PriceData> = storage
            .get(&PRICE_DATA_KEY)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));

        // Get current timestamp
        let timestamp = env.ledger().timestamp();

        // Create new price data
        let price_data = PriceData {
            asset: asset.clone(),
            price: price as u64, // Convert i128 to u64 for storage
            timestamp,
            source: source.clone(),
        };

        // Update the price for the asset
        prices.set(asset.clone(), price_data);

        // Store the updated map
        storage.set(&PRICE_DATA_KEY, &prices);

        // Emit the PriceUpdated event
        PriceUpdated {
            source: source.clone(),
            asset: asset.clone(),
            price,
            timestamp,
        }.publish(&env);
    }
}

mod auth;
mod median;
mod test;
