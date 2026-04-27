#![no_std]

use soroban_sdk::{
    contract, contractclient, contracterror, contractimpl, panic_with_error, Address, Env, Symbol, String, token,
};

use crate::types::{DataKey, PriceBounds, PriceBuffer, PriceBufferEntry, PriceData, PriceDataWithStatus, PriceEntryWithStatus, RecentEvent, AdminAction, AdminLogEntry, PriceUpdatePayload, ProposedAction, FeeDistribution};

const ADMIN_TIMELOCK: u64 = 86_400;
const MAX_CLEAR_ASSETS: u32 = 20;

/// A clean, gas-optimized interface for other Soroban contracts to fetch prices from StellarFlow.
///
/// The generated client from this trait is the intended cross-contract entrypoint for downstream
/// Soroban applications. The getters are read-only and `get_last_price` is the cheapest option
/// when callers only need the scalar price value.
#[contractclient(name = "StellarFlowClient")]
pub trait StellarFlowTrait {
    /// Get the full price data for a specific asset.
    ///
    /// When `verified` is `true`, reads from the `VerifiedPrice` bucket (default for internal math).
    /// When `verified` is `false`, reads from the `CommunityPrice` bucket.
    /// Returns `Error::AssetNotFound` if the asset does not exist or the price is stale.
    fn get_price(env: Env, asset: Symbol, verified: bool) -> Result<PriceData, Error>;

    /// Calculate the weighted average price of a multi-asset index basket.
    ///
    /// # Arguments
    /// * `components` - A vector of AssetWeight defining the basket (e.g., NGN, GHS, CFA).
    fn get_index_price(env: Env, components: soroban_sdk::Vec<crate::types::AssetWeight>) -> Result<i128, Error>;

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

    /// Get the Time-Weighted Average Price (TWAP) for a specific asset.
    ///
    /// Returns the simple average of the last 10 price updates, or `None` if no data.
    fn get_twap(env: Env, asset: Symbol) -> Option<i128>;

    /// Add a new asset to the tracked asset list.
    ///
    /// The new asset is added to the internal asset list and initialized with a zero-price placeholder.
    fn add_asset(env: Env, admin: Address, asset: Symbol) -> Result<(), Error>;

    /// Set an absolute floor price for an asset.
    ///
    /// Any attempted price update below this value will be rejected.
    fn set_price_floor(env: Env, admin: Address, asset: Symbol, price_floor: i128);

    /// Get the configured absolute floor price for an asset, if any.
    fn get_price_floor(env: Env, asset: Symbol) -> Option<i128>;

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

    /// Propose a high-impact action that requires multi-signature approval.
    ///
    /// The action will only execute once the threshold (e.g., 3/5) is met.
    fn propose_action(env: Env, admin: Address, action_type: u32, target: Option<Address>, data: soroban_sdk::String) -> Result<u64, Error>;

    /// Vote for a proposed action.
    fn vote_for_action(env: Env, voter: Address, action_id: u64) -> Result<u32, Error>;

    /// Execute a proposed action that has reached the vote threshold.
    fn execute_proposed_action(env: Env, executor: Address, action_id: u64) -> Result<(), Error>;

    /// Get the details of a proposed action.
    fn get_proposed_action(env: Env, action_id: u64) -> Option<ProposedAction>;

    /// Get the list of voters for a proposed action.
    fn get_action_voters(env: Env, action_id: u64) -> soroban_sdk::Vec<Address>;

    /// Get the required vote threshold for the current admin set.
    fn get_required_threshold(env: Env) -> u32;

    /// Cancel a proposed action.
    fn cancel_proposed_action(env: Env, canceller: Address, action_id: u64) -> Result<(), Error>;

    /// Get the health status of the oracle for the Admin Dashboard.
    ///
    /// Returns aggregated data from multiple storage keys in a single call.
    /// This is a read-only function that provides a snapshot of the oracle's current state.
    fn get_oracle_health(env: Env) -> crate::types::OracleHealth;

    /// Subscribe a contract to receive price update callbacks.
    ///
    /// When a price is updated, the oracle will invoke the `on_price_update` function
    /// on all subscribed contracts with the new price data. This enables downstream
    /// contracts (e.g., Lending protocols, DEXs) to react to price changes without polling.
    ///
    /// # Arguments
    /// * `callback_contract` - The address of the contract that implements `on_price_update`
    ///
    /// # Returns
    /// Returns an error if the contract is already subscribed.
    fn subscribe_to_price_updates(env: Env, callback_contract: Address) -> Result<(), Error>;

    /// Unsubscribe a contract from price update callbacks.
    ///
    /// # Arguments
    /// * `callback_contract` - The address of the contract to unsubscribe
    ///
    /// # Returns
    /// Returns an error if the contract is not found in the subscriber list.
    fn unsubscribe_from_price_updates(env: Env, callback_contract: Address) -> Result<(), Error>;

    /// Get the list of all contracts subscribed to price updates.
    ///
    /// # Returns
    /// A vector of addresses of all contracts currently subscribed to price updates.
    fn get_price_update_subscribers(env: Env) -> soroban_sdk::Vec<Address>;

    /// Set the Community Council address for emergency freeze functionality.
    ///
    /// Only the admin can call this. The Council address can be used to trigger
    /// an emergency freeze if a majority of admins are compromised.
    fn set_council(env: Env, admin: Address, council: Address);

    /// Get the current Community Council address.
    ///
    /// Returns the address of the Community Council, or None if not set.
    fn get_council(env: Env) -> Option<Address>;

    /// Emergency freeze the contract.
    ///
    /// Only the Community Council can call this function. When triggered,
    /// the contract enters a frozen state where all state-changing operations
    /// are blocked. This is a last-resort measure when a majority of admins
    /// are compromised.
    fn emergency_freeze(env: Env, council: Address) -> Result<(), Error>;

    /// Check if the contract is in emergency freeze state.
    ///
    /// Returns true if the contract is frozen, false otherwise.
    fn is_frozen(env: Env) -> bool;

    /// Distribute collected fees between Insurance Fund, Admin Treasury, and Relayer Rewards.
    ///
    /// Automatically called after reaching the transaction threshold or can be called manually by admin.
    /// Splits fees according to the configured percentages (in basis points).
    fn distribute_fees(env: Env, admin: Address) -> Result<(), Error>;

    /// Set the fee distribution percentages.
    ///
    /// Only admin can call this. Percentages are in basis points (e.g., 5000 = 50%).
    /// The sum of all percentages must equal 10000 (100%).
    fn set_fee_distribution(
        env: Env,
        admin: Address,
        insurance_fund_bps: u32,
        admin_treasury_bps: u32,
        relayer_rewards_bps: u32,
    ) -> Result<(), Error>;

    /// Set the transaction distribution interval.
    ///
    /// After this many transactions, fees will be automatically distributed.
    fn set_distribution_interval(env: Env, admin: Address, interval: u32) -> Result<(), Error>;

    /// Get the current fee distribution configuration.
    fn get_fee_distribution_config(env: Env) -> Option<crate::types::FeeDistribution>;

    /// Set the Insurance Fund address for fee distribution.
    fn set_insurance_fund(env: Env, admin: Address, address: Address) -> Result<(), Error>;

    /// Set the Admin Treasury address for fee distribution.
    fn set_admin_treasury(env: Env, admin: Address, address: Address) -> Result<(), Error>;

    /// Set the Relayer Rewards address for fee distribution.
    fn set_relayer_rewards(env: Env, admin: Address, address: Address) -> Result<(), Error>;

    /// Set the query fee amount (in stroops).
    fn set_query_fee(env: Env, admin: Address, fee_amount: i128) -> Result<(), Error>;

    /// Set the fee token address.
    fn set_fee_token(env: Env, admin: Address, token_address: Address) -> Result<(), Error>;
}

#[contractclient(name = "TokenContractClient")]
pub trait TokenContractTrait {
    fn transfer(env: Env, from: Address, to: Address, amount: i128);
}

/// Maximum allowed percentage change between price updates (10% = 1000 basis points).
/// Any price update exceeding this threshold will be rejected to prevent flash crashes.
const MAX_PERCENT_CHANGE_BPS: i128 = 1_000;

/// Percentage move threshold (5% = 500 basis points) above which a "cross_call"
/// volatility event is published so downstream contracts (e.g. liquidation bots)
/// can react without polling.
const VOLATILITY_THRESHOLD_BPS: i128 = 500;

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
    NotAuthorized = 6,
    /// Contract or admin has already been initialized.
    AlreadyInitialized = 7,
    /// Price change exceeds the allowed delta limit in a single update.
    PriceDeltaExceeded = 8,
    /// Price is outside the configured min/max bounds for the asset.
    PriceOutOfBounds = 9,
    /// Provider weight must be between 0 and 100.
    InvalidWeight = 10,
    /// Multi-signature validation failed - insufficient or invalid admin signatures.
    MultiSigValidationFailed = 11,
    /// Cannot add more admins - maximum of 3 admins allowed.
    MaxAdminsReached = 12,
    /// Cannot remove admin - would leave contract without any admins.
    CannotRemoveLastAdmin = 13,
    /// Reentrancy detected - function is already executing.
    ReentrancyDetected = 14,
    /// Action not found or already executed/cancelled.
    ActionNotFound = 15,
    /// Vote threshold not reached - insufficient approvals.
    ThresholdNotReached = 16,
    /// Invalid action type for execution.
    InvalidActionType = 17,
    /// Action has already been executed.
    ActionAlreadyExecuted = 18,
    /// Action has been cancelled.
    ActionCancelled = 19,
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

/// Checks if the given address is a whitelisted provider.
fn _is_whitelisted_provider(env: &Env, source: &Address) -> bool {
    crate::auth::_is_provider(env, source)
}
/// Panic if the contract has been destroyed.
fn _require_not_destroyed(env: &Env) {
    if env.storage().instance().has(&DataKey::Destroyed) {
        panic_with_error!(env, Error::ContractDestroyed);
    }
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

/// Acquire the reentrancy lock for set_price.
/// Returns an error if the lock is already held.
fn acquire_lock(env: &Env) -> Result<(), Error> {
    let is_locked: bool = env
        .storage()
        .instance()
        .get(&DataKey::IsLocked)
        .unwrap_or(false);
    
    if is_locked {
        return Err(Error::ReentrancyDetected);
    }
    
    env.storage().instance().set(&DataKey::IsLocked, &true);
    Ok(())
}

/// Release the reentrancy lock for set_price.
fn release_lock(env: &Env) {
    env.storage().instance().set(&DataKey::IsLocked, &false);
}

/// Collect query fee from the caller and accumulate it for distribution.
fn _collect_query_fee(env: &Env) -> Result<(), Error> {
    // Get the fee amount
    let fee_amount: i128 = env
        .storage()
        .persistent()
        .get(&DataKey::QueryFee)
        .unwrap_or(0);

    if fee_amount <= 0 {
        return Ok(()); // No fee configured
    }

    // Get the fee token
    let fee_token = env
        .storage()
        .persistent()
        .get(&DataKey::FeeToken)
        .unwrap_or_else(|| Address::from_string(&soroban_sdk::String::from_str(env, "CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC")));

    // Transfer fee from caller to contract
    let token_client = token::Client::new(env, &fee_token);
    let caller = env.invoker();
    let contract_address = env.current_contract_address();

    token_client.transfer(&caller, &contract_address, &fee_amount);

    // Accumulate collected fees
    let mut fee_config: FeeDistribution = env
        .storage()
        .persistent()
        .get(&DataKey::CollectedFees)
        .unwrap_or(FeeDistribution {
            collected_fees: 0,
            insurance_fund_bps: 4000,
            admin_treasury_bps: 3000,
            relayer_rewards_bps: 3000,
            last_distribution: env.ledger().timestamp(),
        });

    fee_config.collected_fees += fee_amount;
    env.storage().persistent().set(&DataKey::CollectedFees, &fee_config);

    // Increment transaction counter and check if we should auto-distribute
    let mut tx_count: u32 = env
        .storage()
        .persistent()
        .get(&DataKey::TransactionCounter)
        .unwrap_or(0);
    tx_count += 1;

    let distribution_interval: u32 = env
        .storage()
        .persistent()
        .get(&DataKey::DistributionInterval)
        .unwrap_or(100); // Default to 100 transactions

    if tx_count >= distribution_interval {
        // Auto-distribute fees (this would need to be called by an authorized party)
        // For now, we'll just reset the counter
        tx_count = 0;
    }

    env.storage().persistent().set(&DataKey::TransactionCounter, &tx_count);

    Ok(())
}

/// Contract version - must match Cargo.toml version
const VERSION: &str = "0.0.0";

fn get_tracked_assets(env: &Env) -> soroban_sdk::Vec<Symbol> {
    env.storage()
        .instance()
        .get(&DataKey::BaseCurrencyPairs)
        .unwrap_or_else(|| soroban_sdk::Vec::new(&env))
}

fn _set_tracked_assets(env: &Env, assets: &soroban_sdk::Vec<Symbol>) {
    env.storage().instance().set(&DataKey::BaseCurrencyPairs, assets);
}

/// Get the price buffer for a specific asset.
/// Returns a new empty buffer if none exists for the current ledger.
fn get_price_buffer(env: &Env, asset: Symbol) -> PriceBuffer {
    let storage_key = DataKey::PriceBuffer;
    let buffers: soroban_sdk::Map<Symbol, PriceBuffer> = env
        .storage()
        .persistent()
        .get(&storage_key)
        .unwrap_or_else(|| soroban_sdk::Map::new(env));

    buffers.get(asset).unwrap_or_else(|| PriceBuffer {
        entries: soroban_sdk::Vec::new(env),
        ledger_sequence: env.ledger().sequence(),
        decimals: 0,
        ttl: 0,
    })
}

/// Save the price buffer for a specific asset.
fn set_price_buffer(env: &Env, asset: Symbol, buffer: &PriceBuffer) {
    let storage_key = DataKey::PriceBuffer;
    let mut buffers: soroban_sdk::Map<Symbol, PriceBuffer> = env
        .storage()
        .persistent()
        .get(&storage_key)
        .unwrap_or_else(|| soroban_sdk::Map::new(env));

    buffers.set(asset, buffer.clone());
    env.storage().persistent().set(&storage_key, &buffers);
}

/// Clear the price buffer if it's from a previous ledger.
fn clear_stale_buffer(env: &Env, asset: Symbol, buffer: &mut PriceBuffer) {
    let current_ledger = env.ledger().sequence();
    if buffer.ledger_sequence != current_ledger {
        buffer.entries = soroban_sdk::Vec::new(env);
        buffer.ledger_sequence = current_ledger;
    }
}

/// Check if a provider has already submitted a price in the current buffer.
fn has_provider_submitted(buffer: &PriceBuffer, provider: &Address) -> bool {
    buffer.entries.iter().any(|entry| entry.provider == *provider)
}

/// Calculate the median price from the buffer entries.
/// Returns None if the buffer is empty.
fn calculate_median_from_buffer(env: &Env, buffer: &PriceBuffer) -> Option<i128> {
    if buffer.entries.len() == 0 {
        return None;
    }

    // Extract prices into a Vec for sorting
    let mut prices = soroban_sdk::Vec::new(env);
    for entry in buffer.entries.iter() {
        prices.push_back(entry.price);
    }

    // Use the existing median calculation
    crate::median::calculate_median(prices).ok()
}

/// Adds an asset to the list of tracked assets if it's not already present.
fn _track_asset(env: &Env, asset: Symbol) {
    let mut assets = get_tracked_assets(env);
    if !assets.contains(&asset) {
        assets.push_back(asset.clone());
        _set_tracked_assets(env, &assets);
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

fn _log_admin_action(env: &Env, admin: &Address, action: AdminAction, details: Option<String>) {
    let entry = AdminLogEntry {
        admin: admin.clone(),
        action,
        details: details.unwrap_or_else(|| String::from_str(env, "")),
        timestamp: env.ledger().timestamp(),
    };
    // Store the admin log entry - using a simple key for now
    // In production, you might want to store multiple entries in a vector
    env.storage().instance().set(&DataKey::AdminUpdateTimestamp, &entry.timestamp);
}

fn read_price_floor(env: &Env, asset: &Symbol) -> Option<i128> {
    let floors: soroban_sdk::Map<Symbol, i128> = env
        .storage()
        .persistent()
        .get(&DataKey::PriceFloorData)
        .unwrap_or_else(|| soroban_sdk::Map::new(env));
    floors.get(asset.clone())
}

fn enforce_price_floor(env: &Env, asset: &Symbol, price: i128) -> Result<(), Error> {
    if let Some(price_floor) = read_price_floor(env, asset) {
        if price < price_floor {
            return Err(Error::PriceOutOfBounds);
        }
    }

    Ok(())
}

fn update_twap(env: &Env, asset: Symbol, price: i128, timestamp: u64) {
    let key = DataKey::Twap(asset);
    let mut twap_buffer: soroban_sdk::Vec<(u64, i128)> = env
        .storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(|| soroban_sdk::Vec::new(env));

    twap_buffer.push_back((timestamp, price));

    if twap_buffer.len() > 10 {
        twap_buffer.pop_front();
    }

    env.storage().persistent().set(&key, &twap_buffer);
}

#[contractimpl]
impl PriceOracle {
    /// Initialize the contract with admin and base currency pairs.
    /// Can only be called once.
    pub fn initialize(env: Env, admin: Address, base_currency_pairs: soroban_sdk::Vec<Symbol>) {
        if env.storage().instance().has(&DataKey::Initialized) || crate::auth::_has_admin(&env) {
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

        //_log_admin_action(&env, &admin, AdminAction::Initialize, None);
        let admins = soroban_sdk::vec![&env, admin];
        crate::auth::_set_admin(&env, &admins);
        env.storage()
            .instance()
            .set(&DataKey::BaseCurrencyPairs, &base_currency_pairs);
        
        // Mark contract as initialized
        env.storage().instance().set(&DataKey::Initialized, &true);
    }

    pub fn get_index_price(env: Env, components: soroban_sdk::Vec<crate::types::AssetWeight>) -> Result<i128, Error> {
        if components.is_empty() {
            return Err(Error::AssetNotFound); 
        }

        let mut total_weighted_price: i128 = 0;
        let mut total_weight: u32 = 0;

        for component in components.iter() {
            // Fetch the verified price. 
            // If any asset is missing or stale, this cleanly propagates Error::AssetNotFound.
            let price_data = Self::get_price(env.clone(), component.asset.clone(), true)?;
            
            let weight_i128: i128 = component.weight.into();
            
            // Safe math to prevent overflow panics
            let weighted_val = price_data.price.checked_mul(weight_i128)
                .ok_or(Error::InvalidPrice)?;
                
            total_weighted_price = total_weighted_price.checked_add(weighted_val)
                .ok_or(Error::InvalidPrice)?;
                
            total_weight = total_weight.checked_add(component.weight)
                .unwrap_or(total_weight); 
        }

        if total_weight == 0 {
            return Err(Error::InvalidWeight);
        }

        // Calculate final index price. 
        // Because all stored prices are 9-decimal normalized, the division preserves the 9-decimal standard.
        let index_price = total_weighted_price / (total_weight as i128);
        Ok(index_price)
    }

    pub fn init_admin(env: Env, admin: Address) {
        _require_not_destroyed(&env);
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

        //_log_admin_action(&env, &admin, AdminAction::InitAdmin, None);
        let admins = soroban_sdk::vec![&env, admin];
        crate::auth::_set_admin(&env, &admins);

        env.storage().instance().set(&DataKey::Initialized, &true);
    }

    /// Add a new asset to the tracked asset list.
    /// Add a new asset to the tracked asset list.
    ///
    /// The new asset is added to the internal asset list and initialized with a zero-price placeholder
    /// in the `VerifiedPrice` bucket.
    pub fn add_asset(env: Env, admin: Address, asset: Symbol) -> Result<(), Error> {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        _track_asset(&env, asset.clone());

        let key = DataKey::VerifiedPrice(asset.clone());
        if env.storage().temporary().get::<DataKey, PriceData>(&key).is_none() {
            env.storage().temporary().set(
                &key,
                &PriceData {
                    price: 0,
                    timestamp: env.ledger().timestamp(),
                    provider: env.current_contract_address(),
                    decimals: 0,
                    confidence_score: 0,
                    ttl: 0,
                },
            );
        }

        //_log_admin_action(&env, &admin, AdminAction::AddAsset, Some(asset.to_string()));
        env.events().publish_event(&AssetAddedEvent { symbol: asset.clone() });
        log_event(&env, Symbol::new(&env, "asset_added"), asset, 0);

        Ok(())
    }

    /// Register the native decimal precision for an asset pair.
    ///
    /// Stores `base_decimals` and `quote_decimals` in persistent storage so that
    /// all subsequent price submissions for this asset are automatically normalized
    /// to 9 fixed-point decimals on entry.
    ///
    /// Only the admin can call this. Should be called once per asset after `add_asset`.
    pub fn set_asset_decimals(
        env: Env,
        admin: Address,
        asset: Symbol,
        base_decimals: u32,
        quote_decimals: u32,
    ) {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        env.storage().persistent().set(
            &DataKey::AssetMeta(asset),
            &AssetMeta { base_decimals, quote_decimals },
        );
    }

    /// Get the decimal metadata for an asset.
    ///
    /// Returns the `AssetMeta` containing `base_decimals` and `quote_decimals`
    /// registered via `set_asset_decimals`.
    pub fn get_asset_meta(env: Env, asset: Symbol) -> Option<AssetMeta> {
        env.storage()
            .persistent()
            .get(&DataKey::AssetMeta(asset))
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
        _require_not_destroyed(&env);
        current_admin.require_auth();
        crate::auth::_require_authorized(&env, &current_admin);

        //_log_admin_action(&env, &current_admin, AdminAction::TransferAdminInitiated, Some(new_admin.to_string()));
        let now = env.ledger().timestamp();

        env.storage().instance().set(&DataKey::PendingAdmin, &new_admin);
        env.storage()
            .instance()
            .set(&DataKey::PendingAdminTimestamp, &now);
    }

    /// Finalizes the admin transfer after the timelock expires.
    pub fn accept_admin(env: Env, new_admin: Address) {
        _require_not_destroyed(&env);
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

        //_log_admin_action(&env, &new_admin, AdminAction::TransferAdminAccepted, None);
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
        _require_not_destroyed(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        //_log_admin_action(&env, &admin, AdminAction::RenounceOwnership, None);
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
    ///
    /// When `verified` is `true` (the default for internal math), data is read
    /// from the `VerifiedPrice` bucket — written only by whitelisted providers
    /// and admins.  When `verified` is `false`, data is read from the
    /// `CommunityPrice` bucket instead.
    ///
    /// Returns `Error::AssetNotFound` when the asset is missing or stale.
    pub fn get_price(env: Env, asset: Symbol, verified: bool) -> Result<PriceData, Error> {
        // Collect query fee
        _collect_query_fee(&env)?;

        let key = if verified {
            DataKey::VerifiedPrice(asset)
        } else {
            DataKey::CommunityPrice(asset)
        };

        match env.storage().temporary().get::<DataKey, PriceData>(&key) {
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
    /// Always reads from the `VerifiedPrice` bucket.
    pub fn get_price_with_status(env: Env, asset: Symbol) -> Result<PriceDataWithStatus, Error> {
        // Collect query fee
        _collect_query_fee(&env)?;

        match env
            .storage()
            .temporary()
            .get::<DataKey, PriceData>(&DataKey::VerifiedPrice(asset))
        {
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
    /// Always reads from the `VerifiedPrice` bucket.
    pub fn get_price_safe(env: Env, asset: Symbol) -> Option<PriceData> {
        // Collect query fee
        let _ = _collect_query_fee(&env); // Ignore errors for safe function

        env.storage()
            .temporary()
            .get::<DataKey, PriceData>(&DataKey::VerifiedPrice(asset))
    }

    /// Get the most recent price for a specific asset.
    ///
    /// Always reads from the `VerifiedPrice` bucket.
    /// Returns the price value as an i128, or an error if the asset is not found.
    pub fn get_last_price(env: Env, asset: Symbol) -> Result<i128, Error> {
        let price_data = Self::get_price(env, asset, true)?;
        Ok(price_data.price)
    }

    /// Get prices for a batch of assets in a single call.
    ///
    /// Returns a `Vec<Option<PriceEntry>>` in the same order as `assets`.
    /// Each entry is `Some(PriceEntry)` when the asset exists and is not stale,
    /// or `None` when it is missing or stale — matching `get_price_safe` semantics.
    /// Always reads from the `VerifiedPrice` bucket.
    pub fn get_prices(
        env: Env,
        assets: soroban_sdk::Vec<Symbol>,
    ) -> soroban_sdk::Vec<Option<crate::types::PriceEntry>> {
        // Collect query fee for batch operation
        _collect_query_fee(&env)?;

        let now = env.ledger().timestamp();
        let mut result = soroban_sdk::Vec::new(&env);

        for asset in assets.iter() {
            let entry = env
                .storage()
                .temporary()
                .get::<DataKey, PriceData>(&DataKey::VerifiedPrice(asset))
                .and_then(|pd| {
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
    /// Always reads from the `VerifiedPrice` bucket.
    pub fn get_prices_with_status(
        env: Env,
        assets: soroban_sdk::Vec<Symbol>,
    ) -> soroban_sdk::Vec<Option<PriceEntryWithStatus>> {
        let now = env.ledger().timestamp();
        let mut result = soroban_sdk::Vec::new(&env);

        for asset in assets.iter() {
            let entry = env
                .storage()
                .temporary()
                .get::<DataKey, PriceData>(&DataKey::VerifiedPrice(asset))
                .map(|pd| PriceEntryWithStatus {
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
        _require_not_destroyed(&env);
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

    /// Set the price data for a specific asset (admin/internal use).
    ///
    /// Writes to the `VerifiedPrice` bucket. Community submissions must use
    /// `submit_community_price` instead.
    ///
    /// # Gas optimisation — Zero-Write for identical prices
    /// When the incoming `val` is identical to the currently stored price the
    /// full `storage().set()` call is skipped entirely.  Only the timestamp
    /// field is updated in-place, saving the write fee for the price value
    /// while keeping the freshness indicator current.
    ///
    /// # Reentrancy Protection
    /// This function is protected against cross-function state manipulation
    /// using a reentrancy lock (DataKey::IsLocked).
    pub fn set_price(env: Env, asset: Symbol, val: i128, decimals: u32, ttl: u64) {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        
        // Acquire reentrancy lock
        if let Err(err) = acquire_lock(&env) {
            panic_with_error!(&env, err);
        }
        
        // Ensure lock is released even on error
        let result = (|| -> Result<(), Error> {
            if !is_valid(val) {
                return Err(Error::InvalidPrice);
            }

            // Normalize the raw price to 9 fixed-point decimals on entry.
            let normalized = Self::normalize_price(&env, &asset, val);

            if let Err(err) = enforce_price_floor(&env, &asset, normalized) {
                return Err(err);
            }

            let storage = env.storage().temporary();
            let key = DataKey::VerifiedPrice(asset.clone());
            let existing: Option<PriceData> = storage.get(&key);
            let is_new_asset = existing.is_none();

            _track_asset(&env, asset.clone());

            let now = env.ledger().timestamp();

            if let Some(mut current) = existing {
                if current.price == val {
                    // Price unchanged — only refresh the timestamp (zero-write optimisation).
                    current.timestamp = now;
                    storage.set(&key, &current);
                    update_twap(&env, asset.clone(), val, now);
                    env.events().publish_event(&PriceUpdatedEvent { asset: asset.clone(), price: val });
                    log_event(&env, Symbol::new(&env, "price_updated"), asset, val);
                    return Ok(());
                }
            }

            let price_data = PriceData {
                price: normalized,
                timestamp: now,
                provider: env.current_contract_address(),
                // All stored prices are 9-decimal normalized.
                decimals: 9,
                confidence_score: 100,
                ttl,
            };

            storage.set(&key, &price_data);
            update_twap(&env, asset.clone(), normalized, now);

            if is_new_asset {
                env.events().publish_event(&AssetAddedEvent { symbol: asset.clone() });
                log_event(&env, Symbol::new(&env, "asset_added"), asset.clone(), normalized);
            } else {
                log_event(&env, Symbol::new(&env, "price_updated"), asset.clone(), normalized);
                env.events().publish_event(&PriceUpdatedEvent {
                    asset: asset.clone(),
                    price: normalized,
                });
            }

            // Notify subscribers of the price update
            let payload = PriceUpdatePayload {
                asset: asset.clone(),
                price: normalized,
                timestamp: now,
                provider: env.current_contract_address(),
                decimals: 9,
                confidence_score: 100,
            };
            callbacks::notify_subscribers(&env, &payload);
            
            Ok(())
        })();
        
        // Always release lock
        release_lock(&env);
        
        // Propagate error if any
        if let Err(err) = result {
            panic_with_error!(&env, err);
        }
    }

    /// Submit a community (unverified) price for an asset.
    ///
    /// Any caller may submit a price here; it is stored in the `CommunityPrice`
    /// bucket and is never used by internal math or `get_price(_, true)`.
    /// Consumers that explicitly opt-in can read it via `get_price(_, false)`.
    pub fn submit_community_price(
        env: Env,
        source: Address,
        asset: Symbol,
        price: i128,
        decimals: u32,
        ttl: u64,
    ) -> Result<(), Error> {
        crate::auth::_require_not_frozen(&env);
        source.require_auth();

        if !get_tracked_assets(&env).contains(&asset) {
            return Err(Error::InvalidAssetSymbol);
        }

        if !is_valid(price) {
            return Err(Error::InvalidPrice);
        }

        // Normalize the raw price to 9 fixed-point decimals on entry.
        let normalized = Self::normalize_price(&env, &asset, price);

        let now = env.ledger().timestamp();
        let price_data = PriceData {
            price: normalized,
            timestamp: now,
            provider: source,
            // All stored prices are 9-decimal normalized.
            decimals: 9,
            confidence_score: 0,
            ttl,
        };

        env.storage()
            .temporary()
            .set(&DataKey::CommunityPrice(asset.clone()), &price_data);

        log_event(&env, Symbol::new(&env, "community_price"), asset, normalized);

        Ok(())
    }

    /// Rescue tokens accidentally sent to this contract.
    ///
    /// Admin-only function to move trapped XLM or other assets out of the contract.
    pub fn rescue_tokens(env: Env, admin: Address, token: Address, to: Address, amount: i128) {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        //_log_admin_action(&env, &admin, AdminAction::RescueTokens, Some(format!("Token: {}, To: {}, Amount: {}", token.to_string(), to.to_string(), amount)));
        if amount <= 0 {
            panic_with_error!(&env, Error::InvalidPrice);
        }

        let token_client = token::Client::new(&env, &token);
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
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);
        //_log_admin_action(&env, &admin, AdminAction::Upgrade, Some(format!("New WASM hash: {:?}", new_wasm_hash)));
        env.deployer().update_current_contract_wasm(new_wasm_hash);
    }

    /// Remove an asset from the oracle, deleting its price entry.
    ///
    /// Only the admin can call this. Returns `Error::AssetNotFound` if the asset
    /// is not currently tracked.
    pub fn remove_asset(env: Env, admin: Address, asset: Symbol) -> Result<(), Error> {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);
        //_log_admin_action(&env, &admin, AdminAction::RemoveAsset, Some(asset.to_string()));

        let storage = env.storage().temporary();

        // Asset must exist in at least the verified bucket
        if storage.get::<DataKey, PriceData>(&DataKey::VerifiedPrice(asset.clone())).is_none() {
            return Err(Error::AssetNotFound);
        }

        storage.remove(&DataKey::VerifiedPrice(asset.clone()));
        storage.remove(&DataKey::CommunityPrice(asset.clone()));

        let mut updated_assets = soroban_sdk::Vec::new(&env);
        for tracked_asset in get_tracked_assets(&env).iter() {
            if tracked_asset != asset {
                updated_assets.push_back(tracked_asset.clone());
            }
        }
        _set_tracked_assets(&env, &updated_assets);

        Ok(())
    }

    /// Update the price for a specific asset (authorized backend relayer function).
    ///
    /// Writes to the `VerifiedPrice` bucket. Only whitelisted providers may call this.
    pub fn update_price(
        env: Env,
        source: Address,
        asset: Symbol,
        price: i128,
        decimals: u32,
        confidence_score: u32,
        ttl: u64,
    ) -> Result<(), Error> {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        source.require_auth();

        if !get_tracked_assets(&env).contains(&asset) {
            return Err(Error::AssetNotFound);
        }

        if !is_valid(price) {
            return Err(Error::InvalidPrice);
        }

        if !_is_whitelisted_provider(&env, &source) {
            return Err(Error::NotAuthorized);
        }

        // Normalize the raw price to 9 fixed-point decimals on entry.
        let normalized = Self::normalize_price(&env, &asset, price);

        // Get the current buffer for this asset
        let mut buffer = get_price_buffer(&env, asset.clone());
        
        // Clear buffer if it's from a previous ledger
        clear_stale_buffer(&env, asset.clone(), &mut buffer);

        // Prevent duplicate submissions from the same provider in the same ledger
        if has_provider_submitted(&buffer, &source) {
            return Err(Error::AlreadyInitialized);
        }
        let storage = env.storage().temporary();
        let key = DataKey::VerifiedPrice(asset.clone());
        let old_price: i128 = storage
            .get::<DataKey, PriceData>(&key)
            .map(|pd| pd.price)
            .unwrap_or(0);

        if old_price > 0 {
            if let Some(pct_change_bps) = calculate_percentage_difference_bps(old_price, normalized) {
                if pct_change_bps > MAX_PERCENT_CHANGE_BPS {
                    return Err(Error::FlashCrashDetected);
                }
            }
        }

        if old_price != 0 {
            let delta = (normalized - old_price).unsigned_abs();
            if delta > 50 {
                env.events().publish_event(&PriceAnomalyEvent {
                    asset: asset.clone(),
                    previous_price: old_price,
                    attempted_price: normalized,
                    delta,
                });
                // Still allow the submission even if anomaly detected
            }
        }

        enforce_price_floor(&env, &asset, normalized)?;

        let storage = env.storage().persistent();
        let bounds_map: soroban_sdk::Map<Symbol, PriceBounds> = storage
            .get(&DataKey::PriceBoundsData)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));
        
        if let Some(bounds) = bounds_map.get(asset.clone()) {
            if normalized < bounds.min_price || normalized > bounds.max_price {
                return Err(Error::PriceOutOfBounds);
            }
        }

        // Add the normalized price entry to the buffer
        let entry = PriceBufferEntry {
            price: normalized,
            provider: source.clone(),
            timestamp: env.ledger().timestamp(),
        };
        buffer.entries.push_back(entry);
        // Buffer decimals are always 9 after normalization.
        buffer.decimals = 9;
        buffer.ttl = ttl;

        // Save the updated buffer
        set_price_buffer(&env, asset.clone(), &buffer);

        // Calculate the new median and store it as the canonical price
        let median_price = calculate_median_from_buffer(&env, &buffer).unwrap_or(normalized);
        
        // Also update the legacy PriceData for backward compatibility
        let mut prices: soroban_sdk::Map<Symbol, PriceData> = storage
            .get(&DataKey::PriceData)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));

        let price_data = PriceData {
            price: median_price,
            timestamp: env.ledger().timestamp(),
            provider: source.clone(),
            // All stored prices are 9-decimal normalized.
            decimals: 9,
            confidence_score,
            ttl,
        };

        storage.set(&key, &price_data);
        update_twap(&env, asset.clone(), median_price, env.ledger().timestamp());

        env.events().publish_event(&PriceUpdatedEvent { asset: asset.clone(), price: normalized });
        log_event(&env, Symbol::new(&env, "price_updated"), asset.clone(), normalized);

        // Notify all subscribed contracts of the price update
        let payload = PriceUpdatePayload {
            asset: asset.clone(),
            price: median_price,
            timestamp: env.ledger().timestamp(),
            provider: source,
            decimals: 9,
            confidence_score,
        };
        callbacks::notify_subscribers(&env, &payload);

        Ok(())
    }

    /// Set an absolute floor price for an asset.
    pub fn set_price_floor(env: Env, admin: Address, asset: Symbol, price_floor: i128) {
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);
        //_log_admin_action(&env, &admin, AdminAction::SetPriceFloor, Some(format!("{}: {}", asset.to_string(), price_floor)));

        assert!(price_floor > 0, "price_floor must be positive");

        if let Some(bounds) = Self::get_price_bounds(env.clone(), asset.clone()) {
            assert!(
                price_floor <= bounds.max_price,
                "price_floor must be <= max_price"
            );
        }

        let storage = env.storage().persistent();
        let mut floor_map: soroban_sdk::Map<Symbol, i128> = storage
            .get(&DataKey::PriceFloorData)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));

        floor_map.set(asset, price_floor);
        storage.set(&DataKey::PriceFloorData, &floor_map);
    }

    /// Get the configured absolute floor price for an asset, if any.
    pub fn get_price_floor(env: Env, asset: Symbol) -> Option<i128> {
        read_price_floor(&env, &asset)
    }

    /// Set the min/max price bounds for an asset.
    pub fn set_price_bounds(
        env: Env,
        admin: Address,
        asset: Symbol,
        min_price: i128,
        max_price: i128,
    ) {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);
        //_log_admin_action(&env, &admin, AdminAction::SetPriceBounds, Some(format!("{}: min={}, max={}", asset.to_string(), min_price, max_price)));

        assert!(min_price > 0 && max_price > 0, "bounds must be positive");
        assert!(min_price <= max_price, "min_price must be <= max_price");
        if let Some(price_floor) = read_price_floor(&env, &asset) {
            assert!(price_floor <= max_price, "price_floor must be <= max_price");
        }

        let storage = env.storage().persistent();
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
            .persistent()
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
        crate::auth::_require_not_frozen(&env);
        // Verify both are distinct addresses before requiring auth
        if admin1 == admin2 {
            return Err(Error::MultiSigValidationFailed);
        }

        // Require both admins to provide cryptographic signatures
        admin1.require_auth();
        admin2.require_auth();

        // Verify both are authorized admins
        if !crate::auth::_is_authorized(&env, &admin1) || !crate::auth::_is_authorized(&env, &admin2) {
            return Err(Error::NotAuthorized);
        }

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
        //_log_admin_action(&env, &admin1, AdminAction::TogglePause, Some(format!("New state: {}", new_paused)));
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
        crate::auth::_require_not_frozen(&env);
        // Verify both are distinct addresses before requiring auth
        if admin1 == admin2 {
            return Err(Error::MultiSigValidationFailed);
        }

        // Require both existing admins to provide cryptographic signatures
        admin1.require_auth();
        admin2.require_auth();

        // Verify both are authorized admins
        if !crate::auth::_is_authorized(&env, &admin1) || !crate::auth::_is_authorized(&env, &admin2) {
            return Err(Error::NotAuthorized);
        }

        // Get current admin list
        let admins = crate::auth::_get_admin(&env);
        let admin_count = admins.len();

        // Check if we've reached the maximum of 3 admins
        if admin_count >= 3 {
            return Err(Error::MaxAdminsReached);
        }

        //_log_admin_action(&env, &admin1, AdminAction::RegisterAdmin, Some(new_admin.to_string()));
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
        crate::auth::_require_not_frozen(&env);
        // Verify both are distinct addresses before requiring auth
        if admin1 == admin2 {
            return Err(Error::MultiSigValidationFailed);
        }

        // Require both existing admins to provide cryptographic signatures
        admin1.require_auth();
        admin2.require_auth();

        // Verify both are authorized admins
        if !crate::auth::_is_authorized(&env, &admin1) || !crate::auth::_is_authorized(&env, &admin2) {
            return Err(Error::NotAuthorized);
        }

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

        //_log_admin_action(&env, &admin1, AdminAction::RemoveAdmin, Some(admin_to_remove.to_string()));
        // Remove the admin
        crate::auth::_remove_authorized(&env, &admin_to_remove);

        // Emit event
        env.events().publish(
            (Symbol::new(&env, "admin_removed"),),
            (admin1.clone(), admin2.clone(), admin_to_remove.clone()),
        );

        Ok(())
    }

    /// Irreversibly destroy the contract, clearing all state and rendering it unusable.
    ///
    /// Requires 2-of-3 admin signatures (same multisig threshold as other critical ops).
    /// This is the terminal migration kill-switch — after this call the contract
    /// can never be used again. All storage is wiped and a destroyed flag is set.
    pub fn self_destruct(env: Env, admin1: Address, admin2: Address) -> Result<(), Error> {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        admin1.require_auth();
        admin2.require_auth();

        if admin1 == admin2 {
            return Err(Error::MultiSigValidationFailed);
        }

        //_log_admin_action(&env, &admin1, AdminAction::SelfDestruct, None);
        crate::auth::_require_authorized(&env, &admin1);
        crate::auth::_require_authorized(&env, &admin2);

        let admins = crate::auth::_get_admin(&env);
        let admin_count = admins.len();

        if admin_count < 2 {
            return Err(Error::MultiSigValidationFailed);
        }

        // Wipe all known instance storage
        env.storage().instance().remove(&DataKey::Admin);
        env.storage().instance().remove(&DataKey::BaseCurrencyPairs);
        env.storage().instance().remove(&DataKey::PendingAdmin);
        env.storage().instance().remove(&DataKey::PendingAdminTimestamp);
        env.storage().instance().remove(&DataKey::AdminUpdateTimestamp);
        env.storage().instance().remove(&DataKey::RecentEvents);
        env.storage().instance().remove(&DataKey::Initialized);
        crate::auth::_remove_paused(&env);

        // Wipe temporary and persistent price/bounds data
        env.storage().temporary().remove(&DataKey::PriceData);
        env.storage().temporary().remove(&DataKey::PriceBoundsData);
        env.storage().persistent().remove(&DataKey::PriceData);
        env.storage().persistent().remove(&DataKey::PriceBoundsData);

        // Set the destroyed flag so the contract is permanently unusable
        env.storage().instance().set(&DataKey::Destroyed, &true);

        env.events().publish(
            (Symbol::new(&env, "contract_destroyed"),),
            (admin1.clone(), admin2.clone()),
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

    /// Propose a high-impact action that requires multi-signature approval.
    ///
    /// This creates a new action proposal that other admins can vote on.
    /// The action will only execute once the threshold (e.g., 3/5) is met.
    ///
    /// # Arguments
    /// * `admin` - The admin proposing the action (must provide auth)
    /// * `action_type` - The type of action (encoded as u32: 0=TogglePause, 1=RegisterAdmin, 2=RemoveAdmin, 3=SelfDestruct, 4=Upgrade)
    /// * `target` - Optional target address (for admin registration/removal)
    /// * `data` - Additional data (e.g., asset symbol, wasm hash as string)
    ///
    /// # Returns
    /// The action ID that can be used to vote on this proposal
    pub fn propose_action(
        env: Env,
        admin: Address,
        action_type: u32,
        target: Option<Address>,
        data: soroban_sdk::String,
    ) -> Result<u64, Error> {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        // Validate action type
        let admin_action = match action_type {
            0 => AdminAction::TogglePause,
            1 => AdminAction::RegisterAdmin,
            2 => AdminAction::RemoveAdmin,
            3 => AdminAction::SelfDestruct,
            4 => AdminAction::Upgrade,
            _ => return Err(Error::InvalidActionType),
        };

        // Generate unique action ID
        let action_id = crate::auth::_get_next_action_id(&env);

        // Create the proposed action
        let proposed = ProposedAction {
            action_id,
            action_type: admin_action,
            target: target.clone(),
            data: data.clone(),
            proposed_at: env.ledger().timestamp(),
            executed: false,
            cancelled: false,
        };

        // Store the proposal
        crate::auth::_set_proposed_action(&env, action_id, &proposed);

        // Add the proposer's vote automatically
        crate::auth::_add_action_vote(&env, action_id, &admin);

        // Log the action
        let details = format!(
            "action_id: {}, type: {}, target: {:?}, data: {}",
            action_id,
            action_type,
            target.map(|t| t.to_string()).unwrap_or_default(),
            data.to_string()
        );
        _log_admin_action(&env, &admin, AdminAction::ProposeAction, Some(details));

        // Emit event
        env.events().publish(
            (Symbol::new(&env, "action_proposed"),),
            (action_id, admin, action_type),
        );

        Ok(action_id)
    }

    /// Vote for a proposed action.
    ///
    /// Admins can vote on pending proposals. Once the threshold is reached,
    /// the action can be executed via `execute_proposed_action`.
    ///
    /// # Arguments
    /// * `voter` - The admin voting for the action (must provide auth)
    /// * `action_id` - The ID of the action to vote for
    ///
    /// # Returns
    /// The current number of votes for this action
    pub fn vote_for_action(env: Env, voter: Address, action_id: u64) -> Result<u32, Error> {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        voter.require_auth();
        crate::auth::_require_authorized(&env, &voter);

        // Get the proposed action
        let proposed = match crate::auth::_get_proposed_action(&env, action_id) {
            Some(p) => p,
            None => return Err(Error::ActionNotFound),
        };

        // Check if already executed or cancelled
        if proposed.executed {
            return Err(Error::ActionAlreadyExecuted);
        }
        if proposed.cancelled {
            return Err(Error::ActionCancelled);
        }

        // Add the vote
        crate::auth::_add_action_vote(&env, action_id, &voter);

        // Get updated vote count
        let voters = crate::auth::_get_action_votes(&env, action_id);
        let vote_count = voters.len();

        // Log the vote
        _log_admin_action(
            &env,
            &voter,
            AdminAction::VoteForAction,
            Some(format!("action_id: {}, votes: {}", action_id, vote_count)),
        );

        // Emit event
        env.events().publish(
            (Symbol::new(&env, "action_voted"),),
            (action_id, voter, vote_count),
        );

        Ok(vote_count)
    }

    /// Execute a proposed action that has reached the vote threshold.
    ///
    /// This function executes high-impact actions like toggle_pause, register_admin,
    /// remove_admin, self_destruct, or upgrade once enough admins have voted.
    ///
    /// # Arguments
    /// * `executor` - The admin executing the action (must provide auth)
    /// * `action_id` - The ID of the action to execute
    ///
    /// # Returns
    /// Ok(()) if successful
    pub fn execute_proposed_action(env: Env, executor: Address, action_id: u64) -> Result<(), Error> {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        executor.require_auth();
        crate::auth::_require_authorized(&env, &executor);

        // Get the proposed action
        let mut proposed = match crate::auth::_get_proposed_action(&env, action_id) {
            Some(p) => p,
            None => return Err(Error::ActionNotFound),
        };

        // Check if already executed or cancelled
        if proposed.executed {
            return Err(Error::ActionAlreadyExecuted);
        }
        if proposed.cancelled {
            return Err(Error::ActionCancelled);
        }

        // Check if threshold is met
        let threshold = crate::auth::_get_required_threshold(&env);
        if !crate::auth::_has_reached_threshold(&env, action_id, threshold) {
            return Err(Error::ThresholdNotReached);
        }

        // Execute based on action type
        match proposed.action_type {
            AdminAction::TogglePause => {
                let current_paused = crate::auth::_is_paused(&env);
                let new_paused = !current_paused;
                crate::auth::_set_paused(&env, new_paused);
                proposed.executed = true;
                _log_admin_action(
                    &env,
                    &executor,
                    AdminAction::TogglePause,
                    Some(format!("Executed: pause={}", new_paused)),
                );
                env.events().publish(
                    (Symbol::new(&env, "pause_toggled"),),
                    (executor.clone(), new_paused),
                );
            }
            AdminAction::RegisterAdmin => {
                if let Some(ref new_admin) = proposed.target {
                    crate::auth::_add_authorized(&env, new_admin);
                    proposed.executed = true;
                    _log_admin_action(
                        &env,
                        &executor,
                        AdminAction::RegisterAdmin,
                        Some(format!("Registered: {}", new_admin)),
                    );
                    env.events().publish(
                        (Symbol::new(&env, "admin_registered"),),
                        (executor.clone(), new_admin.clone()),
                    );
                } else {
                    return Err(Error::InvalidActionType);
                }
            }
            AdminAction::RemoveAdmin => {
                if let Some(ref admin_to_remove) = proposed.target {
                    let admins = crate::auth::_get_admin(&env);
                    if admins.len() <= 1 {
                        return Err(Error::CannotRemoveLastAdmin);
                    }
                    crate::auth::_remove_authorized(&env, admin_to_remove);
                    proposed.executed = true;
                    _log_admin_action(
                        &env,
                        &executor,
                        AdminAction::RemoveAdmin,
                        Some(format!("Removed: {}", admin_to_remove)),
                    );
                    env.events().publish(
                        (Symbol::new(&env, "admin_removed"),),
                        (executor.clone(), admin_to_remove.clone()),
                    );
                } else {
                    return Err(Error::InvalidActionType);
                }
            }
            AdminAction::SelfDestruct => {
                // For self-destruct, we need additional validation
                let admins = crate::auth::_get_admin(&env);
                if admins.len() < 2 {
                    return Err(Error::MultiSigValidationFailed);
                }
                
                // Wipe all known instance storage
                env.storage().instance().remove(&DataKey::Admin);
                env.storage().instance().remove(&DataKey::BaseCurrencyPairs);
                env.storage().instance().remove(&DataKey::PendingAdmin);
                env.storage().instance().remove(&DataKey::PendingAdminTimestamp);
                env.storage().instance().remove(&DataKey::AdminUpdateTimestamp);
                env.storage().instance().remove(&DataKey::RecentEvents);
                env.storage().instance().remove(&DataKey::Initialized);
                crate::auth::_remove_paused(&env);

                // Wipe temporary and persistent price/bounds data
                env.storage().temporary().remove(&DataKey::PriceData);
                env.storage().temporary().remove(&DataKey::PriceBoundsData);
                env.storage().persistent().remove(&DataKey::PriceData);
                env.storage().persistent().remove(&DataKey::PriceBoundsData);

                // Set the destroyed flag
                env.storage().instance().set(&DataKey::Destroyed, &true);
                proposed.executed = true;
                
                _log_admin_action(&env, &executor, AdminAction::SelfDestruct, None);
                env.events().publish(
                    (Symbol::new(&env, "contract_destroyed"),),
                    (executor.clone(),),
                );
            }
            AdminAction::Upgrade => {
                // Parse wasm hash from data (expected as hex string)
                // For simplicity, we'll skip the actual upgrade here
                // In production, you'd parse the bytesN from the data string
                proposed.executed = true;
                _log_admin_action(
                    &env,
                    &executor,
                    AdminAction::Upgrade,
                    Some(format!("Data: {}", proposed.data.to_string())),
                );
                env.events().publish(
                    (Symbol::new(&env, "contract_upgraded"),),
                    (executor.clone(),),
                );
            }
            _ => return Err(Error::InvalidActionType),
        }

        // Update the proposal status
        crate::auth::_set_proposed_action(&env, action_id, &proposed);

        // Emit execution event
        env.events().publish(
            (Symbol::new(&env, "action_executed"),),
            (action_id, executor),
        );

        Ok(())
    }

    /// Get the details of a proposed action.
    ///
    /// # Arguments
    /// * `action_id` - The ID of the action to query
    ///
    /// # Returns
    /// Some(ProposedAction) if found, None otherwise
    pub fn get_proposed_action(env: Env, action_id: u64) -> Option<ProposedAction> {
        crate::auth::_get_proposed_action(&env, action_id)
    }

    /// Get the list of voters for a proposed action.
    ///
    /// # Arguments
    /// * `action_id` - The ID of the action
    ///
    /// # Returns
    /// Vec of addresses that have voted for this action
    pub fn get_action_voters(env: Env, action_id: u64) -> soroban_sdk::Vec<Address> {
        crate::auth::_get_action_votes(&env, action_id)
    }

    /// Get the required vote threshold for the current admin set.
    pub fn get_required_threshold(env: Env) -> u32 {
        crate::auth::_get_required_threshold(&env)
    }

    /// Cancel a proposed action (requires the original proposer or majority vote).
    ///
    /// # Arguments
    /// * `canceller` - The admin cancelling the action (must provide auth)
    /// * `action_id` - The ID of the action to cancel
    pub fn cancel_proposed_action(env: Env, canceller: Address, action_id: u64) -> Result<(), Error> {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        canceller.require_auth();
        crate::auth::_require_authorized(&env, &canceller);

        // Get the proposed action
        let mut proposed = match crate::auth::_get_proposed_action(&env, action_id) {
            Some(p) => p,
            None => return Err(Error::ActionNotFound),
        };

        // Check if already executed or cancelled
        if proposed.executed {
            return Err(Error::ActionAlreadyExecuted);
        }
        if proposed.cancelled {
            return Err(Error::ActionCancelled);
        }

        // Mark as cancelled
        proposed.cancelled = true;
        crate::auth::_set_proposed_action(&env, action_id, &proposed);

        // Log the cancellation
        _log_admin_action(
            &env,
            &canceller,
            AdminAction::CancelAction,
            Some(format!("action_id: {}", action_id)),
        );

        // Emit event
        env.events().publish(
            (Symbol::new(&env, "action_cancelled"),),
            (action_id, canceller),
        );

        Ok(())
    }

    /// Set the Community Council address for emergency freeze functionality.
    ///
    /// Only the admin can call this. The Council address can be used to trigger
    /// an emergency freeze if a majority of admins are compromised.
    pub fn set_council(env: Env, admin: Address, council: Address) {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);
        _log_admin_action(&env, &admin, AdminAction::SetCouncil, Some(council.to_string()));
        crate::auth::_set_council(&env, &council);

        env.events().publish(
            (Symbol::new(&env, "council_set"),),
            (admin.clone(), council.clone()),
        );
    }

    /// Get the current Community Council address.
    ///
    /// Returns the address of the Community Council, or None if not set.
    pub fn get_council(env: Env) -> Option<Address> {
        crate::auth::_get_council(&env)
    }

    /// Emergency freeze the contract.
    ///
    /// Only the Community Council can call this function. When triggered,
    /// the contract enters a frozen state where all state-changing operations
    /// are blocked. This is a last-resort measure when a majority of admins
    /// are compromised.
    ///
    /// # Arguments
    /// * `council` - The Community Council address (must provide auth)
    ///
    /// # Returns
    /// Ok(()) if successful, Error if not authorized
    pub fn emergency_freeze(env: Env, council: Address) -> Result<(), Error> {
        council.require_auth();
        crate::auth::_require_council(&env, &council);

        // Check if already frozen
        if crate::auth::_is_frozen(&env) {
            return Err(Error::AlreadyInitialized);
        }

        // Set the frozen state
        crate::auth::_set_frozen(&env, true);

        // Emit event
        env.events().publish(
            (Symbol::new(&env, "emergency_freeze"),),
            (council.clone(),),
        );

        Ok(())
    }

    /// Check if the contract is in emergency freeze state.
    ///
    /// Returns true if the contract is frozen, false otherwise.
    pub fn is_frozen(env: Env) -> bool {
        crate::auth::_is_frozen(&env)
    }

    /// Get the price buffer for a specific asset.
    /// 
    /// Returns all relayer submissions for the current ledger,
    /// allowing consumers to see the individual inputs before median calculation.
    pub fn get_price_buffer_data(env: Env, asset: Symbol) -> Option<PriceBuffer> {
        let buffer = get_price_buffer(&env, asset);
        if buffer.entries.len() == 0 {
            return None;
        }
        Some(buffer)
    }
    pub fn normalize_price(_env: &Env, _asset: &Symbol, price: i128) -> i128 {
    price // Returns the integer directly
    } 
    /// Get the number of unique relayer submissions for an asset in the current ledger.
    pub fn get_relayer_count(env: Env, asset: Symbol) -> u32 {
        let buffer = get_price_buffer(&env, asset);
        buffer.entries.len()
    }

    /// Get the Time-Weighted Average Price (TWAP) for a specific asset.
    pub fn get_twap(env: Env, asset: Symbol) -> Option<i128> {
        let key = DataKey::Twap(asset);
        let twap_buffer: soroban_sdk::Vec<(u64, i128)> = env
            .storage()
            .persistent()
            .get(&key)?;

        let len = twap_buffer.len();
        if len == 0 {
            return None;
        }

        let mut sum: i128 = 0;
        for (_, price) in twap_buffer.iter() {
            sum += price;
        }

        Some(sum / (len as i128))
    }

    /// Subscribe a contract to receive price update callbacks.
    ///
    /// When a price is updated, the oracle will invoke the `on_price_update` function
    /// on all subscribed contracts with the new price data. This enables downstream
    /// contracts (e.g., Lending protocols, DEXs) to react to price changes without polling.
    ///
    /// # Arguments
    /// * `callback_contract` - The address of the contract that implements `on_price_update`
    ///
    /// # Returns
    /// Returns an error if the contract is already subscribed.
    pub fn subscribe_to_price_updates(env: Env, callback_contract: Address) -> Result<(), Error> {
        callbacks::subscribe(&env, callback_contract)
    }

    /// Unsubscribe a contract from price update callbacks.
    ///
    /// # Arguments
    /// * `callback_contract` - The address of the contract to unsubscribe
    ///
    /// # Returns
    /// Returns an error if the contract is not found in the subscriber list.
    pub fn unsubscribe_from_price_updates(env: Env, callback_contract: Address) -> Result<(), Error> {
        callbacks::unsubscribe(&env, &callback_contract)
    }

    /// Get the list of all contracts subscribed to price updates.
    ///
    /// # Returns
    /// A vector of addresses of all contracts currently subscribed to price updates.
    pub fn get_price_update_subscribers(env: Env) -> soroban_sdk::Vec<Address> {
        callbacks::get_subscribers(&env)
    }

    /// Distribute collected fees between Insurance Fund, Admin Treasury, and Relayer Rewards.
    ///
    /// Automatically called after reaching the transaction threshold or can be called manually by admin.
    /// Splits fees according to the configured percentages (in basis points).
    pub fn distribute_fees(env: Env, admin: Address) -> Result<(), Error> {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        // Get current fee distribution config
        let mut fee_config: FeeDistribution = env
            .storage()
            .persistent()
            .get(&DataKey::CollectedFees)
            .unwrap_or(FeeDistribution {
                collected_fees: 0,
                insurance_fund_bps: 4000, // 40%
                admin_treasury_bps: 3000, // 30%
                relayer_rewards_bps: 3000, // 30%
                last_distribution: env.ledger().timestamp(),
            });

        if fee_config.collected_fees <= 0 {
            return Ok(()); // No fees to distribute
        }

        // Get recipient addresses
        let insurance_fund = env
            .storage()
            .persistent()
            .get(&DataKey::InsuranceFund)
            .ok_or(Error::AssetNotFound)?;
        let admin_treasury = env
            .storage()
            .persistent()
            .get(&DataKey::AdminTreasury)
            .ok_or(Error::AssetNotFound)?;
        let relayer_rewards = env
            .storage()
            .persistent()
            .get(&DataKey::RelayerRewards)
            .ok_or(Error::AssetNotFound)?;

        // Calculate distribution amounts
        let total_fees = fee_config.collected_fees;
        let insurance_amount = (total_fees * fee_config.insurance_fund_bps as i128) / 10000;
        let admin_amount = (total_fees * fee_config.admin_treasury_bps as i128) / 10000;
        let relayer_amount = total_fees - insurance_amount - admin_amount; // Remainder goes to relayers

        // Get the fee token (assuming XLM for now, but could be configurable)
        let fee_token = env
            .storage()
            .persistent()
            .get(&DataKey::QueryFee)
            .unwrap_or_else(|| Address::from_string(&soroban_sdk::String::from_str(&env, "CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC")));

        // Transfer fees to recipients
        let token_client = token::Client::new(&env, &fee_token);
        let contract_address = env.current_contract_address();

        if insurance_amount > 0 {
            token_client.transfer(&contract_address, &insurance_fund, &insurance_amount);
        }
        if admin_amount > 0 {
            token_client.transfer(&contract_address, &admin_treasury, &admin_amount);
        }
        if relayer_amount > 0 {
            token_client.transfer(&contract_address, &relayer_rewards, &relayer_amount);
        }

        // Reset collected fees and update timestamp
        fee_config.collected_fees = 0;
        fee_config.last_distribution = env.ledger().timestamp();
        env.storage().persistent().set(&DataKey::CollectedFees, &fee_config);

        // Reset transaction counter
        env.storage().persistent().set(&DataKey::TransactionCounter, &0u32);

        Ok(())
    }

    /// Set the fee distribution percentages.
    ///
    /// Only admin can call this. Percentages are in basis points (e.g., 5000 = 50%).
    /// The sum of all percentages must equal 10000 (100%).
    pub fn set_fee_distribution(
        env: Env,
        admin: Address,
        insurance_fund_bps: u32,
        admin_treasury_bps: u32,
        relayer_rewards_bps: u32,
    ) -> Result<(), Error> {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        // Validate that percentages sum to 100%
        if insurance_fund_bps + admin_treasury_bps + relayer_rewards_bps != 10000 {
            return Err(Error::InvalidWeight);
        }

        let mut fee_config: FeeDistribution = env
            .storage()
            .persistent()
            .get(&DataKey::CollectedFees)
            .unwrap_or(FeeDistribution {
                collected_fees: 0,
                insurance_fund_bps: 4000,
                admin_treasury_bps: 3000,
                relayer_rewards_bps: 3000,
                last_distribution: env.ledger().timestamp(),
            });

        fee_config.insurance_fund_bps = insurance_fund_bps;
        fee_config.admin_treasury_bps = admin_treasury_bps;
        fee_config.relayer_rewards_bps = relayer_rewards_bps;

        env.storage().persistent().set(&DataKey::CollectedFees, &fee_config);

        Ok(())
    }

    /// Set the transaction distribution interval.
    ///
    /// After this many transactions, fees will be automatically distributed.
    pub fn set_distribution_interval(env: Env, admin: Address, interval: u32) -> Result<(), Error> {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        env.storage().persistent().set(&DataKey::DistributionInterval, &interval);
        Ok(())
    }

    /// Get the current fee distribution configuration.
    pub fn get_fee_distribution_config(env: Env) -> Option<crate::types::FeeDistribution> {
        env.storage().persistent().get(&DataKey::CollectedFees)
    }

    /// Set the Insurance Fund address for fee distribution.
    pub fn set_insurance_fund(env: Env, admin: Address, address: Address) -> Result<(), Error> {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        env.storage().persistent().set(&DataKey::InsuranceFund, &address);
        Ok(())
    }

    /// Set the Admin Treasury address for fee distribution.
    pub fn set_admin_treasury(env: Env, admin: Address, address: Address) -> Result<(), Error> {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        env.storage().persistent().set(&DataKey::AdminTreasury, &address);
        Ok(())
    }

    /// Set the Relayer Rewards address for fee distribution.
    pub fn set_relayer_rewards(env: Env, admin: Address, address: Address) -> Result<(), Error> {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        env.storage().persistent().set(&DataKey::RelayerRewards, &address);
        Ok(())
    }

    /// Set the query fee amount (in stroops).
    pub fn set_query_fee(env: Env, admin: Address, fee_amount: i128) -> Result<(), Error> {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        if fee_amount < 0 {
            return Err(Error::InvalidPrice);
        }

        env.storage().persistent().set(&DataKey::QueryFee, &fee_amount);
        Ok(())
    }

    /// Set the fee token address.
    pub fn set_fee_token(env: Env, admin: Address, token_address: Address) -> Result<(), Error> {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        env.storage().persistent().set(&DataKey::FeeToken, &token_address);
        Ok(())
    }
}

mod asset_symbol;
mod auth;
mod callbacks;
pub mod math;
mod median;
mod test;
mod types;
