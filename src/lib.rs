#![no_std]
use soroban_sdk::{contract, contracterror, contractimpl, contracttype, symbol_short, Address, Bytes, BytesN, Env, Map, Symbol};

pub(crate) mod nonce;
use crate::nonce::{consume_nonce, get_nonce};

pub mod consensus;
pub mod staking_tiers;
pub mod validation;

pub use staking_tiers::{AssetFeedMetrics, StakingTier, StakingTierConfig};
use staking_tiers::{
    assign_tier, effective_volume_score, required_stake_for_tier, validate_tier_config,
};

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
    /// Median input array is empty (count == 0).
    EmptyMedianInput = 23,
    /// Validator does not have sufficient locked stake for premium pool access.
    PremiumPoolAccessDenied = 24,
}

// Contract state keys
const DATA_KEY: Symbol = symbol_short!("DATA");
const PENDING_UPGRADE_KEY: Symbol = symbol_short!("PENDING");
const UPGRADE_DELAY_SECONDS: u64 = 48 * 60 * 60; 
const STAKE_REGISTRY_KEY: Symbol = symbol_short!("STAKES");
const TOTAL_STAKED_KEY: Symbol = symbol_short!("TOTAL");
const HEARTBEAT_KEY: Symbol = symbol_short!("HBEAT");
const HB_INTERVAL_KEY: Symbol = symbol_short!("HBINTV");
const DEFAULT_HEARTBEAT_INTERVAL: u64 = 5 * 60;
const SIGNERS_KEY: Symbol = symbol_short!("SIGNERS");
const REVOCATION_KEY: Symbol = symbol_short!("REVOKE");
const NODE_PROFILES_KEY: Symbol = symbol_short!("NODES");
const PLATFORM_CAPITAL_KEY: Symbol = symbol_short!("CAPITAL");
const CONSENSUS_CACHE_KEY: Symbol = symbol_short!("CACHE");
const RELAYER_TTL_THRESHOLD: u32 = 5_000;

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
#[derive(Clone)]
pub struct PendingUpgrade {
    pub new_wasm_hash: BytesN<32>,
    pub proposed_at: u64,
    pub proposer: Address,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ContractData {
    pub admin: Address,
    pub value: u64,
}

#[contracttype]
#[derive(Clone)]
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

#[contract]
pub struct TimeLockedUpgradeContract;

#[contractimpl]
impl TimeLockedUpgradeContract {
    pub fn initialize(env: Env, admin: Address) -> Result<(), ContractError> {
        if env.storage().instance().has(&DATA_KEY) {
            return Err(ContractError::AlreadyInitialized);
        }
        admin.require_auth();
        let data = ContractData { admin: admin.clone(), value: 0 };
        env.storage().instance().set(&DATA_KEY, &data);
        Ok(())
    }

    pub fn stake_and_register(env: Env, node: Address, amount: u64) -> Result<StakeRecord, ContractError> {
        if amount == 0 { return Err(ContractError::InvalidStakeAmount); }
        node.require_auth();
        let mut stakes: Map<Address, u64> = env.storage().instance().get(&STAKE_REGISTRY_KEY).unwrap_or_else(|| Map::new(&env));
        if stakes.contains_key(node.clone()) { return Err(ContractError::AlreadyRegistered); }
        let total: u64 = env.storage().instance().get(&TOTAL_STAKED_KEY).unwrap_or(0u64);
        let new_total = total.checked_add(amount).ok_or(ContractError::Overflow)?;
        stakes.set(node.clone(), amount);
        env.storage().instance().set(&STAKE_REGISTRY_KEY, &stakes);
        env.storage().instance().set(&TOTAL_STAKED_KEY, &new_total);
        Self::_record_heartbeat(&env, symbol_short!("STAKE"));
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

    pub fn remove_signer(env: Env, signer: Address, caller: Address) -> Result<(), ContractError> {
        Self::assert_contract_is_active(&env)?;
        let data = Self::get_data(&env)?;
        if data.admin != caller { return Err(ContractError::NotAdmin); }
        caller.require_auth();

        let mut signers = Self::_get_signers(&env);
        
        // Refactored for Issue #370: Zero-Allocation removal with Map
        signers.remove(signer);
        env.storage().instance().set(&SIGNERS_KEY, &signers);
        Ok(())
    }

    pub fn vote_revocation(env: Env, voter: Address, sig_expires_at: u64) -> Result<(), ContractError> {
        if env.ledger().timestamp() > sig_expires_at { return Err(ContractError::SignatureExpired); }
        voter.require_auth();
        let data = Self::get_data(&env)?;

        if !Self::_is_signer(&env, &voter) && data.admin != voter {
            return Err(ContractError::Unauthorized);
        }

        let mut proposal: RevocationProposal = env.storage().instance().get(&REVOCATION_KEY).ok_or(ContractError::NoActiveProposal)?;

        // Refactored for Issue #370: Use map operations for zero-allocation
        if proposal.votes.contains_key(voter.clone()) {
            return Err(ContractError::AlreadyVoted);
        }

        proposal.votes.set(voter, ()); // Use set for Map

        // Optimized verification scan
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

    // --- Core Logic Boilerplate ---

    pub fn get_data(env: &Env) -> Result<ContractData, ContractError> {
        env.storage().instance().get(&DATA_KEY).ok_or(ContractError::NotInitialized)
    }

    pub fn propose_upgrade(env: Env, new_wasm_hash: BytesN<32>, proposer: Address, nonce: u64, salt: Bytes, salt_signature: BytesN<32>, sig_expires_at: u64) -> Result<(), ContractError> {
        if env.ledger().timestamp() > sig_expires_at { return Err(ContractError::SignatureExpired); }
        let data = Self::get_data(&env)?;
        if data.admin != proposer { return Err(ContractError::NotAdmin); }
        proposer.require_auth();
        consume_nonce(&env, &proposer, nonce, salt, salt_signature);
        let pending = PendingUpgrade { new_wasm_hash, proposed_at: env.ledger().timestamp(), proposer };
        env.storage().instance().set(&PENDING_UPGRADE_KEY, &pending);
        Ok(())
    }

    pub fn execute_upgrade(env: Env, executor: Address, nonce: u64, salt: Bytes, signature: BytesN<32>, sig_expires_at: u64) -> Result<(), ContractError> {
        if env.ledger().timestamp() > sig_expires_at { return Err(ContractError::SignatureExpired); }
        let data = Self::get_data(&env)?;
        if data.admin != executor { return Err(ContractError::NotAdmin); }
        executor.require_auth();
        consume_nonce(&env, &executor, nonce, salt, signature)?;
        let pending: PendingUpgrade = env.storage().instance().get(&PENDING_UPGRADE_KEY).ok_or(ContractError::NoPendingUpgrade)?;
        if env.ledger().timestamp().saturating_sub(pending.proposed_at) < UPGRADE_DELAY_SECONDS {
            return Err(ContractError::UpgradeTimelockNotSatisfied);
        }
        env.deployer().update_current_contract_wasm(pending.new_wasm_hash);
        env.storage().instance().remove(&PENDING_UPGRADE_KEY);
        Ok(())
    }

    pub fn get_pending_upgrade(env: Env) -> Option<PendingUpgrade> {
        env.storage().instance().get(&PENDING_UPGRADE_KEY)
    }

    pub fn get_upgrade_timelock_remaining(env: Env) -> Option<u64> {
        env.storage().instance().get(&PENDING_UPGRADE_KEY).map(|pending: PendingUpgrade| {
            let elapsed = env.ledger().timestamp().saturating_sub(pending.proposed_at);
            UPGRADE_DELAY_SECONDS.saturating_sub(elapsed)
        })
    }

    pub fn cancel_upgrade(env: Env, canceller: Address) -> Result<(), ContractError> {
        let data = Self::get_data(&env)?;
        if data.admin != canceller { return Err(ContractError::NotAdmin); }
        canceller.require_auth();
        env.storage().instance().remove(&PENDING_UPGRADE_KEY);
        Ok(())
    }

    pub fn set_value(env: Env, new_value: u64, caller: Address, nonce: u64, salt: Bytes, signature: BytesN<32>, sig_expires_at: u64) -> Result<(), ContractError> {
        if env.ledger().timestamp() > sig_expires_at { return Err(ContractError::SignatureExpired); }
        let mut data = Self::get_data(&env)?;
        if data.admin != caller { return Err(ContractError::NotAdmin); }
        caller.require_auth();
        consume_nonce(&env, &caller, nonce, salt, signature)?;
        data.value = new_value;
        env.storage().instance().set(&DATA_KEY, &data);
        Self::_record_heartbeat(&env, symbol_short!("VALUE"));
        Ok(())
    }

    pub fn get_coordinator_nonce(env: Env, coordinator: Address) -> u64 {
        get_nonce(&env, &coordinator)
    }

    pub fn get_last_update_timestamp(env: Env, asset: Symbol) -> Option<u64> {
        let timestamps: Map<Symbol, u64> = env.storage().temporary().get(&HEARTBEAT_KEY).unwrap_or_else(|| Map::new(&env));
        timestamps.get(asset)
    }

    pub fn get_heartbeat_interval(env: Env) -> u64 {
        Self::_get_interval(&env)
    }

    pub fn set_heartbeat_interval(env: Env, interval: u64, admin: Address) -> Result<(), ContractError> {
        if interval == 0 { return Err(ContractError::InvalidHeartbeatInterval); }
        let data = Self::get_data(&env)?;
        if data.admin != admin { return Err(ContractError::NotAdmin); }
        admin.require_auth();
        env.storage().instance().set(&HB_INTERVAL_KEY, &interval);
        Ok(())
    }

    pub fn get_stake(env: Env, node: Address) -> u64 {
        let stakes: Map<Address, u64> = env.storage().instance().get(&STAKE_REGISTRY_KEY).unwrap_or_else(|| Map::new(&env));
        stakes.get(node).unwrap_or(0u64)
    }

    pub fn get_total_staked(env: Env) -> u64 {
        env.storage().instance().get(&TOTAL_STAKED_KEY).unwrap_or(0u64)
    }

    pub fn update_heartbeat(env: Env, asset: Symbol, updater: Address) -> Result<(), ContractError> {
        let data = Self::get_data(&env)?;
        if data.admin != updater { return Err(ContractError::NotAdmin); }
        updater.require_auth();
        Self::_record_heartbeat(&env, asset);
        Ok(())
    }

    pub fn is_data_fresh(env: &Env, asset: Symbol) -> bool {
        let timestamps: Map<Symbol, u64> = env.storage().temporary().get(&HEARTBEAT_KEY).unwrap_or_else(|| Map::new(env));
        if let Some(last_update) = timestamps.get(asset) {
            env.ledger().timestamp().saturating_sub(last_update) <= Self::_get_interval(env)
        } else { false }
    }

    pub fn upsert_node_profile(env: Env, admin: Address, node: Address, rate: u64, confidence: u32) -> Result<(), ContractError> {
        let data = Self::get_data(&env)?;
        if data.admin != admin { return Err(ContractError::NotAdmin); }
        admin.require_auth();
        let mut profiles = Self::_get_node_profiles(&env);
        profiles.set(node.clone(), NodeProfile { node, rate, confidence, updated_at: env.ledger().timestamp() });
        env.storage().persistent().set(&NODE_PROFILES_KEY, &profiles);
        Ok(())
    }

    pub fn get_latest_rate(env: Env, node: Address) -> Result<u64, ContractError> {
        Self::_maintain_relayer_profile_ttl(&env);
        let profiles = Self::_get_node_profiles(&env);
        let profile = profiles.get(node).ok_or(ContractError::NotRegistered)?;
        Ok(Self::_scan_profile_for_rate(profile).ok_or(ContractError::NotRegistered)?)
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
        let data = Self::get_data(&env)?;
        if data.admin != admin {
            return Err(ContractError::NotAdmin);
        }
        admin.require_auth();
        validate_tier_config(&config)?;
        env.storage()
            .instance()
            .set(&StakingStorageKey::TierConfig, &config);
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
    ///
    /// The effective volume score is the greater of the admin floor and the
    /// on-chain corridor activity derived score.
    pub fn set_asset_feed_metrics(
        env: Env,
        admin: Address,
        asset: Symbol,
        volume_score_floor: u32,
        volatility_bps: u32,
    ) -> Result<AssetFeedMetrics, ContractError> {
        let data = Self::get_data(&env)?;
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
        Self::_record_heartbeat(&env, asset.clone());

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
        let data = Self::get_data(&env)?;
        if data.admin != caller { return Err(ContractError::NotAdmin); }
        caller.require_auth();
        let mut signers = Self::_get_signers(&env);
        if !signers.contains_key(signer.clone()) {
            signers.set(signer, ());
            env.storage().instance().set(&SIGNERS_KEY, &signers);
        }
        Ok(())
    }

    // --- Private Helpers ---

    fn assert_contract_is_active(env: &Env) -> Result<(), ContractError> {
        if !env.storage().instance().has(&DATA_KEY) {
            return Err(ContractError::NotInitialized);
        }
        Ok(())
    }

    fn _record_heartbeat(env: &Env, asset: Symbol) {
        let mut timestamps: Map<Symbol, u64> = env.storage().temporary().get(&HEARTBEAT_KEY).unwrap_or_else(|| Map::new(env));
        timestamps.set(asset, env.ledger().timestamp());
        env.storage().temporary().set(&HEARTBEAT_KEY, &timestamps);
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

    fn _is_signer(env: &Env, addr: &Address) -> bool {
        Self::_get_signers(env).contains_key(addr.clone())
    }

    fn _revocation_threshold(env: &Env) -> u32 {
        let n = Self::_get_signers(env).len();
        n / 2 + 1
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

    // get_data returns NotInitialized before init and correct data after.
    #[test]
    fn test_get_data_before_and_after_init() {
        let (env, client) = setup();
        let admin = Address::generate(&env);

        let result = client.try_get_data();
        assert!(matches!(result, Err(Ok(ContractError::NotInitialized))));

        client.initialize(&admin);

        let data = client.get_data();
        assert_eq!(data.admin, admin);
        assert_eq!(data.value, 0);
    }

    // Calling get_data repeatedly returns the same result without mutating state.
    #[test]
    fn test_get_data_is_idempotent() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.initialize(&admin);

        let first_admin = client.get_data().admin;
        let first_value = client.get_data().value;
        let second_admin = client.get_data().admin;
        let second_value = client.get_data().value;

        assert_eq!(first_admin, second_admin);
        assert_eq!(first_value, second_value);
        assert_eq!(first_value, 0);
    }

    // is_data_fresh returns false for an asset that was never written.
    #[test]
    fn test_is_data_fresh_unknown_asset_returns_false() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.initialize(&admin);

        let asset = symbol_short!("NGN");
        assert!(!client.is_data_fresh(&asset));
    }

    // is_data_fresh returns true right after a heartbeat and false once stale.
    #[test]
    fn test_is_data_fresh_transitions_on_staleness() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.initialize(&admin);

        let asset = symbol_short!("KES");
        client.update_heartbeat(&asset, &admin);

        assert!(client.is_data_fresh(&asset));

        advance(&env, DEFAULT_HEARTBEAT_INTERVAL + 1);
        assert!(!client.is_data_fresh(&asset));
    }

    // Calling is_data_fresh multiple times never alters the heartbeat storage slot.
    #[test]
    fn test_is_data_fresh_does_not_mutate_heartbeat() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.initialize(&admin);

        let asset = symbol_short!("GHS");
        client.update_heartbeat(&asset, &admin);

        for _ in 0..5 {
            assert!(client.is_data_fresh(&asset));
        }

        // Advance time fully: data should go stale, confirming heartbeat was not refreshed.
        advance(&env, DEFAULT_HEARTBEAT_INTERVAL + 1);
        assert!(!client.is_data_fresh(&asset));
    }

    // get_data and is_data_fresh are independent: one does not affect the other.
    #[test]
    fn test_query_methods_do_not_interfere() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.initialize(&admin);

        let asset = symbol_short!("CFA");

        let admin_before = client.get_data().admin;
        let value_before = client.get_data().value;

        let _ = client.is_data_fresh(&asset);

        let admin_after = client.get_data().admin;
        let value_after = client.get_data().value;

        assert_eq!(admin_before, admin_after);
        assert_eq!(value_before, value_after);
    }
}

#[cfg(test)]
mod test;
