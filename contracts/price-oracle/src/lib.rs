#![no_std]

use soroban_sdk::{contract, contracterror, contractevent, contractimpl, Address, Env, Symbol};

use crate::types::{DataKey, PriceData};

const PRICE_DATA_KEY: Symbol = symbol_short!("prices");

/// Error types for the price oracle contract
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    /// Asset does not exist in the price oracle.
    AssetNotFound = 1,
    /// Unauthorized caller - not a whitelisted provider.
    Unauthorized = 2,
    /// Asset symbol is not in the approved list (NGN, KES, GHS)
    InvalidAssetSymbol = 3,
}

/// Event emitted when a price is updated
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceUpdated {
    pub asset: Symbol,
    pub new_price: i128,
    pub old_price: i128,
    pub provider_address: Address,
}

/// Event emitted when the admin address is changed
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminChanged {
    pub previous_admin: Option<Address>,
    pub new_admin: Address,
}

#[contract]
pub struct PriceOracle;

/// Returns the signed percentage change in basis points.
///
/// Example: 1_000_000 -> 1_200_000 returns 2_000 (20.00%).
/// Example: 1_000_000 -> 800_000 returns -2_000 (-20.00%).
/// Returns `None` when `old_price` is zero because the percentage change is undefined.
pub fn calculate_percentage_change_bps(old_price: i128, new_price: i128) -> Option<i128> {
    if old_price == 0 {
        return None;
    }

    let delta = new_price.checked_sub(old_price)?;
    let scaled = delta.checked_mul(10_000)?;
    scaled.checked_div(old_price)
}

/// Returns the absolute percentage difference in basis points.
///
/// This is convenient for flash-crash or spike detection because the caller can
/// compare the result directly against a threshold without worrying about direction.
pub fn calculate_percentage_difference_bps(old_price: i128, new_price: i128) -> Option<i128> {
    calculate_percentage_change_bps(old_price, new_price).map(i128::abs)
}

#[contractimpl]
impl PriceOracle {
    /// Initialize the contract with admin and base currency pairs.
    /// Can only be called once.
    pub fn initialize(env: Env, admin: Address, base_currency_pairs: soroban_sdk::Vec<Symbol>) {
        // Prevent double initialization
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("Contract already initialized");
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::BaseCurrencyPairs, &base_currency_pairs);
    }
    /// Get the price data for a specific asset.
    pub fn get_price(env: Env, asset: Symbol) -> Result<PriceData, Error> {
        let storage = env.storage().persistent();
        let prices: soroban_sdk::Map<Symbol, PriceData> = storage
            .get(&DataKey::PriceData)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));

        match prices.get(asset) {
            Some(price_data) => Ok(price_data),
            None => Err(Error::AssetNotFound),
        }
    }

    /// Returns `None` instead of an error when the asset is not found.
    pub fn get_price_safe(env: Env, asset: Symbol) -> Option<PriceData> {
        let prices: soroban_sdk::Map<Symbol, PriceData> = env
            .storage()
            .persistent()
            .get(&DataKey::PriceData)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));
        prices.get(asset)
    }

    /// Get the most recent price for a specific asset.
    ///
    /// Returns the price value as an i128, or an error if the asset is not found.
    pub fn get_last_price(env: Env, asset: Symbol) -> Result<i128, Error> {
        let price_data = Self::get_price(env, asset)?;
        Ok(price_data.price)
    }

    /// Returns a vector of all currently tracked asset symbols.
    pub fn get_all_assets(env: Env) -> soroban_sdk::Vec<Symbol> {
        let prices: soroban_sdk::Map<Symbol, PriceData> = env
            .storage()
            .persistent()
            .get(&DataKey::PriceData)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));
        prices.keys()
    }

    /// Set the price data for a specific asset.
    pub fn set_price(env: Env, asset: Symbol, val: i128, decimals: u32) {
        let storage = env.storage().persistent();
        let mut prices: soroban_sdk::Map<Symbol, PriceData> = storage
            .get(&DataKey::PriceData)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));

        let price_data = PriceData {
            price: val,
            timestamp: env.ledger().timestamp(),
            provider: env.current_contract_address(),
            decimals,
        };

        prices.set(asset, price_data);
        storage.set(&DataKey::PriceData, &prices);
    }

    /// Update the price for a specific asset (authorized backend relayer function)
    ///
    /// # Arguments
    /// * `env` - The contract environment
    /// * `source` - The address of the authorized backend relayer
    /// * `asset` - The asset symbol to update
    /// * `price` - The new price (as i128)
    ///
    /// # Errors
    /// * `Error::InvalidAssetSymbol` - If `asset` is not NGN, KES, or GHS
    ///
    /// # Panics
    /// If `source` is not a whitelisted provider or if the contract is paused.
    pub fn update_price(
        env: Env,
        source: Address,
        asset: Symbol,
        price: i128,
        decimals: u32,
    ) -> Result<(), Error> {
        source.require_auth();

        if !asset_symbol::is_approved_asset_symbol(asset.clone()) {
            return Err(Error::InvalidAssetSymbol);
        }

        if !crate::auth::_is_provider(&env, &source) {
            panic!("Unauthorised: caller is not a whitelisted provider");
        }

        let storage = env.storage().persistent();
        let mut prices: soroban_sdk::Map<Symbol, PriceData> = storage
            .get(&DataKey::PriceData)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));

        let old_price = prices
            .get(asset.clone())
            .map(|existing_price| existing_price.price)
            .unwrap_or(0);

        let timestamp = env.ledger().timestamp();
        let price_data = PriceData {
            price,
            timestamp,
            provider: source.clone(),
            decimals,
        };

        prices.set(asset.clone(), price_data);
        storage.set(&DataKey::PriceData, &prices);

        PriceUpdated {
            source,
            asset,
            price,
            timestamp,
        }
        .publish(&env);
        Ok(())
    }
}

mod asset_symbol;
mod auth;
pub mod math;
mod median;
mod test;
mod types;
