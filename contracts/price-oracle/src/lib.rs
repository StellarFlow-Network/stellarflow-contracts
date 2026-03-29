#![no_std]

use soroban_sdk::{contract, contractclient, contracterror, contractimpl, panic_with_error, Address, Env, Symbol};

use crate::types::{DataKey, PriceData};

/// A clean, gas-optimized interface for other Soroban contracts to fetch prices from StellarFlow.
///
/// The generated client from this trait is the intended cross-contract entrypoint for downstream
/// Soroban applications. The getters are read-only and `get_last_price` is the cheapest option
/// when callers only need the scalar price value.
#[contractclient(name = "StellarFlowClient")]
pub trait StellarFlowTrait {
    /// Get the full price data for a specific asset.
    ///
    /// Returns the complete price information including timestamp, decimals, confidence score, and TTL.
    /// Returns `Error::AssetNotFound` if the asset does not exist or the price is stale.
    fn get_price(env: Env, asset: Symbol) -> Result<PriceData, Error>;

    /// Get the price data for a specific asset, or `None` if not found.
    ///
    /// Unlike `get_price`, this does not error on stale or missing prices.
    /// Useful for contracts that want to gracefully handle missing data.
    fn get_price_safe(env: Env, asset: Symbol) -> Option<PriceData>;

    /// Get the most recent price value for a specific asset.
    ///
    /// Returns just the price value as an i128, without other metadata.
    /// This is the fastest getter for contracts that only need the price.
    fn get_last_price(env: Env, asset: Symbol) -> Result<i128, Error>;

    /// Get prices for a list of assets in a single call.
    ///
    /// Returns a `Vec<PriceEntry>` in the same order as the input symbols.
    /// Assets that are missing or stale are represented as `None` entries.
    fn get_prices(env: Env, assets: soroban_sdk::Vec<Symbol>) -> soroban_sdk::Vec<Option<crate::types::PriceEntry>>;

    /// Get all currently tracked asset symbols.
    ///
    /// Returns a vector of all assets that have prices stored in the contract.
    fn get_all_assets(env: Env) -> soroban_sdk::Vec<Symbol>;

    /// Get the current admin address.
    ///
    /// Returns the address of the contract administrator.
    fn get_admin(env: Env) -> Address;
}

/// Error types for the price oracle contract
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    /// Asset does not exist in the price oracle.
    AssetNotFound = 1,
    /// Unauthorized caller - not a whitelisted provider or admin.
    Unauthorized = 2,
    /// Asset symbol is not in the approved list (NGN, KES, GHS)
    InvalidAssetSymbol = 3,
    /// Price must be greater than zero.
    InvalidPrice = 4,
    /// Caller is not authorized to perform this action.
    NotAuthorized = 5,
    /// Contract or admin has already been initialized.
    AlreadyInitialized = 6,
    /// Price change exceeds the allowed delta limit in a single update.
    PriceDeltaExceeded = 7,
}

#[contract]
pub struct PriceOracle;

#[soroban_sdk::contractevent]
pub struct PriceUpdatedEvent {
    pub asset: Symbol,
    pub price: i128,
}

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

/// Returns the absolute difference between two price values.
///
/// Useful for circuit-breaker logic where the raw magnitude of the price move
/// must be compared against a hard threshold. The result is always non-negative.
///
/// Returns `None` only when the subtraction would overflow (practically impossible
/// for realistic price values).
///
/// # Examples
/// ```text
/// calculate_price_volatility(1_000_000, 1_200_000) => Some(200_000)
/// calculate_price_volatility(1_200_000, 1_000_000) => Some(200_000)
/// ```
pub fn calculate_price_volatility(old_price: i128, new_price: i128) -> Option<i128> {
    new_price
        .checked_sub(old_price)
        .map(|delta| delta.abs())
}

fn is_valid(price: i128) -> bool {
    price > 0
}

/// Check if a price entry is stale based on its TTL.
///
/// A price is considered stale if the current ledger timestamp has passed
/// the expiration time (stored_timestamp + ttl).
///
/// # Arguments
/// * `current_time` - The current ledger timestamp
/// * `stored_timestamp` - The timestamp when the price was stored
/// * `ttl` - The time-to-live in seconds
///
/// # Returns
/// `true` if the price is stale (expired), `false` otherwise
pub fn is_stale(current_time: u64, stored_timestamp: u64, ttl: u64) -> bool {
    current_time >= stored_timestamp.saturating_add(ttl)
}

#[contractimpl]
impl PriceOracle {
    /// Initialize the contract with admin and base currency pairs.
    /// Can only be called once.
    pub fn initialize(env: Env, admin: Address, base_currency_pairs: soroban_sdk::Vec<Symbol>) {
        // Prevent double initialization
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }

        #[allow(deprecated)]
        env.events().publish(
            (Symbol::new(&env, "AdminChanged"),),
            admin.clone(),
        );

        let admins = soroban_sdk::vec![&env, admin];
        crate::auth::_set_admin(&env, &admins);
        env.storage()
            .instance()
            .set(&DataKey::BaseCurrencyPairs, &base_currency_pairs);
    }

    pub fn init_admin(env: Env, admin: Address) {
        if crate::auth::_has_admin(&env) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }

        #[allow(deprecated)]
        env.events().publish(
            (Symbol::new(&env, "AdminChanged"),),
            admin.clone(),
        );

        let admins = soroban_sdk::vec![&env, admin];
        crate::auth::_set_admin(&env, &admins);
    }

    /// Return the current admin addresses.
    pub fn get_admin(env: Env) -> Address {
        crate::auth::_get_admin(&env)
            .get(0)
            .expect("No admin set")
    }

    /// Get the price data for a specific asset.
    /// Get the price data for a specific asset. Returns error if price is stale.
    pub fn get_price(env: Env, asset: Symbol) -> Result<PriceData, Error> {
        let storage = env.storage().persistent();
        let prices: soroban_sdk::Map<Symbol, PriceData> = storage
            .get(&DataKey::PriceData)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));

        match prices.get(asset) {
            Some(price_data) => {
                // Check if price is stale using per-asset TTL
                let now = env.ledger().timestamp();
                if is_stale(now, price_data.timestamp, price_data.ttl) {
                    return Err(Error::AssetNotFound); // Could define a new error for StalePrice
                }
                Ok(price_data)
            }
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

    /// Get prices for a batch of assets in a single call.
    ///
    /// Returns a `Vec<Option<PriceEntry>>` in the same order as `assets`.
    /// Each entry is `Some(PriceEntry)` when the asset exists and is not stale,
    /// or `None` when it is missing or stale — matching `get_price_safe` semantics.
    pub fn get_prices(
        env: Env,
        assets: soroban_sdk::Vec<Symbol>,
    ) -> soroban_sdk::Vec<Option<crate::types::PriceEntry>> {
        let prices: soroban_sdk::Map<Symbol, PriceData> = env
            .storage()
            .persistent()
            .get(&DataKey::PriceData)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));

        let now = env.ledger().timestamp();
        let mut result = soroban_sdk::Vec::new(&env);

        for asset in assets.iter() {
            let entry = prices.get(asset).and_then(|pd| {
                if is_stale(now, pd.timestamp, pd.ttl) {
                    None
                } else {
                    Some(crate::types::PriceEntry {
                        price: pd.price,
                        timestamp: pd.timestamp,
                    })
                }
            });
            result.push_back(entry);
        }

        result
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
    ///
    /// # Arguments
    /// * `env` - The contract environment
    /// * `asset` - The asset symbol to set
    /// * `val` - The price value
    /// * `decimals` - Number of decimals for the price
    /// * `ttl` - Time-to-live in seconds for this price (per-asset expiration)
    pub fn set_price(env: Env, asset: Symbol, val: i128, decimals: u32, ttl: u64) {
        let storage = env.storage().persistent();
        let mut prices: soroban_sdk::Map<Symbol, PriceData> = storage
            .get(&DataKey::PriceData)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));

        // For demo/testing, set confidence_score to 100. In production, this should be provided as an argument.
        let price_data = PriceData {
            price: val,
            timestamp: env.ledger().timestamp(),
            provider: env.current_contract_address(),
            decimals,
            confidence_score: 100,
            ttl,
        };

        prices.set(asset, price_data);
        storage.set(&DataKey::PriceData, &prices);
    }

    /// Upgrade the contract WASM code.
    ///
    /// Replaces the on-chain WASM bytecode with the provided hash while preserving
    /// all contract storage. Strictly restricted to the admin.
    ///
    /// # Arguments
    /// * `admin`    - The current admin address (must sign the transaction)
    /// * `new_wasm_hash` - The hash of the new WASM blob already uploaded to the ledger
    ///
    /// # Panics
    /// If `admin` is not the current contract admin.
    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: soroban_sdk::BytesN<32>) {
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);
        env.deployer().update_current_contract_wasm(new_wasm_hash);
    }

    /// Remove an asset from the oracle, deleting its price entry.
    ///
    /// Only the admin can call this. Returns `Error::AssetNotFound` if the asset
    /// is not currently tracked. Frees ledger space for decommissioned pairs.
    pub fn remove_asset(env: Env, admin: Address, asset: Symbol) -> Result<(), Error> {
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        let storage = env.storage().persistent();
        let mut prices: soroban_sdk::Map<Symbol, PriceData> = storage
            .get(&DataKey::PriceData)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));

        if !prices.contains_key(asset.clone()) {
            return Err(Error::AssetNotFound);
        }

        prices.remove(asset);
        storage.set(&DataKey::PriceData, &prices);

        Ok(())
    }

    /// Update the price for a specific asset (authorized backend relayer function)
    ///
    /// # Arguments
    /// * `env` - The contract environment
    /// * `source` - The address of the authorized backend relayer
    /// * `asset` - The asset symbol to update
    /// * `price` - The new price (as i128)
    /// * `decimals` - Number of decimals for the price
    /// * `confidence_score` - Confidence score for this price update
    /// * `ttl` - Time-to-live in seconds for this price (per-asset expiration)
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
        confidence_score: u32,
        ttl: u64,
    ) -> Result<(), Error> {
        source.require_auth();

        if !asset_symbol::is_approved_asset_symbol(asset.clone()) {
            return Err(Error::InvalidAssetSymbol);
        }

        if !is_valid(price) {
            return Err(Error::InvalidPrice);
        }

        if !crate::auth::_is_provider(&env, &source) {
            return Err(Error::NotAuthorized);
        }

        let storage = env.storage().persistent();
        let mut prices: soroban_sdk::Map<Symbol, PriceData> = storage
            .get(&DataKey::PriceData)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));

        let old_price = prices
            .get(asset.clone())
            .map(|existing_price| existing_price.price)
            .unwrap_or(0);

        // Delta limit circuit breaker: reject if price moves more than 50 in one update.
        // Skip on first write (old_price == 0).
        if old_price != 0 {
            if let Some(delta) = calculate_price_volatility(old_price, price) {
                if delta > 50 {
                    return Err(Error::PriceDeltaExceeded);
                }
            }
        }

        let timestamp = env.ledger().timestamp();
        let price_data = PriceData {
            price,
            timestamp,
            provider: source.clone(),
            decimals,
            confidence_score,
            ttl,
        };

        prices.set(asset.clone(), price_data);
        storage.set(&DataKey::PriceData, &prices);

        env.events().publish_event(&PriceUpdatedEvent {
            asset,
            price,
        });

        Ok(())
    }
}

mod asset_symbol;
mod auth;
pub mod math;
mod median;
mod test;
mod types;
