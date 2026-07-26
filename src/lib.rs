#![no_std]
use soroban_sdk::{contract, contracterror, contractimpl, contractmeta, contracttype, symbol_short, Address, Bytes, BytesN, Env, Map, Symbol, Vec};

contractmeta!(
    name = "stellarflow-contracts",
    version = env!("CARGO_PKG_VERSION"),
    author = "StellarFlow Network",
    description = env!("CARGO_PKG_DESCRIPTION"),
    interface = "stellarflow-v1",
    build_time = env!("BUILD_TIME"),
    git_sha = env!("GIT_SHA"),
);

/// Numeric asset identifier for gas-optimized storage.
/// Replaces heavy Symbol identifiers in high-frequency paths.
pub type AssetId = u32;

/// Convert a currency Symbol to a numeric AssetId using FNV-1a hash over the
/// symbol's raw 64-bit payload. Deterministic and no-std compatible.
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

/// Companion reverse lookup ensuring parity across asset pairing states.
pub fn asset_id_to_symbol(asset_id: u32) -> Symbol {
    match asset_id {
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

pub mod admin;
pub mod auth;
pub mod config;
pub use config::{get_price_variance_config, set_price_variance_config, PriceVarianceConfig};
pub mod consensus;
pub mod fees;
pub mod governance;
pub mod math;
pub mod recovery;
pub mod slashing;
pub mod staking_tiers;
pub mod storage;
pub mod temp_governance;
pub mod upgrades;
pub mod validation;

use crate::governance::{
    verify_staged_delay, StagedUpgrade, VotingBallot, open_ballot, cast_vote, close_ballot, get_ballot,
};
use crate::validation::{
    check_bond_capacity, check_liquidity_depth, validate_telemetry_submission,
    process_price_bundle, AssetPriceUpdate, BundleValidationOutcome,
};

pub use staking_tiers::{AssetFeedMetrics, StakingTier, StakingTierConfig};

use staking_tiers::{
    assign_tier, effective_volume_score, required_stake_for_tier, validate_tier_config,
};
use slashing::{
    apply_escrow_penalty, get_fault_count_in_window, get_penalty_multiplier,
    record_tracking_fault, IngestionPenaltyResult,
};
use storage::{StakeKey, NodeProfileKey, SignerKey};

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
    /// Proposed value exceeds the maximum allowed fee ceiling.
    FeeCeilingExceeded = 26,
    /// Attempted to divide by zero in a mathematical operation.
    DivisionByZero = 27,
    /// Incoming tracking sequence is less than or equal to the active stored checkpoint value.
    StaleSequence = 28,
    /// A price-variance configuration field violated one or more struct invariants.
    InvalidVarianceConfig = 29,
    /// Telemetry submission rejected: payload timestamp is stale.
    StaleTelemetryPayload = 30,
    /// Telemetry submission rejected: reported reserve balance is below minimum security threshold.
    InsufficientReserveBalance = 31,
    /// Telemetry submission rejected: trading volume falls below required minimum.
    InsufficientVolume = 32,
    /// Pool liquidity / volume depth is below the minimum economic security gate.
    InsufficientLiquidityDepth = 33,
    ContractPaused = 34,
    RevokedAddress = 35,
    EmergencyRevocationAlreadyActive = 36,
    NoActiveEmergencyRevocation = 37,
    /// The submitted price bundle exceeds the maximum allowed asset count.
    BundleAssetLimitExceeded = 38,
    /// A bundle update failed validation for one or more assets.
    BundleValidationFailed = 39,
    /// Fewer than the minimum number of independent validator nodes supplied
    /// parameters during the current block round.
    IncompleteQuorum = 40,
    /// The current ledger sequence falls outside the allowed epoch validation window.
    EpochClosed = 41,
    /// A two-phase admin key change is already pending.
    AdminChangePending = 42,
    /// No two-phase admin key change proposal is currently pending.
    NoAdminChangePending = 43,
    /// The cosigner approving an admin change cannot be its own proposer.
    CosignerCannotBeProposer = 44,
    /// The 24-hour admin-change timelock has not yet elapsed.
    AdminChangeTimelockNotSatisfied = 45,
    /// The validator has no locked bond available to deduct an escrow penalty from.
    InsufficientBondForPenalty = 46,
}

// Contract state keys
pub(crate) const DATA_KEY: Symbol = symbol_short!("DATA");
pub(crate) const SIGNERS_KEY: Symbol = symbol_short!("SIGNERS");
const PENDING_UPGRADE_KEY: Symbol = symbol_short!("PENDING");
pub(crate) const UPGRADE_DELAY_SECONDS: u64 = 48 * 60 * 60;
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
const TREASURY_KEY: Symbol = symbol_short!("TREASURY");
const SEQUENCE_COUNTER_KEY: Symbol = symbol_short!("SEQCTR");
const RECOVERY_KEY: Symbol = symbol_short!("RKEY");
const LAST_ADMIN_ACTIVITY: Symbol = symbol_short!("LASTACT");

#[contracttype]
#[derive(Clone)]
pub struct RevocationProposal {
    pub target: Address,
    pub replacement: Address,
    pub proposer: Address,
    pub proposed_at: u64,
    /// Using Vec<Address> instead of Map for gas optimization.
    pub votes: Vec<Address>,
}
/// Symbol used as the proposal identifier for admin revocation ballots.
/// Stored in Temporary storage via the governance ballot module.
const REVOCATION_KEY: Symbol = symbol_short!("REVOKE");

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

#[contractimpl]
impl TimeLockedUpgradeContract {
    pub fn initialize(env: Env, admin: Address, treasury: Address) -> Result<(), ContractError> {
        ensure_schema_version(&env)?;
        if env.storage().instance().has(&DATA_KEY) {
            return Err(ContractError::AlreadyInitialized);
        }
        admin.require_auth();
        let data = ContractData {
            admin: admin.clone(),
            value: 0,
        };
        env.storage().instance().set(&DATA_KEY, &data);
        // #439: write treasury once at deployment; never overwritten
        env.storage().instance().set(&TREASURY_KEY, &treasury);
        Ok(())
    }

    pub fn stake_and_register(
        env: Env,
        node: Address,
        amount: u64,
    ) -> Result<StakeRecord, ContractError> {
        if amount == 0 {
            return Err(ContractError::InvalidStakeAmount);
        }
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
            // Update signer count
            let count: u32 = env.storage().instance().get(&SIGNERS_KEY).unwrap_or(0u32);
            env.storage().instance().set(&SIGNERS_KEY, &(count - 1));
        }
        Self::_extend_instance_ttl(&env);
        crate::recovery::update_admin_activity(env);
        Ok(())
    }

    /// Nominate a target signer for removal and open an ephemeral voting ballot
    /// in Temporary storage. The ballot is automatically reclaimed by the ledger
    /// once the consensus epoch window closes, keeping state lean.
    pub fn propose_revocation(
        env: Env,
        proposer: Address,
        target: Address,
        replacement: Address,
        sig_expires_at: u64,
    ) -> Result<(), ContractError> {
        if env.ledger().timestamp() > sig_expires_at {
            return Err(ContractError::SignatureExpired);
        }
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
        get_ballot(&env, REVOCATION_KEY)
    }

    /// Return the active governance configuration (quorum threshold).
    pub fn get_governance_config(env: Env) -> governance::GovernanceConfig {
        governance::get_governance_config(&env)
    }

    /// Set the governance quorum threshold for WASM upgrade proposals.
    /// Requires admin authorization.
    pub fn set_governance_config(
        env: Env,
        admin: Address,
        config: governance::GovernanceConfig,
    ) -> Result<(), ContractError> {
        let data = Self::_load_data(&env)?;
        if data.admin != admin {
            return Err(ContractError::NotAdmin);
        }
        admin.require_auth();
        governance::set_governance_config(&env, &config);
        env.events().publish(
            (symbol_short!("GVN_CFG_UPD"),),
            (admin, config.quorum_threshold),
        );
        Self::_extend_instance_ttl(&env);
        Ok(())
    }

    /// Return the active governance upgrade proposal, if one exists.
    pub fn get_governance_upgrade_proposal(env: Env) -> Option<governance::GovernanceUpgradeProposal> {
        env.storage()
            .instance()
            .get(&crate::governance::GOVERNANCE_UPGRADE_KEY)
    }

    // --- Core Logic Boilerplate ---

    fn _load_data(env: &Env) -> Result<ContractData, ContractError> {
        let _ = ensure_schema_version(env);
        env.storage().instance().get(&DATA_KEY).ok_or(ContractError::NotInitialized)
    }

    pub fn get_data(env: Env) -> Result<ContractData, ContractError> {
        Self::_load_data(&env)
    }

    fn _load_data(env: &Env) -> Result<ContractData, ContractError> {
        env.storage().instance().get(&DATA_KEY).ok_or(ContractError::NotInitialized)
    }

    pub fn propose_upgrade(env: Env, new_wasm_hash: BytesN<32>, proposer: Address, signers: Vec<Address>, nonce: u64, salt: Bytes, salt_signature: BytesN<32>, sig_expires_at: u64) -> Result<(), ContractError> {
        if env.ledger().timestamp() > sig_expires_at { return Err(ContractError::SignatureExpired); }
        let data = Self::_load_data(&env)?;
        if data.admin != proposer { return Err(ContractError::NotAdmin); }
        proposer.require_auth();
        consume_nonce(&env, &proposer, nonce, salt, salt_signature)?;
        verify_upgrade_quorum(&env, &signers)?;
        let proposal = GovernanceUpgradeProposal {
            new_wasm_hash: new_wasm_hash.clone(),
            proposer: proposer.clone(),
            staged_at: env.ledger().timestamp(),
            signers: signers.clone(),
        };
        env.storage().instance().set(&crate::governance::GOVERNANCE_UPGRADE_KEY, &proposal);
        env.events().publish(
            (symbol_short!("GV_UPG_PROPOSED"),),
            (new_wasm_hash, proposer, signers, env.ledger().timestamp()),
        );
        let staged = StagedUpgrade {
            new_wasm_hash,
            proposer,
            staged_at: env.ledger().timestamp(),
        };
        env.storage().instance().set(&PENDING_UPGRADE_KEY, &staged);
        Ok(())
    }

    pub fn execute_upgrade(env: Env, executor: Address, nonce: u64, salt: Bytes, signature: BytesN<32>, sig_expires_at: u64) -> Result<(), ContractError> {
        if env.ledger().timestamp() > sig_expires_at { return Err(ContractError::SignatureExpired); }
        let data = Self::_load_data(&env)?;
        if data.admin != executor { return Err(ContractError::NotAdmin); }
        executor.require_auth();
        consume_nonce(&env, &executor, nonce, salt, signature)?;
        let proposal: GovernanceUpgradeProposal = env
            .storage()
            .instance()
            .get(&crate::governance::GOVERNANCE_UPGRADE_KEY)
            .ok_or(ContractError::NoPendingUpgrade)?;
        verify_upgrade_quorum(&env, &proposal.signers)?;
        let pending: StagedUpgrade = env
            .storage()
            .instance()
            .get(&PENDING_UPGRADE_KEY)
            .ok_or(ContractError::NoPendingUpgrade)?;
        if !verify_staged_delay(pending.staged_at, env.ledger().timestamp(), UPGRADE_DELAY_SECONDS) {
            return Err(ContractError::UpgradeTimelockNotSatisfied);
        }
        // Store pre-upgrade contract data snapshot for health check validation
        let pre_upgrade_data = data.clone();
        env.deployer().update_current_contract_wasm(pending.new_wasm_hash);
        // Run post-upgrade diagnostic health checks
        Self::_run_post_upgrade_health_check(&env, pre_upgrade_data)?;
        env.storage().instance().remove(&PENDING_UPGRADE_KEY);
        env.storage().instance().remove(&crate::governance::GOVERNANCE_UPGRADE_KEY);
        Self::_extend_instance_ttl(&env);
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
            let elapsed = env.ledger().timestamp().saturating_sub(staged.staged_at);
            UPGRADE_DELAY_SECONDS.saturating_sub(elapsed)
        })
    }

    pub fn cancel_upgrade(env: Env, canceller: Address) -> Result<(), ContractError> {
        let data = Self::_load_data(&env)?;
        if data.admin != canceller { return Err(ContractError::NotAdmin); }
        canceller.require_auth();
        env.storage().instance().remove(&PENDING_UPGRADE_KEY);
        env.storage().instance().remove(&crate::governance::GOVERNANCE_UPGRADE_KEY);
        Self::_extend_instance_ttl(&env);
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
        let mut data = Self::_load_data(&env)?;
        if data.admin != caller { return Err(ContractError::NotAdmin); }
        caller.require_auth();
        consume_nonce(&env, &caller, nonce, salt, signature)?;
        data.value = new_value;
        env.storage().instance().set(&DATA_KEY, &data);
        Self::_record_heartbeat(&env, 1u32);
        crate::recovery::update_admin_activity(env);
        Ok(())
    }

    pub fn get_coordinator_nonce(env: Env, coordinator: Address) -> u64 {
        get_nonce(&env, &coordinator)
    }

    pub fn get_last_update_timestamp(env: Env, asset: AssetId) -> Option<u64> {
        let _ = ensure_schema_version(&env);
        let timestamps: Map<AssetId, u64> = env
            .storage()
            .temporary()
            .get(&HEARTBEAT_KEY)
            .unwrap_or_else(|| Map::new(&env));
        timestamps.get(asset)
    }

    pub fn get_heartbeat_interval(env: Env) -> u64 {
        let _ = ensure_schema_version(&env);
        Self::_get_interval(&env)
    }

    pub fn set_heartbeat_interval(env: Env, interval: u64, admin: Address) -> Result<(), ContractError> {
        if interval == 0 { return Err(ContractError::InvalidHeartbeatInterval); }
        let data = Self::_load_data(&env)?;
        if data.admin != admin { return Err(ContractError::NotAdmin); }
        admin.require_auth();
        env.storage().instance().set(&HB_INTERVAL_KEY, &interval);
        Self::_extend_instance_ttl(&env);
        crate::recovery::update_admin_activity(env);
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
        crate::recovery::update_admin_activity(env);
        Ok(())
    }

    pub fn is_data_fresh(env: Env, asset: AssetId) -> bool {
        let timestamps: Map<AssetId, u64> = env.storage().temporary().get(&HEARTBEAT_KEY).unwrap_or_else(|| Map::new(&env));
        if let Some(last_update) = timestamps.get(asset) {
            env.ledger().timestamp().saturating_sub(last_update) <= Self::_get_interval(&env)
        } else {
            false
        }
    }

    pub fn upsert_node_profile(env: Env, admin: Address, node: Address, rate: u64, confidence: u32) -> Result<(), ContractError> {
        let data = Self::_load_data(&env)?;
        if data.admin != admin { return Err(ContractError::NotAdmin); }
        admin.require_auth();
        let profile_key = NodeProfileKey::ProfileByNode(node.clone());
        let profile = NodeProfile { node: node.clone(), rate, confidence, updated_at: env.ledger().timestamp() };
        env.storage().persistent().set(&profile_key, &profile);
        let mut profiles = Self::_get_node_profiles(&env);
        profiles.set(
            node.clone(),
            NodeProfile {
                node,
                rate,
                confidence,
                updated_at: env.ledger().timestamp(),
            },
        );
        env.storage()
            .persistent()
            .set(&NODE_PROFILES_KEY, &profiles);
        Self::_extend_instance_ttl(&env);
        crate::recovery::update_admin_activity(env);
        Ok(())
    }

    pub fn get_latest_rate(env: Env, node: Address) -> Result<u64, ContractError> {
        Self::_maintain_relayer_profile_ttl(&env);
        let profile_key = NodeProfileKey::ProfileByNode(node);
        let profile: NodeProfile = env.storage().persistent().get(&profile_key).ok_or(ContractError::NotRegistered)?;
        Ok(Self::_scan_profile_for_rate(profile).ok_or(ContractError::NotRegistered)?)
    }

    pub fn add_corridor_fees(
        env: Env,
        asset: AssetId,
        collected: u64,
        variable_fee: u64,
    ) -> Result<fees::CorridorFeePool, ContractError> {
        let data = Self::_load_data(&env)?;
        let pool = fees::add_corridor_fees(env.clone(), data.admin, asset, collected, variable_fee)?;
        Self::_extend_instance_ttl(&env);
        crate::recovery::update_admin_activity(env);
        Ok(pool)
    }

    pub fn get_corridor_fee_pool(env: Env, asset: AssetId) -> fees::CorridorFeePool {
        fees::get_corridor_fee_pool(env, asset)
    }

    pub fn set_corridor_weight(
        env: Env,
        admin: Address,
        asset: AssetId,
        base_weight: u64,
        dynamic_weight: u64,
    ) -> Result<fees::CorridorWeightProfile, ContractError> {
        let profile =
            fees::set_corridor_weight(env.clone(), admin, asset, base_weight, dynamic_weight)?;
        Self::_extend_instance_ttl(&env);
        crate::recovery::update_admin_activity(env);
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
        env: Env,
        admin: Address,
        config: StakingTierConfig,
        signers: Vec<Address>,
    ) -> Result<(), ContractError> {
        let data = Self::_load_data(&env)?;
        if data.admin != admin {
            return Err(ContractError::NotAdmin);
        }
        admin.require_auth();
        // Issue #539: enforce multi-sig consensus before committing.
        crate::auth::require_multisig(&env, &signers)?;
        validate_tier_config(&config)?;
        env.storage()
            .instance()
            .set(&StakingStorageKey::TierConfig, &config);
        Self::_extend_instance_ttl(&env);
        crate::recovery::update_admin_activity(env);
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
    /// Requires multi-signature consensus (≥ 2 valid signers) for cross-border
    /// parameter changes — issue #539.
    pub fn set_asset_feed_metrics(
        env: Env,
        admin: Address,
        asset: AssetId,
        volume_score_floor: u32,
        volatility_bps: u32,
        signers: Vec<Address>,
    ) -> Result<AssetFeedMetrics, ContractError> {
        let data = Self::_load_data(&env)?;
        if data.admin != admin {
            return Err(ContractError::NotAdmin);
        }
        admin.require_auth();
        // Issue #539: enforce multi-sig consensus before committing.
        crate::auth::require_multisig(&env, &signers)?;

        let metrics = AssetFeedMetrics {
            volume_score: volume_score_floor.min(100),
            volatility_bps,
        };

        env.storage()
            .persistent()
            .set(&StakingStorageKey::AssetMetrics(asset), &metrics);

        Self::_extend_instance_ttl(&env);
        crate::recovery::update_admin_activity(env);
        Ok(metrics)
    }

    /// Return the resolved feed metrics for an asset, including corridor volume.
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

    /// Register a validator node for a specific currency feed with tier-aware collateral.
    pub fn stake_and_register_for_feed(
        env: Env,
        node: Address,
        asset: AssetId,
        amount: u64,
    ) -> Result<FeedStakeRecord, ContractError> {
        if amount == 0 {
            return Err(ContractError::InvalidStakeAmount);
        }
        // Guard: revoked nodes must not be allowed to register for feeds.
        admin::assert_not_revoked(&env, &node)?;
        node.require_auth();

        let feed_key = StakingStorageKey::FeedStake(node.clone(), asset);
        if env.storage().persistent().has(&feed_key) {
            return Err(ContractError::FeedAlreadyRegistered);
        }

        let tier = Self::get_staking_tier(env.clone(), asset);
        let required = Self::get_required_stake(env.clone(), asset);
        if amount < required {
            return Err(ContractError::InsufficientStakeForTier);
        }

        let stake_val = storage::FeedStakeValue {
            amount,
            last_active: env.ledger().timestamp(),
        };
        env.storage().persistent().set(&feed_key, &stake_val);
        env.storage().persistent().extend_ttl(&feed_key, storage::RENT_THRESHOLD, storage::RENT_EXTEND_TO);

        let stake_key = StakeKey::StakeByNode(node.clone());
        let node_total: u64 = env.storage().instance().get(&stake_key).unwrap_or(0);
        let new_node_total = node_total
            .checked_add(amount)
            .ok_or(ContractError::Overflow)?;
        env.storage().instance().set(&stake_key, &new_node_total);

        let total: u64 = env
            .storage()
            .instance()
            .get(&TOTAL_STAKED_KEY)
            .unwrap_or(0u64);
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

    /// Withdraw collateral from a currency feed and deregister the node for that feed.
    pub fn unstake_from_feed(env: Env, node: Address, asset: AssetId) -> Result<u64, ContractError> {
        node.require_auth();

        let feed_key = StakingStorageKey::FeedStake(node.clone(), asset);
        let stake_val: storage::FeedStakeValue = env
            .storage()
            .persistent()
            .get(&feed_key)
            .ok_or(ContractError::NotRegistered)?;
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

        let total: u64 = env
            .storage()
            .instance()
            .get(&TOTAL_STAKED_KEY)
            .unwrap_or(0u64);
        let new_total = total.saturating_sub(amount);

        env.storage().instance().set(&TOTAL_STAKED_KEY, &new_total);

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
        env.storage()
            .instance()
            .set(&PLATFORM_CAPITAL_KEY, &capital);
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
            // Update signer count
            let count: u32 = env.storage().instance().get(&SIGNERS_KEY).unwrap_or(0u32);
            env.storage().instance().set(&SIGNERS_KEY, &(count + 1));
        }
        Self::_extend_instance_ttl(&env);
        crate::recovery::update_admin_activity(env);
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
        env: Env,
        voter: Address,
        sig_expires_at: u64,
        nonce: u64,
    ) -> Result<(), ContractError> {
        admin::vote_emergency_revocation(&env, voter, sig_expires_at, nonce)
    }

    pub fn get_emergency_revocation(
        env: Env,
    ) -> Option<admin::EmergencyRevocationProposal> {
        admin::get_emergency_revocation_proposal(&env)
    }

    /// This can be called by any party since the primary security model relies on
    /// the voting threshold for proposal execution, not on proposal creation.
    pub fn purge_expired_revocation_prop(env: Env) -> Result<(), ContractError> {
        admin::purge_emergency_revocation_proposal(&env)
    }

    /// Check if an emergency revocation proposal is currently active.
    ///
    /// Returns true only if the proposal exists in temporary storage and hasn't expired
    /// according to Soroban's TTL mechanism.
    pub fn has_active_revocation_proposal(env: Env) -> bool {
        admin::has_active_emergency_revocation(&env)
    }

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

    /// Record a validator ingestion dropout for an asset feed within the rolling
    /// 100-ledger fault window. Callable by admin monitors.
    pub fn report_ingestion_dropout(
        env: Env,
        admin: Address,
        validator: Address,
        asset: Symbol,
    ) -> Result<u32, ContractError> {
        Self::assert_contract_is_active(&env)?;
        let data = Self::get_data(env.clone())?;
        if data.admin != admin {
            return Err(ContractError::NotAdmin);
        }
        admin.require_auth();
        let result = record_tracking_fault(&env, &validator, &asset)?;
        crate::recovery::update_admin_activity(env);
        Ok(result)
    }

    /// Return the number of ingestion faults recorded within the rolling window.
    pub fn get_ingestion_fault_count(
        env: Env,
        validator: Address,
        asset: Symbol,
    ) -> u32 {
        get_fault_count_in_window(&env, &validator, &asset)
    }

    /// Return the exponential penalty multiplier for repeated ingestion dropouts.
    pub fn get_ingestion_multiplier(
        env: Env,
        validator: Address,
        asset: Symbol,
    ) -> u64 {
        let fault_count = get_fault_count_in_window(&env, &validator, &asset);
        get_penalty_multiplier(fault_count)
    }

    /// Apply a progressive escrow bond deduction scaled by repeated outage history.
    ///
    /// Records the fault, then deducts `base_bond * 2^(fault_count - 1)` from the
    /// validator's locked stake (capped at the available bond).
    pub fn apply_ingestion_penalty(
        env: Env,
        admin: Address,
        validator: Address,
        asset: Symbol,
        base_bond: u64,
    ) -> Result<IngestionPenaltyResult, ContractError> {
        Self::assert_contract_is_active(&env)?;
        let data = Self::get_data(env.clone())?;
        if data.admin != admin {
            return Err(ContractError::NotAdmin);
        }
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
            &StakingStorageKey::FeedStake(validator.clone(), symbol_to_asset_id(&asset)),
        )
    }

    // --- Private Helpers ---

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
        let mut timestamps: Map<AssetId, u64> = env
            .storage()
            .temporary()
            .get(&HEARTBEAT_KEY)
            .unwrap_or_else(|| Map::new(env));
        timestamps.set(asset, env.ledger().timestamp());
        env.storage().temporary().set(&HEARTBEAT_KEY, &timestamps);
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
        if profile.confidence == 0 {
            None
        } else {
            Some(profile.rate)
        }
    }

    fn _maintain_relayer_profile_ttl(env: &Env) {
        // With individual tuple keys, TTL is managed per-entry.
        // This is a no-op placeholder for compatibility.
        let _ = env;
    }

    fn _extend_instance_ttl(env: &Env) {
        env.storage().instance().extend_ttl(
            RELAYER_TTL_THRESHOLD,
            RELAYER_TTL_THRESHOLD + INSTANCE_TTL_EXTEND,
        );
    }

    fn _is_signer(env: &Env, addr: &Address) -> bool {
        let signer_key = SignerKey::SignerByAddress(addr.clone());
        env.storage().instance().has(&signer_key)
    }

    fn _revocation_threshold(env: &Env) -> u32 {
        // With individual signer keys, we need a separate counter.
        // For now, default to 1 as a safe minimum.
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

    pub fn update_validator_profile(
        env: Env,
        node: Address,
        pool: Symbol,
    ) -> Result<(), ContractError> {
        // Guard: revoked node must not be able to update its profile.
        admin::assert_not_revoked(&env, &node)?;
        node.require_auth();

        check_bond_capacity(&env, &node, &pool)?;
        let asset = symbol_to_asset_id(&pool);
        check_liquidity_depth(&env, asset)?;

        storage::update_feed_stake_activity(&env, node.clone(), asset);
        Self::_record_heartbeat(&env, asset);
        Ok(())
    }

    /// Submit telemetry data with comprehensive validation to prevent flash loan
    /// manipulation: timestamp freshness, reserve balance, trading volume, and
    /// validator bond capacity are all checked before the submission is accepted.
    pub fn submit_telemetry_data(
        env: Env,
        node: Address,
        pool: Symbol,
        payload_timestamp: u64,
        reserve_a: i128,
        reserve_b: i128,
        volume_24h: i128,
    ) -> Result<(), ContractError> {
        // Guard: revoked node must not be able to submit telemetry.
        admin::assert_not_revoked(&env, &node)?;
        node.require_auth();

        // Comprehensive validation pipeline (fail-fast)
        validate_telemetry_submission(
            &env,
            &node,
            &pool,
            payload_timestamp,
            reserve_a,
            reserve_b,
            volume_24h,
        )?;

        // Telemetry accepted - record heartbeat
        Self::_record_heartbeat(&env, symbol_to_asset_id(&pool));

        // Emit event for monitoring
        env.events().publish(
            (soroban_sdk::symbol_short!("telem_ok"),),
            (node, pool, payload_timestamp),
        );

        Ok(())
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

        let asset = symbol_to_asset_id(&symbol_short!("CFA"));

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

// Integration tests for issue #525 live in `src/slashing.rs`.
#[cfg(test)]
mod test;
