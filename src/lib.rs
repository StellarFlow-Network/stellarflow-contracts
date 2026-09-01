#![no_std]
use soroban_sdk::{
    contract, contracterror, contractimpl, contractmeta, contracttype, symbol_short,
    Address, Bytes, BytesN, Env, Map, Symbol, Vec,
};

#![no_std]
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, Bytes, BytesN, Env,
    Map, Symbol, Vec,
};

/// Numeric asset identifier for gas-optimized storage.
/// Replaces heavy Symbol identifiers in high-frequency paths.
pub type AssetId = u32;

/// Convert a currency Symbol to a numeric AssetId using FNV-1a hash.
/// This provides deterministic mapping while minimizing gas costs.
pub fn symbol_to_asset_id(symbol: &Symbol) -> AssetId {
    // Direct mapping for known currency symbols (deterministic).
    // For unknown symbols, fall back to a hash of the raw SymbolVal.
    if *symbol == symbol_short!("NGN") { 3897123275 }
    else if *symbol == symbol_short!("KES") { 2654435761 }
    else if *symbol == symbol_short!("GHS") { 4026531840 }
    else if *symbol == symbol_short!("CFA") { 4160749568 }
    else if *symbol == symbol_short!("ZAR") { 3219226362 }
    else if *symbol == symbol_short!("UGX") { 2863311530 }
    else if *symbol == symbol_short!("STAKE") { 0 }
    else if *symbol == symbol_short!("VALUE") { 1 }
    else {
        // Fallback: hash the raw bits of the Symbol's underlying Val.
        // Val is #[repr(transparent)] over i64.
        let val = symbol.to_val();
        let raw: i64 = unsafe { core::mem::transmute(val) };
        let bytes = raw.to_le_bytes();
        let mut hash: u32 = 2166136261u32;
        for &byte in bytes.iter() {
            if byte == 0 { break; }
            hash ^= byte as u32;
            hash = hash.wrapping_mul(16777619);
        }
        hash
    }
}

/// Convert an AssetId back to a Symbol for backward compatibility.
/// Note: This is lossy - use pre-defined mappings for production.
    pub fn asset_id_to_symbol(_env: &Env, id: AssetId) -> Symbol {
    // For common currencies, use a mapping table
    match id {
        // Nigerian Naira
        3897123275 => symbol_short!("NGN"),
        // Kenyan Shilling
        2654435761 => symbol_short!("KES"),
        // Ghanaian Cedi
        4026531840 => symbol_short!("GHS"),
        // West African CFA Franc
        4160749568 => symbol_short!("CFA"),
        // South African Rand
        3219226362 => symbol_short!("ZAR"),
        // Ugandan Shilling
        2863311530 => symbol_short!("UGX"),
        // Special asset identifiers
        0 => symbol_short!("STAKE"),
        1 => symbol_short!("VALUE"),
        _ => symbol_short!("UNK"),
    }
}

pub(crate) mod nonce;
use crate::nonce::{consume_nonce, get_nonce};

pub mod action_guard;
pub mod amm;
pub mod admin;
pub mod auth;
pub mod bridge;
pub mod escrow;
pub mod config;
pub mod consensus;
pub mod kernel;
pub use kernel::instance;
pub mod errors;
pub mod events;
pub mod fees;
pub mod temp_governance;
use crate::validation::check_bond_capacity;
pub mod governance;
pub mod math;
pub mod orders;
pub mod recovery;
pub mod rescue;
pub mod roles;
pub mod router;
pub mod security;
pub mod settlement;
pub mod slashing;
pub mod staging;
pub mod staking_tiers;
pub mod state_verification;
pub mod storage;
pub mod temp_governance;
pub mod token;
pub mod upgrades;
pub mod validation;
pub mod zk;
pub use state_verification::{
    assert_contract_state_sanity, verify_contract_state, verify_storage_ttl_bumps,
    verify_zero_loss_accounting,
};
use crate::governance::{
    calculate_collected_weight, cast_vote, close_ballot, get_ballot, get_multisig_config,
    open_ballot, verify_staged_delay, verify_upgrade_quorum, GovernanceUpgradeProposal,
    GovernanceUpgradeProposedEvent, StagedUpgrade, VotingBallot, GOVERNANCE_UPGRADE_KEY,
};
use crate::slashing::{
    apply_escrow_penalty, get_fault_count_in_window, get_penalty_multiplier, record_tracking_fault,
    IngestionPenaltyResult,
};
use crate::staking_tiers::{
    assign_tier, effective_volume_score, required_stake_for_tier, validate_tier_config,
};
use crate::storage::{NodeProfileKey, SignerKey, StakeKey, HeartbeatKey};
use crate::validation::{
    check_bond_capacity, check_liquidity_depth, process_price_bundle, validate_telemetry_submission,
    AssetPriceUpdate, BundleValidationOutcome,
};

use crate::upgrades::migration::ensure_schema_version;

/// Centralised contract error enum — closes issue #720.
///
/// The `#[contracterror]` attribute makes every variant available as a typed
/// `soroban_sdk::Error` value on the host, so callers can pattern-match on
/// specific error codes rather than treating all failures as opaque integers.
///
/// Discriminant layout:
/// - 1–11 : initialisation / admin lifecycle
/// - 12–19: auth / signature errors
/// - 20–29: stake / tier errors
/// - 30–49: protocol logic errors
/// - 50–63: module-specific errors (reentrancy, merkle, governance)
/// - 64+  : new errors added after the initial audit
///
/// The four canonical *external-API* error codes required by issue #720 are
/// exposed as `const` aliases below the enum definition so they remain stable
/// regardless of any future renumbering inside the enum body.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ContractError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    NotAdmin = 3,
    NoPendingUpgrade = 4,
    UpgradeTimelockNotSatisfied = 5,
    InvalidHeartbeatInterval = 6,
    InvalidNonce = 7,
    AlreadyRegistered = 8,
    NotRegistered = 9,
    InvalidStakeAmount = 10,
    Overflow = 11,
    Unauthorized = 12,
    TargetNotAdmin = 13,
    ProposalAlreadyActive = 14,
    NoActiveProposal = 15,
    AlreadyVoted = 16,
    ThresholdNotReached = 17,
    SignatureExpired = 18,
    InvalidSaltSignature = 19,
    /// Stake amount is below the tier minimum for the target currency feed.
    InsufficientStakeForTier = 20,
    /// Staking tier configuration is invalid or non-monotonic.
    InvalidTierConfig = 21,
    /// Node is already registered for this currency feed.
    FeedAlreadyRegistered = 22,
    /// Validator's active locked stake is below the required bond for the
    /// premium asset pool.
    PremiumPoolAccessDenied = 23,
    /// An ownership transfer proposal is already active.
    TransferAlreadyPending = 24,
    /// No pending owner nominee exists to claim ownership.
    NoPendingOwner = 25,
    FeeCeilingExceeded = 26,
    DivisionByZero = 27,
    StaleSequence = 28,
    InvalidVarianceConfig = 29,
    StaleTelemetryPayload = 30,
    InsufficientReserveBalance = 31,
    InsufficientVolume = 32,
    InsufficientLiquidityDepth = 33,
    ContractPaused = 34,
    RevokedAddress = 35,
    EmergencyRevocationActive = 36,
    NoActiveEmergencyRevocation = 37,
    BundleAssetLimitExceeded = 38,
    BundleValidationFailed = 39,
    IncompleteQuorum = 40,
    EpochClosed = 41,
    AdminChangePending = 42,
    NoAdminChangePending = 43,
    CosignerCannotBeProposer = 44,
    AdminTimelockNotSatisfied = 45,
    InsufficientBondForPenalty = 46,
    SlippageExceeded = 47,
    AmountTooLow = 48,
    InvalidProof = 49,
    /// Reentrancy guard detected a reentrant call during execution.
    ReentrancyDetected = 58,
    MerkleTreeFull = 59,
    NotSecurityCouncil = 60,
    ProposalNotFound = 61,
    ProposalNotVetoable = 62,
    ProposalAlreadyVetoed = 63,
    /// Spot price executed by an AMM swap deviates from the TWAP oracle value
    /// by more than the governance-configured safety threshold (Issue #743).
    OracleDeviationTooHigh = 64,
    /// An oracle deviation guard configuration violates its structural bounds.
    InvalidOracleDeviationConfig = 65,
    /// AMM math was called with a structurally invalid input.
    InvalidInput = 66,
    /// Circuit breaker configuration violates its structural invariants.
    InvalidCircuitBreakerConfig = 67,
    /// Pool trading is currently frozen by the spot-price circuit breaker.
    CircuitBreakerTripped = 68,
    /// Tick spacing must be a strictly positive integer.
    InvalidTickSpacing = 69,
    /// The tick index for this pool already exists.
    TickIndexAlreadyExists = 70,
    /// No tick index exists for this pool.
    TickIndexNotFound = 71,
    /// Tick must be aligned to the pool's configured tick spacing.
    TickNotAligned = 72,
    /// Tick index is outside the allowed price range bounds.
    TickOutOfBounds = 73,
    /// Too many initialized ticks for a single pool.
    TooManyTicks = 74,
    /// Protected asset (primary pool or vault reserve) cannot be rescued.
    ProtectedAssetNotRescueable = 75,
    /// Token rescue proposal was not found.
    RescueProposalNotFound = 76,
    /// Token rescue proposal is not pending.
    RescueProposalNotPending = 77,
    /// Mandatory timelock delay has not expired yet.
    RescueTimelockNotExpired = 78,
}

impl ContractError {
    pub const MathOverflow: Self = Self::Overflow;
    pub const NullifierAlreadyUsed: Self = Self::AlreadyRegistered;
    pub const BridgeAssetNotRegistered: Self = Self::NotRegistered;
    pub const BridgeInvalidMaxSupply: Self = Self::Overflow;
    pub const BridgeAssetAlreadyRegistered: Self = Self::AlreadyRegistered;
    pub const BridgeInvalidAmount: Self = Self::AmountTooLow;
    pub const BridgeNotController: Self = Self::Unauthorized;
    pub const BridgeSupplyCapExceeded: Self = Self::Overflow;
    pub const BridgeInsufficientBalance: Self = Self::Overflow;
    pub const BridgeEscrowNotConfigured: Self = Self::NotInitialized;
    pub const AdminChangeTimelockNotSatis: Self = Self::UpgradeTimelockNotSatisfied;
    pub const StagingNotAuthorized: Self = Self::Unauthorized;
    pub const EmptyRoute: Self = Self::AmountTooLow;
    pub const RouteTooLong: Self = Self::Overflow;
    pub const InconsistentRouteAssets: Self = Self::NotInitialized;
    pub const VaultZeroAmount: Self = Self::AmountTooLow;
    pub const VaultInsufficientShares: Self = Self::Overflow;
    pub const VaultInsufficientBalance: Self = Self::Overflow;
    pub const VaultAlreadyInitialized: Self = Self::AlreadyInitialized;
    pub const VaultNotInitialized: Self = Self::NotInitialized;
    pub const VaultPaused: Self = Self::ContractPaused;
    pub const VaultInvalidPerformanceFee: Self = Self::InvalidVarianceConfig;
    pub const VaultMaxDrawdownExceeded: Self = Self::ContractPaused;
    pub const OrderNotFound: Self = Self::NotRegistered;
    pub const OrderZeroAmount: Self = Self::AmountTooLow;
    pub const OrderInvalidPrice: Self = Self::NotInitialized;
    pub const OrderAlreadyClosed: Self = Self::Unauthorized;
    pub const OrderInsufficientRemaining: Self = Self::Overflow;
    pub const OrderNotMaker: Self = Self::Unauthorized;
    pub const RoleExpirationInPast: Self = Self::UpgradeTimelockNotSatisfied;
    pub const RoleNotFound: Self = Self::NotRegistered;
    pub const UnauthorizedReentryAttempt: Self = Self::Unauthorized;
    pub const RoleExpiredOrMissing: Self = Self::Unauthorized;
    pub const HarvestNothingToCompound: Self = Self::AmountTooLow;
    pub const HarvestInvalidMinOut: Self = Self::AmountTooLow;
    pub const HarvestSwapFailed: Self = Self::RouteExecutionFailed;
    pub const HarvestSlippageExceeded: Self = Self::SlippageExceeded;
    pub const HarvestInvalidPath: Self = Self::InconsistentRouteAssets;

    // ── Issue #720 canonical API error aliases ────────────────────────────────
    // These four names are the stable external-facing identifiers documented in
    // the public ABI. Client SDKs SHOULD match against these variants by name.
    //
    // InsufficientBalance => code 31 (InsufficientReserveBalance)
    // Unauthorized        => code 12 (primary variant, no alias needed)
    // SlippageExceeded    => code 47 (primary variant, no alias needed)
    // ExpiredDeadline     => code 64 (primary variant, no alias needed)

    /// Canonical alias: operation failed due to insufficient token balance.
    pub const InsufficientBalance: Self = Self::InsufficientReserveBalance;
}

// Contract state keys
pub(crate) const DATA_KEY: Symbol = symbol_short!("DATA");
pub(crate) const SIGNERS_KEY: Symbol = symbol_short!("SIGNERS");
pub(crate) const STAGING_KEY: Symbol = symbol_short!("STAGING");
const PENDING_UPGRADE_KEY: Symbol = symbol_short!("PENDING");
pub(crate) const UPGRADE_DELAY_SECONDS: u64 = 48 * 60 * 60;
const STAKE_REGISTRY_KEY: Symbol = symbol_short!("STAKES");
const TOTAL_STAKED_KEY: Symbol = symbol_short!("TOTAL");
const HEARTBEAT_KEY: Symbol = symbol_short!("HBEAT");
const HB_INTERVAL_KEY: Symbol = symbol_short!("HBINTV");
pub(crate) const DEFAULT_HEARTBEAT_INTERVAL: u64 = 5 * 60;
pub(crate) const SIGNERS_KEY: Symbol = symbol_short!("SIGNERS");
const REVOCATION_KEY: Symbol = symbol_short!("REVOKE");
// Emergency key revocation / blocking
pub(crate) const REVOKED_SIGNER_KEY: Symbol = symbol_short!("REVOKED");
// EMERGENCY_REVOCATION_KEY is defined in admin.rs
const NODE_PROFILES_KEY: Symbol = symbol_short!("NODES");
const PLATFORM_CAPITAL_KEY: Symbol = symbol_short!("CAPITAL");
const CONSENSUS_CACHE_KEY: Symbol = symbol_short!("CACHE");
const RELAYER_TTL_THRESHOLD: u32 = 5_000;
const INSTANCE_TTL_EXTEND: u32 = 100_000;
pub(crate) const TREASURY_KEY: Symbol = symbol_short!("TREASURY");
pub(crate) const LP_REWARD_POOL_KEY: Symbol = symbol_short!("LPREWARD");
pub const LP_SHARE_BPS: u32 = 8000;
pub const TREASURY_SHARE_BPS: u32 = 2000;
pub const FEE_TIER_005_BPS: u32 = 5;
pub const FEE_TIER_030_BPS: u32 = 30;
pub const FEE_TIER_100_BPS: u32 = 100;
pub const DEFAULT_FEE_TIER_BPS: u32 = FEE_TIER_030_BPS;
const SEQUENCE_COUNTER_KEY: Symbol = symbol_short!("SEQCTR");
const REVOCATION_KEY: Symbol = symbol_short!("REVOKE");
const RECOVERY_KEY: Symbol = symbol_short!("RKEY");
const LAST_ADMIN_ACTIVITY: Symbol = symbol_short!("LASTACT");

/// Auto-refund window for locked fiat escrows: the anchor must claim the
/// payout within 24 hours or the sender may reclaim the locked funds.
pub const FIAT_PAYOUT_TIMEOUT_SECS: u64 = 24 * 60 * 60;

#[contracttype]
#[derive(Clone)]
pub struct RevocationProposal {
    pub target: Address,
    pub replacement: Address,
    pub proposer: Address,
    pub proposed_at: u64,
    pub votes: Map<Address, ()>,
}

#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProposalStatus {
    Active,
    Approved,
    Expired,
}

#[contracttype]
#[derive(Clone)]
pub struct ProposalState {
    pub proposed_at: u64,
    pub status: ProposalStatus,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ContractData {
    pub admin: Address,
    pub value: u64,
    pub max_fee_ceiling: u64,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct StakeRecord {
    pub node: Address,
    pub amount: u64,
    pub registered_at: u64,
}

#[contracttype]
#[derive(Clone)]
pub struct NodeProfile {
    pub node: Address,
    pub rate: u64,
    pub confidence: u32,
    pub updated_at: u64,
}

#[contracttype]
#[derive(Clone)]
pub struct CorridorFeePool {
    pub asset: Symbol,
    pub collected: u64,
    pub variable_pool: u64,
}

#[contracttype]
#[derive(Clone)]
pub enum CorridorFeeKey {
    Asset(Symbol),
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct FeedStakeRecord {
    pub node: Address,
    pub asset: Symbol,
    pub amount: u64,
    pub tier: StakingTier,
    pub registered_at: u64,
}

#[contracttype]
pub enum StakingStorageKey {
    TierConfig,
    AssetMetrics(Symbol),
    FeedStake(Address, Symbol),
}

// Storage key newtype wrappers
#[contracttype] pub struct HeartbeatKey(pub AssetId);
#[contracttype] pub struct CorridorFeeKey(pub Symbol);

// CorridorFeePool is imported/used from the fees module

// AssetMetrics key wrapper
#[contracttype] pub struct AssetMetricsKey(pub AssetId);

/// Lifecycle states for a cross-border fiat settlement escrow.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FiatSettlementState {
    Pending,
    Locked,
    Dispatched,
    Settled,
    Refunded,
}

/// A single cross-border fiat settlement escrow record.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct FiatEscrow {
    pub id: u64,
    pub sender: Address,
    pub anchor: Address,
    pub amount: u64,
    pub asset: AssetId,
    pub state: FiatSettlementState,
    pub created_at: u64,
    pub locked_at: u64,
    pub timeout_secs: u64,
}

/// Persistent storage keys for the fiat settlement escrow subsystem.
#[contracttype]
pub enum FiatEscrowKey {
    Escrow(u64),
    Counter,
}

#[contracttype]
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct FeeTierController {
    pub active_tier_bps: u32,
    pub min_tier_bps: u32,
    pub max_tier_bps: u32,
    pub lp_share_bps: u32,
    pub treasury_share_bps: u32,
}

#[contracttype]
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct PoolFeeConfig {
    pub active_tier_bps: u32,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct PoolFeeTierProposal {
    pub asset: AssetId,
    pub new_tier_bps: u32,
    pub proposer: Address,
    pub votes: Vec<Address>,
    pub created_at: u64,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct PoolFeeState {
    pub asset: AssetId,
    pub collected_lp_fees: u64,
    pub collected_treasury_fees: u64,
    pub last_updated: u64,
}

#[contracttype]
pub enum LiquidityPoolFeeKey {
    Controller,
    PoolConfig(AssetId),
    PoolState(AssetId),
    FeeTierProposal(AssetId),
}

#[contract]
pub struct TimeLockedUpgradeContract;

impl TimeLockedUpgradeContract {
    pub(crate) fn load_data(env: &Env) -> Result<ContractData, crate::ContractError> {
        let _ = ensure_schema_version(env);
        env.storage().instance().get(&DATA_KEY).ok_or(crate::ContractError::NotInitialized)
    }

    pub(crate) fn _load_data(env: &Env) -> Result<ContractData, crate::ContractError> {
        Self::load_data(env)
    }
}

#[contractimpl]
impl TimeLockedUpgradeContract {
    /// Atomically consume a nullifier for a private transfer.
    ///
    /// The persistent key is checked and written in this invocation, so a
    /// replay returns before any caller-supplied transfer side effect runs.
    pub fn consume_private_transfer_nullifier(
        env: Env,
        caller: Address,
        nullifier: BytesN<32>,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        crate::zk::nullifier::register_nullifier(&env, nullifier)
    }

    pub fn initialize(env: Env, admin: Address, treasury: Address) -> Result<(), ContractError> {
        if env.storage().instance().has(&DATA_KEY) {
            return Err(ContractError::AlreadyInitialized);
        }
        admin.require_auth();
        let data = ContractData { admin: admin.clone(), value: 0, max_fee_ceiling: 10_000 };
        env.storage().instance().set(&DATA_KEY, &data);
        // #439: write treasury once at deployment; never overwritten
        env.storage().instance().set(&TREASURY_KEY, &treasury);
        Ok(())
    }

    pub fn stake_and_register(env: Env, node: Address, amount: u64) -> Result<StakeRecord, ContractError> {
        if amount == 0 { return Err(ContractError::InvalidStakeAmount); }
        // Guard: a revoked node must not be allowed to re-stake.
        admin::assert_not_revoked(&env, &node)?;
        node.require_auth();
        let stake_key = StakeKey::StakeByNode(node.clone());
        if env.storage().instance().has(&stake_key) {
            return Err(ContractError::AlreadyRegistered);
        }
        let mut stakes: Map<Address, u64> = env
            .storage()
            .instance()
            .get(&STAKE_REGISTRY_KEY)
            .unwrap_or_else(|| Map::new(&env));
        let total: u64 = env
            .storage()
            .instance()
            .get(&TOTAL_STAKED_KEY)
            .unwrap_or(0u64);
        let new_total = total.checked_add(amount).ok_or(ContractError::Overflow)?;
        stakes.set(node.clone(), amount);
        env.storage().instance().set(&STAKE_REGISTRY_KEY, &stakes);
        env.storage().instance().set(&TOTAL_STAKED_KEY, &new_total);
        Self::_record_heartbeat(&env, symbol_to_asset_id(&symbol_short!("STAKE")));
        Ok(StakeRecord { node, amount, registered_at: env.ledger().timestamp() })
    }

    pub fn unstake(env: Env, node: Address) -> Result<u64, ContractError> {
        node.require_auth();
        let mut stakes: Map<Address, u64> = env.storage().instance().get(&STAKE_REGISTRY_KEY).unwrap_or_else(|| Map::new(&env));
        let amount = stakes.get(node.clone()).ok_or(ContractError::NotRegistered)?;
        let total: u64 = env.storage().instance().get(&TOTAL_STAKED_KEY).unwrap_or(0u64);
        let new_total = total.saturating_sub(amount);
        stakes.remove(node.clone());
        env.storage().instance().set(&STAKE_REGISTRY_KEY, &stakes);
        env.storage().instance().set(&TOTAL_STAKED_KEY, &new_total);
        Ok(amount)
    }

    /// Credit direct voting weight into a staker's balance map.
    ///
    /// This seeds the source balance a staker later moves when delegating
    /// voting power to a delegate.
    pub fn set_voting_weight(env: Env, staker: Address, amount: u128) -> Result<(), ContractError> {
        staker.require_auth();
        crate::voting_delegation::set_voting_weight(&env, &staker, amount);
        Ok(())
    }

    /// Delegate the caller's entire direct voting weight to `delegate`.
    ///
    /// The caller's balance map is cleared and the weight is aggregated into
    /// the delegate's total delegated power metric.
    pub fn delegate(env: Env, staker: Address, delegate: Address) -> Result<(), ContractError> {
        staker.require_auth();
        admin::assert_not_revoked(&env, &staker)?;
        crate::voting_delegation::delegate(&env, &staker, &delegate)
    }

    /// Instantly revoke delegated voting power and reclaim direct voting rights.
    ///
    /// Clears the staker's delegate association, recomputes the former
    /// delegate's total delegated power, and restores the voting weight
    /// directly into the staker's balance map.
    pub fn undelegate(env: Env, staker: Address) -> Result<(), ContractError> {
        staker.require_auth();
        admin::assert_not_revoked(&env, &staker)?;
        crate::voting_delegation::undelegate(&env, &staker)?;
        Ok(())
    }

    /// Read the direct voting weight held in a staker's balance map.
    pub fn get_voting_weight(env: Env, staker: Address) -> u128 {
        crate::voting_delegation::get_voting_weight(&env, &staker)
    }

    /// Read the active delegation for a staker, if any.
    pub fn get_delegation(env: Env, staker: Address) -> Option<crate::voting_delegation::Delegation> {
        crate::voting_delegation::get_delegation(&env, &staker)
    }

    /// Read the total voting power delegated to a delegate.
    pub fn get_delegated_total(env: Env, delegate: Address) -> u128 {
        crate::voting_delegation::get_delegated_total(&env, &delegate)
    }

    pub fn remove_signer(env: Env, signer: Address, caller: Address) -> Result<(), ContractError> {
        Self::assert_contract_is_active(&env)?;
        let data = Self::get_data(env.clone())?;
        if data.admin != caller { return Err(ContractError::NotAdmin); }
        caller.require_auth();

        let mut signers = Self::_get_signers(&env);
        signers.remove(signer);
        env.storage().instance().set(&SIGNERS_KEY, &signers);
        Self::_extend_instance_ttl(&env);
        Ok(())
    }

    pub fn vote_revocation(env: Env, voter: Address, sig_expires_at: u64) -> Result<(), ContractError> {
        if env.ledger().timestamp() > sig_expires_at { return Err(ContractError::SignatureExpired); }
        // Guard: a revoked address must not be allowed to vote on governance actions.
        admin::assert_not_revoked(&env, &voter)?;
        voter.require_auth();
        let data = Self::get_data(env.clone())?;

        if !Self::_is_signer(&env, &voter) && data.admin != voter {
            return Err(ContractError::Unauthorized);
        }

        let mut proposal: RevocationProposal = env.storage().instance().get(&REVOCATION_KEY).ok_or(ContractError::NoActiveProposal)?;

        if proposal.votes.contains_key(voter.clone()) {
            return Err(ContractError::AlreadyVoted);
        }

        proposal.votes.set(voter, ());

        let threshold = Self::_revocation_threshold(&env);
        if proposal.votes.len() >= threshold {
            let mut contract_data = data;
            contract_data.admin = proposal.replacement.clone();
            env.storage().instance().set(&DATA_KEY, &contract_data);
            env.storage().instance().remove(&REVOCATION_KEY);
        } else {
            env.storage().instance().set(&REVOCATION_KEY, &proposal);
        }
        Ok(())
    }

    // --- Core Logic ---

    pub fn get_data(env: Env) -> Result<ContractData, ContractError> {
        env.storage().instance().get(&DATA_KEY).ok_or(ContractError::NotInitialized)
    }

    pub fn verify_storage_ttl(env: Env) -> Result<(), ContractError> {
        verify_storage_ttl_bumps(&env)
    }

    pub fn verify_zero_loss(env: Env) -> Result<(), ContractError> {
        verify_zero_loss_accounting(&env)
    }

    pub fn verify_contract_state(env: Env) -> Result<(), ContractError> {
        verify_contract_state(&env)
    }

    pub fn propose_upgrade(
        env: Env, new_wasm_hash: BytesN<32>, proposer: Address,
        signers: Vec<Address>,
        nonce: u64, salt: Bytes, salt_signature: BytesN<32>, sig_expires_at: u64,
    ) -> Result<(), ContractError> {
        if env.ledger().timestamp() > sig_expires_at { return Err(ContractError::SignatureExpired); }
        admin::assert_not_revoked(&env, &proposer)?;
        let data = Self::get_data(env.clone())?;
        if data.admin != proposer { return Err(ContractError::NotAdmin); }
        proposer.require_auth();
        consume_nonce(&env, &proposer, nonce, salt, salt_signature)?;

        // Verify multi-sig quorum threshold
        let collected_weight = calculate_collected_weight(&env, &signers, &data)?;
        let multisig_config = get_multisig_config(&env);
        if collected_weight < multisig_config.required_weight {
            return Err(ContractError::ThresholdNotReached);
        }

        let staged_at = env.ledger().timestamp();
        let proposal = GovernanceUpgradeProposal {
            new_wasm_hash: new_wasm_hash.clone(),
            proposer: proposer.clone(),
            staged_at,
            signers: signers.clone(),
        };
        env.storage().instance().set(&GOVERNANCE_UPGRADE_KEY, &proposal);
        Self::_store_proposal_state(&env, GOVERNANCE_UPGRADE_KEY, staged_at);

        let staged = StagedUpgrade {
            new_wasm_hash: new_wasm_hash.clone(),
            proposer: proposer.clone(),
            staged_at,
            execute_at: staged_at + UPGRADE_DELAY_SECONDS,
        };
        env.storage().instance().set(&PENDING_UPGRADE_KEY, &staged);

        // Emit GovernanceUpgradeProposed event
        let _ = emit_simple2(
            &env,
            EV_UPGRADE_PROPOSED,
            symbol_short!("gov"),
            GovernanceUpgradeProposedEvent {
                new_wasm_hash,
                proposer: proposer.clone(),
                signers,
                staged_at,
                required_weight: multisig_config.required_weight,
                collected_weight,
            },
        );

        crate::instance::bump_instance_ttl(&env);
        Ok(())
    }

    pub fn execute_upgrade(env: Env, executor: Address, nonce: u64, salt: Bytes, signature: BytesN<32>, sig_expires_at: u64) -> Result<(), ContractError> {
        if env.ledger().timestamp() > sig_expires_at { return Err(ContractError::SignatureExpired); }
        let data = Self::get_data(env.clone())?;
        if data.admin != executor { return Err(ContractError::NotAdmin); }
        executor.require_auth();
        consume_nonce(&env, &executor, nonce, salt, signature)?;
        let pending: StagedUpgrade = env.storage().instance().get(&PENDING_UPGRADE_KEY).ok_or(ContractError::NoPendingUpgrade)?;
        if !verify_staged_delay(pending.staged_at, env.ledger().sequence()) {
            return Err(ContractError::UpgradeTimelockNotSatisfied);
        }
        env.deployer().update_current_contract_wasm(pending.wasm_hash.to_array());
        env.storage().instance().remove(&PENDING_UPGRADE_KEY);
        Self::_remove_proposal_state(&env, GOVERNANCE_UPGRADE_KEY);
        crate::instance::bump_instance_ttl(&env);
        Ok(())
    }

    /// Run diagnostic checks after upgrade to assert storage integrity post-upgrade.
    /// Returns UpgradeHealthCheckFailed if any invariant is violated.
    fn _run_post_upgrade_health_check(env: &Env, pre_upgrade_data: ContractData) -> Result<(), ContractError> {
        // Diagnostic 1: Verify admin still exists and is accessible
        let post_upgrade_data = Self::_load_data(env)?;
        if post_upgrade_data.admin != pre_upgrade_data.admin {
            return Err(ContractError::UpgradeHealthCheckFailed);
        }

        // Diagnostic 2: Verify core state keys are still accessible
        if !env.storage().instance().has(&DATA_KEY) {
            return Err(ContractError::UpgradeHealthCheckFailed);
        }

        // Diagnostic 3: Verify treasury address is still present (immutable after deployment)
        let treasury: Option<Address> = env.storage().instance().get(&TREASURY_KEY);
        if treasury.is_none() {
            return Err(ContractError::UpgradeHealthCheckFailed);
        }

        // Diagnostic 4: Verify instance storage is still readable
        let total_staked: u64 = env.storage().instance().get(&TOTAL_STAKED_KEY).unwrap_or(0u64);
        if total_staked > u64::MAX {
            return Err(ContractError::UpgradeHealthCheckFailed);
        }

        // Diagnostic 5: Verify signers map is still accessible
        let _signers: Map<Address, ()> = env.storage().instance().get(&SIGNERS_KEY).unwrap_or_else(|| Map::new(env));

        // Diagnostic 6: Verify heartbeat interval is still accessible
        let _heartbeat_interval: u64 = env.storage().instance().get(&HB_INTERVAL_KEY).unwrap_or(DEFAULT_HEARTBEAT_INTERVAL);

        Ok(())
    }

    pub fn get_pending_upgrade(env: Env) -> Option<StagedUpgrade> {
        env.storage().instance().get(&PENDING_UPGRADE_KEY)
    }

    pub fn get_upgrade_timelock_remaining(env: Env) -> Option<u32> {
        env.storage().instance().get(&PENDING_UPGRADE_KEY).map(|pending: StagedUpgrade| {
            let current = env.ledger().sequence();
            let elapsed = current.saturating_sub(pending.staged_at);
            MIN_LEDGER_DELAY.saturating_sub(elapsed)
        })
    }

    pub fn cancel_upgrade(env: Env, canceller: Address) -> Result<(), ContractError> {
        let data = Self::get_data(env.clone())?;
        if data.admin != canceller { return Err(ContractError::NotAdmin); }
        canceller.require_auth();
        env.storage().instance().remove(&PENDING_UPGRADE_KEY);
        Self::_extend_instance_ttl(&env);
        crate::instance::bump_instance_ttl(&env);
        Ok(())
    }

    pub fn set_current_wasm(env: Env, admin: Address, wasm_hash: BytesN<32>) -> Result<(), ContractError> {
        let data = Self::_load_data(&env)?;
        if data.admin != admin { return Err(ContractError::NotAdmin); }
        admin.require_auth();
        env.storage().instance().set(&crate::upgrades::rollback::CURRENT_WASM_KEY, &wasm_hash);
        Ok(())
    }

    pub fn set_value(env: Env, new_value: u64, caller: Address, nonce: u64, salt: Bytes, signature: BytesN<32>, sig_expires_at: u64) -> Result<(), ContractError> {
        if env.ledger().timestamp() > sig_expires_at { return Err(ContractError::SignatureExpired); }
        let mut data = Self::get_data(env.clone())?;
        if data.admin != caller { return Err(ContractError::NotAdmin); }
        caller.require_auth();
        consume_nonce(&env, &caller, nonce, salt, signature)?;
        let mut seq_map: Map<Address, u64> = env.storage().instance().get(&SEQUENCE_COUNTER_KEY).unwrap_or_else(|| Map::new(&env));
        seq_map.set(caller, nonce);
        env.storage().instance().set(&SEQUENCE_COUNTER_KEY, &seq_map);
        data.value = new_value;
        env.storage().instance().set(&DATA_KEY, &data);
        Self::_record_heartbeat(&env, symbol_to_asset_id(&symbol_short!("VALUE")));
        Ok(())
    }

    pub fn get_coordinator_nonce(env: Env, coordinator: Address) -> u64 {
        get_nonce(&env, &coordinator)
    }

    /// Takes an `AssetId` like its siblings `update_heartbeat` and
    /// `is_data_fresh`; callers hash a `Symbol` with `symbol_to_asset_id`.
    pub fn get_last_update_timestamp(env: Env, asset: AssetId) -> Option<u64> {
        let heartbeat_key = HeartbeatKey::HeartbeatByAsset(asset);
        env.storage().temporary().get(&heartbeat_key)
    }

    pub fn get_heartbeat_interval(env: Env) -> u64 {
        Self::_get_interval(&env)
    }

    pub fn set_heartbeat_interval(env: Env, interval: u64, admin: Address) -> Result<(), ContractError> {
        if interval == 0 { return Err(ContractError::InvalidHeartbeatInterval); }
        let data = Self::get_data(env.clone())?;
        if data.admin != admin { return Err(ContractError::NotAdmin); }
        admin.require_auth();
        env.storage().instance().set(&HB_INTERVAL_KEY, &interval);
        Self::_extend_instance_ttl(&env);
        Ok(())
    }

    pub fn get_stake(env: Env, node: Address) -> u64 {
        let stakes: Map<Address, u64> = env.storage().instance().get(&STAKE_REGISTRY_KEY).unwrap_or_else(|| Map::new(&env));
        stakes.get(node).unwrap_or(0u64)
    }

    pub fn get_total_staked(env: Env) -> u64 {
        env.storage().instance().get(&TOTAL_STAKED_KEY).unwrap_or(0u64)
    }

    /// Update a validator's profile for a premium asset pool.
    pub fn update_validator_profile(
        env: Env,
        node: Address,
        pool: Symbol,
    ) -> Result<(), ContractError> {
        node.require_auth();
        check_bond_capacity(&env, &node, &pool)?;
        Self::_record_heartbeat(&env, symbol_to_asset_id(&pool));
        Ok(())
    }

    pub fn update_heartbeat(env: Env, asset: AssetId, updater: Address) -> Result<(), ContractError> {
        let data = Self::get_data(env.clone())?;
        if data.admin != updater { return Err(ContractError::NotAdmin); }
        updater.require_auth();
        Self::_record_heartbeat(&env, asset);
        Self::_extend_instance_ttl(&env);
        Ok(())
    }

    pub fn is_data_fresh(env: Env, asset: AssetId) -> bool {
        let heartbeat_key = storage::HeartbeatKey::HeartbeatByAsset(asset);
        if let Some(last_update) = env.storage().temporary().get::<_, u64>(&heartbeat_key) {
            env.ledger().timestamp().saturating_sub(last_update) <= Self::_get_interval(&env)
        } else {
            false
        }
    }


    pub fn upsert_node_profile(env: Env, admin: Address, node: Address, rate: u64, confidence: u32) -> Result<(), ContractError> {
        let data = Self::get_data(env.clone())?;
        if data.admin != admin { return Err(ContractError::NotAdmin); }
        admin.require_auth();
        let mut profiles = Self::_get_node_profiles(&env);
        profiles.set(node.clone(), NodeProfile { node, rate, confidence, updated_at: env.ledger().timestamp() });
        env.storage().persistent().set(&NODE_PROFILES_KEY, &profiles);
        Self::_extend_instance_ttl(&env);
        Ok(())
    }

    pub fn get_latest_rate(env: Env, node: Address) -> Result<u64, ContractError> {
        Self::_maintain_relayer_profile_ttl(&env);
        let profile_key = NodeProfileKey::ProfileByNode(node);
        let profile: NodeProfile = env.storage().persistent().get(&profile_key)
            .ok_or(ContractError::NotRegistered)?;
        Self::_scan_profile_for_rate(profile).ok_or(ContractError::NotRegistered)
    }

    pub fn add_corridor_fees(
        env: Env,
        admin: Address,
        asset: AssetId,
        collected: u64,
        variable_fee: u64,
    ) -> Result<fees::CorridorFeePool, ContractError> {
        let pool = fees::add_corridor_fees(env.clone(), admin, asset, collected, variable_fee)?;
        Self::_extend_instance_ttl(&env);
        crate::recovery::update_admin_activity(&env);
        Ok(pool)
    }
    pub fn get_corridor_fee_pool(env: Env, asset: AssetId) -> fees::CorridorFeePool {
        crate::fees::get_corridor_fee_pool(env, asset)
    }

    pub fn add_corridor_fees(
        env: Env,
        admin: Address,
        asset: AssetId,
        collected: u64,
        variable_fee: u64,
    ) -> Result<fees::CorridorFeePool, ContractError> {
        crate::fees::add_corridor_fees(env, admin, asset, collected, variable_fee)
    }

    pub fn record_lp_fee(
        env: Env,
        admin: Address,
        asset: AssetId,
        fee_amount: u64,
    ) -> Result<settlement::fees::LiquidityPool, ContractError> {
        settlement::fees::record_fee(&env, admin, asset, fee_amount)
    }

    pub fn add_lp_liquidity(
        env: Env,
        provider: Address,
        asset: AssetId,
        reserve_a: u128,
        reserve_b: u128,
        lp_units: u64,
    ) -> Result<settlement::fees::LiquidityPosition, ContractError> {
        settlement::fees::add_liquidity(
            &env,
            provider,
            asset,
            reserve_a,
            reserve_b,
            lp_units,
        )
    }

    pub fn deposit_single_asset(
        env: Env,
        provider: Address,
        asset: AssetId,
        amount_in: u128,
        is_asset_a: bool,
    ) -> Result<(settlement::fees::LiquidityPosition, u128, u128), ContractError> {
        settlement::fees::deposit_single_asset(
            &env,
            provider,
            asset,
            amount_in,
            is_asset_a,
        )
    }

    pub fn redeem_lp_liquidity(
        env: Env,
        provider: Address,
        asset: AssetId,
        lp_units: u64,
    ) -> Result<settlement::fees::RedemptionResult, ContractError> {
        settlement::fees::redeem_liquidity(&env, provider, asset, lp_units)
    }

    pub fn get_lp_pool(env: Env, asset: AssetId) -> settlement::fees::LiquidityPool {
        settlement::fees::get_pool(&env, asset)
    }

    pub fn get_lp_position(
        env: Env,
        asset: AssetId,
        provider: Address,
    ) -> Option<settlement::fees::LiquidityPosition> {
        settlement::fees::get_position(&env, asset, provider)
    }

    /// Record flash loan fee revenue for an asset.
    pub fn record_flash_fee(
        env: Env,
        asset: AssetId,
        fee_amount: u64,
    ) -> Result<u64, ContractError> {
        fees::record_flash_fee(&env, asset, fee_amount)
    }

    /// Query the flash loan fee pool status for an asset.
    pub fn get_flash_fee_pool(env: Env, asset: AssetId) -> fees::FlashLoanFeePool {
        fees::get_flash_fee_pool(&env, asset)
    }

    /// Set the LP reward pool destination address for flash fee distributions.
    pub fn set_lp_reward_pool(
        env: Env,
        admin: Address,
        lp_reward_pool: Address,
    ) -> Result<(), ContractError> {
        fees::set_lp_reward_pool(&env, &admin, lp_reward_pool)
    }

    /// Distribute accumulated flash loan service fees (50% to LP reward pool and 50% to DAO treasury).
    pub fn distribute_flash_fees(
        env: Env,
        caller: Address,
        asset: AssetId,
    ) -> Result<(u64, u64), ContractError> {
        fees::distribute_flash_fees(&env, &caller, asset)
    }

    /// Get the current dynamic trading fee for an asset (in basis points)
    pub fn get_current_dynamic_fee(env: Env, asset: AssetId) -> u32 {
        crate::fees::get_current_dynamic_fee(&env, asset)
    }

    /// Admin function to configure dynamic fee parameters
    pub fn set_dynamic_fee_config(
        env: Env,
        caller: Address,
        asset: AssetId,
        min_fee_bps: u32,
        max_fee_bps: u32,
        period_seconds: u64,
    ) -> Result<(), ContractError> {
        crate::fees::set_dynamic_fee_config(&env, &caller, asset, min_fee_bps, max_fee_bps, period_seconds)
    }

    /// Update volume history and get the current dynamic fee (called internally during swaps)
    pub(crate) fn update_volume_and_get_fee(env: &Env, asset: AssetId, trade_volume: u64) -> Result<u32, ContractError> {
        crate::fees::update_volume_and_adjust_fee(env, asset, trade_volume)
    }

    /// Calculate and deduct the dynamic fee from a trade amount
    pub(crate) fn calculate_and_deduct_fee(amount: u128, fee_bps: u32) -> Result<(u128, u128), ContractError> {
        crate::fees::calculate_and_deduct_fee(amount, fee_bps)
    }

    pub fn set_corridor_weight(
        env: Env, admin: Address, asset: AssetId, base_weight: u64, dynamic_weight: u64,
    ) -> Result<fees::CorridorWeightProfile, ContractError> {
        let profile = fees::set_corridor_weight(env.clone(), admin, asset, base_weight, dynamic_weight)?;
        Self::_extend_instance_ttl(&env);
        crate::recovery::update_admin_activity(&env);
        Ok(profile)
    }

    pub fn add_corridor_fees(env: Env, asset: Symbol, collected: u64, variable_fee: u64) -> Result<CorridorFeePool, ContractError> {
        let key = CorridorFeeKey::Asset(asset.clone());
        let mut pool: CorridorFeePool = env.storage().persistent().get(&key).unwrap_or(CorridorFeePool { asset: asset.clone(), collected: 0, variable_pool: 0 });
        pool.collected = pool.collected.checked_add(collected).ok_or(ContractError::Overflow)?;
        pool.variable_pool = pool.variable_pool.checked_add(variable_fee).ok_or(ContractError::Overflow)?;
        env.storage().persistent().set(&key, &pool);
        Ok(pool)
    }

    // ── Dynamic Staking Tier Assignment (Issue #300) ─────────────────────────

    /// Configure the minimum stake required for each collateral tier.
    pub fn set_staking_tier_config(
        env: Env,
        admin: Address,
        config: StakingTierConfig,
    ) -> Result<(), ContractError> {
        let data = Self::get_data(env.clone())?;
        if data.admin != admin {
            return Err(ContractError::NotAdmin);
        }
        admin.require_auth();
        validate_tier_config(&config)?;
        env.storage()
            .instance()
            .set(&StakingStorageKey::TierConfig, &config);
        Self::_extend_instance_ttl(&env);
        Ok(())
    }

    /// Return the active staking tier configuration.
    pub fn get_staking_tier_config(env: Env) -> StakingTierConfig {
        env.storage()
            .instance()
            .get(&StakingStorageKey::TierConfig)
            .unwrap_or_default()
    }

    /// Set the volume and volatility profile for a currency feed.
    pub fn set_asset_feed_metrics(
        env: Env,
        admin: Address,
        asset: Symbol,
        volume_score_floor: u32,
        volatility_bps: u32, // This argument was missing a comma in the original code.
        signers: Vec<Address>,
    ) -> Result<AssetFeedMetrics, ContractError> {
        let data = Self::get_data(env.clone())?;
        if data.admin != admin {
            return Err(ContractError::NotAdmin);
        }
        admin.require_auth();

        let metrics = AssetFeedMetrics {
            volume_score: volume_score_floor.min(100),
            volatility_bps,
        };

        env.storage()
            .persistent()
            .set(&StakingStorageKey::AssetMetrics(asset.clone()), &metrics);

        Self::_extend_instance_ttl(&env);
        Ok(metrics)
    }

    /// Return the resolved feed metrics for an asset, including corridor volume.
    pub fn get_asset_feed_metrics(env: Env, asset: Symbol) -> AssetFeedMetrics {
        Self::_resolve_feed_metrics(&env, &asset)
    }

    /// Return the staking tier assigned to a currency feed.
    pub fn get_staking_tier(env: Env, asset: Symbol) -> StakingTier {
        assign_tier(&Self::_resolve_feed_metrics(&env, &asset))
    }

    fn _resolve_feed_metrics(env: &Env, asset: &Symbol) -> AssetFeedMetrics {
        let pool = Self::get_corridor_fee_pool(env.clone(), asset.clone());
        let stored: AssetFeedMetrics = env
            .storage()
            .persistent()
            .get(&StakingStorageKey::AssetMetrics(asset.clone()))
            .unwrap_or(AssetFeedMetrics {
                volume_score: 0,
                volatility_bps: 0,
            });

        AssetFeedMetrics {
            volume_score: effective_volume_score(stored.volume_score, pool.collected),
            volatility_bps: stored.volatility_bps,
        }
    }

    /// Return the minimum stake a validator must post for a currency feed.
    pub fn get_required_stake(env: Env, asset: Symbol) -> u64 {
        let tier = Self::get_staking_tier(env.clone(), asset);
        let config = Self::get_staking_tier_config(env);
        required_stake_for_tier(tier, &config)
    }

    /// Register a validator node for a specific currency feed with tier-aware collateral.
    pub fn stake_and_register_for_feed(
        env: Env,
        node: Address,
        asset: Symbol,
        amount: u64,
    ) -> Result<FeedStakeRecord, ContractError> {
        if amount == 0 {
            return Err(ContractError::InvalidStakeAmount);
        }
        // Guard: revoked nodes must not be allowed to register for feeds.
        admin::assert_not_revoked(&env, &node)?;
        node.require_auth();

        let feed_key = StakingStorageKey::FeedStake(node.clone(), asset.clone());
        if env.storage().persistent().has(&feed_key) {
            return Err(ContractError::FeedAlreadyRegistered);
        }

        let tier = Self::get_staking_tier(env.clone(), asset.clone());
        let required = Self::get_required_stake(env.clone(), asset.clone());
        if amount < required {
            return Err(ContractError::InsufficientStakeForTier);
        }

        env.storage().persistent().set(&feed_key, &amount);

        let mut stakes: Map<Address, u64> = env
            .storage()
            .instance()
            .get(&STAKE_REGISTRY_KEY)
            .unwrap_or_else(|| Map::new(&env));
        let node_total = stakes.get(node.clone()).unwrap_or(0);
        let new_node_total = node_total
            .checked_add(amount)
            .ok_or(ContractError::Overflow)?;
        stakes.set(node.clone(), new_node_total);

        let total: u64 = env
            .storage()
            .instance()
            .get(&TOTAL_STAKED_KEY)
            .unwrap_or(0u64);
        let new_total = total.checked_add(amount).ok_or(ContractError::Overflow)?;

        env.storage().instance().set(&STAKE_REGISTRY_KEY, &stakes);
        env.storage().instance().set(&TOTAL_STAKED_KEY, &new_total);
        Self::_record_heartbeat(&env, symbol_to_asset_id(&asset));

        Ok(FeedStakeRecord {
            node,
            asset,
            amount,
            tier,
            registered_at: env.ledger().timestamp(),
        })
    }

    /// Withdraw collateral from a currency feed and deregister the node for that feed.
    pub fn unstake_from_feed(env: Env, node: Address, asset: Symbol) -> Result<u64, ContractError> {
        node.require_auth();

        let feed_key = StakingStorageKey::FeedStake(node.clone(), asset.clone());
        let amount: u64 = env
            .storage()
            .persistent()
            .get(&feed_key)
            .ok_or(ContractError::NotRegistered)?;

        env.storage().persistent().remove(&feed_key);

        let mut stakes: Map<Address, u64> = env
            .storage()
            .instance()
            .get(&STAKE_REGISTRY_KEY)
            .unwrap_or_else(|| Map::new(&env));
        let node_total = stakes.get(node.clone()).unwrap_or(0);
        let new_node_total = node_total.saturating_sub(amount);
        if new_node_total == 0 {
            stakes.remove(node.clone());
        } else {
            stakes.set(node.clone(), new_node_total);
        }

        let total: u64 = env
            .storage()
            .instance()
            .get(&TOTAL_STAKED_KEY)
            .unwrap_or(0u64);
        let new_total = total.saturating_sub(amount);

        env.storage().instance().set(&STAKE_REGISTRY_KEY, &stakes);
        env.storage().instance().set(&TOTAL_STAKED_KEY, &new_total);

        Ok(amount)
    }

    /// Return the collateral posted by a node for a specific currency feed.
    pub fn get_feed_stake(env: Env, node: Address, asset: Symbol) -> u64 {
        env.storage()
            .persistent()
            .get(&StakingStorageKey::FeedStake(node, asset))
            .unwrap_or(0)
    }

    pub fn get_corridor_fee_pool(env: Env, asset: Symbol) -> CorridorFeePool {
        env.storage().persistent().get(&CorridorFeeKey::Asset(asset.clone())).unwrap_or(CorridorFeePool { asset, collected: 0, variable_pool: 0 })
    }

    pub fn set_platform_capital(env: Env, capital: u64) {
        env.storage().instance().set(&PLATFORM_CAPITAL_KEY, &capital);
    }

    pub fn finalize_consensus(env: Env) {
        env.storage().temporary().remove(&CONSENSUS_CACHE_KEY);
        env.storage().temporary().remove(&HEARTBEAT_KEY);
    }

    pub fn register_signer(env: Env, signer: Address, caller: Address) -> Result<(), ContractError> {
        admin::assert_not_revoked(&env, &caller)?;
        let data = Self::get_data(env.clone())?;
        if data.admin != caller { return Err(ContractError::NotAdmin); }
        caller.require_auth();
        let mut signers = Self::_get_signers(&env);
        if !signers.contains_key(signer.clone()) {
            signers.set(signer, ());
            env.storage().instance().set(&SIGNERS_KEY, &signers);
        }
        Self::_extend_instance_ttl(&env);
        Ok(())
    }

    // --- Admin Ownership Transfer (Issue #429) ---

    pub fn propose_ownership_transfer(env: Env, current_admin: Address, nominee: Address) -> Result<(), ContractError> {
        admin::propose_ownership_transfer(&env, current_admin, nominee)?;
        Self::_extend_instance_ttl(&env);
        Ok(())
    }

    pub fn claim_ownership(env: Env, claimer: Address) -> Result<(), ContractError> {
        admin::claim_ownership(&env, claimer)?;
        Self::_extend_instance_ttl(&env);
        Ok(())
    }

    // #439: read-only treasury accessor; no setter exposed
    pub fn get_treasury(env: Env) -> Result<Address, ContractError> {
        env.storage().instance().get(&TREASURY_KEY).ok_or(ContractError::NotInitialized)
    }

    // #423: emergency pause controls
    pub fn set_paused(env: Env, caller: Address, paused: bool) -> Result<(), ContractError> {
        admin::set_paused(&env, caller, paused)
    }

    pub fn is_paused(env: Env) -> bool {
        admin::is_paused(&env)
    }

    // #432: pre-flight rent check hook
    pub fn preflight_rent_check(env: Env) {
        storage::preflight_rent_check(&env);
    }

    // ── Governance Proposal Execution Timelock Cancellation (Issue #796) ──

    /// Submit a governance proposal for contract upgrade with timelock.
    pub fn submit_governance_proposal(
        env: Env,
        proposer: Address,
        wasm_hash: BytesN<32>,
    ) -> Result<u64, ContractError> {
        admin::assert_not_revoked(&env, &proposer)?;
        governance::submit_governance_proposal(&env, proposer, wasm_hash)
    }

    /// Vote to cancel a governance proposal during its timelock window.
    pub fn vote_cancel_governance_proposal(
        env: Env,
        voter: Address,
        proposal_id: u64,
        sig_expires_at: u64,
    ) -> Result<(), ContractError> {
        admin::assert_not_revoked(&env, &voter)?;
        let data = Self::get_data(env.clone())?;
        if !Self::_is_signer(&env, &voter) && data.admin != voter {
            return Err(ContractError::Unauthorized);
        }
        governance::vote_cancel_proposal(&env, voter, proposal_id, sig_expires_at)
    }

    /// Admin-only direct cancellation of a governance proposal.
    pub fn cancel_governance_proposal(
        env: Env,
        canceller: Address,
        proposal_id: u64,
    ) -> Result<(), ContractError> {
        let data = Self::get_data(env.clone())?;
        if data.admin != canceller { return Err(ContractError::NotAdmin); }
        governance::cancel_governance_proposal(&env, canceller, proposal_id)
    }

    /// Query a governance proposal by ID.
    pub fn get_governance_proposal(
        env: Env,
        proposal_id: u64,
    ) -> Result<GovernanceProposal, ContractError> {
        governance::get_governance_proposal(&env, proposal_id)
    }

    /// Return the number of ledger sequences remaining before a governance
    /// proposal's timelock elapses.
    pub fn get_gov_proposal_tl(
        env: Env,
        proposal_id: u64,
    ) -> Option<u32> {
        governance::get_gov_proposal_tl_remaining(&env, proposal_id)
    }

    /// Check whether a governance proposal is eligible for execution.
    pub fn is_gov_proposal_executable(
        env: Env,
        proposal_id: u64,
    ) -> bool {
        governance::is_proposal_executable(&env, proposal_id)
    }

    // ── Emergency Key Revocation (multi-sig coordinator group) ───────────────

    /// Phase 1: any registered signer or the current admin opens an emergency
    /// revocation proposal against a compromised hot-wallet address.
    ///
    /// The caller must not be the target.  Only one proposal may be active
    /// at a time.
    pub fn propose_emergency_revocation(
        env: Env,
        proposer: Address,
        target: Address,
        replacement: Address,
    ) -> Result<(), ContractError> {
        // Guard: a revoked coordinator must not be able to open proposals.
        admin::assert_not_revoked(&env, &proposer)?;
        admin::propose_emergency_revocation(&env, proposer, target, replacement)
    }

    /// Phase 2: any registered signer or the current admin casts a vote on
    /// the active emergency revocation proposal.
    ///
    /// Once majority threshold is reached the target address is **immediately**
    /// blocked in storage (`REVOKED_SIGNER_KEY`) and removed from the signer
    /// set, preventing it from signing or modifying configurations from that
    /// point forward.
    pub fn vote_emergency_revocation(
        env: Env, voter: Address, sig_expires_at: u64, nonce: u64,
    ) -> Result<(), ContractError> {
        admin::vote_emergency_revocation(&env, voter, sig_expires_at, nonce)
    }

    pub fn get_emergency_revocation(env: Env) -> Option<admin::EmergencyRevocationProposal> {
        admin::get_emergency_revocation_proposal(&env)
    }

    /// This can be called by any party since the primary security model relies on
    /// the voting threshold for proposal execution, not on proposal creation.
    pub fn purge_expired_revocation_prop(env: Env) -> Result<(), ContractError> {
        let result = admin::purge_emergency_revocation_proposal(&env);
        if result.is_ok() {
            Self::_remove_proposal_state(&env, EMERGENCY_REVOCATION_TOPIC);
        }
        result
    }

    pub fn has_active_revocation_proposal(env: Env) -> bool {
        admin::has_active_emergency_revocation(&env)
    }

    /// Expire multi-sig proposals whose approval threshold was not reached
    /// within `PROPOSAL_EXPIRY_SECONDS`. Cleans the tracked proposal state and
    /// releases any locked upgrade/revocation state so storage deposits are
    /// reclaimed by the contract.
    pub fn cleanup_expired_proposals(env: Env) -> Result<u32, ContractError> {
        let mut expired_count = 0u32;
        let now = env.ledger().timestamp();
        let states: Map<Symbol, ProposalState> = env
            .storage()
            .instance()
            .get(&PROPOSAL_STATE_KEY)
            .unwrap_or_else(|| Map::new(&env));
        let topics: Vec<Symbol> = states.keys();
        for topic_ref in topics.iter() {
            let topic = *topic_ref;
            if let Some(state) = states.get(&topic) {
                if state.status == ProposalStatus::Active
                    && now.saturating_sub(state.proposed_at) >= PROPOSAL_EXPIRY_SECONDS
                {
                    if topic == REVOCATION_KEY {
                        close_ballot(&env, REVOCATION_KEY);
                    } else if topic == GOVERNANCE_UPGRADE_KEY {
                        env.storage().instance().remove(&GOVERNANCE_UPGRADE_KEY);
                        env.storage().instance().remove(&PENDING_UPGRADE_KEY);
                    } else if topic == EMERGENCY_REVOCATION_TOPIC {
                        admin::purge_emergency_revocation_proposal(&env)?;
                    }
                    Self::_mark_proposal_expired(&env, topic);
                    expired_count = expired_count.saturating_add(1);
                }
            }
        }
        Ok(expired_count)
    }

    // ── Governance Proposal Veto Engine (Issue #769) ────────────────────────────
    // 
    // Emergency veto control allowing the designated Security Council multi-sig
    // address to cancel malicious or dangerous proposals during their timelock
    // windows, providing a last-resort circuit-breaker mechanism.

    /// Configure the Security Council address that has authority to veto proposals.
    ///
    /// Only the current admin may set the Security Council. Once configured,
    /// this multi-sig address gains exclusive authority to veto any proposal.
    ///
    /// # Arguments
    /// * `env` - The contract environment
    /// * `caller` - The caller (must be the contract admin)
    /// * `council` - The Security Council multi-sig address
    ///
    /// # Errors
    /// - [`ContractError::NotAdmin`] if the caller is not the contract admin
    /// - [`ContractError::NotInitialized`] if the contract is not initialized
    pub fn set_security_council(env: Env, caller: Address, council: Address) -> Result<(), ContractError> {
        veto::set_security_council(&env, caller, council)
    }

    /// Retrieve the current Security Council address, if configured.
    pub fn get_security_council(env: Env) -> Option<Address> {
        veto::get_security_council(&env)
    }

    /// Veto an active proposal, instantly transitioning it to `Vetoed` state.
    ///
    /// Only the designated Security Council may invoke this function. Upon veto:
    /// 1. The proposal is marked as vetoed
    /// 2. Execution payload is invalidated
    /// 3. Audit trail is recorded with reason hash
    /// 4. `ProposalVetoed` event is emitted
    ///
    /// # Arguments
    /// * `env` - The contract environment
    /// * `caller` - The address attempting the veto (must be Security Council)
    /// * `proposal_id` - The ID of the proposal to veto
    /// * `reason` - Audit reason string (logged as hash for transparency)
    ///
    /// # Errors
    /// - [`ContractError::NotSecurityCouncil`] if the caller is not the Security Council
    /// - [`ContractError::ProposalNotFound`] if the proposal does not exist
    /// - [`ContractError::ProposalAlreadyVetoed`] if the proposal is already vetoed
    pub fn veto_proposal(
        env: Env,
        caller: Address,
        proposal_id: u64,
        reason: soroban_sdk::String,
    ) -> Result<(), ContractError> {
        veto::veto_proposal(&env, caller, proposal_id, reason)
    }

    /// Retrieve the veto record for a proposal, if it has been vetoed.
    ///
    /// Returns None if the proposal has not been vetoed.
    pub fn get_veto_record(env: Env, proposal_id: u64) -> Option<crate::veto::ProposalVeto> {
        veto::get_veto_record(&env, proposal_id)
    }

    /// Check if a proposal has been vetoed.
    pub fn is_proposal_vetoed(env: Env, proposal_id: u64) -> bool {
        veto::is_proposal_vetoed(&env, proposal_id)
    }

    // ── Timelocked Protocol Treasury Emergency Rescue Handler (Issue #783) ───

    /// Register a token address as a protected asset (primary pool or vault reserve asset).
    /// Protected assets CANNOT be extracted via emergency rescue.
    pub fn register_protected_asset(
        env: Env,
        caller: Address,
        asset: Address,
    ) -> Result<(), ContractError> {
        rescue::register_protected_asset(&env, caller, asset)
    }

    /// Check if a token address is a protected asset.
    pub fn is_protected_asset(env: Env, asset: Address) -> bool {
        rescue::is_protected_asset(&env, &asset)
    }

    /// Queue a governance proposal for recovering mis-sent non-protocol tokens.
    pub fn queue_token_rescue(
        env: Env,
        proposer: Address,
        token: Address,
        amount: i128,
        recipient: Address,
    ) -> Result<u64, ContractError> {
        rescue::queue_token_rescue(&env, proposer, token, amount, recipient)
    }

    /// Execute token transfer to treasury address once mandatory timelock expires.
    pub fn execute_token_rescue(
        env: Env,
        executor: Address,
        proposal_id: u64,
    ) -> Result<(), ContractError> {
        rescue::execute_token_rescue(&env, executor, proposal_id)
    }

    /// Cancel a pending token rescue proposal during its timelock window.
    pub fn cancel_token_rescue(
        env: Env,
        canceller: Address,
        proposal_id: u64,
    ) -> Result<(), ContractError> {
        rescue::cancel_token_rescue(&env, canceller, proposal_id)
    }

    /// Get details of a rescue proposal by proposal ID.
    pub fn get_rescue_proposal(
        env: Env,
        proposal_id: u64,
    ) -> Option<rescue::RescueProposal> {
        rescue::get_rescue_proposal(&env, proposal_id)
    }

    // ── Multi-Tier Escrow Penalties (Issue #525) ──────────────────────────────
    // ── Dead-Man's Switch Recovery (Issue #617) ──────────────────────────

    /// Configure or update the secondary recovery key.
    ///
    /// Only the current administrator may call this function. The recovery
    /// key is stored in instance storage and persists across contract upgrades.
    ///
    /// # Errors
    ///
    /// - [`ContractError::NotAdmin`] if `caller` is not the current admin.
    /// - [`ContractError::NotInitialized`] if the contract has not been initialized.
    pub fn set_recovery_key(env: Env, caller: Address, recovery_key: Address) -> Result<(), ContractError> {
        crate::recovery::set_recovery_key(&env, &caller, &recovery_key)
    }

    /// Returns the configured recovery key address, if one has been set.
    pub fn get_recovery_key(env: Env) -> Option<Address> {
        crate::recovery::get_recovery_key(&env)
    }

    /// Attempt to reclaim administrative ownership using the secondary recovery key.
    ///
    /// Succeeds only when the administrator has been inactive for at least
    /// 180 days and the caller is the configured recovery key.
    /// On success, admin ownership is transferred and the inactivity timer is reset.
    ///
    /// # Errors
    ///
    /// - [`ContractError::RecoveryKeyNotConfigured`] if no recovery key has been set.
    /// - [`ContractError::NotRecoveryKey`] if the caller is not the recovery key.
    /// - [`ContractError::RecoveryNotAvailableYet`] if the inactivity threshold has not been reached.
    /// - [`ContractError::NotInitialized`] if the contract has not been initialized.
    pub fn recover_admin(env: Env, recovery_key: Address) -> Result<(), ContractError> {
        crate::recovery::recover_admin(&env, &recovery_key)
    }

    // ── Multi-Tier Escrow Penalties (Issue #525) ───────────────────────────────

    pub fn report_ingestion_dropout(
        env: Env, admin: Address, validator: Address, asset: Symbol,
    ) -> Result<u32, ContractError> {
        Self::assert_contract_is_active(&env)?;
        let data = Self::get_data(env.clone())?;
        if data.admin != admin { return Err(ContractError::NotAdmin); }
        admin.require_auth();
        let result = record_tracking_fault(&env, &validator, &asset)?;
        crate::recovery::update_admin_activity(&env);
        Ok(result)
    }

    pub fn get_ingestion_fault_count(env: Env, validator: Address, asset: Symbol) -> u32 {
        get_fault_count_in_window(&env, &validator, &asset)
    }

    pub fn get_ingestion_multiplier(env: Env, validator: Address, asset: Symbol) -> u64 {
        let fault_count = get_fault_count_in_window(&env, &validator, &asset);
        get_penalty_multiplier(fault_count)
    }

    pub fn apply_ingestion_penalty(
        env: Env, admin: Address, validator: Address, asset: Symbol, base_bond: u64,
    ) -> Result<IngestionPenaltyResult, ContractError> {
        Self::assert_contract_is_active(&env)?;
        let data = Self::get_data(env.clone())?;
        if data.admin != admin { return Err(ContractError::NotAdmin); }
        admin.require_auth();
        let fault_count = record_tracking_fault(&env, &validator, &asset)?;

        let result = apply_escrow_penalty(
    &env,
    &validator,
    &asset,
    base_bond,
    fault_count,
    &STAKE_REGISTRY_KEY,
    &TOTAL_STAKED_KEY,
    &StakingStorageKey::FeedStake(
        validator.clone(),
        symbol_to_asset_id(&asset),
    ),
)?;
        Ok(result)
    }

    // ── Revocable admin role delegation with expiration (Issue #703) ────────

    /// Grant `role` to `grantee` until (but excluding) `expiration_ledger`.
    /// Admin-only.
    pub fn grant_role(
        env: Env, admin: Address, grantee: Address, role: roles::Role, expiration_ledger: u32,
    ) -> Result<roles::RoleGrant, ContractError> {
        roles::grant_role(&env, admin, grantee, role, expiration_ledger)
    }

    /// Explicit admin override: revoke a role before its natural expiration.
    pub fn revoke_role(env: Env, admin: Address, grantee: Address, role: roles::Role) -> Result<(), ContractError> {
        roles::revoke_role(&env, admin, grantee, role)
    }

    /// Returns `true` only when `grantee` currently holds a live (non-expired,
    /// non-revoked) grant of `role`.
    pub fn has_role(env: Env, grantee: Address, role: roles::Role) -> bool {
        roles::has_role(&env, &grantee, role)
    }

    pub fn get_role_grant(env: Env, grantee: Address, role: roles::Role) -> Option<roles::RoleGrant> {
        roles::get_role_grant(&env, grantee, role)
    }

    // ── Auto-compounding yield vault (Issue #694) ────────────────────────────

    pub fn init_vault(
        env: Env, admin: Address, asset: Address, fee_recipient: Address,
    ) -> Result<vaults::autocompound::VaultConfig, ContractError> {
        vaults::autocompound::initialize(&env, admin, asset, fee_recipient)
    }

    pub fn set_vault_performance_fee(
        env: Env, admin: Address, fee_bps: u32,
    ) -> Result<vaults::autocompound::VaultConfig, ContractError> {
        vaults::autocompound::set_performance_fee(&env, admin, fee_bps)
    }

    pub fn vault_deposit(env: Env, depositor: Address, amount: i128) -> Result<i128, ContractError> {
        let _guard = security::reentrancy::ReentrancyGuard::new(&env)?;
        vaults::autocompound::deposit(&env, depositor, amount)
    }

    pub fn vault_withdraw(env: Env, owner: Address, shares: i128) -> Result<i128, ContractError> {
        let _guard = security::reentrancy::ReentrancyGuard::new(&env)?;
        vaults::autocompound::withdraw(&env, owner, shares)
    }

    /// Keeper-facing harvest: pulls `yield_amount` from `keeper`, skims the
    /// configured performance fee, and compounds the remainder into the vault.
    pub fn vault_harvest(
        env: Env, keeper: Address, yield_amount: i128,
    ) -> Result<vaults::autocompound::HarvestResult, ContractError> {
        let _guard = security::reentrancy::ReentrancyGuard::new(&env)?;
        vaults::autocompound::harvest(&env, keeper, yield_amount)
    }

    pub fn vault_flash_loan(
        env: Env, borrower: Address, amount: i128,
    ) -> Result<i128, ContractError> {
        let _guard = security::reentrancy::ReentrancyGuard::new(&env)?;
        vaults::autocompound::flash_loan(&env, borrower, amount)
    }

    pub fn vault_total_assets(env: Env) -> i128 {
        vaults::autocompound::get_total_assets(&env)
    }

    pub fn vault_total_shares(env: Env) -> i128 {
        vaults::autocompound::get_total_shares(&env)
    }

    pub fn vault_share_balance(env: Env, holder: Address) -> i128 {
        vaults::autocompound::get_share_balance(&env, holder)
    }

    /// Evaluate a vault liquidation against verified TWAP prices from the
    /// oracle. Liquidation is allowed below 110% collateralization and
    /// allocates 5% of confiscated collateral to the liquidator.
    pub fn vault_liquidation_quote(
        env: Env,
        oracle: Address,
        collateral_asset: Symbol,
        debt_asset: Symbol,
        position: vaults::liquidation::VaultPosition,
        purchase_collateral: u128,
    ) -> Result<vaults::liquidation::LiquidationResult, ContractError> {
        vaults::liquidation::liquidate_at_twap(
            &env,
            &oracle,
            &collateral_asset,
            &debt_asset,
            &position,
            purchase_collateral,
        )
    }

    pub fn vault_config(env: Env) -> Option<vaults::autocompound::VaultConfig> {
        vaults::autocompound::get_config(&env)
    }

    pub fn vault_peak_share_value(env: Env) -> i128 {
        vaults::autocompound::get_peak_share_value(&env)
    }

    pub fn vault_is_circuit_breaker_triggered(env: Env) -> bool {
        vaults::autocompound::is_circuit_breaker_triggered(&env)
    }

    pub fn vault_check_circuit_breaker(env: Env) -> Result<bool, ContractError> {
        vaults::autocompound::check_and_trigger_circuit_breaker(&env)
    }

    pub fn init_yield_farming(
        env: Env,
        voter: Address,
        sig_expires_at: u64,
    ) -> Result<(), ContractError> {
        // Guard: a revoked coordinator must not be allowed to vote.
        admin::assert_not_revoked(&env, &voter)?;
        admin::vote_emergency_revocation(&env, voter, sig_expires_at)
    }

    /// Returns the active emergency revocation proposal, if one exists.
    pub fn get_emerg_revocation_proposal(
        env: Env,
        user: Address,
    ) -> Result<i128, ContractError> {
        vaults::lp_farming::pending_rewards(&env, user)
    }

    pub fn yield_farming_share_balance(env: Env, user: Address) -> i128 {
        vaults::lp_farming::get_share_balance(&env, user)
    }

    // ── Yield farm harvest-compound auto-router (Issue #798) ─────────────────

    /// Claim accrued farm rewards, swap them to LP through `router` along
    /// `path`, and re-stake the proceeds — atomically. `min_lp_out` is the
    /// caller's slippage floor, enforced against the vault's measured LP
    /// balance delta rather than anything the router reports.
    ///
    /// The reentrancy guard here is the *only* one on this path: it must hold
    /// across the untrusted `router` call, and the `lp_farming` helpers this
    /// delegates to take no guard of their own.
    pub fn harvest_and_compound(
        env: Env,
        user: Address,
        router: Address,
        path: Vec<Address>,
        min_lp_out: i128,
    ) -> Result<vaults::harvest_compound::HarvestCompoundResult, ContractError> {
        let _guard = security::reentrancy::ReentrancyGuard::new(&env)?;
        vaults::harvest_compound::harvest_and_compound(&env, user, router, path, min_lp_out)
    }

    // ── On-chain limit order book (Issue #701) ───────────────────────────────

    pub fn place_limit_order(
        env: Env, maker: Address, pair: orders::limit::AssetPair, price_tick: i128, sell_amount: i128,
    ) -> Result<orders::limit::LimitOrder, ContractError> {
        let _guard = security::reentrancy::ReentrancyGuard::new(&env)?;
        orders::limit::place_order(&env, maker, pair, price_tick, sell_amount)
    }

    pub fn place_limit_order_with_expiry(
        env: Env,
        maker: Address,
        pair: orders::limit::AssetPair,
        price_tick: i128,
        sell_amount: i128,
        expiry: u32,
    ) -> Result<orders::limit::LimitOrder, ContractError> {
        let _guard = security::reentrancy::ReentrancyGuard::new(&env)?;
        orders::limit::place_order_with_expiry(
            &env,
            maker,
            pair,
            price_tick,
            sell_amount,
            expiry,
        )
    }

    pub fn fill_limit_order(
        env: Env, filler: Address, order_id: u64, fill_amount: i128,
    ) -> Result<orders::limit::FillResult, ContractError> {
        let _guard = security::reentrancy::ReentrancyGuard::new(&env)?;
        orders::limit::fill_order(&env, filler, order_id, fill_amount)
    }

    pub fn match_limit_orders(
        env: Env, seller_order_id: u64, buyer_order_id: u64, fill_amount: i128,
    ) -> Result<orders::limit::SettlementResult, ContractError> {
        let _guard = security::reentrancy::ReentrancyGuard::new(&env)?;
        orders::limit::match_orders(&env, seller_order_id, buyer_order_id, fill_amount)
    }

    /// Cancel a still-open order and return its unfilled balance to the maker.
    pub fn cancel_limit_order(env: Env, maker: Address, order_id: u64) -> Result<i128, ContractError> {
        let _guard = security::reentrancy::ReentrancyGuard::new(&env)?;
        maker.require_auth();
        orders::limit::cancel_order(&env, maker, order_id)
    }

    pub fn get_limit_order(env: Env, order_id: u64) -> Option<orders::limit::LimitOrder> {
        orders::limit::get_order(&env, order_id)
    }

    pub fn get_orders_at_tick(env: Env, pair: orders::limit::AssetPair, price_tick: i128) -> Vec<u64> {
        orders::limit::get_orders_at_tick(&env, pair, price_tick)
    }

    pub fn get_order_balance(env: Env, owner: Address, asset: Address) -> i128 {
        orders::limit::get_balance(&env, owner, asset)
    }

    pub fn withdraw_order_balance(
        env: Env, owner: Address, asset: Address, amount: i128,
    ) -> Result<i128, ContractError> {
        let _guard = security::reentrancy::ReentrancyGuard::new(&env)?;
        orders::limit::withdraw_balance(&env, owner, asset, amount)
    }

    // ── Anti-frontrunning Commit-Reveal Order Scheme (Issue #761) ───────────

    /// Phase 1 of a commit-reveal order: lock `collateral_amount` of
    /// `collateral_asset` behind `commitment_hash` (`sha256(secret ‖
    /// trade_details)`) until `expiration_sequence`. Only the hash is stored,
    /// so the trade's price/size/direction stay hidden from MEV bots until
    /// reveal.
    pub fn commit_order(
        env: Env,
        trader: Address,
        commitment_hash: BytesN<32>,
        collateral_asset: Address,
        collateral_amount: i128,
        expiration_sequence: u32,
    ) -> Result<orders::commit_reveal::Commitment, ContractError> {
        let _guard = security::reentrancy::ReentrancyGuard::new(&env)?;
        orders::commit_reveal::commit(
            &env,
            trader,
            commitment_hash,
            collateral_asset,
            collateral_amount,
            expiration_sequence,
        )
    }

    /// Phase 2 of a commit-reveal order: reveal the hidden trade terms in a
    /// ledger after the committing ledger and execute them against the order
    /// book at the committed price. Returns the commitment bond once the
    /// revealed terms reproduce the committed hash.
    pub fn reveal_order(
        env: Env,
        commitment_id: u64,
        trader: Address,
        secret: Bytes,
        pair: orders::limit::AssetPair,
        price_tick: i128,
        amount: i128,
        is_buy: bool,
    ) -> Result<orders::commit_reveal::RevealResult, ContractError> {
        let _guard = security::reentrancy::ReentrancyGuard::new(&env)?;
        orders::commit_reveal::reveal(
            &env,
            commitment_id,
            trader,
            secret,
            pair,
            price_tick,
            amount,
            is_buy,
        )
    }

    /// Forfeit a commitment's bond to the treasury once its reveal deadline
    /// has passed without a valid reveal. Callable by anyone (keeper).
    pub fn forfeit_order(env: Env, commitment_id: u64) -> Result<u64, ContractError> {
        let _guard = security::reentrancy::ReentrancyGuard::new(&env)?;
        orders::commit_reveal::forfeit(&env, commitment_id)
    }

    /// Load a stored commitment by id.
    pub fn get_commitment(
        env: Env,
        commitment_id: u64,
    ) -> Result<orders::commit_reveal::Commitment, ContractError> {
        orders::commit_reveal::get_commitment(&env, commitment_id)
    }

    /// Number of active (unrevealed/unforfeited) commitments for a trader.
    pub fn active_commitment_count(env: Env, trader: Address) -> u32 {
        orders::commit_reveal::active_commitment_count(&env, &trader)
    }

    // ── Multi-hop Route Swaps ───────────────────────────────────────────────

    pub fn execute_route(
        env: Env, route: router::multihop::Route,
    ) -> Result<router::multihop::RouteResult, ContractError> {
        let _guard = security::reentrancy::ReentrancyGuard::new(&env)?;
        router::multihop::execute_route(&env, &route)
    }

    // ── Wrapped cross-chain asset mint/burn controls (Issue #692) ───────────

    pub fn register_wrapped_asset(
        env: Env, admin: Address, asset_code: Symbol, controller: Address, max_supply: i128,
    ) -> Result<bridge::mint::BridgeAssetConfig, ContractError> {
        bridge::mint::register_wrapped_asset(&env, admin, asset_code, controller, max_supply)
    }

    pub fn set_bridge_controller(
        env: Env, admin: Address, asset_code: Symbol, new_controller: Address,
    ) -> Result<bridge::mint::BridgeAssetConfig, ContractError> {
        bridge::mint::set_bridge_controller(&env, admin, asset_code, new_controller)
    }

    /// Mint wrapped `asset_code` to `to`. Restricted to the asset's
    /// registered Bridge Controller and capped by `max_supply`.
    pub fn mint_wrapped(
        env: Env, controller: Address, asset_code: Symbol, to: Address, amount: i128,
    ) -> Result<i128, ContractError> {
        let _guard = security::reentrancy::ReentrancyGuard::new(&env)?;
        bridge::mint::mint(&env, controller, asset_code, to, amount)
    }

    /// Burn wrapped `asset_code` from `from`. Restricted to the asset's
    /// registered Bridge Controller.
    pub fn burn_wrapped(
        env: Env, controller: Address, asset_code: Symbol, from: Address, amount: i128,
    ) -> Result<i128, ContractError> {
        let _guard = security::reentrancy::ReentrancyGuard::new(&env)?;
        bridge::mint::burn(&env, controller, asset_code, from, amount)
    }

    pub fn wrapped_balance_of(env: Env, asset_code: Symbol, holder: Address) -> i128 {
        bridge::mint::balance_of(&env, asset_code, holder)
    }

    pub fn wrapped_asset_config(env: Env, asset_code: Symbol) -> Option<bridge::mint::BridgeAssetConfig> {
        bridge::mint::get_config(&env, asset_code)
    }

    pub fn set_wrapped_mint_rate_limit(
        env: Env,
        admin: Address,
        asset_code: Symbol,
        max_rolling_amount: i128,
    ) -> Result<bridge::rate_limit::MintRateLimit, ContractError> {
        bridge::rate_limit::set_limit(
            &env,
            admin,
            bridge::rate_limit::RateLimitAsset::Wrapped(asset_code),
            max_rolling_amount,
        )
    }

    pub fn get_wrapped_mint_rate_limit(
        env: Env,
        asset_code: Symbol,
    ) -> Option<bridge::rate_limit::MintRateLimit> {
        bridge::rate_limit::get_limit(&env, bridge::rate_limit::RateLimitAsset::Wrapped(asset_code))
    }

    // --- Native bridge escrow (Issue #750) ---

    pub fn configure_bridge_escrow(
        env: Env, admin: Address, native_token: Address,
    ) -> Result<bridge::escrow::BridgeEscrowConfig, ContractError> {
        bridge::escrow::configure(&env, admin, native_token)
    }

    pub fn lock_tokens(
        env: Env, depositor: Address, amount: i128, target_chain_id: u32, recipient_address: Address,
    ) -> Result<bridge::escrow::TokenLock, ContractError> {
        let _guard = security::reentrancy::ReentrancyGuard::new(&env)?;
        bridge::escrow::lock_tokens(&env, depositor, amount, target_chain_id, recipient_address)
    }

    pub fn unlock_tokens(
        env: Env,
        proof: bridge::escrow::UnlockProof,
        signatures: Vec<(BytesN<32>, BytesN<64>)>,
    ) -> Result<i128, ContractError> {
        let _guard = security::reentrancy::ReentrancyGuard::new(&env)?;
        bridge::escrow::unlock_tokens(&env, proof, signatures)
    }

    pub fn get_bridge_lock(env: Env, lock_id: u64) -> Option<bridge::escrow::TokenLock> {
        bridge::escrow::get_lock(&env, lock_id)
    }

    pub fn bridge_vault_balance(env: Env) -> i128 {
        bridge::escrow::vault_balance(&env)
    }

    pub fn bridge_escrow_config(env: Env) -> Option<bridge::escrow::BridgeEscrowConfig> {
        bridge::escrow::get_config(&env)
    }

    pub fn reclaim_expired(env: Env, id: u64, sender: Address) -> Result<(), ContractError> {
        bridge::escrow::reclaim_expired(&env, id, sender)
    }

    // --- Private remittance commitment tree ---

    pub fn insert_commitment(
        env: Env, commitment: BytesN<32>,
    ) -> Result<(u64, BytesN<32>), ContractError> {
        escrow::merkle::insert(&env, commitment)
    }

    pub fn commitment_root(env: Env) -> BytesN<32> {
        escrow::merkle::current_root(&env)
    }

    pub fn commitment_next_index(env: Env) -> u64 {
        escrow::merkle::next_index(&env)
    }

    pub fn is_known_commitment_root(env: Env, root: BytesN<32>) -> bool {
        escrow::merkle::is_known_root(&env, root)
    }

    /// Returns `true` if `addr` has been stamped as revoked by the
    /// multi-sig coordinator group.
    pub fn is_revoked(env: Env, addr: Address) -> bool {
        admin::is_revoked(&env, &addr)
    }

    // ── Cross-Border Fiat Escrow Settlement ──────────────────────────────

    /// Open a new fiat settlement escrow in the `Pending` state.
    pub fn open_fiat_escrow(
        env: Env, sender: Address, anchor: Address, asset: AssetId, amount: u64,
    ) -> Result<FiatEscrow, ContractError> {
        if amount == 0 { return Err(ContractError::AmountTooLow); }
        sender.require_auth();
        let id: u64 = env.storage().persistent().get(&FiatEscrowKey::Counter).unwrap_or(0u64);
        let next = id.checked_add(1).ok_or(ContractError::Overflow)?;
        let now = env.ledger().timestamp();
        let escrow = FiatEscrow {
            id,
            sender: sender.clone(),
            anchor,
            amount,
            asset,
            state: FiatSettlementState::Pending,
            created_at: now,
            locked_at: 0,
            timeout_secs: FIAT_PAYOUT_TIMEOUT_SECS,
        };
        env.storage().persistent().set(&FiatEscrowKey::Escrow(id), &escrow);
        env.storage().persistent().set(&FiatEscrowKey::Counter, &next);
        Ok(escrow)
    }

    /// Lock the sender's funds, transitioning `Pending` -> `Locked` and
    /// starting the 24h anchor-claim countdown.
    pub fn lock_fiat_escrow(env: Env, sender: Address, escrow_id: u64) -> Result<FiatEscrow, ContractError> {
        sender.require_auth();
        let mut escrow: FiatEscrow = env.storage().persistent()
            .get(&FiatEscrowKey::Escrow(escrow_id)).ok_or(ContractError::NotRegistered)?;
        if escrow.sender != sender { return Err(ContractError::Unauthorized); }
        if escrow.state != FiatSettlementState::Pending { return Err(ContractError::Unauthorized); }
        escrow.state = FiatSettlementState::Locked;
        escrow.locked_at = env.ledger().timestamp();
        env.storage().persistent().set(&FiatEscrowKey::Escrow(escrow_id), &escrow);
        Ok(escrow)
    }

    /// Anchor marks the off-chain fiat payout as dispatched, transitioning
    /// `Locked` -> `Dispatched`.
    pub fn dispatch_fiat_payout(env: Env, anchor: Address, escrow_id: u64) -> Result<FiatEscrow, ContractError> {
        anchor.require_auth();
        let mut escrow: FiatEscrow = env.storage().persistent()
            .get(&FiatEscrowKey::Escrow(escrow_id)).ok_or(ContractError::NotRegistered)?;
        if escrow.anchor != anchor { return Err(ContractError::Unauthorized); }
        if escrow.state != FiatSettlementState::Locked { return Err(ContractError::Unauthorized); }
        if Self::_fiat_escrow_expired(&env, &escrow) { return Err(ContractError::DeadlineReached); }
        escrow.state = FiatSettlementState::Dispatched;
        env.storage().persistent().set(&FiatEscrowKey::Escrow(escrow_id), &escrow);
        Ok(escrow)
    }

    /// Anchor keypair signals fiat payout completion, releasing the escrowed
    /// funds and transitioning `Locked`/`Dispatched` -> `Settled`.
    pub fn settle_fiat_escrow(env: Env, anchor: Address, escrow_id: u64) -> Result<FiatEscrow, ContractError> {
        anchor.require_auth();
        let mut escrow: FiatEscrow = env.storage().persistent()
            .get(&FiatEscrowKey::Escrow(escrow_id)).ok_or(ContractError::NotRegistered)?;
        if escrow.anchor != anchor { return Err(ContractError::Unauthorized); }
        match escrow.state {
            FiatSettlementState::Locked | FiatSettlementState::Dispatched => {}
            _ => return Err(ContractError::Unauthorized),
        }
        if Self::_fiat_escrow_expired(&env, &escrow) { return Err(ContractError::DeadlineReached); }
        escrow.state = FiatSettlementState::Settled;
        env.storage().persistent().set(&FiatEscrowKey::Escrow(escrow_id), &escrow);
        Ok(escrow)
    }

    /// Reclaim locked funds for the sender once the 24h anchor-claim window
    /// has elapsed without settlement, transitioning to `Refunded`.
    pub fn refund_fiat_escrow(env: Env, caller: Address, escrow_id: u64) -> Result<FiatEscrow, ContractError> {
        caller.require_auth();
        let mut escrow: FiatEscrow = env.storage().persistent()
            .get(&FiatEscrowKey::Escrow(escrow_id)).ok_or(ContractError::NotRegistered)?;
        match escrow.state {
            FiatSettlementState::Locked | FiatSettlementState::Dispatched => {}
            _ => return Err(ContractError::Unauthorized),
        }
        if !Self::_fiat_escrow_expired(&env, &escrow) {
            return Err(ContractError::DeadlineNotReached);
        }
        escrow.state = FiatSettlementState::Refunded;
        env.storage().persistent().set(&FiatEscrowKey::Escrow(escrow_id), &escrow);
        Ok(escrow)
    }

    /// Read a fiat settlement escrow record by id.
    pub fn get_fiat_escrow(env: Env, escrow_id: u64) -> Option<FiatEscrow> {
        env.storage().persistent().get(&FiatEscrowKey::Escrow(escrow_id))
    }

    // --- Private Helpers ---

    /// Returns `true` when a locked escrow has passed its anchor-claim
    /// timeout window. Escrows that have never been locked never expire.
    fn _fiat_escrow_expired(env: &Env, escrow: &FiatEscrow) -> bool {
        if escrow.locked_at == 0 { return false; }
        env.ledger().timestamp().saturating_sub(escrow.locked_at) >= escrow.timeout_secs
    }

    fn _store_proposal_state(env: &Env, topic: Symbol, proposed_at: u64) {
        let mut states: Map<Symbol, ProposalState> = env
            .storage()
            .instance()
            .get(&PROPOSAL_STATE_KEY)
            .unwrap_or_else(|| Map::new(env));
        states.set(topic, ProposalState { proposed_at, status: ProposalStatus::Active });
        env.storage().instance().set(&PROPOSAL_STATE_KEY, &states);
    }

    fn _mark_proposal_expired(env: &Env, topic: Symbol) {
        let mut states: Map<Symbol, ProposalState> = env
            .storage()
            .instance()
            .get(&PROPOSAL_STATE_KEY)
            .unwrap_or_else(|| Map::new(env));
        if let Some(mut state) = states.get(&topic) {
            state.status = ProposalStatus::Expired;
            states.set(topic, state);
            env.storage().instance().set(&PROPOSAL_STATE_KEY, &states);
        }
    }

    fn _remove_proposal_state(env: &Env, topic: Symbol) {
        let mut states: Map<Symbol, ProposalState> = env
            .storage()
            .instance()
            .get(&PROPOSAL_STATE_KEY)
            .unwrap_or_else(|| Map::new(env));
        states.remove(topic);
        env.storage().instance().set(&PROPOSAL_STATE_KEY, &states);
    }

    fn assert_contract_is_active(env: &Env) -> Result<(), ContractError> {
        if !env.storage().instance().has(&DATA_KEY) {
            return Err(ContractError::NotInitialized);
        }
        if admin::is_paused(env) {
            return Err(ContractError::ContractPaused);
        }
        Ok(())
    }

    fn _record_heartbeat(env: &Env, asset: AssetId) {
        let heartbeat_key = storage::HeartbeatKey::HeartbeatByAsset(asset);
        env.storage().temporary().set(&heartbeat_key, &env.ledger().timestamp());
    }

    fn _get_interval(env: &Env) -> u64 {
        env.storage().instance().get(&HB_INTERVAL_KEY).unwrap_or(DEFAULT_HEARTBEAT_INTERVAL)
    }

    fn _get_signers(env: &Env) -> Map<Address, ()> {
        env.storage().instance().get(&SIGNERS_KEY).unwrap_or_else(|| Map::new(env))
    }

    fn _get_node_profiles(env: &Env) -> Map<Address, NodeProfile> {
        env.storage().persistent().get(&NODE_PROFILES_KEY).unwrap_or_else(|| Map::new(env))
    }

    fn _scan_profile_for_rate(profile: NodeProfile) -> Option<u64> {
        if profile.confidence == 0 { None } else { Some(profile.rate) }
    }

    fn _maintain_relayer_profile_ttl(env: &Env) {
        env.storage().persistent().extend_ttl(
            &NODE_PROFILES_KEY,
            RELAYER_TTL_THRESHOLD,
            env.storage().max_ttl(),
        );
    }

    fn _extend_instance_ttl(env: &Env) {
        env.storage()
            .instance()
            .extend_ttl(RELAYER_TTL_THRESHOLD, INSTANCE_TTL_EXTEND);
    }


    fn _is_signer(env: &Env, addr: &Address) -> bool {
        Self::_get_signers(env).contains_key(addr.clone())
    }

    fn _revocation_threshold(env: &Env) -> u32 {
        let n = Self::_get_signers(env).len();
        if n == 0 { 1 } else { n / 2 + 1 }
    }

    fn _resolve_feed_metrics(env: &Env, asset: AssetId) -> AssetFeedMetrics {
        let stored: AssetFeedMetrics = env
            .storage()
            .persistent()
            .get(&StakingStorageKey::AssetMetrics(asset))
            .unwrap_or(AssetFeedMetrics {
                volume_score: 10,
                volatility_bps: 100,
            });
        let corridor = fees::get_corridor_fee_pool(env.clone(), asset);
        AssetFeedMetrics {
            volume_score: effective_volume_score(stored.volume_score, corridor.collected),
            volatility_bps: stored.volatility_bps,
        }
    }

    // ── Issue #592: Batch Purge of Abandoned Zero-Balance Keys ───────────────

    /// Batch-evict abandoned zero-balance persistent storage keys to reclaim
    /// ledger footprint consumed by exited liquidity positions.
    ///
    /// Requires multi-sig quorum (≥ 2 registered signers). Each signer in
    /// `signers` must have already called `require_auth` on the transaction.
    ///
    /// Returns the number of entries actually removed.
    pub fn cleanup_zero_balances(
        env: Env,
        signers: Vec<Address>,
        targets: Vec<admin::cleanup::CleanupTarget>,
    ) -> Result<u32, ContractError> {
        // Require auth from every co-signing address so the host logs them
        // as authorised participants of this call.
        for signer in signers.iter() {
            signer.require_auth();
        }
        admin::cleanup::cleanup_zero_balances(&env, &signers, &targets)
    }

    // ── Issue #782: Key Pruning Utility for Obsolete Contract Data ──────────

    /// Clean up obsolete contract storage keys (spent orders, closed escrows,
    /// settled HTLCs, expired stakes) to reduce state bloat and reclaim storage deposits.
    ///
    /// Only the contract admin can call this entrypoint.
    /// Returns the count of deleted storage entries.
    pub fn prune_expired_keys(
        env: Env,
        admin: Address,
        targets: Vec<admin::prune::PruneTarget>,
    ) -> Result<u32, ContractError> {
        admin::prune::prune_expired_keys(&env, &admin, &targets)
    }

    // ── Dynamic Liquidity Pool Swap Fee Tier Controller ─────────────────────

    /// Initialize the fee tier controller with bounded safety ranges.
    pub fn initialize_fee_tier_controller(
        env: Env,
        admin: Address,
        default_tier_bps: u32,
    ) -> Result<FeeTierController, ContractError> {
        let data = Self::_load_data(&env)?;
        if data.admin != admin {
            return Err(ContractError::NotAdmin);
        }
        admin.require_auth();
        if default_tier_bps != FEE_TIER_005_BPS
            && default_tier_bps != FEE_TIER_030_BPS
            && default_tier_bps != FEE_TIER_100_BPS
        {
            return Err(ContractError::InvalidTierConfig);
        }
        if env.storage().instance().has(&LiquidityPoolFeeKey::Controller) {
            return Err(ContractError::AlreadyInitialized);
        }
        let controller = FeeTierController {
            active_tier_bps: default_tier_bps,
            min_tier_bps: FEE_TIER_005_BPS,
            max_tier_bps: FEE_TIER_100_BPS,
            lp_share_bps: LP_SHARE_BPS,
            treasury_share_bps: TREASURY_SHARE_BPS,
        };
        env.storage().instance().set(&LiquidityPoolFeeKey::Controller, &controller);
        let default_config = PoolFeeConfig { active_tier_bps: default_tier_bps };
        for asset in [ID_NGN, ID_GHS, ID_CFA, ID_KES, ID_ZAR, ID_UGX] {
            env.storage().instance().set(&LiquidityPoolFeeKey::PoolConfig(asset), &default_config);
        }
        Ok(controller)
    }

    /// Return the controller that enforces fee tier safety bounds.
    pub fn get_fee_tier_controller(env: Env) -> FeeTierController {
        env.storage().instance().get(&LiquidityPoolFeeKey::Controller).unwrap_or(FeeTierController {
            active_tier_bps: DEFAULT_FEE_TIER_BPS,
            min_tier_bps: FEE_TIER_005_BPS,
            max_tier_bps: FEE_TIER_100_BPS,
            lp_share_bps: LP_SHARE_BPS,
            treasury_share_bps: TREASURY_SHARE_BPS,
        })
    }

    /// Return the active fee tier for a specific pool.
    pub fn get_pool_fee_tier(env: Env, asset: AssetId) -> u32 {
        let config: Option<PoolFeeConfig> = env
            .storage()
            .instance()
            .get(&LiquidityPoolFeeKey::PoolConfig(asset));
        config
            .unwrap_or(PoolFeeConfig { active_tier_bps: DEFAULT_FEE_TIER_BPS })
            .active_tier_bps
    }

    /// Open a governance vote to adjust a pool's fee tier.
    pub fn propose_pool_fee_tier_change(
        env: Env,
        proposer: Address,
        asset: AssetId,
        new_tier_bps: u32,
    ) -> Result<(), ContractError> {
        let data = Self::get_data(env.clone())?;
        if data.admin != proposer {
            return Err(ContractError::NotAdmin);
        }
        proposer.require_auth();
        let controller = Self::get_fee_tier_controller(env.clone());
        if new_tier_bps < controller.min_tier_bps || new_tier_bps > controller.max_tier_bps {
            return Err(ContractError::FeeCeilingExceeded);
        }
        if new_tier_bps != FEE_TIER_005_BPS
            && new_tier_bps != FEE_TIER_030_BPS
            && new_tier_bps != FEE_TIER_100_BPS
        {
            return Err(ContractError::InvalidTierConfig);
        }
        let proposal_key = LiquidityPoolFeeKey::FeeTierProposal(asset);
        if env.storage().instance().has(&proposal_key) {
            return Err(ContractError::ProposalAlreadyActive);
        }
        let proposal = PoolFeeTierProposal {
            asset,
            new_tier_bps,
            proposer: proposer.clone(),
            votes: Vec::new(&env),
            created_at: env.ledger().timestamp(),
        };
        env.storage().instance().set(&proposal_key, &proposal);
        Ok(())
    }

    /// Cast a governance vote on a pending pool fee tier change.
    pub fn vote_pool_fee_tier_change(
        env: Env,
        voter: Address,
        asset: AssetId,
    ) -> Result<(), ContractError> {
        voter.require_auth();
        let data = Self::get_data(env.clone())?;
        if !Self::_is_signer(&env, &voter) && data.admin != voter {
            return Err(ContractError::Unauthorized);
        }
        let proposal_key = LiquidityPoolFeeKey::FeeTierProposal(asset);
        let mut proposal: PoolFeeTierProposal = env
            .storage()
            .instance()
            .get(&proposal_key)
            .ok_or(ContractError::NoActiveProposal)?;
        for existing_voter in proposal.votes.iter() {
            if existing_voter == &voter {
                return Err(ContractError::AlreadyVoted);
            }
        }
        proposal.votes.push(voter);
        let threshold = Self::_revocation_threshold(&env);
        if proposal.votes.len() >= threshold {
            let mut config: PoolFeeConfig = env
                .storage()
                .instance()
                .get(&LiquidityPoolFeeKey::PoolConfig(asset))
                .unwrap_or(PoolFeeConfig { active_tier_bps: DEFAULT_FEE_TIER_BPS });
            config.active_tier_bps = proposal.new_tier_bps;
            env.storage().instance().set(&LiquidityPoolFeeKey::PoolConfig(asset), &config);

            let mut controller: FeeTierController = env
                .storage()
                .instance()
                .get(&LiquidityPoolFeeKey::Controller)
                .unwrap_or(FeeTierController {
                    active_tier_bps: DEFAULT_FEE_TIER_BPS,
                    min_tier_bps: FEE_TIER_005_BPS,
                    max_tier_bps: FEE_TIER_100_BPS,
                    lp_share_bps: LP_SHARE_BPS,
                    treasury_share_bps: TREASURY_SHARE_BPS,
                });
            controller.active_tier_bps = proposal.new_tier_bps;
            env.storage().instance().set(&LiquidityPoolFeeKey::Controller, &controller);
            env.storage().instance().remove(&proposal_key);
        } else {
            env.storage().instance().set(&proposal_key, &proposal);
        }
        Ok(())
    }

    /// Read a pending pool fee tier governance proposal.
    pub fn get_pool_fee_tier_proposal(env: Env, asset: AssetId) -> Option<PoolFeeTierProposal> {
        env.storage().instance().get(&LiquidityPoolFeeKey::FeeTierProposal(asset))
    }

    /// Cancel a pending pool fee tier governance proposal.
    pub fn cancel_pool_fee_tier_change(
        env: Env,
        canceller: Address,
        asset: AssetId,
    ) -> Result<(), ContractError> {
        let data = Self::get_data(env.clone())?;
        if data.admin != canceller {
            return Err(ContractError::NotAdmin);
        }
        canceller.require_auth();
        let proposal_key = LiquidityPoolFeeKey::FeeTierProposal(asset);
        if env.storage().instance().has(&proposal_key) {
            env.storage().instance().remove(&proposal_key);
        }
        Ok(())
    }

    /// Record a swap fee and split it 80% to LP holders / 20% to treasury.
    pub fn record_pool_swap_fee(
        env: Env,
        caller: Address,
        asset: AssetId,
        collected_fee: u64,
    ) -> Result<PoolFeeState, ContractError> {
        let data = Self::get_data(env.clone())?;
        if data.admin != caller {
            return Err(ContractError::NotAdmin);
        }
        caller.require_auth();
        let controller = Self::get_fee_tier_controller(env.clone());
        let lp_amount =
            ((u128::from(collected_fee) * u128::from(controller.lp_share_bps)) / 10000) as u64;
        let treasury_amount = collected_fee.saturating_sub(lp_amount);
        let state_key = LiquidityPoolFeeKey::PoolState(asset);
        let mut state: PoolFeeState = env
            .storage()
            .instance()
            .get(&state_key)
            .unwrap_or(PoolFeeState {
                asset,
                collected_lp_fees: 0,
                collected_treasury_fees: 0,
                last_updated: env.ledger().timestamp(),
            });
        state.collected_lp_fees = state
            .collected_lp_fees
            .checked_add(lp_amount)
            .ok_or(ContractError::Overflow)?;
        state.collected_treasury_fees = state
            .collected_treasury_fees
            .checked_add(treasury_amount)
            .ok_or(ContractError::Overflow)?;
        state.last_updated = env.ledger().timestamp();
        env.storage().instance().set(&state_key, &state);
        Ok(state)
    }

    /// Return the accumulated split fee state for a pool.
    pub fn get_pool_fee_state(env: Env, asset: AssetId) -> PoolFeeState {
        env.storage().instance().get(&LiquidityPoolFeeKey::PoolState(asset)).unwrap_or(PoolFeeState {
            asset,
            collected_lp_fees: 0,
            collected_treasury_fees: 0,
            last_updated: 0,
        })
    }
}

    // ── Groth16 ZK Proof Verification (Issue #725) ────────────────────────

    /// Register a Groth16 verification key for a circuit on-chain.
    pub fn register_zk_verification_key(
        env: Env,
        caller: Address,
        vkey: zk::verifier::VerificationKey,
    ) -> Result<(), ContractError> {
        let data = Self::_load_data(&env)?;
        if data.admin != caller {
            return Err(ContractError::NotAdmin);
        }
        caller.require_auth();
        zk::verifier::register_verification_key(&env, &vkey)
    }

    /// Retrieve a registered Groth16 verification key by circuit ID.
    pub fn get_zk_verification_key(
        env: Env,
        circuit_id: BytesN<32>,
    ) -> Option<zk::verifier::VerificationKey> {
        zk::verifier::get_verification_key(&env, &circuit_id)
    }

    /// Remove a Groth16 verification key from storage.
    pub fn remove_zk_verification_key(
        env: Env,
        caller: Address,
        circuit_id: BytesN<32>,
    ) -> Result<(), ContractError> {
        let data = Self::_load_data(&env)?;
        if data.admin != caller {
            return Err(ContractError::NotAdmin);
        }
        caller.require_auth();
        zk::verifier::remove_verification_key(&env, &circuit_id)
    }

    /// Verify a Groth16 proof against the registered verification key.
    pub fn verify_zk_proof(
        env: Env,
        proof: zk::verifier::Groth16Proof,
        vkey: zk::verifier::VerificationKey,
        public_inputs: Vec<BytesN<32>>,
    ) -> Result<zk::verifier::VerificationResult, ContractError> {
        zk::verifier::verify_proof(&env, &proof, &vkey, &public_inputs)
    }

    /// Verify a Groth16 proof with an off-chain pairing commitment.
    pub fn verify_zk_proof_with_commitment(
        env: Env,
        proof: zk::verifier::Groth16Proof,
        vkey: zk::verifier::VerificationKey,
        public_inputs: Vec<BytesN<32>>,
        pairing_commitment: zk::verifier::PairingCommitment,
    ) -> Result<zk::verifier::VerificationResult, ContractError> {
        zk::verifier::verify_proof_with_commitment(
            &env, &proof, &vkey, &public_inputs, &pairing_commitment,
        )
    }

    /// Batch-verify multiple Groth16 proofs in a single transaction.
    pub fn batch_verify_zk_proofs(
        env: Env,
        proofs: Vec<(
            zk::verifier::Groth16Proof,
            zk::verifier::VerificationKey,
            Vec<BytesN<32>>,
        )>,
    ) -> Result<Vec<zk::verifier::VerificationResult>, ContractError> {
        zk::verifier::batch_verify_proofs(&env, &proofs)
    }

#[cfg(test)]
mod query_guardrail_tests {
    use super::*;
    use soroban_sdk::{Env, symbol_short};
    use soroban_sdk::testutils::{Address as _, Ledger, LedgerInfo};

    fn setup() -> (Env, crate::TimeLockedUpgradeContractClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();
        let id = env.register_contract(None, TimeLockedUpgradeContract);
        let client = crate::TimeLockedUpgradeContractClient::new(&env, &id);
        (env, client)
    }

    fn advance(env: &Env, delta: u64) {
        let ts = env.ledger().timestamp();
        env.ledger().set(LedgerInfo {
            timestamp: ts + delta,
            protocol_version: env.ledger().protocol_version(),
            sequence_number: env.ledger().sequence(),
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 0,
            min_persistent_entry_ttl: 0,
            max_entry_ttl: u32::MAX,
        });
    }

    #[test]
    fn test_get_data_before_and_after_init() {
        let (env, client) = setup();
        let admin = Address::generate(&env);

        let result = client.try_get_data();
        assert_eq!(result, Err(Ok(ContractError::NotInitialized)));

        let treasury = soroban_sdk::Address::generate(&env);
        client.initialize(&admin, &treasury);

        let data = client.get_data();
        assert_eq!(data.admin, admin);
        assert_eq!(data.value, 0u64);
    }

    #[test]
    fn test_get_data_is_idempotent() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let treasury = soroban_sdk::Address::generate(&env);
        client.initialize(&admin, &treasury);

        let first_admin = client.get_data().admin;
        let first_value = client.get_data().value;
        let second_admin = client.get_data().admin;
        let second_value = client.get_data().value;

        assert_eq!(first_admin, second_admin);
        assert_eq!(first_value, second_value);
        assert_eq!(first_value, 0);
    }

    #[test]
    fn test_is_data_fresh_unknown_asset_returns_false() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let treasury = soroban_sdk::Address::generate(&env);
        client.initialize(&admin, &treasury);

        let asset: AssetId = 3897123275; // NGN
        assert!(!client.is_data_fresh(&asset));
    }

    #[test]
    fn test_is_data_fresh_transitions_on_staleness() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let treasury = soroban_sdk::Address::generate(&env);
        client.initialize(&admin, &treasury);

        let asset: AssetId = 2654435761; // KES
        client.update_heartbeat(&asset, &admin);

        assert!(client.is_data_fresh(&asset));

        advance(&env, DEFAULT_HEARTBEAT_INTERVAL + 1);
        assert!(!client.is_data_fresh(&asset));
    }

    #[test]
    fn test_is_data_fresh_does_not_mutate_heartbeat() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let treasury = soroban_sdk::Address::generate(&env);
        client.initialize(&admin, &treasury);

        let asset: AssetId = 4026531840; // GHS
        client.update_heartbeat(&asset, &admin);

        for _ in 0..5 {
            assert!(client.is_data_fresh(&asset));
        }

        advance(&env, DEFAULT_HEARTBEAT_INTERVAL + 1);
        assert!(!client.is_data_fresh(&asset));
    }

    #[test]
    fn test_query_methods_do_not_interfere() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let treasury = soroban_sdk::Address::generate(&env);
        client.initialize(&admin, &treasury);

        let asset: AssetId = 4160749568; // CFA

        let admin_before = client.get_data().admin;
        let value_before = client.get_data().value;

        let _ = client.is_data_fresh(&asset);

        let admin_after = client.get_data().admin;
        let value_after = client.get_data().value;

        assert_eq!(admin_before, admin_after);
        assert_eq!(value_before, value_after);
    }
}

// NOTE: _resolve_feed_metrics is defined inside the main contract impl.

#[cfg(test)]
mod test;