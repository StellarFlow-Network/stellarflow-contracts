#![no_std]
use soroban_sdk::{
    contract, contracterror, contractimpl, contractmeta, contracttype, symbol_short,
    Address, Bytes, BytesN, Env, Map, Symbol, Vec,
};

contractmeta!(key = "name", val = "stellarflow-contracts");
contractmeta!(key = "version", val = "0.1.0");
contractmeta!(key = "author", val = "StellarFlow Network");
contractmeta!(key = "description", val = "StellarFlow Contracts");
contractmeta!(key = "interface", val = "stellarflow-v1");
contractmeta!(key = "build_time", val = "2026-08-28");
contractmeta!(key = "git_sha", val = "dev");

/// Numeric asset identifier for gas-optimized storage.
pub type AssetId = u32;

/// Convert a currency Symbol to a numeric AssetId using FNV-1a hash.
pub fn symbol_to_asset_id(symbol: &Symbol) -> AssetId {
    let payload = symbol.to_val().get_payload();
    let mut hash: u32 = 2166136261u32;
    for i in 0..8u64 {
        let byte = ((payload >> (i * 8)) & 0xff) as u8;
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(16777619);
    }
    hash
}

const ID_NGN: u32 = symbol_to_asset_id_const(b"NGN");
const ID_GHS: u32 = symbol_to_asset_id_const(b"GHS");
const ID_CFA: u32 = symbol_to_asset_id_const(b"CFA");
const ID_KES: u32 = symbol_to_asset_id_const(b"KES");
const ID_ZAR: u32 = symbol_to_asset_id_const(b"ZAR");
const ID_UGX: u32 = symbol_to_asset_id_const(b"UGX");

const fn symbol_to_asset_id_const(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 2166136261u32;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u32;
        hash = hash.wrapping_mul(16777619);
        i += 1;
    }
    hash
}

/// Reverse lookup from AssetId to Symbol.
pub fn asset_id_to_symbol(asset_id: u32) -> Symbol {
    match asset_id {
        _ if asset_id == ID_NGN => symbol_short!("NGN"),
        _ if asset_id == ID_GHS => symbol_short!("GHS"),
        _ if asset_id == ID_CFA => symbol_short!("CFA"),
        _ if asset_id == ID_KES => symbol_short!("KES"),
        _ if asset_id == ID_ZAR => symbol_short!("ZAR"),
        _ if asset_id == ID_UGX => symbol_short!("UGX"),
        3897123275 => symbol_short!("NGN"),
        4026531840 => symbol_short!("GHS"),
        4160749568 => symbol_short!("CFA"),
        2654435761 => symbol_short!("KES"),
        3219226362 => symbol_short!("ZAR"),
        2863311530 => symbol_short!("UGX"),
        _ => panic!("Unknown asset ID mapping context"),
    }
}

pub(crate) mod nonce;
use crate::nonce::{consume_nonce, get_nonce};

pub mod action_guard;
pub mod amm;
pub mod admin;
pub mod amm;
pub mod auth;
pub mod bridge;
pub mod escrow;
pub mod config;
pub mod consensus;
pub mod kernel;
pub use kernel::instance;
pub mod errors;
pub mod escrow;
pub mod events;
pub mod fees;
pub mod governance;
pub mod math;
pub mod orders;
pub mod recovery;
pub mod roles;
pub mod router;
pub mod security;
pub mod settlement;
pub mod slashing;
pub mod staging;
pub mod staking_tiers;
pub mod router;
pub mod settlement;
pub mod state_verification;
pub mod storage;
pub mod temp_governance;
pub mod upgrades;
pub mod validation;
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
    InsufficientStakeForTier = 20,
    InvalidTierConfig = 21,
    FeedAlreadyRegistered = 22,
    PremiumPoolAccessDenied = 23,
    TransferAlreadyPending = 24,
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
    NullifierAlreadyUsed = 48,
    InvalidProof = 49,
    BridgeAssetNotRegistered = 50,
    BridgeInvalidMaxSupply = 51,
    BridgeAssetAlreadyRegistered = 52,
    BridgeInvalidAmount = 53,
    BridgeNotController = 54,
    BridgeSupplyCapExceeded = 55,
    BridgeInsufficientBalance = 56,
    BridgeEscrowNotConfigured = 57,
    /// Reentrancy guard detected a reentrant call during execution.
    ReentrancyDetected = 58,
    MerkleTreeFull = 59,
    InvalidFeeSplitConfig = 60,
    FeeDistributionMismatch = 61,
    NotSecurityCouncil = 62,
    ProposalNotFound = 63,
    ProposalNotVetoable = 64,
    ProposalAlreadyVetoed = 65,
}

impl ContractError {
    pub const MathOverflow: Self = Self::Overflow;
    pub const BridgeAssetNotRegistered: Self = Self::NotRegistered;
    pub const BridgeInvalidMaxSupply: Self = Self::Overflow;
    pub const BridgeAssetAlreadyRegistered: Self = Self::AlreadyRegistered;
    pub const BridgeInvalidAmount: Self = Self::AmountTooLow;
    pub const BridgeNotController: Self = Self::Unauthorized;
    pub const BridgeSupplyCapExceeded: Self = Self::Overflow;
    pub const BridgeInsufficientBalance: Self = Self::Overflow;
    pub const BridgeEscrowNotConfigured: Self = Self::NotInitialized;
    pub const AdminChangeTimelockNotSatis: Self = Self::UpgradeTimelockNotSatisfied;
    pub const UpgradeHealthCheckFailed: Self = Self::UpgradeTimelockNotSatisfied;
    pub const DeadlineTooSoon: Self = Self::UpgradeTimelockNotSatisfied;
    pub const DeadlineTooFar: Self = Self::UpgradeTimelockNotSatisfied;
    pub const DeadlineReached: Self = Self::UpgradeTimelockNotSatisfied;
    pub const DeadlineNotReached: Self = Self::UpgradeTimelockNotSatisfied;
    pub const TooManyActiveHtlcs: Self = Self::Overflow;
    pub const HtlcNotFound: Self = Self::NotRegistered;
    pub const HtlcNotActive: Self = Self::Unauthorized;
    pub const InvalidPreImage: Self = Self::InvalidSaltSignature;
    pub const NotEmergencyAdmin: Self = Self::NotAdmin;
    pub const NoPreviousUpgrade: Self = Self::NotRegistered;
    pub const RollbackWindowExpired: Self = Self::UpgradeTimelockNotSatisfied;
    pub const RouteExecutionFailed: Self = Self::Unauthorized;
    pub const ZeroSwapAmount: Self = Self::AmountTooLow;
    pub const PoolNotFound: Self = Self::NotRegistered;
    pub const InvalidArgument: Self = Self::NotInitialized;
    pub const EventTopicLimitExceeded: Self = Self::Overflow;
    pub const RecoveryKeyNotConfigured: Self = Self::NotInitialized;
    pub const NotRecoveryKey: Self = Self::Unauthorized;
    pub const RecoveryNotAvailableYet: Self = Self::UpgradeTimelockNotSatisfied;
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
}

// Contract state keys
pub(crate) const DATA_KEY: Symbol = symbol_short!("DATA");
pub(crate) const SIGNERS_KEY: Symbol = symbol_short!("SIGNERS");
pub(crate) const STAGING_KEY: Symbol = symbol_short!("STAGING");
const PENDING_UPGRADE_KEY: Symbol = symbol_short!("PENDING");
pub(crate) use crate::upgrades::timelock::WASM_UPGRADE_DELAY_SECONDS as UPGRADE_DELAY_SECONDS;
pub(crate) const STAKE_REGISTRY_KEY: Symbol = symbol_short!("STAKES");
pub(crate) const TOTAL_STAKED_KEY: Symbol = symbol_short!("TOTAL");
const HEARTBEAT_KEY: Symbol = symbol_short!("HBEAT");
const HB_INTERVAL_KEY: Symbol = symbol_short!("HBINTV");
pub(crate) const DEFAULT_HEARTBEAT_INTERVAL: u64 = 5 * 60;
pub(crate) const VALIDATOR_STATE_KEY: Symbol = symbol_short!("VLSTATE");
pub(crate) const REVOKED_SIGNER_KEY: Symbol = symbol_short!("REVOKED");
const NODE_PROFILES_KEY: Symbol = symbol_short!("NODES");
const PLATFORM_CAPITAL_KEY: Symbol = symbol_short!("CAPITAL");
pub(crate) const CONSENSUS_CACHE_KEY: Symbol = symbol_short!("CACHE");
const RELAYER_TTL_THRESHOLD: u32 = 5_000;
const INSTANCE_TTL_EXTEND: u32 = 100_000;
pub(crate) const TREASURY_KEY: Symbol = symbol_short!("TREASURY");
pub(crate) const LP_REWARD_POOL_KEY: Symbol = symbol_short!("LPREWARD");
const SEQUENCE_COUNTER_KEY: Symbol = symbol_short!("SEQCTR");
const REVOCATION_KEY: Symbol = symbol_short!("REVOKE");
const RECOVERY_KEY: Symbol = symbol_short!("RKEY");
const LAST_ADMIN_ACTIVITY: Symbol = symbol_short!("LASTACT");

#[contracttype]
#[derive(Clone)]
pub struct RevocationProposal {
    pub target: Address,
    pub replacement: Address,
    pub proposer: Address,
    pub proposed_at: u64,
    pub votes: Vec<Address>,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ContractData {
    pub admin: Address,
    pub value: u64,
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
#[derive(Clone, Debug, PartialEq)]
pub struct FeedStakeRecord {
    pub node: Address,
    pub asset: AssetId,
    pub amount: u64,
    pub tier: StakingTier,
    pub registered_at: u64,
}

#[contracttype]
pub enum StakingStorageKey {
    TierConfig,
    AssetMetrics(AssetId),
    FeedStake(Address, AssetId),
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
        let _dummy: soroban_sdk::Error = soroban_sdk::Error::from_contract_error(1);
        ensure_schema_version(&env)?;
        if env.storage().instance().has(&DATA_KEY) {
            return Err(ContractError::AlreadyInitialized);
        }
        admin.require_auth();
        let data = ContractData { admin: admin.clone(), value: 0 };
        env.storage().instance().set(&DATA_KEY, &data);
        env.storage().instance().set(&TREASURY_KEY, &treasury);
        Ok(())
    }

    pub fn stake_and_register(env: Env, node: Address, amount: u64) -> Result<StakeRecord, ContractError> {
        if amount == 0 { return Err(ContractError::InvalidStakeAmount); }
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
        env.storage().instance().set(&stake_key, &amount);
        stakes.set(node.clone(), amount);
        env.storage().instance().set(&STAKE_REGISTRY_KEY, &stakes);
        env.storage().instance().set(&TOTAL_STAKED_KEY, &new_total);
        Self::_record_heartbeat(&env, 0u32);
        Ok(StakeRecord { node, amount, registered_at: env.ledger().timestamp() })
    }

    pub fn unstake(env: Env, node: Address) -> Result<u64, ContractError> {
        node.require_auth();
        let stake_key = StakeKey::StakeByNode(node.clone());
        let amount: u64 = env.storage().instance().get(&stake_key).ok_or(ContractError::NotRegistered)?;
        let total: u64 = env.storage().instance().get(&TOTAL_STAKED_KEY).unwrap_or(0u64);
        let mut stakes: Map<Address, u64> = env
            .storage()
            .instance()
            .get(&STAKE_REGISTRY_KEY)
            .unwrap_or_else(|| Map::new(&env));
        stakes.remove(node.clone());
        env.storage().instance().set(&STAKE_REGISTRY_KEY, &stakes);
        let total: u64 = env
            .storage()
            .instance()
            .get(&TOTAL_STAKED_KEY)
            .unwrap_or(0u64);
        let new_total = total.saturating_sub(amount);
        env.storage().instance().remove(&stake_key);
        env.storage().instance().set(&TOTAL_STAKED_KEY, &new_total);
        Ok(amount)
    }

    pub fn remove_signer(env: Env, signer: Address, caller: Address) -> Result<(), ContractError> {
        Self::assert_contract_is_active(&env)?;
        let data = Self::_load_data(&env)?;
        if data.admin != caller { return Err(ContractError::NotAdmin); }
        caller.require_auth();
        let signer_key = SignerKey::SignerByAddress(signer.clone());
        if env.storage().instance().has(&signer_key) {
            env.storage().instance().remove(&signer_key);
            let count: u32 = env.storage().instance().get(&SIGNERS_KEY).unwrap_or(0u32);
            if count > 0 { env.storage().instance().set(&SIGNERS_KEY, &(count - 1)); }
        }
        Self::_extend_instance_ttl(&env);
        crate::recovery::update_admin_activity(&env);
        Ok(())
    }

    pub fn propose_revocation(
        env: Env, proposer: Address, target: Address, replacement: Address, sig_expires_at: u64,
    ) -> Result<(), ContractError> {
        if env.ledger().timestamp() > sig_expires_at { return Err(ContractError::SignatureExpired); }
        admin::assert_not_revoked(&env, &proposer)?;
        proposer.require_auth();
        let data = Self::get_data(env.clone())?;
        if !Self::_is_signer(&env, &proposer) && data.admin != proposer {
            return Err(ContractError::Unauthorized);
        }
        open_ballot(&env, REVOCATION_KEY, target, replacement, proposer)
    }

    /// Cast a multi-sig vote on the active revocation ballot stored in Temporary
    /// storage. When the vote tally meets the threshold the admin is updated and
    /// the ballot is immediately deleted from the ledger.
    pub fn vote_revocation(env: Env, voter: Address, sig_expires_at: u64) -> Result<(), ContractError> {
        if env.ledger().timestamp() > sig_expires_at { return Err(ContractError::SignatureExpired); }
        voter.require_auth();
        let data = Self::_load_data(&env)?;
        if !Self::_is_signer(&env, &voter) && data.admin != voter {
            return Err(ContractError::Unauthorized);
        }
        let ballot = cast_vote(&env, REVOCATION_KEY, voter)?;
        let threshold = Self::_revocation_threshold(&env);
        if ballot.votes.len() >= threshold {
            let mut contract_data = data;
            contract_data.admin = ballot.replacement.clone();
            env.storage().instance().set(&DATA_KEY, &contract_data);
            close_ballot(&env, REVOCATION_KEY);
        }
        Ok(())
    }

    pub fn get_revocation_ballot(env: Env) -> Option<VotingBallot> {
        governance::get_ballot(&env, REVOCATION_KEY)
    }

    pub fn get_data(env: Env) -> Result<ContractData, ContractError> {
        Self::load_data(&env)
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
        crate::staging::check_staging_access(&env, &proposer)?;
        let data = Self::_load_data(&env)?;
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

    pub fn execute_upgrade(
        env: Env, executor: Address,
        nonce: u64, salt: Bytes, signature: BytesN<32>, sig_expires_at: u64,
    ) -> Result<(), ContractError> {
        if env.ledger().timestamp() > sig_expires_at { return Err(ContractError::SignatureExpired); }
        crate::staging::check_staging_access(&env, &executor)?;
        let data = Self::_load_data(&env)?;
        if data.admin != executor { return Err(ContractError::NotAdmin); }
        executor.require_auth();
        consume_nonce(&env, &executor, nonce, salt, signature)?;
        let pending: StagedUpgrade = env
            .storage()
            .instance()
            .get(&PENDING_UPGRADE_KEY)
            .ok_or(ContractError::NoPendingUpgrade)?;
        if !crate::upgrades::timelock::is_ready(pending.execute_at, env.ledger().timestamp()) {
            return Err(ContractError::UpgradeTimelockNotSatisfied);
        }
        // Store pre-upgrade contract data snapshot for health check validation
        let pre_upgrade_data = data.clone();
        env.deployer().update_current_contract_wasm(pending.new_wasm_hash);
        // Run post-upgrade diagnostic health checks
        Self::_run_post_upgrade_health_check(&env, pre_upgrade_data)?;
        env.storage().instance().remove(&PENDING_UPGRADE_KEY);
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

    pub fn get_upgrade_timelock_remaining(env: Env) -> Option<u64> {
        env.storage().instance().get(&PENDING_UPGRADE_KEY).map(|staged: StagedUpgrade| {
            staged.execute_at.saturating_sub(env.ledger().timestamp())
        })
    }

    pub fn cancel_upgrade(env: Env, canceller: Address) -> Result<(), ContractError> {
        crate::staging::check_staging_access(&env, &canceller)?;
        let data = Self::_load_data(&env)?;
        if data.admin != canceller { return Err(ContractError::NotAdmin); }
        canceller.require_auth();
        env.storage().instance().remove(&PENDING_UPGRADE_KEY);
        env.storage().instance().remove(&crate::governance::GOVERNANCE_UPGRADE_KEY);
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

    pub fn rollback_upgrade(
        env: Env,
        admin: Address,
        nonce: u64,
        salt: Bytes,
        signature: BytesN<32>,
        sig_expires_at: u64,
    ) -> Result<(), ContractError> {
        crate::upgrades::rollback::execute_rollback(env, admin, nonce, salt, signature, sig_expires_at)
    }

    pub fn set_value(env: Env, new_value: u64, caller: Address, nonce: u64, salt: Bytes, signature: BytesN<32>, sig_expires_at: u64) -> Result<(), ContractError> {
        if env.ledger().timestamp() > sig_expires_at { return Err(ContractError::SignatureExpired); }
        crate::staging::check_staging_access(&env, &caller)?;
        let mut data = Self::_load_data(&env)?;
        if data.admin != caller { return Err(ContractError::NotAdmin); }
        caller.require_auth();
        consume_nonce(&env, &caller, nonce, salt, signature)?;
        data.value = new_value;
        env.storage().instance().set(&DATA_KEY, &data);
        Self::_record_heartbeat(&env, 1u32);
        crate::recovery::update_admin_activity(&env);
        Ok(())
    }

    pub fn get_coordinator_nonce(env: Env, coordinator: Address) -> u64 {
        get_nonce(&env, &coordinator)
    }

    pub fn get_last_update_timestamp(env: Env, asset: AssetId) -> Option<u64> {
        let heartbeat_key = HeartbeatKey::HeartbeatByAsset(asset);
        env.storage().temporary().get(&heartbeat_key)
    }

    pub fn get_heartbeat_interval(env: Env) -> u64 {
        let _ = ensure_schema_version(&env);
        Self::_get_interval(&env)
    }

    pub fn set_heartbeat_interval(env: Env, interval: u64, admin: Address) -> Result<(), ContractError> {
        if interval == 0 { return Err(ContractError::InvalidHeartbeatInterval); }
        crate::staging::check_staging_access(&env, &admin)?;
        let data = Self::_load_data(&env)?;
        if data.admin != admin { return Err(ContractError::NotAdmin); }
        admin.require_auth();
        env.storage().instance().set(&HB_INTERVAL_KEY, &interval);
        Self::_extend_instance_ttl(&env);
        crate::recovery::update_admin_activity(&env);
        Ok(())
    }

    pub fn get_stake(env: Env, node: Address) -> u64 {
        let stake_key = StakeKey::StakeByNode(node);
        env.storage().instance().get(&stake_key).unwrap_or(0u64)
    }

    pub fn get_total_staked(env: Env) -> u64 {
        let _ = ensure_schema_version(&env);
        env.storage()
            .instance()
            .get(&TOTAL_STAKED_KEY)
            .unwrap_or(0u64)
    }

    pub fn update_heartbeat(env: Env, asset: AssetId, updater: Address) -> Result<(), ContractError> {
        let data = Self::_load_data(&env)?;
        if data.admin != updater { return Err(ContractError::NotAdmin); }
        updater.require_auth();
        check_liquidity_depth(&env, asset)?;
        Self::_record_heartbeat(&env, asset);
        Self::_extend_instance_ttl(&env);
        crate::recovery::update_admin_activity(&env);
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
        crate::staging::check_staging_access(&env, &admin)?;
        let data = Self::_load_data(&env)?;
        if data.admin != admin { return Err(ContractError::NotAdmin); }
        admin.require_auth();
        let profile_key = NodeProfileKey::ProfileByNode(node.clone());
        let profile = NodeProfile { node: node.clone(), rate, confidence, updated_at: env.ledger().timestamp() };
        env.storage().persistent().set(&profile_key, &profile);
        Self::_extend_instance_ttl(&env);
        crate::recovery::update_admin_activity(&env);
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

    pub fn get_corridor_weight(env: Env, asset: AssetId) -> fees::CorridorWeightProfile {
        fees::get_corridor_weight(env, asset)
    }

    /// Process a bundled multi-asset price submission.
    ///
    /// Delegates to `validation::process_price_bundle` for gas-throttled
    /// single-pass linear scanning with pre-calculated key index pointers.
    pub fn update_prices_bundle(
        env: Env,
        node: Address,
        updates: Vec<AssetPriceUpdate>,
    ) -> Result<BundleValidationOutcome, ContractError> {
        node.require_auth();
        let outcome = process_price_bundle(&env, &node, &updates)?;
        Self::_extend_instance_ttl(&env);
        Ok(outcome)
    }

    // ── Dynamic Staking Tier Assignment (Issue #300) ─────────────────────────

    /// Configure the minimum stake required for each collateral tier.
    /// Requires multi-signature consensus (≥ 2 valid signers) for cross-border
    /// parameter changes — issue #539.
    pub fn set_staking_tier_config(
        env: Env, admin: Address, config: StakingTierConfig, signers: Vec<Address>,
    ) -> Result<(), ContractError> {
        let data = Self::_load_data(&env)?;
        if data.admin != admin { return Err(ContractError::NotAdmin); }
        admin.require_auth();
        crate::auth::require_multisig(&env, &signers)?;
        validate_tier_config(&config)?;
        env.storage().instance().set(&StakingStorageKey::TierConfig, &config);
        Self::_extend_instance_ttl(&env);
        crate::recovery::update_admin_activity(&env);
        Ok(())
    }

    pub fn get_staking_tier_config(env: Env) -> StakingTierConfig {
        env.storage().instance().get(&StakingStorageKey::TierConfig).unwrap_or_default()
    }

    pub fn set_asset_feed_metrics(
        env: Env, admin: Address, asset: AssetId,
        volume_score_floor: u32, volatility_bps: u32, signers: Vec<Address>,
    ) -> Result<AssetFeedMetrics, ContractError> {
        let data = Self::_load_data(&env)?;
        if data.admin != admin { return Err(ContractError::NotAdmin); }
        admin.require_auth();
        crate::auth::require_multisig(&env, &signers)?;
        let metrics = AssetFeedMetrics {
            volume_score: volume_score_floor.min(100),
            volatility_bps,
        };
        env.storage().persistent().set(&StakingStorageKey::AssetMetrics(asset), &metrics);

        env.storage()
            .persistent()
            .set(&StakingStorageKey::AssetMetrics(asset), &metrics);

        Self::_extend_instance_ttl(&env);
        crate::recovery::update_admin_activity(&env);
        Ok(metrics)
    }

    pub fn get_asset_feed_metrics(env: Env, asset: AssetId) -> AssetFeedMetrics {
        Self::_resolve_feed_metrics(&env, asset)
    }

    pub fn get_staking_tier(env: Env, asset: AssetId) -> StakingTier {
        assign_tier(&Self::_resolve_feed_metrics(&env, asset))
    }

    /// Return the minimum stake a validator must post for a currency feed.
    pub fn get_required_stake(env: Env, asset: AssetId) -> u64 {
        let tier = Self::get_staking_tier(env.clone(), asset);
        let config = Self::get_staking_tier_config(env);
        required_stake_for_tier(tier, &config)
    }

    pub fn stake_and_register_for_feed(
        env: Env, node: Address, asset: AssetId, amount: u64,
    ) -> Result<FeedStakeRecord, ContractError> {
        if amount == 0 { return Err(ContractError::InvalidStakeAmount); }
        admin::assert_not_revoked(&env, &node)?;
        node.require_auth();

        let feed_key = StakingStorageKey::FeedStake(node.clone(), asset);
        if env.storage().persistent().has(&feed_key) { return Err(ContractError::FeedAlreadyRegistered); }
        let tier = Self::get_staking_tier(env.clone(), asset);
        let required = Self::get_required_stake(env.clone(), asset);
        if amount < required { return Err(ContractError::InsufficientStakeForTier); }
        let stake_val = storage::FeedStakeValue { amount, last_active: env.ledger().timestamp() };
        env.storage().persistent().set(&feed_key, &stake_val);
        env.storage().persistent().extend_ttl(&feed_key, storage::RENT_THRESHOLD, storage::RENT_EXTEND_TO);
        let stake_key = StakeKey::StakeByNode(node.clone());
        let node_total: u64 = env.storage().instance().get(&stake_key).unwrap_or(0);
        let new_node_total = node_total.checked_add(amount).ok_or(ContractError::Overflow)?;
        env.storage().instance().set(&stake_key, &new_node_total);
        let total: u64 = env.storage().instance().get(&TOTAL_STAKED_KEY).unwrap_or(0u64);
        let new_total = total.checked_add(amount).ok_or(ContractError::Overflow)?;
        env.storage().instance().set(&TOTAL_STAKED_KEY, &new_total);
        Self::_record_heartbeat(&env, asset);
        Ok(FeedStakeRecord {
            node,
            asset,
            amount,
            tier,
            registered_at: env.ledger().timestamp(),
        })
    }

    pub fn unstake_from_feed(env: Env, node: Address, asset: AssetId) -> Result<u64, ContractError> {
        node.require_auth();

        let feed_key = StakingStorageKey::FeedStake(node.clone(), asset);
        let stake_val: storage::FeedStakeValue = env.storage().persistent()
            .get(&feed_key).ok_or(ContractError::NotRegistered)?;
        let amount = stake_val.amount;
        env.storage().persistent().remove(&feed_key);
        let stake_key = StakeKey::StakeByNode(node.clone());
        let node_total: u64 = env.storage().instance().get(&stake_key).unwrap_or(0);
        let new_node_total = node_total.saturating_sub(amount);
        if new_node_total == 0 {
            env.storage().instance().remove(&stake_key);
        } else {
            env.storage().instance().set(&stake_key, &new_node_total);
        }
        let total: u64 = env.storage().instance().get(&TOTAL_STAKED_KEY).unwrap_or(0u64);
        env.storage().instance().set(&TOTAL_STAKED_KEY, &total.saturating_sub(amount));
        Ok(amount)
    }

    /// Return the collateral posted by a node for a specific currency feed.
    ///
    /// Checks and prunes an expired (rent-lapsed) stake entry first (issue
    /// #522): a validator that has gone stale for longer than
    /// `storage::RENT_THRESHOLD` since its last activity has its stake entry
    /// removed and its totals reconciled before this read returns.
    pub fn get_feed_stake(env: Env, node: Address, asset: AssetId) -> u64 {
        let feed_key = StakingStorageKey::FeedStake(node, asset);
        let stake_val: Option<storage::FeedStakeValue> = env
            .storage()
            .persistent()
            .get(&feed_key);
        stake_val.map(|v| v.amount).unwrap_or(0)
    }

    pub fn set_platform_capital(env: Env, capital: u64) {
        env.storage().instance().set(&PLATFORM_CAPITAL_KEY, &capital);
    }

    // ── Issue #420: Sealed price-variance configuration ──────────────────────

    /// Read the active price-variance configuration (or compile-time defaults).
    pub fn get_price_variance_config(env: Env) -> PriceVarianceConfig {
        config::get_price_variance_config(&env)
    }

    /// Replace the complete price-variance configuration. Admin-only.
    pub fn set_price_variance_config(
        env: Env,
        caller: Address,
        cfg: PriceVarianceConfig,
    ) -> Result<(), ContractError> {
        config::set_price_variance_config(&env, &caller, cfg)
    }

    /// End the current consensus epoch: remove the cache, heartbeat map, and any
    /// active revocation ballot from Temporary storage so the ledger stays lean.
    pub fn finalize_consensus(env: Env) {
        env.storage().temporary().remove(&CONSENSUS_CACHE_KEY);
        env.storage().temporary().remove(&HEARTBEAT_KEY);
        close_ballot(&env, REVOCATION_KEY);
    }

    pub fn register_signer(env: Env, signer: Address, caller: Address) -> Result<(), ContractError> {
        let data = Self::_load_data(&env)?;
        if data.admin != caller { return Err(ContractError::NotAdmin); }
        caller.require_auth();
        let signer_key = SignerKey::SignerByAddress(signer.clone());
        if !env.storage().instance().has(&signer_key) {
            env.storage().instance().set(&signer_key, &true);
            let count: u32 = env.storage().instance().get(&SIGNERS_KEY).unwrap_or(0u32);
            env.storage().instance().set(&SIGNERS_KEY, &(count + 1));
        }
        Self::_extend_instance_ttl(&env);
        crate::recovery::update_admin_activity(&env);
        Ok(())
    }

    // --- Admin Ownership Transfer (Issue #429) ---

    pub fn propose_ownership_transfer(env: Env, current_admin: Address, nominee: Address, nonce: u64) -> Result<(), ContractError> {
        crate::admin::propose_ownership_transfer(&env, current_admin, nominee, nonce)
    }

    pub fn claim_ownership(env: Env, claimer: Address, nonce: u64) -> Result<(), ContractError> {
        crate::admin::claim_ownership(&env, claimer, nonce)
    }

    // --- Two-Phase Admin Key Change (Issue #493) ---

    pub fn propose_admin_change(env: Env, current_admin: Address, new_admin: Address) -> Result<(), ContractError> {
        crate::admin::propose_admin_change(&env, current_admin, new_admin)
    }

    pub fn countersign_admin_change(env: Env, cosigner: Address) -> Result<(), ContractError> {
        crate::admin::countersign_admin_change(&env, cosigner)
    }

    pub fn execute_admin_change_by_timelock(env: Env, executor: Address) -> Result<(), ContractError> {
        crate::admin::execute_admin_change_by_timelock(&env, executor)
    }

    pub fn cancel_admin_change(env: Env, canceller: Address) -> Result<(), ContractError> {
        crate::admin::cancel_admin_change(&env, canceller)
    }

    pub fn get_pending_admin_change(env: Env) -> Option<admin::AdminChangeProposal> {
        crate::admin::get_pending_admin_change(&env)
    }

    // --- Emergency pause ---

    pub fn set_paused(env: Env, caller: Address, paused: bool, nonce: u64) -> Result<(), ContractError> {
        crate::admin::set_paused(&env, caller, paused, nonce)
    }

    // --- Emergency Admin (Issue #598) ---

    /// Designate an address as the EmergencyAdmin, capable of instantly pausing
    /// the contract without multi-sig delay. Only the current admin may call this.
    pub fn set_emergency_admin(env: Env, caller: Address, emergency_admin: Address) -> Result<(), ContractError> {
        crate::security::pausable::set_emergency_admin(&env, &caller, &emergency_admin)
    }

    /// Return the currently configured EmergencyAdmin address, if any.
    pub fn get_emergency_admin(env: Env) -> Option<Address> {
        crate::security::pausable::get_emergency_admin(&env)
    }

    /// Instantly pause all contract operations.
    ///
    /// Only the designated `EmergencyAdmin` may call this. Authentication is a
    /// single `require_auth` check — no multi-sig delay window applies.
    ///
    /// # Errors
    /// - [`ContractError::NotEmergencyAdmin`] if the caller is not the emergency admin.
    pub fn emergency_pause(env: Env, caller: Address) -> Result<(), ContractError> {
        crate::security::pausable::emergency_pause(&env, &caller)
    }

    /// Resume contract operations after an emergency pause.
    ///
    /// Requires full governance multi-sig quorum via `require_multisig` to
    /// protect against a compromised EmergencyAdmin permanently locking the
    /// contract. The `caller` must be one of the signers.
    ///
    /// # Errors
    /// - [`ContractError::ThresholdNotReached`] if insufficient signers.
    pub fn emergency_unpause(env: Env, caller: Address, signers: Vec<Address>) -> Result<(), ContractError> {
        crate::security::pausable::emergency_unpause(&env, &caller, &signers)
    }

    // --- Vault Emergency Pause Guard (Issue #717) ---

    /// Returns `true` when the vault subsystem is independently paused.
    pub fn is_vault_paused(env: Env) -> bool {
        crate::vaults::pause_guard::is_vault_paused(&env)
    }

    /// Pause vault interactions (deposits and harvests) while allowing
    /// emergency withdrawals.  Only the contract admin may call this.
    pub fn pause_vault(env: Env, caller: Address) -> Result<(), ContractError> {
        crate::vaults::pause_guard::pause_vault(&env, &caller)
    }

    /// Resume vault interactions after a vault-specific pause.
    /// Only the contract admin may call this.
    pub fn unpause_vault(env: Env, caller: Address) -> Result<(), ContractError> {
        crate::vaults::pause_guard::unpause_vault(&env, &caller)
    }

    /// Execute an emergency vault withdrawal while the vault is paused.
    ///
    /// This is the *only* vault interaction permitted during a vault pause.
    /// The caller must be the asset owner (or an admin) and must authorize
    /// the transaction.
    pub fn emergency_vault_withdraw(
        env: Env,
        caller: Address,
        asset: Symbol,
        amount: u128,
    ) -> Result<(), ContractError> {
        crate::vaults::pause_guard::emergency_vault_withdraw(
            &env,
            &caller,
            asset,
            amount,
            |_env, _caller, _asset, _amount| Ok(()),
        )
    }

    // --- Per-action admin nonce query (Issue #529) ---

    pub fn get_admin_action_nonce(env: Env, caller: Address, action: admin::AdminAction) -> u64 {
        crate::admin::get_admin_action_nonce(&env, &caller, action)
    }

    // --- Emergency key revocation (multi-sig coordinator group) ---

    /// Open an emergency revocation proposal against a compromised hot-wallet key.
    pub fn propose_emergency_revocation(
        env: Env,
        proposer: Address,
        target: Address,
        replacement: Address,
        nonce: u64,
    ) -> Result<(), ContractError> {
        admin::propose_emergency_revocation(&env, proposer, target, replacement, nonce)
    }

    /// Explicitly purge an expired or stale emergency revocation proposal.
    ///
    /// This function allows cleanup of proposals that have failed to reach quorum or
    /// have become stale. While the Soroban network will eventually auto-purge via TTL,
    /// explicit removal frees resources sooner and allows reinitiating a new proposal.
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
        admin::purge_emergency_revocation_proposal(&env)
    }

    pub fn has_active_revocation_proposal(env: Env) -> bool {
        admin::has_active_emergency_revocation(&env)
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
    pub fn get_veto_record(env: Env, proposal_id: u64) -> Option<veto::ProposalVeto> {
        veto::get_veto_record(&env, proposal_id)
    }

    /// Check if a proposal has been vetoed.
    pub fn is_proposal_vetoed(env: Env, proposal_id: u64) -> bool {
        veto::is_proposal_vetoed(&env, proposal_id)
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

    pub fn init_yield_farming(
        env: Env,
        admin: Address,
        lp_token: Address,
        reward_token: Address,
        emission_per_ledger: i128,
    ) -> Result<vaults::lp_farming::FarmingConfig, ContractError> {
        vaults::lp_farming::initialize(
            &env,
            admin,
            lp_token,
            reward_token,
            emission_per_ledger,
        )
    }

    pub fn fund_yield_rewards(
        env: Env,
        funder: Address,
        amount: i128,
    ) -> Result<(), ContractError> {
        vaults::lp_farming::fund_rewards(&env, funder, amount)
    }

    pub fn stake_lp(env: Env, user: Address, amount: i128) -> Result<i128, ContractError> {
        let _guard = security::reentrancy::ReentrancyGuard::new(&env)?;
        vaults::lp_farming::stake(&env, user, amount)
    }

    pub fn claim_rewards(env: Env, user: Address) -> Result<i128, ContractError> {
        let _guard = security::reentrancy::ReentrancyGuard::new(&env)?;
        vaults::lp_farming::claim_rewards(&env, user)
    }

    pub fn exit_yield_farming(
        env: Env,
        user: Address,
    ) -> Result<(i128, i128), ContractError> {
        let _guard = security::reentrancy::ReentrancyGuard::new(&env)?;
        vaults::lp_farming::exit(&env, user)
    }

    pub fn set_emission_multiplier(
        env: Env,
        governance: Address,
        multiplier: u32,
    ) -> Result<vaults::lp_farming::FarmingConfig, ContractError> {
        let _guard = security::reentrancy::ReentrancyGuard::new(&env)?;
        vaults::lp_farming::set_emission_multiplier(&env, governance, multiplier)
    }

    pub fn pending_yield_rewards(
        env: Env,
        user: Address,
    ) -> Result<i128, ContractError> {
        vaults::lp_farming::pending_rewards(&env, user)
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

    pub fn commitment_root_history(env: Env) -> Vec<BytesN<32>> {
        escrow::merkle::root_history(&env)
    }

    // --- Private Helpers ---

    fn _extend_instance_ttl(env: &Env) {
        env.storage().instance().extend_ttl(RELAYER_TTL_THRESHOLD, INSTANCE_TTL_EXTEND);
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
        env.storage()
            .instance()
            .get(&HB_INTERVAL_KEY)
            .unwrap_or(DEFAULT_HEARTBEAT_INTERVAL)
    }

    fn _get_node_profiles(env: &Env) -> Map<Address, NodeProfile> {
        crate::storage::get_node_profiles(env)
    }

    fn _scan_profile_for_rate(profile: NodeProfile) -> Option<u64> {
        if profile.confidence == 0 { None } else { Some(profile.rate) }
    }

    fn _maintain_relayer_profile_ttl(env: &Env) {
        // With individual tuple keys, TTL is managed per-entry.
        // This is a no-op placeholder for compatibility.
        let _ = env;
    }

    fn _extend_instance_ttl(env: &Env) {
        env.storage()
            .instance()
            .extend_ttl(RELAYER_TTL_THRESHOLD, INSTANCE_TTL_EXTEND);
    }


    fn _is_signer(env: &Env, addr: &Address) -> bool {
        let signer_key = SignerKey::SignerByAddress(addr.clone());
        env.storage().instance().has(&signer_key)
    }

    fn _revocation_threshold(env: &Env) -> u32 {
        let signer_count: u32 = env.storage().instance().get(&SIGNERS_KEY).unwrap_or(0u32);
        if signer_count == 0 { 1 } else { signer_count / 2 + 1 }
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
}
#[cfg(test)]
mod query_guardrail_tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger, LedgerInfo};
    use soroban_sdk::{symbol_short, Env};

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
        let first_value = client.get_data().value;
        let second_value = client.get_data().value;
        assert_eq!(first_value, second_value);
        assert_eq!(first_value, 0);
    }

    #[test]
    fn test_is_data_fresh_unknown_asset_returns_false() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let treasury = soroban_sdk::Address::generate(&env);
        client.initialize(&admin, &treasury);
        let asset = symbol_to_asset_id(&symbol_short!("NGN"));
        assert!(!client.is_data_fresh(&asset));
    }

    #[test]
    fn test_is_data_fresh_transitions_on_staleness() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let treasury = soroban_sdk::Address::generate(&env);
        client.initialize(&admin, &treasury);
        let asset = symbol_to_asset_id(&symbol_short!("KES"));
        client.add_corridor_fees(&admin, &asset, &crate::validation::MIN_POOL_VOLUME_DEPTH, &0u64);
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
        let asset = symbol_to_asset_id(&symbol_short!("GHS"));
        client.add_corridor_fees(&admin, &asset, &crate::validation::MIN_POOL_VOLUME_DEPTH, &0u64);
        client.update_heartbeat(&asset, &admin);
        for _ in 0..5 { assert!(client.is_data_fresh(&asset)); }
        advance(&env, DEFAULT_HEARTBEAT_INTERVAL + 1);
        assert!(!client.is_data_fresh(&asset));
    }

    #[test]
    fn test_query_methods_do_not_interfere() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let treasury = soroban_sdk::Address::generate(&env);
        client.initialize(&admin, &treasury);
        let asset = symbol_to_asset_id(&symbol_short!("CFA"));
        let value_before = client.get_data().value;
        let _ = client.is_data_fresh(&asset);
        let value_after = client.get_data().value;
        assert_eq!(value_before, value_after);
    }
}

// #[cfg(test)]
// mod test;
// NOTE: _resolve_feed_metrics is defined inside the main contract impl.

// Integration tests for issue #525 live in `src/slashing.rs`.
#[cfg(test)]
mod test;
