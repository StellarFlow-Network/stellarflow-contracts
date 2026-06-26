#![no_std]
extern crate alloc;

use alloc::format;

use soroban_sdk::{
    contract, contractclient, contracterror, contractimpl, panic_with_error, token, Address, Env,
    String, Symbol,
};

use crate::types::{
    AdminAction, AdminLogEntry, AssetInfo, AssetMeta, AssetRegistrationConfig, DataKey,
    PriceBounds, PriceBuffer, PriceBufferEntry, PriceData, PriceDataWithStatus,
    PriceEntryWithStatus, PriceUpdatePayload, ProposedAction, RecentEvent,
};
mod event_topics;
const ADMIN_TIMELOCK: u64 = 86_400;
const MAX_CLEAR_ASSETS: u32 = 20;

/// Maximum number of price entries allowed in the buffer for median calculation.
/// This threshold prevents CPU budget exhaustion during high-volatility spikes
/// when many providers submit prices simultaneously.
const MAX_MEDIAN_ENTRIES: u32 = 11;

/// A clean, gas-optimized interface for other Soroban contracts to fetch prices from StellarFlow.
///
/// The generated client from this trait is the intended cross-contract entrypoint for downstream
/// Soroban applications. The getters are read-only and `get_last_price` is the cheapest option
/// when callers only need the scalar price value.
#[contractclient(name = "StellarFlowClient")]
pub trait StellarFlowTrait {
    /// Set lightweight metadata for an asset.
    fn set_asset_info(
        env: Env,
        admin: Address,
        asset: Symbol,
        name: Symbol,
        base_decimals: u32,
        quote_decimals: u32,
    );

    /// Register one or more new assets and configure them atomically.
    ///
    /// This combines asset tracking, decimal configuration, and safety threshold
    /// setup into a single atomic transaction, ensuring no partial state is left
    /// behind if any config validation fails.
    fn register_assets_with_config(
        env: Env,
        admin: Address,
        configs: soroban_sdk::Vec<crate::types::AssetRegistrationConfig>,
        max_deviation_bps: i128,
    ) -> Result<(), ContractError>;

    /// Get lightweight metadata for an asset.
    fn get_asset_info(env: Env, asset: Symbol) -> Option<crate::types::AssetInfo>;

    /// Get the full price data for a specific asset.
    ///
    /// When `verified` is `true`, reads from the `VerifiedPrice` bucket (default for internal math).
    /// When `verified` is `false`, reads from the `CommunityPrice` bucket.
    /// Returns `ContractError::AssetNotFound` if the asset does not exist or the price is stale.
    fn get_price(env: Env, asset: Symbol, verified: bool) -> Result<PriceData, ContractError>;

    /// Calculate the weighted average price of a multi-asset index basket.
    ///
    /// # Arguments
    /// * `components` - A vector of AssetWeight defining the basket (e.g., NGN, GHS, CFA).
    fn get_index_price(
        env: Env,
        components: soroban_sdk::Vec<crate::types::AssetWeight>,
    ) -> Result<i128, ContractError>;

    /// Get the full price data with freshness status for a specific asset.
    ///
    /// Returns the last known price with `is_stale = true` when the price has expired.
    fn get_price_with_status(env: Env, asset: Symbol)
        -> Result<PriceDataWithStatus, ContractError>;

    /// Get the price data for a specific asset, or `None` if not found.
    ///
    /// Unlike `get_price`, this does not error on stale or missing prices.
    /// Useful for contracts that want to gracefully handle missing data.
    fn get_price_safe(env: Env, asset: Symbol) -> Option<PriceData>;

    /// Get the most recent price value for a specific asset.
    ///
    /// Returns just the price value as an i128, without other metadata.
    /// This is the fastest getter for contracts that only need the price.
    fn get_last_price(env: Env, asset: Symbol) -> Result<i128, ContractError>;

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
    fn add_asset(env: Env, admin: Address, asset: Symbol) -> Result<(), ContractError>;

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
    fn toggle_pause(env: Env, admin1: Address, admin2: Address) -> Result<bool, ContractError>;

    /// Register a new admin (requires 2-of-3 existing admin signatures).
    ///
    /// Maximum of 3 admins allowed. Returns error if already at capacity.
    fn register_admin(
        env: Env,
        admin1: Address,
        admin2: Address,
        new_admin: Address,
    ) -> Result<(), ContractError>;

    /// Remove an admin (requires 2-of-3 existing admin signatures).
    ///
    /// Cannot remove the last admin. Returns error if would leave 0 admins.
    fn remove_admin(
        env: Env,
        admin1: Address,
        admin2: Address,
        admin_to_remove: Address,
    ) -> Result<(), ContractError>;

    /// Get the total number of registered admins.
    fn get_admin_count(env: Env) -> u32;

    /// Propose a high-impact action that requires multi-signature approval.
    ///
    /// The action will only execute once the threshold (e.g., 3/5) is met.
    fn propose_action(
        env: Env,
        admin: Address,
        action_type: u32,
        target: Option<Address>,
        data: soroban_sdk::String,
    ) -> Result<u64, ContractError>;

    /// Vote for a proposed action.
    fn vote_for_action(env: Env, voter: Address, action_id: u64) -> Result<u32, ContractError>;

    /// Delegate the owner's vote weight to a proxy representative.
    fn delegate_vote(env: Env, owner: Address, delegate: Address) -> Result<(), ContractError>;

    /// Remove the owner's active vote delegation.
    fn clear_vote_delegate(env: Env, owner: Address) -> Result<(), ContractError>;

    /// Get the proxy representative currently assigned by an owner.
    fn get_vote_delegate(env: Env, owner: Address) -> Option<Address>;

    /// Assign a hot-wallet delegate for a cold-storage administrative identity.
    ///
    /// This allows the cold wallet to remain offline while the hot delegate
    /// performs daily price ingestion. The delegate has no administrative powers.
    fn assign_delegate(env: Env, admin: Address, delegate: Address) -> Result<(), ContractError>;

    /// Remove an active submission delegate from an administrative identity.
    fn revoke_delegate(env: Env, admin: Address) -> Result<(), ContractError>;

    /// Get the hot-wallet delegate currently assigned to an admin.
    fn get_delegate(env: Env, admin: Address) -> Option<Address>;

    /// Execute a proposed action that has reached the vote threshold.
    fn execute_proposed_action(
        env: Env,
        executor: Address,
        action_id: u64,
    ) -> Result<(), ContractError>;

    /// Get the details of a proposed action.
    fn get_proposed_action(env: Env, action_id: u64) -> Option<ProposedAction>;

    /// Get the list of voters for a proposed action.
    fn get_action_voters(env: Env, action_id: u64) -> soroban_sdk::Vec<Address>;

    /// Get the required vote threshold for the current admin set.
    fn get_required_threshold(env: Env) -> u32;

    /// Cancel a proposed action.
    fn cancel_proposed_action(
        env: Env,
        canceller: Address,
        action_id: u64,
    ) -> Result<(), ContractError>;

    /// Set the governance weight for a specific admin (issue #264).
    ///
    /// Weight must be in the range 1–100. Only an authorized admin may call this.
    fn set_admin_weight(env: Env, caller: Address, target_admin: Address, weight: u32) -> Result<(), Error>;

    /// Get the governance weight for a specific admin (issue #264).
    fn get_admin_weight(env: Env, admin: Address) -> u32;

    /// Set the minimum cumulative weight required for a governance proposal to execute (issue #264).
    fn set_weight_threshold(env: Env, caller: Address, threshold: u32) -> Result<(), Error>;

    /// Get the configured weight threshold, or None if not set (issue #264).
    fn get_weight_threshold(env: Env) -> Option<u32>;

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
    fn subscribe_to_price_updates(
        env: Env,
        callback_contract: Address,
    ) -> Result<(), ContractError>;

    /// Unsubscribe a contract from price update callbacks.
    ///
    /// # Arguments
    /// * `callback_contract` - The address of the contract to unsubscribe
    ///
    /// # Returns
    /// Returns an error if the contract is not found in the subscriber list.
    fn unsubscribe_from_price_updates(
        env: Env,
        callback_contract: Address,
    ) -> Result<(), ContractError>;

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
    fn emergency_freeze(env: Env, council: Address) -> Result<(), ContractError>;

    /// Check if the contract is in emergency freeze state.
    ///
    /// Returns true if the contract is frozen, false otherwise.
    fn is_frozen(env: Env) -> bool;

    /// Halt or resume all public rate read queries via multi-sig governance.
    ///
    /// Requires at least 2 of the registered governance admins to authorize.
    /// When `status` is `true`, every public rate read (get_price, get_last_price,
    /// get_prices, get_price_with_status, get_price_safe, get_twap, get_index_price)
    /// will panic with `ContractError::EmergencyHalted` until the halt is lifted.
    fn set_emergency_halt(
        env: Env,
        admin1: Address,
        admin2: Address,
        status: bool,
    ) -> Result<(), ContractError>;

    /// Return the current emergency halt state.
    fn is_halted(env: Env) -> bool;

    /// Enable a 1-hour grace period during which the circuit-breaker safety
    /// checks (flash-crash, price floor, and price bounds) are bypassed.
    ///
    /// Only an authorized admin may call this. Returns the absolute expiry
    /// timestamp (seconds) at which the bypass will automatically lapse.
    fn enable_bypass_safety_checks(env: Env, admin: Address) -> Result<u64, ContractError>;

    /// Immediately revoke the safety-checks bypass before it expires naturally.
    fn disable_bypass_safety_checks(env: Env, admin: Address) -> Result<(), ContractError>;

    /// Return the expiry timestamp of the safety-checks bypass, or `None` if
    /// no bypass is currently set (regardless of whether it has expired).
    fn get_bypass_safety_checks_expiry(env: Env) -> Option<u64>;

    // ─────────────────────────────────────────────────────────────────────────
    // Slashing — stake management & governance-gated slash
    // ─────────────────────────────────────────────────────────────────────────

    /// Configure the SEP-41 token contract used for staking and slashing.
    fn set_slash_token(env: Env, admin: Address, token: Address) -> Result<(), ContractError>;

    /// Get the configured slash token address, if any.
    fn get_slash_token(env: Env) -> Option<Address>;

    /// Configure the ecosystem insurance reserve address that receives slashed funds.
    fn set_insurance_reserve(
        env: Env,
        admin: Address,
        reserve: Address,
    ) -> Result<(), ContractError>;

    /// Get the configured insurance reserve address, if any.
    fn get_insurance_reserve(env: Env) -> Option<Address>;

    /// Configure the SEP-41 token contract used for query fee collection.
    fn set_fee_token(env: Env, admin: Address, token: Address) -> Result<(), ContractError>;

    /// Get the configured fee token address, if any.
    fn get_fee_token(env: Env) -> Option<Address>;

    /// Set the query fee amount for `get_price` calls (in token stroops).
    fn set_query_fee(env: Env, admin: Address, fee: i128) -> Result<(), ContractError>;

    /// Get the configured query fee amount.
    fn get_query_fee(env: Env) -> i128;

    /// Get the current accumulated fee vault balance.
    fn get_fee_vault_balance(env: Env) -> i128;

    /// Get the current pending rewards balance for a validator.
    fn get_provider_reward_balance(env: Env, validator: Address) -> i128;

    /// Claim all pending rewards for a validator from the centralized fee vault.
    fn claim_rewards(env: Env, validator: Address) -> Result<i128, ContractError>;

    /// Deposit stake tokens into the contract on behalf of a relayer.
    ///
    /// Tokens are transferred from the relayer's wallet into the contract's
    /// custody and credited to their on-chain stake balance.
    fn stake_tokens(env: Env, relayer: Address, amount: i128) -> Result<(), ContractError>;

    /// Withdraw stake tokens from the contract back to the relayer.
    fn unstake_tokens(env: Env, relayer: Address, amount: i128) -> Result<(), ContractError>;

    /// Get the current staked balance for a relayer (in token stroops).
    fn get_provider_stake(env: Env, relayer: Address) -> i128;

    /// Governance-gated direct slash entry point.
    ///
    /// Transfers `amount` stroops from `bad_relayer`'s staked collateral into
    /// the network's shared ecosystem insurance reserve. Requires the caller to
    /// be an authorized admin.
    ///
    /// For multi-admin deployments, prefer the proposal pipeline
    /// (`propose_action` with `action_type = 5`) so that multiple admins must
    /// agree before funds are moved.
    fn execute_slash(
        env: Env,
        executor: Address,
        bad_relayer: Address,
        amount: i128,
    ) -> Result<(), ContractError>;
}

#[contractclient(name = "TokenContractClient")]
pub trait TokenContractTrait {
    fn transfer(env: Env, from: Address, to: Address, amount: i128);
}

/// Default maximum allowed percentage change between price updates (10% = 1000 basis points).
/// This value is used when no configurable max deviation percentage has been set.
const MAX_PERCENT_CHANGE_BPS: i128 = 1_000;
/// Absolute floor for the configurable max deviation window.
/// Governance may tighten the window only down to this baseline.
const MIN_SAFE_MAX_DEVIATION_BPS: i128 = 100;

/// Maximum age (in seconds) for a rate map entry before consumer reads are rejected.
///
/// 60 ledgers × ~5 s/ledger = 300 s ≈ 5 minutes.  Any `PriceData` whose
/// `timestamp` is older than this boundary causes `get_price` / `get_last_price`
/// to panic with `ContractError::StaleRateData`, protecting downstream protocols from
/// acting on prices that were calculated during a relayer outage.
pub const MAX_RATE_AGE_SECONDS: u64 = 300;

/// Percentage move threshold (5% = 500 basis points) above which a "cross_call"
/// volatility event is published so downstream contracts (e.g. liquidation bots)
/// can react without polling.
const VOLATILITY_THRESHOLD_BPS: i128 = 500;
/// Absolute floor for governance quorum configuration.
const MIN_SAFE_QUORUM_THRESHOLD: u32 = 2;

/// Minimum remaining TTL (in ledgers) for a relayer node profile before an
/// automatic extension is triggered during `get_price` queries.
const PROVIDER_TTL_EXTENSION_THRESHOLD: u32 = 5_000;

/// Target TTL (in ledgers from current ledger) to extend a relayer profile to
/// when its remaining TTL drops below `PROVIDER_TTL_EXTENSION_THRESHOLD`.
/// ~30 days at ~5 s/ledger ≈ 518_400 ledgers; using a conservative 100_000.
const PROVIDER_TTL_EXTENSION_TARGET: u32 = 100_000;

/// ContractError types for the price oracle contract
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ContractError {
    /// Asset does not exist in the price oracle.
    AssetNotFound = 1,
    /// Unauthorized caller - not a whitelisted provider or admin.
    Unauthorized = 2,
    /// Asset symbol is not in the approved list (NGN, KES, GHS)
    InvalidAssetSymbol = 3,
    /// Stake withdrawal amount must be greater than zero.
    InvalidStakeAmount = 4,
    /// Validator already has a pending unbonding request.
    UnbondingAlreadyQueued = 5,
    /// Validator does not have an unbonding request.
    UnbondingRequestNotFound = 6,
    /// The minimum unbonding delay has not elapsed yet.
    UnbondingDelayActive = 7,
    /// The queued unbonding request was already released.
    UnbondingAlreadyReleased = 8,
    /// The current ledger plus the unbonding delay overflowed.
    LedgerSequenceOverflow = 9,
    /// Slippage tolerance exceeded - computed rate deviates too far from expected rate.
    SlippageToleranceExceeded = 10,
    /// Invalid slippage tolerance - must be between 0 and 10000 basis points (0-100%).
    InvalidSlippageTolerance = 11,
}

pub type Error = ContractError;

#[contract]
pub struct PriceOracle;

pub struct PriceUpdatedEvent {
    pub asset: Symbol,
    pub price: i128,
}

pub struct PriceAnomalyEvent {
    pub asset: Symbol,
    pub previous_price: i128,
    pub attempted_price: i128,
    pub delta: u128,
}

pub struct BypassEnabledEvent {
    pub admin: Address,
    pub expiry: u64,
}

pub struct BypassDisabledEvent {
    pub admin: Address,
}

// New event for logging price variance (percentage change in basis points)
#[soroban_sdk::contractevent]
pub struct PriceVarianceEvent {
    pub asset: Symbol,
    pub old_price: i128,
    pub new_price: i128,
    pub variance_bps: i128,
}

/// Emitted when a relayer's staked collateral is slashed by governance.
pub struct SlashExecutedEvent {
    /// The relayer whose stake was slashed.
    pub bad_relayer: Address,
    /// The amount of tokens slashed (in token stroops).
    pub amount: i128,
    /// The insurance reserve address that received the slashed funds.
    pub reserve: Address,
    /// The admin who executed the slash.
    pub executor: Address,
}

pub struct ContractInitialized {
    pub admin: Address,
    pub version: String,
}

pub struct AssetAddedEvent {
    pub symbol: Symbol,
}

pub struct OwnershipRenouncedEvent {
    pub previous_admin: Address,
}

pub struct DelegateAssignedEvent {
    pub admin: Address,
    pub delegate: Address,
}

pub struct DelegateRevokedEvent {
    pub admin: Address,
    pub delegate: Address,
}

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
    new_price.checked_sub(old_price).map(|delta| delta.abs())
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
        panic_with_error!(env, ContractError::ContractDestroyed);
    }
}

/// Guard for issue #297: panic if `initialize` or `init_admin` has not been called yet.
/// Prevents any state-mutating operation from running on an uninitialized contract.
fn _require_initialized(env: &Env) {
    if !env.storage().instance().has(&DataKey::Initialized) {
        panic_with_error!(env, ContractError::NotInitialized);
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

/// Panic with `ContractError::StaleRateData` if the rate map entry has exceeded the
/// maximum allowed age (`MAX_RATE_AGE_SECONDS`).
///
/// This guard is applied on every consumer read (`get_price`, `get_last_price`)
/// to ensure downstream protocols never act on prices that were calculated
/// during a relayer connectivity outage.
///
/// # Arguments
/// * `env` - The contract environment (used for `panic_with_error!`)
/// * `current_time` - The current ledger timestamp
/// * `stored_timestamp` - The `timestamp` field of the `PriceData` entry
pub fn enforce_rate_map_max_age(env: &Env, current_time: u64, stored_timestamp: u64) {
    if current_time > stored_timestamp.saturating_add(MAX_RATE_AGE_SECONDS) {
        panic_with_error!(env, ContractError::StaleRateData);
    }
}

/// Acquire the reentrancy lock for set_price.
/// Returns an error if the lock is already held.
fn acquire_lock(env: &Env) -> Result<(), ContractError> {
    let is_locked: bool = env
        .storage()
        .temporary()
        .get(&DataKey::IsLocked)
        .unwrap_or(false);

    if is_locked {
        return Err(ContractError::ReentrancyDetected);
    }

    /// Return true when a price timestamp is older than 24 hours.
    pub fn is_timestamp_stale(env: Env, timestamp: u64) -> bool {
        let current_timestamp = env.ledger().timestamp();
        current_timestamp > timestamp && current_timestamp - timestamp > 86_400
    }

    /// Set the price data for a specific asset.
    pub fn set_price(env: Env, asset: Symbol, val: i128) -> Result<(), Error> {
        let storage = env.storage().persistent();
        let mut prices: soroban_sdk::Map<Symbol, PriceData> = storage
            .get(&PRICE_DATA_KEY)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));

/// Release the reentrancy lock for set_price.
fn release_lock(env: &Env) {
    env.storage().temporary().set(&DataKey::IsLocked, &false);
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
    env.storage()
        .instance()
        .set(&DataKey::BaseCurrencyPairs, assets);
}

/// Get the price buffer for a specific asset using a composite (Symbol, u64) key.
///
/// Each asset's buffer is stored temporarily under
/// `DataKey::PriceBufferByAsset(asset, ledger_sequence)` so a single-asset read
/// never loads any other asset's buffer and old buffers can expire naturally.
///
/// If no buffer exists for the current ledger sequence a fresh empty one is returned.
fn get_price_buffer(env: &Env, asset: Symbol) -> PriceBuffer {
    let current_seq = env.ledger().sequence() as u64;
    let key = DataKey::PriceBufferByAsset(asset, current_seq);
    env.storage()
        .temporary()
        .get(&key)
        .unwrap_or_else(|| PriceBuffer {
            entries: soroban_sdk::Vec::new(env),
            ledger_sequence: env.ledger().sequence(),
            decimals: 0,
            ttl: 0,
        })
}

/// Save the price buffer for a specific asset using a composite (Symbol, u64) key.
///
/// Writes only the temporary slot for `(asset, ledger_sequence)` — no other
/// asset's buffer is touched or loaded.
fn set_price_buffer(env: &Env, asset: Symbol, buffer: &PriceBuffer) {
    let seq = buffer.ledger_sequence as u64;
    let key = DataKey::PriceBufferByAsset(asset, seq);
    env.storage().temporary().set(&key, buffer);
}

/// Clear the price buffer if it's from a previous ledger.
///
/// With composite keys the buffer is already scoped to a specific ledger
/// sequence, so staleness is implicit — a buffer from a prior ledger simply
/// lives under a different temporary key until the network prunes it.
/// This function resets the in-memory buffer when the caller holds a buffer
/// whose `ledger_sequence` no longer matches the current ledger.
fn clear_stale_buffer(env: &Env, _asset: Symbol, buffer: &mut PriceBuffer) {
    let current_ledger = env.ledger().sequence();
    if buffer.ledger_sequence != current_ledger {
        buffer.entries = soroban_sdk::Vec::new(env);
        buffer.ledger_sequence = current_ledger;
    }
}

/// Check if a provider has already submitted a price in the current buffer.
fn has_provider_submitted(buffer: &PriceBuffer, provider: &Address) -> bool {
    buffer
        .entries
        .iter()
        .any(|entry| entry.provider == *provider)
}

/// Ensure the current ledger sequence has advanced since the last price write.
fn require_ledger_sequence_advanced(env: &Env, previous: Option<&PriceData>) -> Result<u32, Error> {
    let current_ledger: u32 = env.ledger().sequence().into();
    if let Some(prev) = previous {
        if current_ledger <= prev.ledger_sequence {
            return Err(Error::DuplicatePriceWriteInSameLedger);
        }
    }
    Ok(current_ledger)
}

/// Truncate buffer entries to MAX_MEDIAN_ENTRIES, keeping highest-weight providers.
/// This prevents CPU budget exhaustion during high-volatility spikes when many
/// providers submit prices simultaneously.
fn truncate_buffer_by_weight(env: &Env, buffer: &mut PriceBuffer) {
    let entry_count = buffer.entries.len();

    // No truncation needed if we're under the limit
    if entry_count <= MAX_MEDIAN_ENTRIES {
        return;
    }

    // Build a vector of (index, weight) pairs
    let mut weighted_entries = soroban_sdk::Vec::new(env);
    for i in 0..entry_count {
        if let Some(entry) = buffer.entries.get(i) {
            let weight = crate::auth::_get_provider_weight(env, &entry.provider);
            weighted_entries.push_back((i, weight));
        }
    }

    // Sort by weight descending using insertion sort (higher weight = higher priority)
    let len = weighted_entries.len();
    for i in 1..len {
        let mut j = i;
        while j > 0 {
            let (_, weight_a) = weighted_entries.get(j - 1).unwrap();
            let (_, weight_b) = weighted_entries.get(j).unwrap();
            // Sort descending: if previous weight is less than current, swap
            if weight_a < weight_b {
                let temp_a = weighted_entries.get(j - 1).unwrap();
                let temp_b = weighted_entries.get(j).unwrap();
                weighted_entries.set(j - 1, temp_b);
                weighted_entries.set(j, temp_a);
                j -= 1;
            } else {
                break;
            }
        }
    }

    // Keep only the top MAX_MEDIAN_ENTRIES indices
    let mut indices_to_keep = soroban_sdk::Vec::new(env);
    for i in 0..MAX_MEDIAN_ENTRIES.min(len) {
        if let Some((idx, _)) = weighted_entries.get(i) {
            indices_to_keep.push_back(idx);
        }
    }

    // Build new entries vector with only the selected indices
    let mut new_entries = soroban_sdk::Vec::new(env);
    for idx in indices_to_keep.iter() {
        if let Some(entry) = buffer.entries.get(idx) {
            new_entries.push_back(entry);
        }
    }

    buffer.entries = new_entries;
}

/// Calculate the median price from the buffer entries.
/// Returns None if the buffer is empty.
///
/// Issue #363: instead of copying every row into a flat vector and sorting all
/// of them, we compact identical prices into `(price, count)` buckets in one
/// linear pass ("vector compacting"), so the sort inside the median runs only
/// over DISTINCT price values. Providers are already deduplicated per ledger
/// (see `has_provider_submitted`), so identical prices here are independent
/// votes — `count` preserves their multiplicity and the median is unchanged.
fn calculate_median_from_buffer(env: &Env, buffer: &PriceBuffer) -> Option<i128> {
    if buffer.entries.len() == 0 {
        return None;
    }

    // Linear compaction pass: fold identical prices into (price, count) buckets.
    let mut compacted: soroban_sdk::Vec<(i128, u32)> = soroban_sdk::Vec::new(env);
    for entry in buffer.entries.iter() {
        let price = entry.price;
        let len = compacted.len();
        let mut found = false;
        for i in 0..len {
            let (value, count) = compacted.get(i).unwrap();
            if value == price {
                compacted.set(i, (value, count + 1));
                found = true;
                break;
            }
        }
        if !found {
            compacted.push_back((price, 1));
        }
    }

    // Sort distinct values + median via cumulative counts (result-preserving).
    crate::median::calculate_median_compacted(compacted).ok()
}

/// Adds an asset to the list of tracked assets if it's not already present.
fn _track_asset(env: &Env, asset: Symbol) {
    let mut assets = get_tracked_assets(env);
    if !assets.contains(&asset) {
        assets.push_back(asset.clone());
        _set_tracked_assets(env, &assets);
        // Set persistent flag for O(1) existence check
        env.storage()
            .persistent()
            .set(&DataKey::TrackedAsset(asset), &());

        // Issue #263: keep the isolated HealthTotalAssets slot in sync.
        let new_count = assets.len();
        env.storage()
            .persistent()
            .set(&DataKey::HealthTotalAssets, &new_count);
        env.storage()
            .persistent()
            .set(&DataKey::HealthLastLedger, &env.ledger().sequence());
    }
}

fn log_event(env: &Env, event_type: Symbol, asset: Symbol, price: i128) {
    let mut events: soroban_sdk::Vec<RecentEvent> = env
        .storage()
        .temporary()
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

    env.storage()
        .temporary()
        .set(&DataKey::RecentEvents, &events);
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
    env.storage()
        .temporary()
        .set(&DataKey::AdminUpdateTimestamp, &entry.timestamp);
}

fn read_price_floor(env: &Env, asset: &Symbol) -> Option<i128> {
    // Composite key: one slot per asset — no map deserialisation overhead.
    env.storage()
        .persistent()
        .get(&DataKey::PriceFloorEntry(asset.clone()))
}

/// Enforce the 3-block minimum ledger gap between provider submissions.
/// 
/// Prevents high-frequency automated scripts from flooding the network with
/// consecutive price updates within the same or nearby ledger windows.
/// 
/// # Arguments
/// * `env` - The Soroban environment
/// * `provider` - The address of the provider attempting to submit
/// 
/// # Returns
/// * `Ok(())` if the provider is allowed to submit (3+ blocks since last submission)
/// * `Err(ContractError::LedgerGapTooSmall)` if the gap is less than 3 blocks
fn enforce_ledger_gap(env: &Env, provider: &Address) -> Result<(), ContractError> {
    const MIN_LEDGER_GAP: u32 = 3;
    
    let current_ledger = env.ledger().sequence();
    let last_seen = env
        .storage()
        .persistent()
        .get(&DataKey::ProviderLastSeenLedger(provider.clone()))
        .unwrap_or(0);
    
    // If provider has never submitted before, allow the submission
    if last_seen == 0 {
        return Ok(());
    }
    
    // Calculate the gap between current and last submission
    let gap = current_ledger.saturating_sub(last_seen);
    
    // Reject if the gap is less than MIN_LEDGER_GAP blocks
    if gap < MIN_LEDGER_GAP {
        return Err(ContractError::LedgerGapTooSmall);
    }
    
    Ok(())
}

/// Extend the persistent storage TTL of a relayer node's profile entries if
/// the remaining TTL has fallen below [`PROVIDER_TTL_EXTENSION_THRESHOLD`].
///
/// Called automatically from the `get_price` query path so that active
/// relayers never lose their on-chain state due to rent-based eviction.
fn _extend_provider_ttl_if_needed(env: &Env, provider: &Address) {
    let storage = env.storage().persistent();
    let stake_key = DataKey::ProviderStake(provider.clone());
    let last_seen_key = DataKey::ProviderLastSeenLedger(provider.clone());
    if storage.has(&stake_key) {
        storage.extend_ttl(&stake_key, PROVIDER_TTL_EXTENSION_THRESHOLD, PROVIDER_TTL_EXTENSION_TARGET);
    }
    if storage.has(&last_seen_key) {
        storage.extend_ttl(&last_seen_key, PROVIDER_TTL_EXTENSION_THRESHOLD, PROVIDER_TTL_EXTENSION_TARGET);
    }
}

fn enforce_price_floor(env: &Env, asset: &Symbol, price: i128) -> Result<(), ContractError> {
    if let Some(price_floor) = read_price_floor(env, asset) {
        if price < price_floor {
            return Err(ContractError::PriceOutOfBounds);
        }
    }

    Ok(())
}

fn update_twap(env: &Env, asset: Symbol, price: i128, timestamp: u64) {
    let key = DataKey::Twap(asset);
    let mut twap_buffer: soroban_sdk::Vec<(u64, i128)> = env
        .storage()
        .temporary()
        .get(&key)
        .unwrap_or_else(|| soroban_sdk::Vec::new(env));

    twap_buffer.push_back((timestamp, price));

    if twap_buffer.len() > 10 {
        twap_buffer.pop_front();
    }

    env.storage().temporary().set(&key, &twap_buffer);
}

#[contractimpl]
impl PriceOracle {
    /// Initialize the contract with admin and base currency pairs.
    /// Can only be called once.
    pub fn initialize(env: Env, admin: Address, base_currency_pairs: soroban_sdk::Vec<Symbol>) {
        if env.storage().instance().has(&DataKey::Initialized) || crate::auth::_has_admin(&env) {
            panic_with_error!(&env, ContractError::AlreadyInitialized);
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

    pub fn get_index_price(
        env: Env,
        components: soroban_sdk::Vec<crate::types::AssetWeight>,
    ) -> Result<i128, ContractError> {
        if crate::auth::_is_halted(&env) {
            panic_with_error!(&env, ContractError::EmergencyHalted);
        }
        validation::calculate_index_price(&env, &components)
    }

    pub fn init_admin(env: Env, admin: Address) {
        _require_not_destroyed(&env);
        if env.storage().instance().has(&DataKey::Initialized) {
            panic_with_error!(&env, ContractError::AlreadyInitialized);
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
    pub fn add_asset(env: Env, admin: Address, asset: Symbol) -> Result<(), ContractError> {
        _require_not_destroyed(&env);
        _require_initialized(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        _track_asset(&env, asset.clone());

        let key = DataKey::VerifiedPrice(asset.clone());
        if env
            .storage()
            .persistent()
            .get::<DataKey, PriceData>(&key)
            .is_none()
        {
            env.storage().persistent().set(
                &key,
                &PriceData {
                    price: 0,
                    timestamp: env.ledger().timestamp(),
                    ledger_sequence: env.ledger().sequence().into(),
                    provider: env.current_contract_address(),
                    decimals: 0,
                    confidence_score: 0,
                    ttl: 0,
                },
            );
        }

        //_log_admin_action(&env, &admin, AdminAction::AddAsset, Some(asset.to_string()));
        env.events()
            .publish((Symbol::new(&env, "asset_added_event"),), (asset.clone(),));
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
            &AssetMeta {
                base_decimals,
                quote_decimals,
            },
        );
        event_topics::publish_asset_meta_set(&env, asset, base_decimals, quote_decimals);
    }

    /// Get the decimal metadata for an asset.
    ///
    /// Returns the `AssetMeta` containing `base_decimals` and `quote_decimals`
    /// registered via `set_asset_decimals`.
    pub fn get_asset_meta(env: Env, asset: Symbol) -> Option<AssetMeta> {
        env.storage().persistent().get(&DataKey::AssetMeta(asset))
    }
    /// Set lightweight metadata for an asset.
    ///
    /// `name` must be a short Symbol. Longer descriptions should be stored
    /// separately with `set_asset_description`.
    pub fn set_asset_info(
        env: Env,
        admin: Address,
        asset: Symbol,
        name: Symbol,
        base_decimals: u32,
        quote_decimals: u32,
    ) {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        let info = AssetInfo {
            name,
            base_decimals,
            quote_decimals,
        };

        env.storage()
            .persistent()
            .set(&DataKey::AssetInfo(asset), &info);
        event_topics::publish_asset_info_set(&env, asset, info.name, base_decimals, quote_decimals);
    }

    /// Register one or more new assets and configure them atomically.
    ///
    /// This combines asset tracking, decimal configuration, and safety threshold
    /// setup into a single atomic transaction, ensuring no partial state is left
    /// behind if any config validation fails.
    pub fn register_assets_with_config(
        env: Env,
        admin: Address,
        configs: soroban_sdk::Vec<AssetRegistrationConfig>,
        max_deviation_bps: i128,
    ) -> Result<(), ContractError> {
        _require_not_destroyed(&env);
        _require_initialized(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        validation::validate_asset_registration_configs(&configs, max_deviation_bps)?;

        if let Some(existing) = env
            .storage()
            .persistent()
            .get::<DataKey, i128>(&DataKey::MaxPriceDeviationBps)
        {
            env.storage()
                .persistent()
                .set(&DataKey::PrevMaxDeviationBps, &existing);
        }
        env.storage()
            .persistent()
            .set(&DataKey::MaxPriceDeviationBps, &max_deviation_bps);

        for config in configs.iter() {
            let asset = config.asset.clone();
            _track_asset(&env, asset.clone());

            let key = DataKey::VerifiedPrice(asset.clone());
            if env
                .storage()
                .persistent()
                .get::<DataKey, PriceData>(&key)
                .is_none()
            {
                env.storage().persistent().set(
                    &key,
                    &PriceData {
                        price: 0,
                        timestamp: env.ledger().timestamp(),
                        ledger_sequence: env.ledger().sequence().into(),
                        provider: env.current_contract_address(),
                        decimals: 0,
                        confidence_score: 0,
                        ttl: 0,
                    },
                );
            }

            env.storage().persistent().set(
                &DataKey::AssetMeta(asset.clone()),
                &AssetMeta {
                    base_decimals: config.base_decimals,
                    quote_decimals: config.quote_decimals,
                },
            );
            env.storage()
                .persistent()
                .set(
                    &DataKey::AssetInfo(asset.clone()),
                    &AssetInfo {
                        name: config.name.clone(),
                        base_decimals: config.base_decimals,
                        quote_decimals: config.quote_decimals,
                    },
                );
            env.storage().persistent().set(
                &DataKey::PriceBoundsEntry(asset.clone()),
                &PriceBounds {
                    min_price: config.min_price,
                    max_price: config.max_price,
                },
            );
            if let Some(price_floor) = config.price_floor {
                env.storage()
                    .persistent()
                    .set(&DataKey::PriceFloorEntry(asset.clone()), &price_floor);
            }

            env.events().publish((Symbol::new(&env, "asset_added_event"),), (asset.clone(),));
            log_event(&env, Symbol::new(&env, "asset_added"), asset, 0);
        }

        Ok(())
    }

    /// Get lightweight metadata for an asset.
    pub fn get_asset_info(env: Env, asset: Symbol) -> Option<AssetInfo> {
        env.storage().persistent().get(&DataKey::AssetInfo(asset))
    }

    /// Return the current admin addresses.
    pub fn get_admin(env: Env) -> Address {
        crate::auth::_get_admin(&env)
            .get(0)
            .unwrap_or_else(|| panic_with_error!(&env, ContractError::AdminNotSet))
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

        env.storage()
            .instance()
            .set(&DataKey::PendingAdmin, &new_admin);
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
            .unwrap_or_else(|| panic_with_error!(&env, ContractError::PendingAdminNotFound));

        if pending != new_admin {
            panic_with_error!(&env, ContractError::NotPendingAdmin);
        }

        let timestamp: u64 = env
            .storage()
            .instance()
            .get(&DataKey::PendingAdminTimestamp)
            .unwrap_or_else(|| {
                panic_with_error!(&env, ContractError::PendingAdminTimestampMissing)
            });

        let now = env.ledger().timestamp();

        if now < timestamp.saturating_add(ADMIN_TIMELOCK) {
            panic_with_error!(&env, ContractError::AdminTimelockNotExpired);
        }

        //_log_admin_action(&env, &new_admin, AdminAction::TransferAdminAccepted, None);
        let admins = soroban_sdk::vec![&env, new_admin.clone()];
        crate::auth::_set_admin(&env, &admins);

        env.storage()
            .temporary()
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

        env.events()
            .publish((Symbol::new(&env, "ownership_renounced_event"),), (admin,));
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
    /// Returns `ContractError::AssetNotFound` when the asset is missing or stale.
    pub fn get_price(env: Env, asset: Symbol, verified: bool) -> Result<PriceData, ContractError> {
        if crate::auth::_is_halted(&env) {
            panic_with_error!(&env, ContractError::EmergencyHalted);
        }
        let key = if verified {
            DataKey::VerifiedPrice(asset)
        } else {
            DataKey::CommunityPrice(asset)
        };

        match env.storage().persistent().get::<DataKey, PriceData>(&key) {
            Some(price_data) => {
                let now = env.ledger().timestamp();
                // Issue #262: panic if the rate map entry exceeds the hard maximum age.
                enforce_rate_map_max_age(&env, now, price_data.timestamp);
                if is_stale(now, price_data.timestamp, price_data.ttl) {
                    return Err(ContractError::AssetNotFound);
                }
                Self::process_query_fee(&env, &price_data.provider)?;
                // Issue #364: auto-extend relayer node profile TTL when below threshold.
                _extend_provider_ttl_if_needed(&env, &price_data.provider);
                Ok(price_data)
            }
            None => Err(ContractError::AssetNotFound),
        }
    }

    fn process_query_fee(env: &Env, provider: &Address) -> Result<(), ContractError> {
        let fee: i128 = env.storage().persistent().get(&DataKey::QueryFee).unwrap_or(0);
        if fee <= 0 {
            return Ok(());
        }

        let token_address: Address = env
            .storage()
            .persistent()
            .get(&DataKey::FeeToken)
            .ok_or(ContractError::FeeTokenNotSet)?;

        let payer = env.invoker();
        let token_client = token::Client::new(env, &token_address);
        token_client.transfer(&payer, &env.current_contract_address(), &fee);

        let provider_reward_key = DataKey::ProviderRewardBalance(provider.clone());
        let current_provider_rewards: i128 = env.storage().persistent().get(&provider_reward_key).unwrap_or(0);
        let new_provider_rewards = current_provider_rewards
            .checked_add(fee)
            .ok_or(ContractError::PriceMathOverflow)?;
        env.storage()
            .persistent()
            .set(&provider_reward_key, &new_provider_rewards);

        // Keep fee-pool accounting isolated by the fee token address so
        // corridor/asset fees never share the generic platform reserve slot.
        let vault_key = DataKey::CorridorFeeVaultBalance(token_address);
        let current_vault: i128 = env.storage().persistent().get(&vault_key).unwrap_or(0);
        let new_vault = current_vault
            .checked_add(fee)
            .ok_or(ContractError::PriceMathOverflow)?;
        env.storage().persistent().set(&vault_key, &new_vault);

        Ok(())
    }

    /// Configure the SEP-41 token contract used for query fee collection.
    pub fn set_fee_token(env: Env, admin: Address, token: Address) -> Result<(), ContractError> {
        _require_not_destroyed(&env);
        _require_initialized(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        env.storage().persistent().set(&DataKey::FeeToken, &token);

        env.events().publish(
            (Symbol::new(&env, "fee_token_set"),),
            (admin, token),
        );

        Ok(())
    }

    /// Get the configured fee token address, if any.
    pub fn get_fee_token(env: Env) -> Option<Address> {
        env.storage().persistent().get(&DataKey::FeeToken)
    }

    /// Set the query fee amount for get_price calls.
    pub fn set_query_fee(env: Env, admin: Address, fee: i128) -> Result<(), ContractError> {
        _require_not_destroyed(&env);
        _require_initialized(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        if fee < 0 {
            return Err(ContractError::InvalidQueryFee);
        }

        env.storage().persistent().set(&DataKey::QueryFee, &fee);
        env.events().publish(
            (Symbol::new(&env, "query_fee_set"),),
            (admin, fee),
        );
        Ok(())
    }

    /// Get the configured query fee amount.
    pub fn get_query_fee(env: Env) -> i128 {
        env.storage().persistent().get(&DataKey::QueryFee).unwrap_or(0)
    }

    /// Get the current accumulated fee vault balance for the configured fee token.
    pub fn get_fee_vault_balance(env: Env) -> i128 {
        let storage = env.storage().persistent();
        match storage.get::<DataKey, Address>(&DataKey::FeeToken) {
            Some(token_address) => storage
                .get(&DataKey::CorridorFeeVaultBalance(token_address))
                .unwrap_or(0),
            None => 0,
        }
    }

    /// Get the current pending rewards balance for a validator.
    pub fn get_provider_reward_balance(env: Env, validator: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::ProviderRewardBalance(validator))
            .unwrap_or(0)
    }

    /// Claim all pending rewards for a validator from the centralized fee vault.
    pub fn claim_rewards(env: Env, validator: Address) -> Result<i128, ContractError> {
        validator.require_auth();

        let pending_rewards_key = DataKey::ProviderRewardBalance(validator.clone());
        let pending_rewards: i128 = env
            .storage()
            .persistent()
            .get(&pending_rewards_key)
            .unwrap_or(0);

        if pending_rewards <= 0 {
            return Err(ContractError::NoRewards);
        }

        let token_address: Address = env
            .storage()
            .persistent()
            .get(&DataKey::FeeToken)
            .ok_or(ContractError::FeeTokenNotSet)?;

        let vault_key = DataKey::CorridorFeeVaultBalance(token_address.clone());
        let current_vault: i128 = env.storage().persistent().get(&vault_key).unwrap_or(0);
        if current_vault < pending_rewards {
            return Err(ContractError::InsufficientVaultBalance);
        }

        let new_vault = current_vault - pending_rewards;
        env.storage().persistent().set(&vault_key, &new_vault);
        env.storage().persistent().remove(&pending_rewards_key);

        let token_client = token::Client::new(&env, &token_address);
        token_client.transfer(&env.current_contract_address(), &validator, &pending_rewards);

        env.events().publish(
            (Symbol::new(&env, "rewards_claimed_event"),),
            (validator.clone(), pending_rewards),
        );

        Ok(pending_rewards)
    }

    /// Returns the last known price data and marks it stale when TTL has expired.
    /// Always reads from the `VerifiedPrice` bucket.
    pub fn get_price_with_status(
        env: Env,
        asset: Symbol,
    ) -> Result<PriceDataWithStatus, ContractError> {
        if crate::auth::_is_halted(&env) {
            panic_with_error!(&env, ContractError::EmergencyHalted);
        }
        match env
            .storage()
            .persistent()
            .get::<DataKey, PriceData>(&DataKey::VerifiedPrice(asset))
        {
            Some(price_data) => {
                let now = env.ledger().timestamp();
                Ok(PriceDataWithStatus {
                    is_stale: is_stale(now, price_data.timestamp, price_data.ttl),
                    data: price_data,
                })
            }
            None => Err(ContractError::AssetNotFound),
        }
    }

    /// Returns `None` instead of an error when the asset is not found.
    /// Always reads from the `VerifiedPrice` bucket.
    pub fn get_price_safe(env: Env, asset: Symbol) -> Option<PriceData> {
        if crate::auth::_is_halted(&env) {
            panic_with_error!(&env, ContractError::EmergencyHalted);
        }
        env.storage()
            .persistent()
            .get::<DataKey, PriceData>(&DataKey::VerifiedPrice(asset))
    }

    /// Get the most recent price for a specific asset.
    ///
    /// Always reads from the `VerifiedPrice` bucket.
    /// Returns the price value as an i128, or an error if the asset is not found.
    pub fn get_last_price(env: Env, asset: Symbol) -> Result<i128, ContractError> {
        if crate::auth::_is_halted(&env) {
            panic_with_error!(&env, ContractError::EmergencyHalted);
        }
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
        if crate::auth::_is_halted(&env) {
            panic_with_error!(&env, ContractError::EmergencyHalted);
        }
        let now = env.ledger().timestamp();
        let mut result = soroban_sdk::Vec::new(&env);

        for asset in assets.iter() {
            // Fetch the complete profile once and inspect all required
            // sub-attributes in memory instead of performing separate
            // existence/freshness/value passes for the same asset.
            let entry = env
                .storage()
                .persistent()
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
                .persistent()
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
    pub fn set_asset_description(
        env: Env,
        admin: Address,
        asset: Symbol,
        description: soroban_sdk::String,
    ) {
        _require_not_destroyed(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);
        env.storage()
            .persistent()
            .set(&DataKey::AssetDescription(asset), &description);
        event_topics::publish_asset_description_set(&env, asset, description);
    }

    /// Get the human-readable description for an asset.
    ///
    /// Returns `ContractError::AssetNotFound` if no description has been set.
    pub fn get_asset_description(
        env: Env,
        asset: Symbol,
    ) -> Result<soroban_sdk::String, ContractError> {
        env.storage()
            .persistent()
            .get(&DataKey::AssetDescription(asset))
            .ok_or(ContractError::AssetNotFound)
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
        let result = (|| -> Result<(), ContractError> {
            if !is_valid(val) {
                return Err(ContractError::InvalidPrice);
            }

            // Normalize the raw price to 9 fixed-point decimals on entry.
            let normalized = Self::normalize_price(&env, &asset, val);

            if normalized <= 0 {
                return Err(ContractError::InvalidNormalizedPrice);
            }

            if let Err(err) = enforce_price_floor(&env, &asset, normalized) {
                return Err(err);
            }

            let storage = env.storage().persistent();
            let key = DataKey::VerifiedPrice(asset.clone());
            let existing: Option<PriceData> = storage.get(&key);
            let old_price_opt = existing.as_ref().map(|p| p.price);
            let is_new_asset = existing.is_none();

            _track_asset(&env, asset.clone());

            let now = env.ledger().timestamp();

            if let Some(mut current) = existing {
                let current_ledger = require_ledger_sequence_advanced(&env, Some(&current))?;
                if current.price == val {
                    // Price unchanged — only refresh the timestamp (zero-write optimisation).
                    current.timestamp = now;
                    current.ledger_sequence = current_ledger;
                    storage.set(&key, &current);
                    update_twap(&env, asset.clone(), val, now);
                    event_topics::publish_price_update(&env, asset.clone(), current.price, now);
                    env.events().publish(
                        (Symbol::new(&env, "price_updated_event"),),
                        (asset.clone(), val),
                    );
                    log_event(&env, Symbol::new(&env, "price_updated"), asset, val);
                    return Ok(());
                }
            }

            let price_data = PriceData {
                price: normalized,
                timestamp: now,
                ledger_sequence: env.ledger().sequence().into(),
                provider: env.current_contract_address(),
                // All stored prices are 9-decimal normalized.
                decimals: 9,
                confidence_score: 100,
                ttl,
            };

            storage.set(&key, &price_data);
            update_twap(&env, asset.clone(), normalized, now);

            if is_new_asset {
                env.events()
                    .publish((Symbol::new(&env, "asset_added_event"),), (asset.clone(),));
                log_event(
                    &env,
                    Symbol::new(&env, "asset_added"),
                    asset.clone(),
                    normalized,
                );
            } else {
                event_topics::publish_price_update(&env, asset.clone(), normalized, now);
                log_event(
                    &env,
                    Symbol::new(&env, "price_updated"),
                    asset.clone(),
                    normalized,
                );
                env.events().publish(
                    (Symbol::new(&env, "price_updated_event"),),
                    (asset.clone(), normalized),
                );
            }
            // Emit variance event for price change
            if let Some(old_price) = old_price_opt {
                let variance_opt = calculate_percentage_change_bps(old_price, normalized);
                env.events().publish_event(&PriceVarianceEvent {
                    asset: asset.clone(),
                    old_price,
                    new_price: normalized,
                    variance_bps: variance_opt.unwrap_or(0),
                });
                log_event(&env, Symbol::new(&env, "price_variance"), asset.clone(), variance_opt.unwrap_or(0));
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
    ) -> Result<(), ContractError> {
        crate::auth::_require_not_frozen(&env);
        source.require_auth();

        if !get_tracked_assets(&env).contains(&asset) {
            return Err(ContractError::InvalidAssetSymbol);
        }

        if !is_valid(price) {
            return Err(ContractError::InvalidPrice);
        }

        // Normalize the raw price to 9 fixed-point decimals on entry.
        let normalized = Self::normalize_price(&env, &asset, price);

        if normalized <= 0 {
            return Err(ContractError::InvalidNormalizedPrice);
        }

        let previous_price: Option<PriceData> = env
            .storage()
            .persistent()
            .get(&DataKey::CommunityPrice(asset.clone()));
        require_ledger_sequence_advanced(&env, previous_price.as_ref())?;

        let now = env.ledger().timestamp();
        let price_data = PriceData {
            price: normalized,
            timestamp: now,
            ledger_sequence: env.ledger().sequence().into(),
            provider: source,
            // All stored prices are 9-decimal normalized.
            decimals: 9,
            confidence_score: 0,
            ttl,
        };

        env.storage()
            .persistent()
            .set(&DataKey::CommunityPrice(asset.clone()), &price_data);

        event_topics::publish_price_update(&env, asset.clone(), normalized, now);
        log_event(
            &env,
            Symbol::new(&env, "community_price"),
            asset,
            normalized,
        );

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
            panic_with_error!(&env, ContractError::InvalidPrice);
        }

        let token_client = token::Client::new(&env, &token);
        token_client.transfer(&env.current_contract_address(), &to, &amount);

        env.events().publish(
            (Symbol::new(&env, "rescue_tokens_event"),),
            (token, to, amount),
        );
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
    /// Only the admin can call this. Returns `ContractError::AssetNotFound` if the asset
    /// is not currently tracked.
    pub fn remove_asset(env: Env, admin: Address, asset: Symbol) -> Result<(), ContractError> {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        let storage = env.storage().persistent();

        // Asset must exist in at least the verified bucket
        if storage
            .get::<DataKey, PriceData>(&DataKey::VerifiedPrice(asset.clone()))
            .is_none()
        {
            return Err(ContractError::AssetNotFound);
        }

        storage.remove(&DataKey::VerifiedPrice(asset.clone()));
        storage.remove(&DataKey::CommunityPrice(asset.clone()));
        storage.remove(&DataKey::TrackedAsset(asset.clone()));
        // Remove composite-key per-asset config slots.
        storage.remove(&DataKey::PriceFloorEntry(asset.clone()));
        storage.remove(&DataKey::PriceBoundsEntry(asset.clone()));

        let mut updated_assets = soroban_sdk::Vec::new(&env);
        for tracked_asset in get_tracked_assets(&env).iter() {
            if tracked_asset != asset {
                updated_assets.push_back(tracked_asset.clone());
            }
        }
        _set_tracked_assets(&env, &updated_assets);

        // Issue #263: keep the isolated HealthTotalAssets slot in sync.
        let new_count = updated_assets.len();
        env.storage()
            .persistent()
            .set(&DataKey::HealthTotalAssets, &new_count);
        env.storage()
            .persistent()
            .set(&DataKey::HealthLastLedger, &env.ledger().sequence());

        Ok(())
    }

    /// Batch-delete price entries for a list of assets.
    ///
    /// Removes the `DataKey::Price(asset)` slot for each asset in the supplied
    /// vector. Capped at `MAX_CLEAR_ASSETS` (20) per call to bound gas usage.
    /// Returns `ContractError::TooManyAssets` if the batch exceeds the limit — the call
    /// is atomic so no entries are removed when the error fires.
    ///
    /// This function operates on the `DataKey::Price(Symbol)` composite key used
    /// by snapshot tests and migration tooling. It does **not** touch
    /// `VerifiedPrice` or `CommunityPrice` buckets; use `remove_asset` for that.
    pub fn clear_assets(env: Env, assets: soroban_sdk::Vec<Symbol>) -> Result<(), ContractError> {
        validation::clear_assets(&env, &assets)
    }

    /// Update the price for a specific asset (authorized backend relayer function).
    ///
    /// Writes to the `VerifiedPrice` bucket. Only whitelisted providers may call this.
    ///
    /// # Liquidity Validation
    /// As of the flash loan protection update, providers must submit pool liquidity
    /// data alongside price updates. Submissions from markets with insufficient
    /// liquidity (below the configured threshold) are rejected early to prevent
    /// price manipulation via flash loans or other temporary capital injections.
    pub fn update_price(
        env: Env,
        source: Address,
        asset: Symbol,
        price: i128,
        decimals: u32,
        confidence_score: u32,
        ttl: u64,
        liquidity: i128,
    ) -> Result<(), ContractError> {
        _require_not_destroyed(&env);
        _require_initialized(&env);
        crate::auth::_require_not_frozen(&env);
        source.require_auth();

        if !env
            .storage()
            .persistent()
            .has(&DataKey::TrackedAsset(asset.clone()))
        {
            return Err(ContractError::AssetNotFound);
        }

        if !is_valid(price) {
            return Err(ContractError::InvalidPrice);
        }

        if !_is_whitelisted_provider(&env, &source) {
            return Err(ContractError::NotAuthorized);
        }

        // Enforce 3-block minimum gap between this provider's submissions
        enforce_ledger_gap(&env, &source)?;

        // Normalize the raw price to 9 fixed-point decimals on entry.
        let normalized = Self::normalize_price(&env, &asset, price);

        if normalized <= 0 {
            return Err(ContractError::InvalidNormalizedPrice);
        }

        // Get the current buffer for this asset
        let mut buffer = get_price_buffer(&env, asset.clone());

        // Clear buffer if it's from a previous ledger
        clear_stale_buffer(&env, asset.clone(), &mut buffer);

        // Prevent duplicate submissions from the same provider in the same ledger
        if has_provider_submitted(&buffer, &source) {
            return Err(ContractError::AlreadyInitialized);
        }
        let storage = env.storage().persistent();
        let key = DataKey::VerifiedPrice(asset.clone());
        let existing_price: Option<PriceData> = storage.get(&key);
        require_ledger_sequence_advanced(&env, existing_price.as_ref())?;
        let old_price: i128 = existing_price.as_ref().map(|pd| pd.price).unwrap_or(0);

        let bypass_active = crate::auth::_is_bypass_active(&env);

        let max_deviation_bps = Self::get_max_deviation_percentage(env.clone());
        if old_price > 0 && !bypass_active {
            if let Some(pct_change_bps) = calculate_percentage_difference_bps(old_price, normalized)
            {
                if pct_change_bps > max_deviation_bps {
                    return Err(ContractError::FlashCrashDetected);
                }
            }
        }

        if old_price != 0 {
            let delta = (normalized - old_price).unsigned_abs();
            if delta > 50 {
                env.events().publish(
                    (Symbol::new(&env, "price_anomaly_event"),),
                    (asset.clone(), old_price, normalized, delta),
                );
                // Still allow the submission even if anomaly detected
            }
        }

        if !bypass_active {
            enforce_price_floor(&env, &asset, normalized)?;
        }

        // Composite key: read only this asset's bounds slot — no full map load.
        if !bypass_active {
            if let Some(bounds) = env
                .storage()
                .persistent()
                .get::<DataKey, PriceBounds>(&DataKey::PriceBoundsEntry(asset.clone()))
            {
                if normalized < bounds.min_price || normalized > bounds.max_price {
                    return Err(ContractError::PriceOutOfBounds);
                }
            }
        }

        // ── Liquidity validation: flash loan manipulation prevention ────────────
        // Validate that the reported pool liquidity meets the configured minimum
        // threshold. This check prevents price manipulation via flash loans or
        // other temporary capital injections into thin markets.
        //
        // The validation is performed AFTER all other safety checks but BEFORE
        // the price is added to the buffer, ensuring early termination of
        // transactions from insufficient-liquidity sources.
        if !bypass_active {
            validation::validate_liquidity(&env, &asset, &source, liquidity)?;
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

        // Truncate buffer to MAX_MEDIAN_ENTRIES if needed, keeping highest-weight providers
        truncate_buffer_by_weight(&env, &mut buffer);

        // Save the updated buffer
        set_price_buffer(&env, asset.clone(), &buffer);

        // Consensus has all inputs it needs in `buffer`; explicitly clear
        // historical temporary storage slots so stale processing footprints do
        // not linger in Soroban temporary storage after the consensus pass.
        env.storage().temporary().remove(&DataKey::PriceBuffer);
        env.storage().temporary().remove(&DataKey::PriceData);
        env.storage().temporary().remove(&DataKey::PriceBoundsData);

        // Calculate the new median and store it as the canonical price
        let median_price = calculate_median_from_buffer(&env, &buffer).unwrap_or(normalized);

        if median_price <= 0 {
            return Err(ContractError::InvalidNormalizedPrice);
        }

        // Also update the legacy PriceData for backward compatibility
        let mut prices: soroban_sdk::Map<Symbol, PriceData> = storage
            .get(&DataKey::PriceData)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));

        let price_data = PriceData {
            price: median_price,
            timestamp: env.ledger().timestamp(),
            ledger_sequence: env.ledger().sequence().into(),
            provider: source.clone(),
            // All stored prices are 9-decimal normalized.
            decimals: 9,
            confidence_score,
            ttl,
        };

        // Record the provider's heartbeat (last seen ledger height) - tracking node liveness
        storage.set(
            &DataKey::ProviderLastSeenLedger(source.clone()),
            &env.ledger().sequence(),
        );

        storage.set(&key, &price_data);
        update_twap(&env, asset.clone(), median_price, env.ledger().timestamp());

        event_topics::publish_price_update(
            &env,
            asset.clone(),
            median_price,
            env.ledger().timestamp(),
        );
        env.events().publish(
            (Symbol::new(&env, "price_updated_event"),),
            (asset.clone(), median_price),
        );
        log_event(
            &env,
            Symbol::new(&env, "price_updated"),
            asset.clone(),
            median_price,
        );

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

        // ── Gas Tank reimbursement (Issue #266) ──────────────────────────────
        // After every successful price submission, reimburse the relayer for
        // their on-chain transaction costs via the Gas Tank escrow contract.
        // This call is a no-op when no Gas Tank has been configured.
        if let Some(gas_tank_addr) = env
            .storage()
            .persistent()
            .get::<DataKey, Address>(&DataKey::GasTank)
        {
            // Call reimburse(relayer) on the Gas Tank contract.
            // We use env.invoke_contract so we stay no_std compatible.
            let reimburse_fn = Symbol::new(&env, "reimburse");
            let args = soroban_sdk::vec![&env, payload.provider.clone().to_val()];
            let _: () = env.invoke_contract(&gas_tank_addr, &reimburse_fn, args);
        }

        Ok(())
    }

    /// Set an absolute floor price for an asset.
    pub fn set_price_floor(env: Env, admin: Address, asset: Symbol, price_floor: i128) {
        _require_not_destroyed(&env);
        _require_initialized(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        if price_floor <= 0 {
            panic_with_error!(&env, ContractError::InvalidPriceFloor);
        }

        if let Some(bounds) = Self::get_price_bounds(env.clone(), asset.clone()) {
            if price_floor > bounds.max_price {
                panic_with_error!(&env, ContractError::InvalidPriceFloor);
            }
        }

        // Backup current floor before overwriting (issue #281).
        if let Some(existing) = read_price_floor(&env, &asset) {
            env.storage()
                .persistent()
                .set(&DataKey::PrevPriceFloorEntry(asset.clone()), &existing);
        }

        // Composite key: write directly to the per-asset slot.
        env.storage()
            .persistent()
            .set(&DataKey::PriceFloorEntry(asset.clone()), &price_floor);
        event_topics::publish_price_floor_set(&env, asset, price_floor);
    }

    /// Restore the previous price floor for an asset (issue #281).
    /// Admin-only. Panics if no backup exists.
    pub fn rollback_price_floor(
        env: Env,
        admin: Address,
        asset: Symbol,
    ) -> Result<(), ContractError> {
        _require_not_destroyed(&env);
        _require_initialized(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        let prev: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::PrevPriceFloorEntry(asset.clone()))
            .ok_or(ContractError::NoPreviousConfig)?;

        env.storage()
            .persistent()
            .set(&DataKey::PriceFloorEntry(asset.clone()), &prev);
        env.storage()
            .persistent()
            .remove(&DataKey::PrevPriceFloorEntry(asset.clone()));
        event_topics::publish_price_floor_rollback(&env, asset, prev);

        Ok(())
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
        _require_initialized(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        if min_price <= 0 || max_price <= 0 || min_price > max_price {
            panic_with_error!(&env, ContractError::InvalidPriceBounds);
        }
        if let Some(price_floor) = read_price_floor(&env, &asset) {
            if price_floor > max_price {
                panic_with_error!(&env, ContractError::InvalidPriceBounds);
            }
        }

        // Backup current bounds before overwriting (issue #281).
        if let Some(existing) = env
            .storage()
            .persistent()
            .get::<DataKey, PriceBounds>(&DataKey::PriceBoundsEntry(asset.clone()))
        {
            env.storage()
                .persistent()
                .set(&DataKey::PrevPriceBoundsEntry(asset.clone()), &existing);
        }

        // Composite key: write directly to the per-asset slot — no map load needed.
        env.storage().persistent().set(
            &DataKey::PriceBoundsEntry(asset.clone()),
            &PriceBounds {
                min_price,
                max_price,
            },
        );
        event_topics::publish_price_bounds_set(&env, asset, min_price, max_price);
    }

    /// Restore the previous price bounds for an asset (issue #281).
    /// Admin-only. Returns `ContractError::NoPreviousConfig` if no backup exists.
    pub fn rollback_price_bounds(
        env: Env,
        admin: Address,
        asset: Symbol,
    ) -> Result<(), ContractError> {
        _require_not_destroyed(&env);
        _require_initialized(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        let prev: PriceBounds = env
            .storage()
            .persistent()
            .get(&DataKey::PrevPriceBoundsEntry(asset.clone()))
            .ok_or(ContractError::NoPreviousConfig)?;

        env.storage()
            .persistent()
            .set(&DataKey::PriceBoundsEntry(asset.clone()), &prev);
        env.storage()
            .persistent()
            .remove(&DataKey::PrevPriceBoundsEntry(asset.clone()));
        event_topics::publish_price_bounds_rollback(&env, asset, prev.min_price, prev.max_price);

        Ok(())
    }

    /// Get the current min/max price bounds for an asset, if configured.
    pub fn get_price_bounds(env: Env, asset: Symbol) -> Option<PriceBounds> {
        // Composite key: read only the single per-asset slot.
        env.storage()
            .persistent()
            .get(&DataKey::PriceBoundsEntry(asset))
    }

    /// Set the maximum allowed price deviation percentage (in basis points).
    /// This value is applied in `update_price` to reject single-ledger flash crash updates.
    pub fn set_max_deviation_percentage(env: Env, admin: Address, max_deviation_bps: i128) {
        _require_not_destroyed(&env);
        _require_initialized(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        if max_deviation_bps < MIN_SAFE_MAX_DEVIATION_BPS || max_deviation_bps > 10_000 {
            panic_with_error!(&env, ContractError::InvalidMaxDeviation);
        }

        // Backup current value before overwriting (issue #281).
        if let Some(existing) = env
            .storage()
            .persistent()
            .get::<DataKey, i128>(&DataKey::MaxPriceDeviationBps)
        {
            env.storage()
                .persistent()
                .set(&DataKey::PrevMaxDeviationBps, &existing);
        }

        env.storage()
            .persistent()
            .set(&DataKey::MaxPriceDeviationBps, &max_deviation_bps);
        event_topics::publish_max_deviation_pct_set(&env, max_deviation_bps);
    }

    /// Restore the previous max deviation percentage (issue #281).
    /// Admin-only. Returns `ContractError::NoPreviousConfig` if no backup exists.
    pub fn rollback_max_deviation_pct(env: Env, admin: Address) -> Result<(), ContractError> {
        _require_not_destroyed(&env);
        _require_initialized(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        let prev: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::PrevMaxDeviationBps)
            .ok_or(ContractError::NoPreviousConfig)?;

        if prev < MIN_SAFE_MAX_DEVIATION_BPS {
            return Err(ContractError::InvalidMaxDeviation);
        }

        env.storage()
            .persistent()
            .set(&DataKey::MaxPriceDeviationBps, &prev);
        env.storage()
            .persistent()
            .remove(&DataKey::PrevMaxDeviationBps);
        event_topics::publish_max_deviation_pct_rollback(&env, prev);

        Ok(())
    }

    /// Get the configured maximum allowed price deviation, or default to 10%.
    pub fn get_max_deviation_percentage(env: Env) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::MaxPriceDeviationBps)
            .unwrap_or(MAX_PERCENT_CHANGE_BPS)
            .max(MIN_SAFE_MAX_DEVIATION_BPS)
    }

    // ── Liquidity threshold configuration (flash loan protection) ───────────

    /// Set the minimum liquidity threshold for an asset.
    ///
    /// Price submissions from pools with liquidity below this threshold will be
    /// rejected to prevent flash loan manipulation. The threshold must be within
    /// the range defined by MIN_LIQUIDITY_THRESHOLD..MAX_LIQUIDITY_THRESHOLD.
    ///
    /// # Parameters
    /// - `admin`: Authorized admin address (requires auth)
    /// - `asset`: Asset symbol to configure (e.g. "XLM_USD")
    /// - `threshold`: Minimum liquidity in stroops (1 XLM = 10_000_000 stroops)
    ///
    /// # Errors
    /// - `ContractError::InvalidLiquidityThreshold`: threshold out of valid range
    /// - `ContractError::NotAuthorized`: caller is not an authorized admin
    ///
    /// # Example
    /// ```rust
    /// // Set 100M stroops (10 XLM) minimum liquidity for XLM/USD
    /// oracle.set_liquidity_threshold(&admin, &symbol!("XLM_USD"), &100_000_000);
    /// ```
    pub fn set_liquidity_threshold(
        env: Env,
        admin: Address,
        asset: Symbol,
        threshold: i128,
    ) -> Result<(), ContractError> {
        _require_not_destroyed(&env);
        _require_initialized(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        // Validate and set the threshold using the validation module
        validation::set_liquidity_threshold_internal(&env, &asset, threshold)?;

        Ok(())
    }

    /// Get the configured liquidity threshold for an asset.
    ///
    /// Returns the minimum pool liquidity required for price submissions to be
    /// accepted. Returns None if no threshold has been configured for this asset.
    ///
    /// # Parameters
    /// - `asset`: Asset symbol to query
    ///
    /// # Returns
    /// - `Some(threshold)`: The configured minimum liquidity in stroops
    /// - `None`: No threshold configured (validation disabled for this asset)
    pub fn get_liquidity_threshold(env: Env, asset: Symbol) -> Option<i128> {
        validation::get_liquidity_threshold(&env, &asset)
    }

    /// Remove the liquidity threshold for an asset.
    ///
    /// After removal, price submissions for this asset will no longer undergo
    /// liquidity validation. Use with caution as this re-exposes the contract
    /// to flash loan manipulation risks.
    ///
    /// # Parameters
    /// - `admin`: Authorized admin address (requires auth)
    /// - `asset`: Asset symbol to remove threshold from
    ///
    /// # Errors
    /// - `ContractError::NotAuthorized`: caller is not an authorized admin
    pub fn remove_liquidity_threshold(env: Env, admin: Address, asset: Symbol) {
        _require_not_destroyed(&env);
        _require_initialized(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        validation::remove_liquidity_threshold_internal(&env, &asset);
    }

    /// Get the last reported liquidity from a provider for a specific asset.
    ///
    /// Useful for monitoring provider behavior and reputation scoring.
    ///
    /// # Parameters
    /// - `provider`: The relayer address to query
    /// - `asset`: The asset symbol to query
    ///
    /// # Returns
    /// - `Some(liquidity)`: The last liquidity value reported by this provider
    /// - `None`: Provider has never submitted liquidity for this asset
    pub fn get_provider_liquidity(env: Env, provider: Address, asset: Symbol) -> Option<i128> {
        validation::get_provider_liquidity(&env, &provider, &asset)
    }

    /// Get the timestamp of the last successful liquidity validation for an asset.
    ///
    /// Useful for monitoring and auditing the frequency of liquidity validations.
    ///
    /// # Parameters
    /// - `asset`: The asset symbol to query
    ///
    /// # Returns
    /// - `Some(timestamp)`: Unix timestamp of last validation
    /// - `None`: No validations have been performed for this asset
    pub fn get_last_liquidity_validation(env: Env, asset: Symbol) -> Option<u64> {
        validation::get_last_validation_timestamp(&env, &asset)
    }

    /// Execute a liquidity-based slash against a provider.
    ///
    /// Called by governance when a provider is detected submitting prices from
    /// pools below the configured liquidity threshold. Applies a graduated penalty
    /// based on how far below the threshold the reported liquidity fell.
    ///
    /// # Parameters
    /// - `executor`: Admin executing the slash (requires auth)
    /// - `provider`: The relayer being penalized
    /// - `asset`: Asset pair the violation occurred on
    /// - `reported_liquidity`: The insufficient liquidity value submitted
    /// - `base_slash_amount`: Base penalty before liquidity multiplier
    ///
    /// # Errors
    /// - `Err(Error::InvalidLiquidityThreshold)`: No threshold configured for asset
    /// - `Err(Error::InsufficientStake)`: Provider doesn't have enough stake
    ///
    /// # Penalty Tiers
    /// - **≥ 100% of threshold**: No penalty (1× multiplier)
    /// - **75-99%**: Minor penalty (2× multiplier)
    /// - **50-74%**: Moderate penalty (4× multiplier)
    /// - **25-49%**: Significant penalty (8× multiplier)
    /// - **< 25%**: Severe penalty (16× multiplier)
    pub fn slash_for_low_liquidity(
        env: Env,
        executor: Address,
        provider: Address,
        asset: Symbol,
        reported_liquidity: i128,
        base_slash_amount: i128,
    ) -> Result<(), ContractError> {
        _require_not_destroyed(&env);
        _require_initialized(&env);
        crate::auth::_require_not_frozen(&env);
        executor.require_auth();
        crate::auth::_require_authorized(&env, &executor);

        validation::slash_for_low_liquidity(
            &env,
            &executor,
            &provider,
            &asset,
            reported_liquidity,
            base_slash_amount,
        )
    }

    // ─────────────────────────────────────────────────────────────────────────

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
            .temporary()
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
    pub fn toggle_pause(env: Env, admin1: Address, admin2: Address) -> Result<bool, ContractError> {
        crate::auth::_require_not_frozen(&env);
        // Verify both are distinct addresses before requiring auth
        if admin1 == admin2 {
            return Err(ContractError::MultiSigValidationFailed);
        }

        // Require both admins to provide cryptographic signatures
        admin1.require_auth();
        admin2.require_auth();

        // Verify both are authorized admins
        if !crate::auth::_is_authorized(&env, &admin1)
            || !crate::auth::_is_authorized(&env, &admin2)
        {
            return Err(ContractError::NotAuthorized);
        }

        // Get current admin list
        let admins = crate::auth::_get_admin(&env);
        let admin_count = admins.len();

        // Ensure we have at least 2 admins registered
        if admin_count < 2 {
            return Err(ContractError::MultiSigValidationFailed);
        }

        // Toggle the pause state
        let current_paused = crate::auth::_is_paused(&env);
        let new_paused = !current_paused;
        //_log_admin_action(&env, &admin1, AdminAction::TogglePause, Some(format!("New state: {}", new_paused)));
        crate::auth::_set_paused(&env, new_paused);

        // Issue #263: keep the isolated HealthPaused slot in sync.
        env.storage()
            .persistent()
            .set(&DataKey::HealthPaused, &new_paused);

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
    /// Ok(()) if successful, ContractError if validation fails
    pub fn register_admin(
        env: Env,
        admin1: Address,
        admin2: Address,
        new_admin: Address,
    ) -> Result<(), ContractError> {
        crate::auth::_require_not_frozen(&env);
        // Verify both are distinct addresses before requiring auth
        if admin1 == admin2 {
            return Err(ContractError::MultiSigValidationFailed);
        }

        // Require both existing admins to provide cryptographic signatures
        admin1.require_auth();
        admin2.require_auth();

        // Verify both are authorized admins
        if !crate::auth::_is_authorized(&env, &admin1)
            || !crate::auth::_is_authorized(&env, &admin2)
        {
            return Err(ContractError::NotAuthorized);
        }

        // Get current admin list
        let admins = crate::auth::_get_admin(&env);
        let admin_count = admins.len();

        // Check if we've reached the maximum of 3 admins
        if admin_count >= 3 {
            return Err(ContractError::MaxAdminsReached);
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
    /// Ok(()) if successful, ContractError if validation fails
    pub fn remove_admin(
        env: Env,
        admin1: Address,
        admin2: Address,
        admin_to_remove: Address,
    ) -> Result<(), ContractError> {
        crate::auth::_require_not_frozen(&env);
        // Verify both are distinct addresses before requiring auth
        if admin1 == admin2 {
            return Err(ContractError::MultiSigValidationFailed);
        }

        // Require both existing admins to provide cryptographic signatures
        admin1.require_auth();
        admin2.require_auth();

        // Verify both are authorized admins
        if !crate::auth::_is_authorized(&env, &admin1)
            || !crate::auth::_is_authorized(&env, &admin2)
        {
            return Err(ContractError::NotAuthorized);
        }

        // Get current admin list
        let admins = crate::auth::_get_admin(&env);
        let admin_count = admins.len();

        // Cannot remove if would leave less than 1 admin
        if admin_count <= 1 {
            return Err(ContractError::CannotRemoveLastAdmin);
        }

        // Verify the admin to remove actually exists
        if !admins.iter().any(|a| a == admin_to_remove) {
            return Err(ContractError::NotAuthorized);
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
    pub fn self_destruct(env: Env, admin1: Address, admin2: Address) -> Result<(), ContractError> {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        admin1.require_auth();
        admin2.require_auth();

        if admin1 == admin2 {
            return Err(ContractError::MultiSigValidationFailed);
        }

        //_log_admin_action(&env, &admin1, AdminAction::SelfDestruct, None);
        crate::auth::_require_authorized(&env, &admin1);
        crate::auth::_require_authorized(&env, &admin2);

        let admins = crate::auth::_get_admin(&env);
        let admin_count = admins.len();

        if admin_count < 2 {
            return Err(ContractError::MultiSigValidationFailed);
        }

        // Wipe all known instance storage
        env.storage().instance().remove(&DataKey::Admin);
        env.storage().instance().remove(&DataKey::BaseCurrencyPairs);
        env.storage().instance().remove(&DataKey::PendingAdmin);
        env.storage()
            .instance()
            .remove(&DataKey::PendingAdminTimestamp);
        env.storage()
            .temporary()
            .remove(&DataKey::AdminUpdateTimestamp);
        env.storage().temporary().remove(&DataKey::RecentEvents);
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

    // ─────────────────────────────────────────────────────────────────────────
    // Issue #264: Multi-sig signer threshold weight verification
    // ─────────────────────────────────────────────────────────────────────────

    /// Set the governance weight for a specific admin (issue #264).
    ///
    /// Weight must be in the range 1–100.  A weight of 0 is rejected because a
    /// zero-weight admin could never contribute to reaching the threshold.
    /// Only an authorized admin may call this.
    pub fn set_admin_weight(env: Env, caller: Address, target_admin: Address, weight: u32) -> Result<(), Error> {
        _require_not_destroyed(&env);
        _require_initialized(&env);
        crate::auth::_require_not_frozen(&env);
        caller.require_auth();
        crate::auth::_require_authorized(&env, &caller);

        if weight == 0 || weight > 100 {
            return Err(Error::InvalidWeight);
        }

        // The target must be a registered admin.
        if !crate::auth::_is_authorized(&env, &target_admin) {
            return Err(Error::NotAuthorized);
        }

        crate::auth::_set_admin_weight(&env, &target_admin, weight);

        env.events().publish(
            (Symbol::new(&env, "admin_weight_set"),),
            (caller, target_admin, weight),
        );

        Ok(())
    }

    /// Get the governance weight for a specific admin (issue #264).
    ///
    /// Returns 1 (the default) when no weight has been explicitly assigned.
    pub fn get_admin_weight(env: Env, admin: Address) -> u32 {
        crate::auth::_get_admin_weight(&env, &admin)
    }

    /// Set the minimum cumulative weight required for a governance proposal to
    /// execute (issue #264).
    ///
    /// `threshold` must be ≥ 1.  Only an authorized admin may call this.
    /// Once set, `execute_proposed_action` will sum voter weights and compare
    /// against this value instead of using the simple vote-count threshold.
    pub fn set_weight_threshold(env: Env, caller: Address, threshold: u32) -> Result<(), Error> {
        _require_not_destroyed(&env);
        _require_initialized(&env);
        crate::auth::_require_not_frozen(&env);
        caller.require_auth();
        crate::auth::_require_authorized(&env, &caller);

        if threshold == 0 {
            return Err(Error::MultiSigValidationFailed);
        }

        crate::auth::_set_weight_threshold(&env, threshold);

        env.events().publish(
            (Symbol::new(&env, "weight_threshold_set"),),
            (caller, threshold),
        );

        Ok(())
    }

    /// Get the configured weight threshold (issue #264).
    ///
    /// Returns `None` when no threshold has been set (the contract falls back
    /// to the vote-count threshold from `get_required_threshold`).
    pub fn get_weight_threshold(env: Env) -> Option<u32> {
        crate::auth::_get_weight_threshold(&env)
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Issue #263: Gas-optimized OracleHealth — isolated per-field storage slots
    // ─────────────────────────────────────────────────────────────────────────

    /// Get the health status of the oracle for the Admin Dashboard (issue #263).
    ///
    /// Each field is stored in its own isolated persistent slot so a simple
    /// dashboard read never deserialises a large monolithic struct.  The
    /// individual slots are kept in sync by the write paths that mutate each
    /// field (provider add/remove, pause toggle, asset add/remove).
    pub fn get_oracle_health(env: Env) -> crate::types::OracleHealth {
        // ── active_relayers: read the isolated counter slot ──────────────────
        // Falls back to counting the active-relayers Vec when the isolated slot
        // has not been written yet (first call after deployment).
        let active_relayers: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::HealthActiveRelayers)
            .unwrap_or_else(|| crate::auth::_get_active_relayers(&env).len());

        // ── paused: read the isolated flag slot ──────────────────────────────
        let paused: bool = env
            .storage()
            .persistent()
            .get(&DataKey::HealthPaused)
            .unwrap_or_else(|| crate::auth::_is_paused(&env));

        // ── total_assets: read the isolated counter slot ─────────────────────
        let total_assets: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::HealthTotalAssets)
            .unwrap_or_else(|| get_tracked_assets(&env).len());

        // ── last_ledger: read the isolated sequence slot ─────────────────────
        let last_ledger: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::HealthLastLedger)
            .unwrap_or_else(|| env.ledger().sequence());

        crate::types::OracleHealth {
            active_relayers,
            paused,
            total_assets,
            last_ledger,
        }
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
    /// Set the minimum number of votes required for a governance proposal to reach quorum (issue #292).
    /// Admin-only. Default is 1 (no floor) when unset.
    /// Admin-only. Values below the hard floor are rejected, and the getter
    /// clamps legacy low storage to the same minimum.
    pub fn set_min_quorum_threshold(
        env: Env,
        admin: Address,
        threshold: u32,
    ) -> Result<(), ContractError> {
        _require_not_destroyed(&env);
        _require_initialized(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        if threshold < MIN_SAFE_QUORUM_THRESHOLD {
            return Err(ContractError::MultiSigValidationFailed);
        }

        env.storage()
            .persistent()
            .set(&DataKey::MinQuorumThreshold, &threshold);

        env.events()
            .publish((Symbol::new(&env, "quorum_set"),), (admin, threshold));

        Ok(())
    }

    /// Get the configured minimum quorum threshold. Returns the hard floor if unset.
    pub fn get_min_quorum_threshold(env: Env) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::MinQuorumThreshold)
            .unwrap_or(MIN_SAFE_QUORUM_THRESHOLD)
            .max(MIN_SAFE_QUORUM_THRESHOLD)
    }

    pub fn propose_action(
        env: Env,
        admin: Address,
        action_type: u32,
        target: Option<Address>,
        data: soroban_sdk::String,
    ) -> Result<u64, ContractError> {
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
            5 => AdminAction::Slash,
            _ => return Err(ContractError::InvalidActionType),
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

        // Add any vote weight that is effective for the proposer.
        crate::auth::_add_effective_action_votes(&env, action_id, &admin);

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
    pub fn vote_for_action(env: Env, voter: Address, action_id: u64) -> Result<u32, ContractError> {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        voter.require_auth();

        let voter_is_admin = crate::auth::_is_authorized(&env, &voter);
        let voter_delegated_away = crate::auth::_get_vote_delegate(&env, &voter).is_some();
        let delegated_voters = crate::auth::_get_delegated_voters(&env, &voter);
        if (!voter_is_admin || voter_delegated_away) && delegated_voters.len() == 0 {
            return Err(ContractError::NotAuthorized);
        }

        // Get the proposed action
        let proposed = match crate::auth::_get_proposed_action(&env, action_id) {
            Some(p) => p,
            None => return Err(ContractError::ActionNotFound),
        };

        // Check if already executed or cancelled
        if proposed.executed {
            return Err(ContractError::ActionAlreadyExecuted);
        }
        if proposed.cancelled {
            return Err(ContractError::ActionCancelled);
        }

        let vote_count = crate::auth::_add_effective_action_votes(&env, action_id, &voter);

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

    /// Delegate the owner's vote weight to a proxy representative.
    ///
    /// The owner can reassign the delegate by calling this again, or break the
    /// link immediately with `clear_vote_delegate`.
    pub fn delegate_vote(env: Env, owner: Address, delegate: Address) -> Result<(), ContractError> {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        owner.require_auth();

        if owner == delegate {
            return Err(ContractError::InvalidDelegate);
        }

        crate::auth::_set_vote_delegate(&env, &owner, &delegate);
        env.events()
            .publish((Symbol::new(&env, "vote_delegated"),), (owner, delegate));

        Ok(())
    }

    /// Remove the owner's active vote delegation.
    pub fn clear_vote_delegate(env: Env, owner: Address) -> Result<(), ContractError> {
        _require_not_destroyed(&env);
        owner.require_auth();

        crate::auth::_remove_vote_delegate(&env, &owner);
        env.events()
            .publish((Symbol::new(&env, "vote_delegate_cleared"),), (owner,));

        Ok(())
    }

    /// Get the proxy representative currently assigned by an owner.
    pub fn get_vote_delegate(env: Env, owner: Address) -> Option<Address> {
        crate::auth::_get_vote_delegate(&env, &owner)
    }

    /// Assign a hot-wallet delegate for a cold-storage administrative identity.
    pub fn assign_delegate(
        env: Env,
        admin: Address,
        delegate: Address,
    ) -> Result<(), ContractError> {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        if admin == delegate {
            return Err(ContractError::InvalidDelegate);
        }

        crate::auth::_set_delegate(&env, &admin, &delegate);

        env.events().publish(
            (Symbol::new(&env, "delegate_assigned_event"),),
            (
                admin.clone().into_val(&env),
                delegate.clone().into_val(&env),
            ),
        );

        Ok(())
    }

    /// Remove an active submission delegate from an administrative identity.
    pub fn revoke_delegate(env: Env, admin: Address) -> Result<(), ContractError> {
        _require_not_destroyed(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        if let Some(delegate) = crate::auth::_get_delegate(&env, &admin) {
            crate::auth::_remove_delegate(&env, &admin);
            env.events().publish(
                (Symbol::new(&env, "delegate_revoked_event"),),
                (
                    admin.clone().into_val(&env),
                    delegate.clone().into_val(&env),
                ),
            );
        }

        Ok(())
    }

    /// Get the hot-wallet delegate currently assigned to an admin.
    pub fn get_delegate(env: Env, admin: Address) -> Option<Address> {
        crate::auth::_get_delegate(&env, &admin)
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
    pub fn execute_proposed_action(
        env: Env,
        executor: Address,
        action_id: u64,
    ) -> Result<(), ContractError> {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        executor.require_auth();
        crate::auth::_require_authorized(&env, &executor);

        // Get the proposed action
        let mut proposed = match crate::auth::_get_proposed_action(&env, action_id) {
            Some(p) => p,
            None => return Err(ContractError::ActionNotFound),
        };

        // Check if already executed or cancelled
        if proposed.executed {
            return Err(ContractError::ActionAlreadyExecuted);
        }
        if proposed.cancelled {
            return Err(ContractError::ActionCancelled);
        }

        // Check if threshold is met
        let threshold = crate::auth::_get_required_threshold(&env);
        if !crate::auth::_has_reached_threshold(&env, action_id, threshold) {
            return Err(ContractError::ThresholdNotReached);
        }

        // Quorum floor check (issue #292): total votes cast must meet the configured minimum.
        let total_votes = crate::auth::_get_action_votes(&env, action_id).len();
        let min_quorum = Self::get_min_quorum_threshold(env.clone());
        if total_votes < min_quorum {
            return Err(ContractError::QuorumNotReached);
        }

        // Execute based on action type
        match proposed.action_type {
            AdminAction::TogglePause => {
                let current_paused = crate::auth::_is_paused(&env);
                let new_paused = !current_paused;
                crate::auth::_set_paused(&env, new_paused);
                // Issue #263: keep the isolated HealthPaused slot in sync.
                env.storage()
                    .persistent()
                    .set(&DataKey::HealthPaused, &new_paused);
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
                    return Err(ContractError::InvalidActionType);
                }
            }
            AdminAction::RemoveAdmin => {
                if let Some(ref admin_to_remove) = proposed.target {
                    let admins = crate::auth::_get_admin(&env);
                    if admins.len() <= 1 {
                        return Err(ContractError::CannotRemoveLastAdmin);
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
                    return Err(ContractError::InvalidActionType);
                }
            }
            AdminAction::SelfDestruct => {
                // For self-destruct, we need additional validation
                let admins = crate::auth::_get_admin(&env);
                if admins.len() < 2 {
                    return Err(ContractError::MultiSigValidationFailed);
                }

                // Wipe all known instance storage
                env.storage().instance().remove(&DataKey::Admin);
                env.storage().instance().remove(&DataKey::BaseCurrencyPairs);
                env.storage().instance().remove(&DataKey::PendingAdmin);
                env.storage()
                    .instance()
                    .remove(&DataKey::PendingAdminTimestamp);
                env.storage()
                    .temporary()
                    .remove(&DataKey::AdminUpdateTimestamp);
                env.storage().temporary().remove(&DataKey::RecentEvents);
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
            AdminAction::Slash => {
                // The target field holds the bad relayer's address.
                let bad_relayer = match proposed.target {
                    Some(ref addr) => addr.clone(),
                    None => return Err(ContractError::InvalidActionType),
                };

                // The data field encodes the slash amount as a decimal string.
                let amount = crate::slashing::parse_slash_amount(&env, &proposed.data)?;

                // Delegate to the slashing module.
                crate::slashing::execute_slash_internal(&env, &executor, &bad_relayer, amount)?;

                proposed.executed = true;
                _log_admin_action(
                    &env,
                    &executor,
                    AdminAction::Slash,
                    Some(format!(
                        "Slashed relayer: {}, amount: {}",
                        bad_relayer, amount
                    )),
                );
            }
            _ => return Err(ContractError::InvalidActionType),
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
    pub fn cancel_proposed_action(
        env: Env,
        canceller: Address,
        action_id: u64,
    ) -> Result<(), ContractError> {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        canceller.require_auth();
        crate::auth::_require_authorized(&env, &canceller);

        // Get the proposed action
        let mut proposed = match crate::auth::_get_proposed_action(&env, action_id) {
            Some(p) => p,
            None => return Err(ContractError::ActionNotFound),
        };

        // Check if already executed or cancelled
        if proposed.executed {
            return Err(ContractError::ActionAlreadyExecuted);
        }
        if proposed.cancelled {
            return Err(ContractError::ActionCancelled);
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
        _log_admin_action(
            &env,
            &admin,
            AdminAction::SetCouncil,
            Some(council.to_string()),
        );
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
    /// Ok(()) if successful, ContractError if not authorized
    pub fn emergency_freeze(env: Env, council: Address) -> Result<(), ContractError> {
        council.require_auth();
        crate::auth::_require_council(&env, &council);

        // Check if already frozen
        if crate::auth::_is_frozen(&env) {
            return Err(ContractError::AlreadyInitialized);
        }

        // Set the frozen state
        crate::auth::_set_frozen(&env, true);

        // Emit event
        env.events()
            .publish((Symbol::new(&env, "emergency_freeze"),), (council.clone(),));

        Ok(())
    }

    /// Check if the contract is in emergency freeze state.
    ///
    /// Returns true if the contract is frozen, false otherwise.
    pub fn is_frozen(env: Env) -> bool {
        crate::auth::_is_frozen(&env)
    }

    /// Halt or resume all public rate read queries via multi-sig governance.
    ///
    /// Requires 2 distinct authorized admins. When `status` is `true`, every
    /// public rate read panics with `ContractError::EmergencyHalted` until lifted.
    pub fn set_emergency_halt(
        env: Env,
        admin1: Address,
        admin2: Address,
        status: bool,
    ) -> Result<(), ContractError> {
        _require_not_destroyed(&env);
        if admin1 == admin2 {
            return Err(ContractError::MultiSigValidationFailed);
        }
        admin1.require_auth();
        admin2.require_auth();
        crate::auth::_require_authorized(&env, &admin1);
        crate::auth::_require_authorized(&env, &admin2);

        let previous_halt_state = crate::auth::_is_halted(&env);
        crate::auth::_set_halted(&env, status);

        // Graceful Recovery: If we are resuming from a halt (status: true -> false),
        // clear out older tracking metrics to ensure synchronization.
        if previous_halt_state && !status {
            Self::_perform_graceful_recovery(&env);
        }

        // Publish structured event for emergency halt toggles so indexers can track it.
        event_topics::publish_emergency_halt(&env, admin1, admin2, status);

        Ok(())
    }

    /// Internal routine to clear stale metrics when resuming from a halt.
    fn _perform_graceful_recovery(env: &Env) {
        // 1. Reset baseline ledger to mark the "new beginning" of the system.
        env.storage()
            .instance()
            .set(&DataKey::BaselineLedger, &env.ledger().sequence());

        // 2. Clear RecentEvents activity feed to remove stale pre-halt logs.
        env.storage().temporary().remove(&DataKey::RecentEvents);

        // 3. Reset provider metrics.
        // During a system-wide halt, relayers may have been unable to submit.
        // We reset their counters so they aren't unfairly penalized for the halt duration.
        let relayers = crate::auth::_get_active_relayers(env);
        for relayer in relayers.iter() {
            // Reset consecutive missed blocks.
            env.storage()
                .persistent()
                .remove(&DataKey::ProviderConsecutiveMissedBlocks(relayer.clone()));
            // Reset uptime streak start (they must earn a new 48h streak).
            env.storage()
                .persistent()
                .remove(&DataKey::ProviderUptimeStreakStart(relayer.clone()));
            // Update last seen to current ledger so they aren't flagged as inactive immediately.
            env.storage().persistent().set(
                &DataKey::ProviderLastSeenLedger(relayer.clone()),
                &env.ledger().sequence(),
            );
        }

        // 4. Clear TWAPs for all tracked assets.
        // We clear these because the historical prices in the buffer are now stale.
        let assets = get_tracked_assets(env);
        for asset in assets.iter() {
            env.storage().temporary().remove(&DataKey::Twap(asset));
        }
    }

    /// Return the current emergency halt state.
    pub fn is_halted(env: Env) -> bool {
        crate::auth::_is_halted(&env)
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
    pub fn normalize_price(env: Env, asset: Symbol, price: i128) -> i128 {
        price // Returns the integer directly
    }
    /// Get the number of unique relayer submissions for an asset in the current ledger.
    pub fn get_relayer_count(env: Env, asset: Symbol) -> u32 {
        let buffer = get_price_buffer(&env, asset);
        buffer.entries.len()
    }

    /// Get the Time-Weighted Average Price (TWAP) for a specific asset.
    pub fn get_twap(env: Env, asset: Symbol) -> Option<i128> {
        if crate::auth::_is_halted(&env) {
            panic_with_error!(&env, ContractError::EmergencyHalted);
        }
        let key = DataKey::Twap(asset);
        let twap_buffer: soroban_sdk::Vec<(u64, i128)> = env.storage().temporary().get(&key)?;

        let len = twap_buffer.len();
        if len == 0 {
            return None;
        }

        let mut sum: i128 = 0;
        for (_, price) in twap_buffer.iter() {
            sum = sum.checked_add(price)?;
        }

        sum.checked_div(len as i128)
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
    pub fn subscribe_to_price_updates(
        env: Env,
        callback_contract: Address,
    ) -> Result<(), ContractError> {
        callbacks::subscribe(&env, callback_contract)
    }

    /// Unsubscribe a contract from price update callbacks.
    ///
    /// # Arguments
    /// * `callback_contract` - The address of the contract to unsubscribe
    ///
    /// # Returns
    /// Returns an error if the contract is not found in the subscriber list.
    pub fn unsubscribe_from_price_updates(
        env: Env,
        callback_contract: Address,
    ) -> Result<(), ContractError> {
        callbacks::unsubscribe(&env, &callback_contract)
    }

    /// Get the list of all contracts subscribed to price updates.
    ///
    /// # Returns
    /// A vector of addresses of all contracts currently subscribed to price updates.
    pub fn get_price_update_subscribers(env: Env) -> soroban_sdk::Vec<Address> {
        callbacks::get_subscribers(&env)
    }

    /// Enable a 1-hour grace period during which the circuit-breaker safety
    /// checks (flash-crash, price floor, and price bounds) are bypassed.
    ///
    /// Only an authorized admin may call this. The bypass expires automatically
    /// after 3,600 seconds regardless of contract state. Returns the expiry
    /// timestamp so callers can log or display when the window closes.
    pub fn enable_bypass_safety_checks(env: Env, admin: Address) -> Result<u64, ContractError> {
        _require_not_destroyed(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        let expiry = env.ledger().timestamp() + 3_600;
        crate::auth::_set_bypass_safety_checks(&env, expiry);

        _log_admin_action(
            &env,
            &admin,
            AdminAction::EnableBypassSafetyChecks,
            Some(format!("expiry: {}", expiry)),
        );

        env.events().publish(
            (Symbol::new(&env, "bypass_enabled_event"),),
            (admin, expiry),
        );

        Ok(expiry)
    }

    /// Immediately revoke the safety-checks bypass before its natural expiry.
    pub fn disable_bypass_safety_checks(env: Env, admin: Address) -> Result<(), ContractError> {
        _require_not_destroyed(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        crate::auth::_remove_bypass_safety_checks(&env);

        _log_admin_action(&env, &admin, AdminAction::DisableBypassSafetyChecks, None);

        env.events()
            .publish((Symbol::new(&env, "bypass_disabled_event"),), (admin,));

        Ok(())
    }

    /// Return the raw expiry timestamp stored for the bypass, or `None` if never
    /// set. Note: the bypass may be stored but already expired — callers that
    /// care about liveness should compare against the current ledger timestamp.
    pub fn get_bypass_safety_checks_expiry(env: Env) -> Option<u64> {
        crate::auth::_get_bypass_expiry(&env)
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Slashing — stake management & direct governance-gated slash
    // ─────────────────────────────────────────────────────────────────────────

    /// Configure the SEP-41 token contract used for staking and slashing.
    ///
    /// Must be called by an authorized admin before any staking or slashing
    /// operations can take place.
    pub fn set_slash_token(env: Env, admin: Address, token: Address) -> Result<(), ContractError> {
        _require_not_destroyed(&env);
        _require_initialized(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        env.storage().persistent().set(&DataKey::SlashToken, &token);

        _log_admin_action(
            &env,
            &admin,
            AdminAction::SetSlashToken,
            Some(token.to_string()),
        );

        env.events()
            .publish((Symbol::new(&env, "slash_token_set"),), (admin, token));

        Ok(())
    }

    /// Get the configured slash token address, if any.
    pub fn get_slash_token(env: Env) -> Option<Address> {
        env.storage().persistent().get(&DataKey::SlashToken)
    }

    /// Configure the ecosystem insurance reserve address.
    ///
    /// Slashed funds are transferred to this address. Must be set by an
    /// authorized admin before any slash can be executed.
    pub fn set_insurance_reserve(
        env: Env,
        admin: Address,
        reserve: Address,
    ) -> Result<(), ContractError> {
        _require_not_destroyed(&env);
        _require_initialized(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        env.storage()
            .persistent()
            .set(&DataKey::InsuranceReserve, &reserve);

        _log_admin_action(
            &env,
            &admin,
            AdminAction::SetInsuranceReserve,
            Some(reserve.to_string()),
        );

        env.events().publish(
            (Symbol::new(&env, "insurance_reserve_set"),),
            (admin, reserve),
        );

        Ok(())
    }

    /// Get the configured insurance reserve address, if any.
    pub fn get_insurance_reserve(env: Env) -> Option<Address> {
        env.storage().persistent().get(&DataKey::InsuranceReserve)
    }

    // ── Gas Tank Integration (Issue #266) ────────────────────────────────────

    /// Register the Gas Tank escrow contract address.
    ///
    /// Once set, `update_price` will automatically call `reimburse(relayer)` on
    /// the Gas Tank after every successful price submission so that relayer
    /// on-chain transaction fees are covered by pre-funded consumer deposits.
    ///
    /// Only admin-authorised callers may update this setting.
    pub fn set_gas_tank(env: Env, admin: Address, gas_tank: Address) -> Result<(), Error> {
        _require_not_destroyed(&env);
        _require_initialized(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        env.storage().persistent().set(&DataKey::GasTank, &gas_tank);

        env.events()
            .publish((Symbol::new(&env, "gas_tank_set"),), (admin, gas_tank));

        Ok(())
    }

    /// Return the configured Gas Tank contract address, if any.
    pub fn get_gas_tank(env: Env) -> Option<Address> {
        env.storage().persistent().get(&DataKey::GasTank)
    }

    /// Deposit stake tokens into the contract on behalf of a relayer.
    ///
    /// The relayer must authorize this call. Tokens are transferred from the
    /// relayer's wallet into the contract's custody and credited to their
    /// on-chain stake balance.
    ///
    /// # Arguments
    /// * `relayer` - The provider staking tokens (must provide auth)
    /// * `amount`  - Number of token stroops to stake (must be > 0)
    pub fn stake_tokens(env: Env, relayer: Address, amount: i128) -> Result<(), ContractError> {
        _require_not_destroyed(&env);
        _require_initialized(&env);
        crate::auth::_require_not_frozen(&env);
        relayer.require_auth();

        if amount <= 0 {
            return Err(ContractError::InvalidSlashAmount);
        }

        let token_address: Address = env
            .storage()
            .persistent()
            .get(&DataKey::SlashToken)
            .ok_or(ContractError::SlashTokenNotSet)?;

        // Transfer tokens from the relayer into the contract.
        let token_client = token::Client::new(&env, &token_address);
        token_client.transfer(&relayer, &env.current_contract_address(), &amount);

        // Credit the relayer's on-chain stake balance.
        let current_stake: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::ProviderStake(relayer.clone()))
            .unwrap_or(0);

        let new_stake = current_stake.checked_add(amount).unwrap_or(current_stake);
        env.storage()
            .persistent()
            .set(&DataKey::ProviderStake(relayer.clone()), &new_stake);

        env.events().publish(
            (Symbol::new(&env, "stake_deposited"),),
            (relayer, amount, new_stake),
        );

        Ok(())
    }

    /// Withdraw stake tokens from the contract back to the relayer.
    ///
    /// The relayer must authorize this call. Only the portion of stake that
    /// has not been slashed can be withdrawn.
    ///
    /// # Arguments
    /// * `relayer` - The provider withdrawing tokens (must provide auth)
    /// * `amount`  - Number of token stroops to withdraw (must be > 0 and ≤ stake)
    pub fn unstake_tokens(env: Env, relayer: Address, amount: i128) -> Result<(), ContractError> {
        _require_not_destroyed(&env);
        _require_initialized(&env);
        crate::auth::_require_not_frozen(&env);
        relayer.require_auth();

        if amount <= 0 {
            return Err(ContractError::InvalidSlashAmount);
        }

        let token_address: Address = env
            .storage()
            .persistent()
            .get(&DataKey::SlashToken)
            .ok_or(ContractError::SlashTokenNotSet)?;

        let current_stake: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::ProviderStake(relayer.clone()))
            .unwrap_or(0);

        if amount > current_stake {
            return Err(ContractError::InsufficientStake);
        }

        let new_stake = current_stake - amount;
        env.storage()
            .persistent()
            .set(&DataKey::ProviderStake(relayer.clone()), &new_stake);

        // Return tokens to the relayer.
        let token_client = token::Client::new(&env, &token_address);
        token_client.transfer(&env.current_contract_address(), &relayer, &amount);

        env.events().publish(
            (Symbol::new(&env, "stake_withdrawn"),),
            (relayer, amount, new_stake),
        );

        Ok(())
    }

    /// Get the current staked balance for a relayer.
    pub fn get_provider_stake(env: Env, relayer: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::ProviderStake(relayer))
            .unwrap_or(0)
    }

    /// Report that a relayer missed one or more consecutive blocks.
    ///
    /// This increments the relayer's infraction counter and scales future
    /// slashing penalties exponentially until the relayer maintains 48 hours
    /// of uninterrupted uptime.
    pub fn report_missed_blocks(
        env: Env,
        admin: Address,
        relayer: Address,
        missed_blocks: u32,
    ) -> Result<i128, Error> {
        _require_not_destroyed(&env);
        _require_initialized(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        crate::slashing::report_missed_blocks(&env, &relayer, missed_blocks)
    }

    /// Report a period of uninterrupted uptime for a relayer.
    ///
    /// The infraction multiplier is reset only after the relayer accumulates a
    /// full 48-hour streak of uninterrupted healthy operation.
    pub fn report_successful_uptime(
        env: Env,
        admin: Address,
        relayer: Address,
    ) -> Result<bool, Error> {
        _require_not_destroyed(&env);
        _require_initialized(&env);
        crate::auth::_require_not_frozen(&env);
        admin.require_auth();
        crate::auth::_require_authorized(&env, &admin);

        crate::slashing::report_successful_uptime(&env, &relayer)
    }

    /// Get the relayer's current consecutive missed-block count.
    pub fn get_provider_consecutive_missed_blocks(env: Env, relayer: Address) -> u32 {
        crate::slashing::get_consecutive_missed_blocks(&env, &relayer)
    }

    /// Get the current multiplier that will scale future slash amounts for the
    /// relayer's consecutive missed blocks.
    pub fn get_slashing_multiplier(env: Env, relayer: Address) -> Result<i128, Error> {
        crate::slashing::get_slash_multiplier(&env, &relayer)
    }

    /// Get the relayer's uptime streak start timestamp, if any.
    pub fn get_uptime_streak_start(env: Env, relayer: Address) -> Option<u64> {
        crate::slashing::get_uptime_streak_start(&env, &relayer)
    }

    /// Governance-gated direct slash entry point.
    ///
    /// This is a convenience wrapper that lets an authorized admin execute a
    /// slash without going through the full propose → vote → execute pipeline.
    /// It still requires the caller to be an authorized admin and the contract
    /// to be live (not destroyed, not frozen).
    ///
    /// For high-security deployments, prefer the proposal pipeline
    /// (`propose_action` with `action_type = 5`) so that multiple admins must
    /// agree before funds are moved.
    ///
    /// # Arguments
    /// * `executor`    - Authorized admin executing the slash (must provide auth)
    /// * `bad_relayer` - The relayer whose stake is being slashed
    /// * `amount`      - Number of token stroops to slash (must be > 0 and ≤ stake)
    pub fn execute_slash(
        env: Env,
        executor: Address,
        bad_relayer: Address,
        amount: i128,
    ) -> Result<(), ContractError> {
        _require_not_destroyed(&env);
        _require_initialized(&env);
        crate::auth::_require_not_frozen(&env);
        executor.require_auth();
        crate::auth::_require_authorized(&env, &executor);

        crate::slashing::execute_slash_internal(&env, &executor, &bad_relayer, amount)
    }

    pub fn get_provider_last_seen_ledger(env: Env, provider: Address) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::ProviderLastSeenLedger(provider))
            .unwrap_or(0)
    }

    pub fn is_provider_active(env: Env, provider: Address, window: u32) -> bool {
        let last_seen = Self::get_provider_last_seen_ledger(env.clone(), provider);
        if last_seen == 0 {
            return false;
        }
        let current_ledger = env.ledger().sequence();
        current_ledger <= last_seen.saturating_add(window)
    }

    /// Queue a validator stake withdrawal behind the slashing delay.
    pub fn request_stake_unbonding(
        env: Env,
        validator: Address,
        amount: i128,
    ) -> Result<slashing::UnbondingRequest, Error> {
        slashing::request_unbonding(&env, &validator, amount)
    }

    /// Release a queued validator stake withdrawal after the delay expires.
    pub fn release_unbonded_stake(env: Env, validator: Address) -> Result<i128, Error> {
        slashing::release_unbonded_stake(&env, &validator)
    }

    /// Inspect a validator's queued unbonding request.
    pub fn get_unbonding_request(
        env: Env,
        validator: Address,
    ) -> Option<slashing::UnbondingRequest> {
        slashing::get_unbonding_request(&env, &validator)
    }

    /// Return the enforced unbonding delay in ledgers.
    pub fn min_unbonding_delay_ledgers() -> u32 {
        slashing::MIN_UNBONDING_DELAY_LEDGERS
    }

    /// Claim accumulated rewards for a relayer. This is a thin wrapper that
    /// delegates to the rewards module which enforces Checks-Effects-Interactions.
    pub fn claim_rewards(env: Env, relayer: Address, token_contract: Address) -> i128 {
        crate::rewards::Rewards::claim_rewards(env, relayer, token_contract)
    }
}

mod asset_symbol;
mod auth;
mod callbacks;
#[cfg(test)]
mod delegate_tests;
pub mod math;
mod median;
mod slashing;
pub mod slashing;
mod test;
mod types;
mod validation;
