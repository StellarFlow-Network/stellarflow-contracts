use soroban_sdk::{contracttype, Address, Symbol};

/// Storage keys for contract data
#[allow(clippy::enum_variant_names)] // Soroban SDK generates these names
#[contracttype]
pub enum DataKey {
    Admin,
    BaseCurrencyPairs,
    /// Legacy flat price map — kept for migration compatibility only.
    PriceData,
    /// Legacy single-key buffer map — superseded by PriceBufferByAsset(Symbol, u64).
    /// Kept for migration compatibility only; no longer written by new code.
    PriceBuffer,
    /// Legacy single-key bounds map — superseded by PriceBoundsEntry(Symbol).
    /// Kept for migration compatibility only; no longer written by new code.
    PriceBoundsData,
    /// Configurable global maximum allowed price deviation in basis points.
    MaxPriceDeviationBps,
    IsLocked,
    /// Legacy single-key floor map — superseded by PriceFloorEntry(Symbol).
    /// Kept for migration compatibility only; no longer written by new code.
    PriceFloorData,
    AssetDescription(Symbol),
    PendingAdmin,
    PendingAdminTimestamp,
    AdminUpdateTimestamp,
    RecentEvents,
    /// Mapping of relayer address -> accumulated reward balance
    Rewards,
    Initialized,
    /// TWAP Buffer: Stores last 10 (Timestamp, Price) updates.
    Twap(Symbol),
    /// Verified price bucket: written only by whitelisted providers / admins.
    /// Internal math and `get_price` default to this bucket.
    VerifiedPrice(Symbol),
    /// Community price bucket: written by any caller; never used in internal math.
    CommunityPrice(Symbol),
    /// Query fee amount for get_price calls (in stroops).
    QueryFee,
    /// Destroyed flag to mark contract as permanently unusable.
    Destroyed,
    /// Asset decimal metadata (base_decimals, quote_decimals).
    AssetMeta(Symbol),
    /// Lightweight asset metadata stored separately from hot-path price data.
    AssetInfo(Symbol),
    /// List of contracts subscribed to price update callbacks.
    PriceUpdateSubscribers,
    /// Tracked asset flag for O(1) existence check.
    TrackedAsset(Symbol),
    /// Composite-key price buffer: one storage slot per (asset, ledger_sequence) pair.
    ///
    /// Replaces the legacy `PriceBuffer` map so a single-asset read no longer
    /// loads every other asset's buffer. The `u64` component is the ledger
    /// sequence number, which naturally scopes each buffer to one ledger.
    PriceBufferByAsset(Symbol, u64),
    /// Composite-key price bounds: one storage slot per asset.
    ///
    /// Replaces the legacy `PriceBoundsData` map so reading one asset's bounds
    /// does not load bounds for every other asset.
    PriceBoundsEntry(Symbol),
    /// Composite-key price floor: one storage slot per asset.
    ///
    /// Replaces the legacy `PriceFloorData` map so reading one asset's floor
    /// does not load floors for every other asset.
    PriceFloorEntry(Symbol),
    /// Composite-key price entry: one storage slot per (asset, sequence) pair.
    ///
    /// Used by `clear_assets` and snapshot tests that reference `DataKey::Price`.
    Price(Symbol),
    /// Rollback slot for per-asset price bounds — written before every bounds update.
    PrevPriceBoundsEntry(Symbol),
    /// Rollback slot for the global max deviation percentage — written before every update.
    PrevMaxDeviationBps,
    /// Rollback slot for per-asset price floor — written before every floor update.
    PrevPriceFloorEntry(Symbol),
    /// Minimum number of votes required for a governance action to reach quorum.
    MinQuorumThreshold,
    /// Staked collateral balance for a relayer/provider (i128, in token stroops).
    ProviderStake(Address),
    /// Consecutive missed-block infractions for a relayer/provider.
    ProviderConsecutiveMissedBlocks(Address),
    /// Uptime streak start timestamp used to reset slashing multipliers after 48h
    /// of uninterrupted healthy operation.
    ProviderUptimeStreakStart(Address),
    /// The exact ledger height of the provider's last successful price update.
    ProviderLastSeenLedger(Address),
    /// The SEP-41 token contract address used for staking and slashing.
    SlashToken,
    /// The address of the insurance reserve that receives slashed funds.
    InsuranceReserve,
    /// The SEP-41 token contract address used for query fee collection.
    FeeToken,
    /// Legacy aggregate fee vault balance; retained for migration compatibility only.
    FeeVaultBalance,
    /// Asset-isolated fee vault balance keyed by the SEP-41 fee token address.
    CorridorFeeVaultBalance(Address),
    /// The pending reward balance for a relayer/validator.
    ProviderRewardBalance(Address),

    // ── Issue #264: per-admin signature weight ────────────────────────────────
    /// Governance weight assigned to a specific admin (u32, 0–100).
    ///
    /// Used by the multi-sig weight-accumulation algorithm: before executing a
    /// proposed action the contract sums the weights of all voters and checks
    /// the total against `WeightThreshold`. Defaults to 1 when unset so that
    /// legacy single-weight deployments continue to work without migration.
    AdminWeight(Address),
    /// Minimum cumulative weight required for a governance proposal to execute.
    ///
    /// When unset the contract falls back to the simple vote-count threshold
    /// returned by `_get_required_threshold` (expressed as weight units where
    /// each admin contributes 1 unit).
    WeightThreshold,

    // ── Liquidity validation: flash loan manipulation prevention ──────────────
    /// Minimum liquidity threshold required for price submissions (in stroops).
    ///
    /// When set, price submissions must include liquidity data that meets or
    /// exceeds this threshold. Submissions from thin markets are rejected early
    /// to prevent flash loan price manipulation attacks.
    LiquidityThreshold(Symbol),
    /// Last reported liquidity value from a provider for a specific asset.
    ///
    /// Tracked for reputation scoring and slash enforcement. Key structure:
    /// (provider_address, asset_symbol) => liquidity_value_stroops.
    ProviderReportedLiquidity(Address, Symbol),
    /// Timestamp of the last successful liquidity validation for an asset.
    ///
    /// Used for audit trails and monitoring liquidity validation frequency.
    LastLiquidityValidation(Symbol),

    // ── Issue #263: isolated OracleHealth slots ───────────────────────────────
    /// Isolated slot: number of active relayers (whitelisted providers).
    HealthActiveRelayers,
    /// Isolated slot: whether the contract is currently paused.
    HealthPaused,
    /// Isolated slot: total number of tracked assets.
    HealthTotalAssets,
    /// Isolated slot: last ledger sequence number at which health was written.
    HealthLastLedger,
    /// The ledger sequence number when the oracle last resumed from a halt.
    /// Used to ignore tracking metrics (TWAP, RecentEvents) from before the recovery.
    BaselineLedger,
    /// Last recorded deviation (in basis points) between a provider's submitted
    /// price and the consensus median.  Written on every `report_price_deviation`
    /// call for audit and off-chain indexing purposes.
    ProviderLastDeviationBps(Address),
    /// Gas Tank escrow contract address for relayer reimbursement (Issue #266).
    GasTank,
}

/// Decimal metadata for an asset pair.
///
/// Stores the native decimal precision of the base and quote assets so the
/// contract can normalize all prices to 9 fixed-point decimals on entry.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetMeta {
    /// Native decimal precision of the base asset (e.g. 7 for XLM).
    pub base_decimals: u32,
    /// Native decimal precision of the quote asset (e.g. 2 for NGN).
    pub quote_decimals: u32,
}

/// Lightweight metadata for an asset.

/// `name` uses `Symbol` instead of `String` because short values are stored
/// more efficiently on-chain. Longer descriptions should use
/// `DataKey::AssetDescription(asset)` instead.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetInfo {
    /// Short human-readable asset name, max 32 characters.
    pub name: Symbol,
    /// Native decimal precision of the base asset.
    pub base_decimals: u32,
    /// Native decimal precision of the quote asset.
    pub quote_decimals: u32,
}

/// Configuration for atomic asset registration and initialization.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetRegistrationConfig {
    /// Asset symbol for this registration.
    pub asset: Symbol,
    /// Short human-readable asset name.
    pub name: Symbol,
    /// Native decimal precision of the base asset.
    pub base_decimals: u32,
    /// Native decimal precision of the quote asset.
    pub quote_decimals: u32,
    /// Minimum allowed price for the asset pair.
    pub min_price: i128,
    /// Maximum allowed price for the asset pair.
    pub max_price: i128,
    /// Optional absolute floor price for the asset.
    pub price_floor: Option<i128>,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetWeight {
    pub asset: Symbol,
    pub weight: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceData {
    /// The price value stored as a scaled integer.
    pub price: i128,
    /// Ledger timestamp when this price was written.
    pub timestamp: u64,
    /// Exact ledger sequence number for this price write.
    pub ledger_sequence: u32,
    /// Address that provided the price update.
    pub provider: Address,
    /// Number of decimals for the price value.
    pub decimals: u32,
    /// Confidence score (0-100, higher is more confident)
    pub confidence_score: u32,
    /// Time-to-live in seconds for this price (per-asset expiration)
    pub ttl: u64,
}

/// A simplified price entry for external consumers.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceEntry {
    pub price: i128,
    pub timestamp: u64,
    pub decimals: u32,
}

/// Full price payload returned to consumers with freshness status.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceDataWithStatus {
    pub data: PriceData,
    pub is_stale: bool,
}

/// Lightweight price payload returned to consumers with freshness status.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceEntryWithStatus {
    pub price: i128,
    pub timestamp: u64,
    pub is_stale: bool,
}

/// Min/max price bounds for an asset to prevent fat-finger errors.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceBounds {
    pub min_price: i128,
    pub max_price: i128,
}

/// A recent activity event for the dashboard feed.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecentEvent {
    pub event_type: soroban_sdk::Symbol,
    pub asset: soroban_sdk::Symbol,
    pub price: i128,
    pub timestamp: u64,
}

/// A single relayer price submission within the current ledger buffer.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceBufferEntry {
    /// The price value submitted by this relayer.
    pub price: i128,
    /// Address of the relayer who submitted this price.
    pub provider: Address,
    /// Timestamp when this price was submitted.
    pub timestamp: u64,
}

/// Buffer containing multiple relayer submissions for median calculation.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceBuffer {
    /// List of price entries from different relayers for the current ledger.
    pub entries: soroban_sdk::Vec<PriceBufferEntry>,
    /// The ledger sequence number this buffer belongs to.
    pub ledger_sequence: u32,
    /// Number of decimals for the price values.
    pub decimals: u32,
    /// Time-to-live in seconds for this buffer.
    pub ttl: u64,
}

/// Health status of the oracle for the Admin Dashboard.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleHealth {
    /// Number of active relayers (whitelisted providers).
    pub active_relayers: u32,
    /// Whether the contract is currently paused.
    pub paused: bool,
    /// Total number of tracked assets.
    pub total_assets: u32,
    /// Current ledger sequence number.
    pub last_ledger: u32,
}

/// Callback payload sent to subscribed contracts when a price is updated.
///
/// This struct is passed to the `on_price_update` function of subscribed contracts.
/// It contains all necessary information for a downstream contract to react to price changes.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceUpdatePayload {
    /// The asset symbol that was updated (e.g., NGN, KES, GHS).
    pub asset: Symbol,
    /// The new price value (normalized to 9 decimal places).
    pub price: i128,
    /// Timestamp when the price was updated.
    pub timestamp: u64,
    /// The provider/relayer that submitted this price update.
    pub provider: Address,
    /// Number of decimals for the price (always 9 for normalized prices).
    pub decimals: u32,
    /// Confidence score (0-100, higher is more confident).
    pub confidence_score: u32,
}

/// Admin action types for logging.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdminAction {
    Initialize,
    InitAdmin,
    AddAsset,
    TransferAdminInitiated,
    TransferAdminAccepted,
    RenounceOwnership,
    RescueTokens,
    Upgrade,
    RemoveAsset,
    SetPriceFloor,
    SetPriceBounds,
    TogglePause,
    RegisterAdmin,
    RemoveAdmin,
    SelfDestruct,
    SetCouncil,
    /// Multi-sig: Propose a high-impact action
    ProposeAction,
    /// Multi-sig: Vote for a proposed action
    VoteForAction,
    /// Multi-sig: Cancel a proposed action
    CancelAction,
    /// Admin enabled the safety-checks grace-period bypass
    EnableBypassSafetyChecks,
    /// Admin disabled the safety-checks grace-period bypass
    DisableBypassSafetyChecks,
    /// Governance-gated slash of a malicious relayer's staked collateral
    Slash,
    /// Admin set the insurance reserve address
    SetInsuranceReserve,
    /// Admin set the slash token address
    SetSlashToken,
}

/// Admin log entry for tracking admin actions.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminLogEntry {
    pub admin: Address,
    pub action: AdminAction,
    pub details: soroban_sdk::String,
    pub timestamp: u64,
}

/// Proposed action waiting for multi-signature approval.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProposedAction {
    /// Unique identifier for this action.
    pub action_id: u64,
    /// The type of action being proposed.
    pub action_type: AdminAction,
    /// Target address (for admin registration/removal).
    pub target: Option<Address>,
    /// Additional data (e.g., asset symbol, wasm hash).
    pub data: soroban_sdk::String,
    /// Timestamp when the action was proposed.
    pub proposed_at: u64,
    /// Whether the action has been executed.
    pub executed: bool,
    /// Whether the action has been cancelled.
    pub cancelled: bool,
}
