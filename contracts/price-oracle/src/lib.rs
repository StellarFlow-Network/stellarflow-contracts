#![no_std]

use soroban_sdk::{
    contract, contractclient, contracterror, contractimpl, panic_with_error, Address, Env, Symbol, String,
};

use crate::types::{DataKey, PriceBounds, PriceData, RecentEvent, PriceDataWithStatus, PriceEntryWithStatus};
use soroban_sdk::token::TokenClient as TokenContractClient;

const ADMIN_TIMELOCK: u64 = 86_400;

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

    /// Get the full price data with freshness status for a specific asset.
    ///
    /// Returns the last known price with `is_stale = true` when the price has expired.
    fn get_price_with_status(env: Env, asset: Symbol) -> Result<PriceDataWithStatus, Error>;

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
    fn get_prices(
        env: Env,
        assets: soroban_sdk::Vec<Symbol>,
    ) -> soroban_sdk::Vec<Option<crate::types::PriceEntry>>;

    /// Get all currently tracked asset symbols.
    ///
    /// Returns a vector of all assets that are currently being tracked by the oracle.
    fn get_all_assets(env: Env) -> soroban_sdk::Vec<Symbol>;

    /// Get the total number of currently tracked asset symbols.
    ///
    /// Returns the number of unique assets that are currently being tracked by the oracle.
    fn get_asset_count(env: Env) -> u32;

    /// Add a new asset to the tracked asset list.
    ///
    /// The new asset is added to the internal asset list and initialized with a zero-price placeholder.
    fn add_asset(env: Env, admin: Address, asset: Symbol) -> Result<(), Error>;

    /// Whitelist a provider address.
    fn add_provider(env: Env, admin: Address, provider: Address);

    /// Remove a provider from the whitelist.
    fn remove_provider(env: Env, admin: Address, provider: Address);


    /// Get the current admin address.
    ///
    /// Returns the address of the contract administrator.
    fn get_admin(env: Env) -> Address;

    /// Returns `true` when the supplied address is an admin.
    ///
    /// This allows clients to quickly verify admin status without fetching the full admin address.
    fn is_admin(env: Env, user: Address) -> bool;

    /// Start an admin transfer by setting a pending admin and timestamp.
    fn transfer_admin(env: Env, current_admin: Address, new_admin: Address);

    /// Finalize an admin transfer after the timelock has passed.
    fn accept_admin(env: Env, new_admin: Address);

    /// Permanently renounce ownership of the contract.
    ///
    /// This deletes all admin keys from storage, making the contract immutable.
    /// No admin-only functions (upgrade, add_asset, set_price_bounds, etc.)
    /// will ever be callable again. This action is irreversible.
    fn renounce_ownership(env: Env, admin: Address);

    /// Get the last N activity events from the on-chain log.
    ///
    /// Returns a vector of the most recent events (max 5).
    fn get_last_n_events(env: Env, n: u32) -> soroban_sdk::Vec<RecentEvent>;

    /// Get the current ledger sequence number.
    ///
    /// Useful for the frontend and backend to verify they are talking to the
    /// correct version of the oracle and to track contract compatibility.
    fn get_ledger_version(env: Env) -> u32;

    /// Get the human-readable name of this contract.
    ///
    /// Returns a static string identifying the oracle contract.
    fn get_contract_name(env: Env) -> String;

    /// Toggle the pause state of the contract (requires 2-of-3 admin signatures).
    ///
    /// This function prevents a single compromised admin key from shutting down
    /// the network. At least 2 out of 3 registered admins must authorize this action.
    fn toggle_pause(env: Env, admin1: Address, admin2: Address) -> Result<bool, Error>;

    /// Register a new admin (requires 2-of-3 existing admin signatures).
    ///
    /// Maximum of 3 admins allowed. Returns error if already at capacity.
    fn register_admin(env: Env, admin1: Address, admin2: Address, new_admin: Address) -> Result<(), Error>;

    /// Remove an admin (requires 2-of-3 existing admin signatures).
    ///
    /// Cannot remove the last admin. Returns error if would leave 0 admins.
    fn remove_admin(env: Env, admin1: Address, admin2: Address, admin_to_remove: Address) -> Result<(), Error>;

    /// Get the total number of registered admins.
    fn get_admin_count(env: Env) -> u32;
}

/// Maximum allowed percentage change between price updates (10% = 1000 basis points).
/// Any price update exceeding this threshold will be rejected to prevent flash crashes.
const MAX_PERCENT_CHANGE_BPS: i128 = 1_000;

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
    /// Price change exceeds maximum allowed threshold (flash crash protection).
    FlashCrashDetected = 5,
    /// Caller is not authorized to perform this action.
    NotAuthorized = 15,
    /// Contract or admin has already been initialized.
    AlreadyInitialized = 6,
    /// Price change exceeds the allowed delta limit in a single update.
    PriceDeltaExceeded = 7,
    /// Price is outside the configured min/max bounds for the asset.
    PriceOutOfBounds = 8,
    /// Provider weight must be between 0 and 100.
    InvalidWeight = 9,
    /// Multi-signature validation failed - insufficient or invalid admin signatures.
    MultiSigValidationFailed = 10,
    /// Cannot add more admins - maximum of 3 admins allowed.
    MaxAdminsReached = 11,
    /// Cannot remove admin - would leave contract without any admins.
    CannotRemoveLastAdmin = 12,
}

#[contract]
pub struct PriceOracle;

#[soroban_sdk::contractevent]
pub struct PriceUpdatedEvent {
    pub asset: Symbol,
    pub price: i128,
}

#[soroban_sdk::contractevent]
pub struct PriceAnomalyEvent {
    pub asset: Symbol,
    pub previous_price: i128,
    pub attempted_price: i128,
    pub delta: u128,
}

#[soroban_sdk::contractevent]
pub struct ContractInitialized {
    pub admin: Address,
    pub version: String,
}

#[soroban_sdk::contractevent]
pub struct AssetAddedEvent {
    pub symbol: Symbol,
}

#[soroban_sdk::contractevent]
pub struct OwnershipRenouncedEvent {
    pub previous_admin: Address,
}

#[soroban_sdk::contractevent]
pub struct RescueTokensEvent {
    pub token: Address,
    pub recipient: Address,
    pub amount: i128,
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

fn is_whitelisted_provider(env: &Env, source: &Address) -> bool {
    crate::auth::_is_provider(env, source)
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

/// Contract version - must match Cargo.toml version
const VERSION: &str = "0.0.0";

fn get_tracked_assets(env: &Env) -> soroban_sdk::Vec<Symbol> {
    env.storage()
        .instance()
        .get(&DataKey::BaseCurrencyPairs)
        .unwrap_or_else(|| soroban_sdk::Vec::new(&env))
}

fn set_tracked_assets(env: &Env, assets: &soroban_sdk::Vec<Symbol>) {
    env.storage().instance().set(&DataKey::BaseCurrencyPairs, assets);
}

fn track_asset(env: &Env, asset: Symbol) {
    let mut assets = get_tracked_assets(env);
    if !assets.contains(&asset) {
        assets.push_back(asset);
        set_tracked_assets(env, &assets);
    }
}

fn log_event(env: &Env, event_type: Symbol, asset: Symbol, price: i128) {
    let mut events: soroban_sdk::Vec<RecentEvent> = env
        .storage()
        .instance()
        .get(&DataKey::RecentEvents)
        .unwrap_or_else(|| soroban_sdk::Vec::new(env));

    let new_event = RecentEvent {
        event_type,
        asset,
        price,
        timestamp: env.ledger().timestamp(),
    };

    events.push_front(new_event);

    if events.len() > 5 {
        events.pop_back();
    }

    env.storage().instance().set(&DataKey::RecentEvents, &events);
}

#[contractimpl]
impl PriceOracle {
    /// Initialize the contract with admin and base currency pairs.
    /// Can only be called once.
    pub fn initialize(env: Env, admin: Address, base_currency_pairs: soroban_sdk::Vec<Symbol>) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }

        #[allow(deprecated)]
        env.events()
            .publish((Symbol::new(&env, "AdminChanged"),), admin.clone());

        // Emit ContractInitialized event to log when the Oracle goes live
        env.events().publish(
            (Symbol::new(&env, "ContractInitialized"),),
            (admin.clone(), String::from_str(&env, VERSION)),
        );

        let admins = soroban_sdk::vec![&env, admin];
        crate::auth::_set_admin(&env, &admins);
        env.storage()
            .instance()
            .set(&DataKey::BaseCurrencyPairs, &base_currency_pairs);
        
        // Mark contract as initialized
        env.storage().instance().set(&DataKey::Initialized, &true);
    }

    pub fn init_admin(env: Env, admin: Address) {
        if env.storage().instance().has(&DataKey::Initialized) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }

        #[allow(deprecated)]
        env.events()
            .publish((Symbol::new(&env, "AdminChanged"),), admin.clone());

        // Emit ContractInitialized event to log when the Oracle goes live
        env.events().publish(
            (Symbol::new(&env, "ContractInitialized"),),
            (admin.clone(), String::from_str(&env, VERSION)),
        );

        let admins = soroban_sdk::vec![&env, admin];
        crate::auth::_set_admin(&env, &admins);

        env.storage().instance().set(&DataKey::Initialized, &true);
    }

    /// Add a new asset to the tracked asset list.
    ///
    /// The new asset is added to the internal asset list and initialized with a zero-price placeholder.
    pub fn add_asset(env: Env, admin: Address, asset: Symbol) -> Result<(), Error> {
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        if !crate::asset_symbol::is_approved_asset_symbol(asset.clone()) { return Err(Error::InvalidAssetSymbol); }
        track_asset(&env, asset.clone());

        let storage = env.storage().temporary();
        let mut prices: soroban_sdk::Map<Symbol, PriceData> = storage
            .get(&DataKey::PriceData)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));

        if !prices.contains_key(asset.clone()) {
            prices.set(
                asset.clone(),
                PriceData {
                    price: 0,
                    timestamp: env.ledger().timestamp(),
                    provider: env.current_contract_address(),
                    decimals: 0,
                    confidence_score: 0,
                    ttl: 0,
                },
            );
            storage.set(&DataKey::PriceData, &prices);
        }

        env.events().publish_event(&AssetAddedEvent { symbol: asset.clone() });
        log_event(&env, Symbol::new(&env, "asset_added"), asset, 0);

        Ok(())
    }

    /// Whitelist a provider address.
    pub fn add_provider(env: Env, admin: Address, provider: Address) {
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        crate::auth::_add_provider(&env, &provider);
    }

    /// Remove a provider from the whitelist.
    pub fn remove_provider(env: Env, admin: Address, provider: Address) {
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        crate::auth::_remove_provider(&env, &provider);
    }


    /// Return the current admin addresses.
    pub fn get_admin(env: Env) -> Address {
        crate::auth::_get_admin(&env)
            .get(0)
            .expect("No admin set")
    }

    /// Returns true if the supplied address is one of the admin addresses.
    pub fn is_admin(env: Env, user: Address) -> bool {
        crate::auth::_is_authorized(&env, &user)
    }

    /// Starts an admin transfer by storing the pending admin and timestamp.
    pub fn transfer_admin(env: Env, current_admin: Address, new_admin: Address) {
        current_admin.require_auth();
        crate::auth::_require_authorized(&env, &current_admin);

        let now = env.ledger().timestamp();

        env.storage().instance().set(&DataKey::PendingAdmin, &new_admin);
        env.storage()
            .instance()
            .set(&DataKey::PendingAdminTimestamp, &now);
    }

    /// Finalizes the admin transfer after the timelock expires.
    pub fn accept_admin(env: Env, new_admin: Address) {
        new_admin.require_auth();

        let pending: Address = env
            .storage()
            .instance()
            .get(&DataKey::PendingAdmin)
            .expect("No pending admin");

        if pending != new_admin {
            panic!("Not pending admin");
        }

        let timestamp: u64 = env
            .storage()
            .instance()
            .get(&DataKey::PendingAdminTimestamp)
            .expect("No pending admin timestamp");

        let now = env.ledger().timestamp();

        if now < timestamp.saturating_add(ADMIN_TIMELOCK) {
            panic!("Timelock not expired");
        }

        let admins = soroban_sdk::vec![&env, new_admin.clone()];
        crate::auth::_set_admin(&env, &admins);

        env.storage()
            .instance()
            .set(&DataKey::AdminUpdateTimestamp, &now);

        env.storage().instance().remove(&DataKey::PendingAdmin);
        env.storage()
            .instance()
            .remove(&DataKey::PendingAdminTimestamp);
    }

    /// Permanently renounce ownership of the contract.
    ///
    /// This deletes all admin keys from storage, making the contract immutable.
    /// No admin-only functions (upgrade, add_asset, set_price_bounds, etc.)
    /// will ever be callable again. This action is irreversible.
    pub fn renounce_ownership(env: Env, admin: Address) {
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        crate::auth::_renounce_ownership(&env);

        env.events().publish_event(&OwnershipRenouncedEvent {
            previous_admin: admin,
        });
    }

    /// A low-gas health check to verify the contract is responding.
    ///
    /// Returns a simple "PONG" symbol with minimal gas consumption.
    /// Useful for monitoring and liveness checks without state access.
    pub fn ping(_env: Env) -> Symbol {
        soroban_sdk::symbol_short!("PONG")
    }

    /// Get the price data for a specific asset.
    /// Returns error if price is stale.
    pub fn get_price(env: Env, asset: Symbol) -> Result<PriceData, Error> {
        let storage = env.storage().temporary();
        let prices: soroban_sdk::Map<Symbol, PriceData> = storage
            .get(&DataKey::PriceData)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));

        match prices.get(asset) {
            Some(price_data) => {
                let now = env.ledger().timestamp();
                if is_stale(now, price_data.timestamp, price_data.ttl) {
                    return Err(Error::AssetNotFound);
                }
                Ok(price_data)
            }
            None => Err(Error::AssetNotFound),
        }
    }

    /// Returns the last known price data and marks it stale when TTL has expired.
    pub fn get_price_with_status(env: Env, asset: Symbol) -> Result<PriceDataWithStatus, Error> {
        let prices: soroban_sdk::Map<Symbol, PriceData> = env
            .storage()
            .temporary()
            .get(&DataKey::PriceData)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));

        match prices.get(asset) {
            Some(price_data) => {
                let now = env.ledger().timestamp();
                Ok(PriceDataWithStatus {
                    is_stale: is_stale(now, price_data.timestamp, price_data.ttl),
                    data: price_data,
                })
            }
            None => Err(Error::AssetNotFound),
        }
    }

    /// Returns `None` instead of an error when the asset is not found.
    pub fn get_price_safe(env: Env, asset: Symbol) -> Option<PriceData> {
        let prices: soroban_sdk::Map<Symbol, PriceData> = env
            .storage()
            .temporary()
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
            .temporary()
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
                        decimals: pd.decimals,
                    })
                }
            });
            result.push_back(entry);
        }

        result
    }

    /// Returns prices for all found assets and marks stale entries with `is_stale = true`.
    pub fn get_prices_with_status(
        env: Env,
        assets: soroban_sdk::Vec<Symbol>,
    ) -> soroban_sdk::Vec<Option<PriceEntryWithStatus>> {
        let prices: soroban_sdk::Map<Symbol, PriceData> = env
            .storage()
            .temporary()
            .get(&DataKey::PriceData)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));

        let now = env.ledger().timestamp();
        let mut result = soroban_sdk::Vec::new(&env);

        for asset in assets.iter() {
            let entry = prices.get(asset).map(|pd| PriceEntryWithStatus {
                price: pd.price,
                timestamp: pd.timestamp,
                is_stale: is_stale(now, pd.timestamp, pd.ttl),
            });
            result.push_back(entry);
        }

        result
    }

    /// Returns a vector of all currently tracked asset symbols.
    pub fn get_all_assets(env: Env) -> soroban_sdk::Vec<Symbol> {
        get_tracked_assets(&env)
    }

    /// Returns the total number of currently tracked asset symbols.
    pub fn get_asset_count(env: Env) -> u32 {
        get_tracked_assets(&env).len()
    }

    /// Store a human-readable description for an asset (e.g. "Nigerian Naira").
    ///
    /// Only the admin can call this.
    pub fn set_asset_description(env: Env, admin: Address, asset: Symbol, description: soroban_sdk::String) {
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);
        env.storage()
            .persistent()
            .set(&DataKey::AssetDescription(asset), &description);
    }

    /// Get the human-readable description for an asset.
    ///
    /// Returns `Error::AssetNotFound` if no description has been set.
    pub fn get_asset_description(env: Env, asset: Symbol) -> Result<soroban_sdk::String, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::AssetDescription(asset))
            .ok_or(Error::AssetNotFound)
    }

    /// Set the price data for a specific asset.
    ///
    /// # Gas optimisation — Zero-Write for identical prices
    /// When the incoming `val` is identical to the currently stored price the
    /// full `storage().set()` call is skipped entirely.  Only the timestamp
    /// field is updated in-place, saving the write fee for the price value
    /// while keeping the freshness indicator current.
    ///
    /// # Arguments
    /// * `env` - The contract environment
    /// * `asset` - The asset symbol to set
    /// * `val` - The price value
    /// * `decimals` - Number of decimals for the price
    /// * `ttl` - Time-to-live in seconds for this price (per-asset expiration)
    pub fn set_price(env: Env, asset: Symbol, val: i128, decimals: u32, ttl: u64) {
        if !is_valid(val) {
            panic_with_error!(&env, Error::InvalidPrice);
        }

        let storage = env.storage().temporary();
        let mut prices: soroban_sdk::Map<Symbol, PriceData> = storage
            .get(&DataKey::PriceData)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));

        let existing = prices.get(asset.clone());
        let is_new_asset = existing.is_none();

        track_asset(&env, asset.clone());

        let now = env.ledger().timestamp();

        if let Some(mut current) = existing {
            if current.price == val {
                // Price unchanged — only refresh the timestamp to avoid a
                // full storage write for the price field (zero-write optimisation).
                current.timestamp = now;
                prices.set(asset.clone(), current);
                storage.set(&DataKey::PriceData, &prices);
                env.events().publish_event(&PriceUpdatedEvent { asset: asset.clone(), price: val });
                log_event(&env, Symbol::new(&env, "price_updated"), asset, val);
                return;
            }
        }

        // Price changed (or first write) — store the full entry.
        let price_data = PriceData {
            price: val,
            timestamp: now,
            provider: env.current_contract_address(),
            decimals,
            confidence_score: 100,
            ttl,
        };

        prices.set(asset.clone(), price_data);
        storage.set(&DataKey::PriceData, &prices);

        if is_new_asset {
            env.events().publish_event(&AssetAddedEvent {
                symbol: asset.clone(),
            });
            log_event(&env, Symbol::new(&env, "asset_added"), asset, val);
        } else {
            log_event(&env, Symbol::new(&env, "price_updated"), asset, val);
        }
    }

    /// Rescue tokens accidentally sent to this contract.
    ///
    /// Admin-only function to move trapped XLM or other assets out of the contract.
    pub fn rescue_tokens(env: Env, admin: Address, token: Address, to: Address, amount: i128) {
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        if amount <= 0 {
            panic_with_error!(&env, Error::InvalidPrice);
        }

        let token_client = TokenContractClient::new(&env, &token);
        token_client.transfer(&env.current_contract_address(), &to, &amount);

        env.events().publish_event(&RescueTokensEvent {
            token,
            recipient: to,
            amount,
        });
    }

    /// Upgrade the contract WASM code.
    ///
    /// Replaces the on-chain WASM bytecode with the provided hash while preserving
    /// all contract storage. Strictly restricted to the admin.
    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: soroban_sdk::BytesN<32>) {
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);
        env.deployer().update_current_contract_wasm(new_wasm_hash);
    }

    /// Remove an asset from the oracle, deleting its price entry.
    ///
    /// Only the admin can call this. Returns `Error::AssetNotFound` if the asset
    /// is not currently tracked.
    pub fn remove_asset(env: Env, admin: Address, asset: Symbol) -> Result<(), Error> {
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        let storage = env.storage().temporary();
        let mut prices: soroban_sdk::Map<Symbol, PriceData> = storage
            .get(&DataKey::PriceData)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));

        if !prices.contains_key(asset.clone()) {
            return Err(Error::AssetNotFound);
        }

        prices.remove(asset.clone());
        storage.set(&DataKey::PriceData, &prices);

        let tracked = get_tracked_assets(&env);
        let mut updated_assets = soroban_sdk::Vec::new(&env);
        for tracked_asset in tracked.iter() {
            if tracked_asset != asset {
                updated_assets.push_back(tracked_asset.clone());
            }
        }
        set_tracked_assets(&env, &updated_assets);

        Ok(())
    }

    /// Update the price for a specific asset (authorized backend relayer function)
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

        if !get_tracked_assets(&env).contains(&asset) {
            return Err(Error::InvalidAssetSymbol);
        }

        if !is_valid(price) {
            return Err(Error::InvalidPrice);
        }

        if !is_whitelisted_provider(&env, &source) {
            return Err(Error::NotAuthorized);
        }

        let provider = crate::auth::_get_provider(&env, &source).unwrap();
        if env.ledger().sequence() < provider.joined_ledger + 100 {
            // Warming up: save to history but don't update Active Price
            log_event(&env, Symbol::new(&env, "price_warmup"), asset, price);
            return Ok(());
        }


        let storage = env.storage().temporary();
        let mut prices: soroban_sdk::Map<Symbol, PriceData> = storage
            .get(&DataKey::PriceData)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));

        let old_price = prices
            .get(asset.clone())
            .map(|existing_price| existing_price.price)
            .unwrap_or(0);

        // Flash crash protection: reject if price change exceeds MAX_PERCENT_CHANGE
        if old_price > 0 {
            if let Some(pct_change_bps) = calculate_percentage_difference_bps(old_price, price) {
                if pct_change_bps > MAX_PERCENT_CHANGE_BPS {
                    return Err(Error::FlashCrashDetected);
                }
            }
        }

        let bounds_map: soroban_sdk::Map<Symbol, PriceBounds> = env.storage().instance()
            .get(&DataKey::PriceBoundsData)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));
        if let Some(bounds) = bounds_map.get(asset.clone()) {
            if price < bounds.min_price || price > bounds.max_price {
                return Err(Error::PriceOutOfBounds);
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

        env.events().publish_event(&PriceUpdatedEvent { asset: asset.clone(), price });

        log_event(&env, Symbol::new(&env, "price_updated"), asset, price);

        Ok(())
    }

    /// Set the min/max price bounds for an asset.
    pub fn set_price_bounds(
        env: Env,
        admin: Address,
        asset: Symbol,
        min_price: i128,
        max_price: i128,
    ) {
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        assert!(min_price > 0 && max_price > 0, "bounds must be positive");
        assert!(min_price <= max_price, "min_price must be <= max_price");

        let storage = env.storage().instance();
        let mut bounds_map: soroban_sdk::Map<Symbol, PriceBounds> = storage
            .get(&DataKey::PriceBoundsData)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));

        bounds_map.set(
            asset,
            PriceBounds {
                min_price,
                max_price,
            },
        );
        storage.set(&DataKey::PriceBoundsData, &bounds_map);
    }

    /// Get the current min/max price bounds for an asset, if configured.
    pub fn get_price_bounds(env: Env, asset: Symbol) -> Option<PriceBounds> {
        let bounds_map: soroban_sdk::Map<Symbol, PriceBounds> = env
            .storage()
            .instance()
            .get(&DataKey::PriceBoundsData)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));
        bounds_map.get(asset)
    }

    /// Get the current ledger sequence number.
    ///
    /// Returns the ledger sequence number at the time of the call.
    /// Useful for the frontend and backend to verify contract compatibility.
    pub fn get_ledger_version(env: Env) -> u32 {
        env.ledger().sequence()
    }

    /// Get the human-readable name of this contract.
    ///
    /// Returns a static string identifying the oracle contract.
    pub fn get_contract_name(env: Env) -> String {
        String::from_str(&env, "StellarFlow Africa Oracle")
    }

    /// Get the last N activity events from the on-chain log.
    pub fn get_last_n_events(env: Env, n: u32) -> soroban_sdk::Vec<RecentEvent> {
        let events: soroban_sdk::Vec<RecentEvent> = env
            .storage()
            .instance()
            .get(&DataKey::RecentEvents)
            .unwrap_or_else(|| soroban_sdk::Vec::new(&env));

        let mut result = soroban_sdk::Vec::new(&env);
        let limit = n.min(events.len());

        for i in 0..limit {
            if let Some(event) = events.get(i) {
                result.push_back(event);
            }
        }

        result
    }

    /// Toggle the pause state of the contract (requires 2-of-3 admin signatures).
    ///
    /// This function prevents a single compromised admin key from shutting down
    /// the network. At least 2 out of 3 registered admins must authorize this action.
    ///
    /// # Arguments
    /// * `admin1` - First admin address (must provide auth)
    /// * `admin2` - Second admin address (must provide auth)
    ///
    /// # Returns
    /// The new pause state (true = paused, false = unpaused)
    pub fn toggle_pause(env: Env, admin1: Address, admin2: Address) -> Result<bool, Error> {
        // Require both admins to provide cryptographic signatures
        admin1.require_auth();
        admin2.require_auth();

        // Verify both are distinct addresses
        if admin1 == admin2 {
            return Err(Error::MultiSigValidationFailed);
        }

        // Verify both are authorized admins
        crate::auth::_require_authorized(&env, &admin1);
        crate::auth::_require_authorized(&env, &admin2);

        // Get current admin list
        let admins = crate::auth::_get_admin(&env);
        let admin_count = admins.len();

        // Ensure we have at least 2 admins registered
        if admin_count < 2 {
            return Err(Error::MultiSigValidationFailed);
        }

        // Toggle the pause state
        let current_paused = crate::auth::_is_paused(&env);
        let new_paused = !current_paused;
        crate::auth::_set_paused(&env, new_paused);

        // Emit event
        env.events().publish(
            (Symbol::new(&env, "pause_toggled"),),
            (admin1.clone(), admin2.clone(), new_paused),
        );

        Ok(new_paused)
    }

    /// Register a new admin (requires 2-of-3 existing admin signatures).
    ///
    /// # Arguments
    /// * `admin1` - First admin address (must provide auth)
    /// * `admin2` - Second admin address (must provide auth)
    /// * `new_admin` - The new admin to register
    ///
    /// # Returns
    /// Ok(()) if successful, Error if validation fails
    pub fn register_admin(env: Env, admin1: Address, admin2: Address, new_admin: Address) -> Result<(), Error> {
        // Require both existing admins to provide cryptographic signatures
        admin1.require_auth();
        admin2.require_auth();

        // Verify both are distinct addresses
        if admin1 == admin2 {
            return Err(Error::MultiSigValidationFailed);
        }

        // Verify both are authorized admins
        crate::auth::_require_authorized(&env, &admin1);
        crate::auth::_require_authorized(&env, &admin2);

        // Get current admin list
        let admins = crate::auth::_get_admin(&env);
        let admin_count = admins.len();

        // Check if we've reached the maximum of 3 admins
        if admin_count >= 3 {
            return Err(Error::MaxAdminsReached);
        }

        // Add the new admin
        crate::auth::_add_authorized(&env, &new_admin);

        // Emit event
        env.events().publish(
            (Symbol::new(&env, "admin_registered"),),
            (admin1.clone(), admin2.clone(), new_admin.clone()),
        );

        Ok(())
    }

    /// Remove an admin (requires 2-of-3 existing admin signatures).
    ///
    /// # Arguments
    /// * `admin1` - First admin address (must provide auth)
    /// * `admin2` - Second admin address (must provide auth)
    /// * `admin_to_remove` - The admin to remove
    ///
    /// # Returns
    /// Ok(()) if successful, Error if validation fails
    pub fn remove_admin(env: Env, admin1: Address, admin2: Address, admin_to_remove: Address) -> Result<(), Error> {
        // Require both existing admins to provide cryptographic signatures
        admin1.require_auth();
        admin2.require_auth();

        // Verify both are distinct addresses
        if admin1 == admin2 {
            return Err(Error::MultiSigValidationFailed);
        }

        // Verify both are authorized admins
        crate::auth::_require_authorized(&env, &admin1);
        crate::auth::_require_authorized(&env, &admin2);

        // Get current admin list
        let admins = crate::auth::_get_admin(&env);
        let admin_count = admins.len();

        // Cannot remove if would leave less than 1 admin
        if admin_count <= 1 {
            return Err(Error::CannotRemoveLastAdmin);
        }

        // Verify the admin to remove actually exists
        if !admins.iter().any(|a| a == admin_to_remove) {
            return Err(Error::NotAuthorized);
        }

        // Remove the admin
        crate::auth::_remove_authorized(&env, &admin_to_remove);

        // Emit event
        env.events().publish(
            (Symbol::new(&env, "admin_removed"),),
            (admin1.clone(), admin2.clone(), admin_to_remove.clone()),
        );

        Ok(())
    }

    /// Get the total number of registered admins.
    pub fn get_admin_count(env: Env) -> u32 {
        if !crate::auth::_has_admin(&env) {
            return 0;
        }
        crate::auth::_get_admin(&env).len()
    }
}

mod asset_symbol;
mod auth;
pub mod math;
mod median;
mod test;
mod types;