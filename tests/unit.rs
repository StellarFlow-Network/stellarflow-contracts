use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger, LedgerInfo},
    Address, Bytes, BytesN, Env, Symbol, Vec,
};

/// The main contract from the root crate.
use stellarflow_contracts::{
    ContractError, TimeLockedUpgradeContract, TimeLockedUpgradeContractClient,
    DEFAULT_HEARTBEAT_INTERVAL, PriceVarianceConfig, StakingTierConfig,
};

// ── Helpers ──────────────────────────────────────────────────────────────────

fn setup_env() -> (Env, TimeLockedUpgradeContractClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury);
    (env, client, admin)
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

fn nonce_proof(env: &Env, nonce: u64, salt_seed: &[u8]) -> (Bytes, BytesN<32>) {
    let salt = Bytes::from_slice(env, salt_seed);
    let signature = stellarflow_contracts::nonce::derive_salt_signature(env, nonce, salt.clone());
    (salt, signature)
}

// ═════════════════════════════════════════════════════════════════════════════
// Governance Execution Paths
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn test_governance_initialize_and_get_data() {
    let (env, client, admin) = setup_env();
    let data = client.get_data();
    assert_eq!(data.admin, admin);
    assert_eq!(data.value, 0u64);
}

#[test]
fn test_governance_double_initialize_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    assert!(client.try_initialize(&admin, &treasury).is_ok());
    let result = client.try_initialize(&admin, &treasury);
    assert_eq!(result, Err(Ok(ContractError::AlreadyInitialized)));
}

#[test]
fn test_governance_set_value_with_nonce() {
    let (env, client, admin) = setup_env();
    let (salt, sig) = nonce_proof(&env, 0, b"set-value-test");
    client.set_value(&42, &admin, &0, &salt, &sig, &u64::MAX);
    assert_eq!(client.get_data().value, 42);
}

#[test]
fn test_governance_set_value_wrong_nonce_rejected() {
    let (env, client, admin) = setup_env();
    let (salt, sig) = nonce_proof(&env, 0, b"set-value-test");
    let result = client.try_set_value(&42, &admin, &1, &salt, &sig, &u64::MAX);
    assert_eq!(result, Err(Ok(ContractError::InvalidNonce)));
}

#[test]
fn test_governance_register_signer() {
    let (env, client, admin) = setup_env();
    let signer = Address::generate(&env);
    client.register_signer(&signer, &admin);
}

#[test]
fn test_governance_remove_signer() {
    let (env, client, admin) = setup_env();
    let signer = Address::generate(&env);
    client.register_signer(&signer, &admin);
    client.remove_signer(&signer, &admin);
}

#[test]
fn test_governance_propose_ownership_transfer() {
    let (env, client, admin) = setup_env();
    let nominee = Address::generate(&env);
    client.propose_ownership_transfer(&admin, &nominee, &0);
}

#[test]
fn test_governance_propose_admin_change() {
    let (env, client, admin) = setup_env();
    let new_admin = Address::generate(&env);
    client.propose_admin_change(&admin, &new_admin);
}

#[test]
fn test_governance_empty_stake_returns_zero() {
    let (env, client, admin) = setup_env();
    let random = Address::generate(&env);
    let stake = client.get_stake(&random);
    assert_eq!(stake, 0);
}

// ═════════════════════════════════════════════════════════════════════════════
// Deposit / Staking Execution Paths
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn test_deposit_stake_and_register() {
    let (env, client, admin) = setup_env();
    let node = Address::generate(&env);
    let record = client.stake_and_register(&node, &1000);
    assert_eq!(record.amount, 1000);
    assert_eq!(record.node, node);
}

#[test]
fn test_deposit_stake_twice_rejected() {
    let (env, client, admin) = setup_env();
    let node = Address::generate(&env);
    client.stake_and_register(&node, &1000);
    let result = client.try_stake_and_register(&node, &500);
    assert_eq!(result, Err(Ok(ContractError::AlreadyRegistered)));
}

#[test]
fn test_deposit_stake_zero_rejected() {
    let (env, client, admin) = setup_env();
    let node = Address::generate(&env);
    let result = client.try_stake_and_register(&node, &0);
    assert_eq!(result, Err(Ok(ContractError::InvalidStakeAmount)));
}

#[test]
fn test_deposit_stake_and_unstake() {
    let (env, client, admin) = setup_env();
    let node = Address::generate(&env);
    client.stake_and_register(&node, &1000);
    let withdrawn = client.unstake(&node);
    assert_eq!(withdrawn, 1000);
    assert_eq!(client.get_stake(&node), 0);
}

#[test]
fn test_deposit_stake_updates_total() {
    let (env, client, admin) = setup_env();
    let node_a = Address::generate(&env);
    let node_b = Address::generate(&env);
    client.stake_and_register(&node_a, &2000);
    assert_eq!(client.get_total_staked(), 2000);
    client.stake_and_register(&node_b, &3000);
    assert_eq!(client.get_total_staked(), 5000);
    client.unstake(&node_a);
    assert_eq!(client.get_total_staked(), 3000);
}

#[test]
fn test_deposit_unstake_unregistered_rejected() {
    let (env, client, admin) = setup_env();
    let node = Address::generate(&env);
    let result = client.try_unstake(&node);
    assert_eq!(result, Err(Ok(ContractError::NotRegistered)));
}

#[test]
fn test_deposit_stake_and_register_for_feed() {
    let (env, client, admin) = setup_env();
    let node = Address::generate(&env);
    let asset = stellarflow_contracts::symbol_to_asset_id(&symbol_short!("NGN"));
    let feed_record = client.stake_and_register_for_feed(&node, &asset, &2000);
    assert_eq!(feed_record.amount, 2000);
    assert_eq!(feed_record.node, node);
    assert_eq!(feed_record.asset, asset);
}

#[test]
fn test_deposit_stake_for_feed_zero_rejected() {
    let (env, client, admin) = setup_env();
    let node = Address::generate(&env);
    let asset = stellarflow_contracts::symbol_to_asset_id(&symbol_short!("NGN"));
    let result = client.try_stake_and_register_for_feed(&node, &asset, &0);
    assert_eq!(result, Err(Ok(ContractError::InvalidStakeAmount)));
}

#[test]
fn test_deposit_feed_stake_and_unstake() {
    let (env, client, admin) = setup_env();
    let node = Address::generate(&env);
    let asset = stellarflow_contracts::symbol_to_asset_id(&symbol_short!("NGN"));
    client.stake_and_register_for_feed(&node, &asset, &5000);
    let withdrawn = client.unstake_from_feed(&node, &asset);
    assert_eq!(withdrawn, 5000);
    assert_eq!(client.get_feed_stake(&node, &asset), 0);
}

#[test]
fn test_deposit_set_and_get_staking_tier_config() {
    let (env, client, admin) = setup_env();
    let config = StakingTierConfig {
        tier1_min: 100,
        tier2_min: 500,
        tier3_min: 2000,
        tier4_min: 10000,
    };
    let signers: Vec<Address> = Vec::new(&env);
    assert!(client.try_set_staking_tier_config(&admin, &config, &signers).is_err());
}

// ═════════════════════════════════════════════════════════════════════════════
// Swap / Fee Execution Paths
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn test_swap_add_and_get_corridor_fees() {
    let (env, client, admin) = setup_env();
    let asset = stellarflow_contracts::symbol_to_asset_id(&symbol_short!("NGN"));
    client.add_corridor_fees(&admin, &asset, &5000, &500);
    let pool = client.get_corridor_fee_pool(&asset);
    assert_eq!(pool.asset, asset);
    assert_eq!(pool.collected, 5000);
}

#[test]
fn test_swap_add_corridor_fees_accumulates() {
    let (env, client, admin) = setup_env();
    let asset = stellarflow_contracts::symbol_to_asset_id(&symbol_short!("KES"));
    client.add_corridor_fees(&admin, &asset, &1000, &100);
    client.add_corridor_fees(&admin, &asset, &2000, &200);
    let pool = client.get_corridor_fee_pool(&asset);
    assert_eq!(pool.collected, 3000);
}

#[test]
fn test_swap_set_and_get_corridor_weight() {
    let (env, client, admin) = setup_env();
    let asset = stellarflow_contracts::symbol_to_asset_id(&symbol_short!("GHS"));
    let profile = client.set_corridor_weight(&admin, &asset, &100, &200);
    assert_eq!(profile.asset, asset);
    assert_eq!(profile.base_weight, 100);
    assert_eq!(profile.dynamic_weight, 200);
    let retrieved = client.get_corridor_weight(&asset);
    assert_eq!(retrieved.base_weight, 100);
}

// ═════════════════════════════════════════════════════════════════════════════
// Heartbeat / Telemetry Paths
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn test_heartbeat_set_and_get_interval() {
    let (env, client, admin) = setup_env();
    assert_eq!(client.get_heartbeat_interval(), DEFAULT_HEARTBEAT_INTERVAL);
    client.set_heartbeat_interval(&300, &admin);
    assert_eq!(client.get_heartbeat_interval(), 300);
}

#[test]
fn test_heartbeat_zero_interval_rejected() {
    let (env, client, admin) = setup_env();
    let result = client.try_set_heartbeat_interval(&0, &admin);
    assert_eq!(result, Err(Ok(ContractError::InvalidHeartbeatInterval)));
}

#[test]
fn test_heartbeat_update_and_freshness() {
    let (env, client, admin) = setup_env();
    let asset = stellarflow_contracts::symbol_to_asset_id(&symbol_short!("NGN"));
    client.add_corridor_fees(&admin, &asset, &2_000_000_000, &0u64);
    client.update_heartbeat(&asset, &admin);
    assert!(client.is_data_fresh(&asset));
    advance(&env, DEFAULT_HEARTBEAT_INTERVAL + 1);
    assert!(!client.is_data_fresh(&asset));
}

// ═════════════════════════════════════════════════════════════════════════════
// Price Variance Config Paths
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn test_price_variance_config_default() {
    let (env, client, admin) = setup_env();
    let cfg = client.get_price_variance_config();
    assert_eq!(cfg, PriceVarianceConfig::default());
}

#[test]
fn test_price_variance_config_set_and_get() {
    let (env, client, admin) = setup_env();
    let custom = PriceVarianceConfig {
        max_spread_bps: 150,
        max_deviation_bps: 400,
        min_submission_count: 5,
        max_submission_age_secs: 120,
    };
    client.set_price_variance_config(&admin, &custom);
    let retrieved = client.get_price_variance_config();
    assert_eq!(retrieved, custom);
}

// ═════════════════════════════════════════════════════════════════════════════
// Upgrade Paths
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn test_upgrade_propose_and_get_pending() {
    let (env, client, admin) = setup_env();
    let wasm_hash = BytesN::from_array(&env, &[0u8; 32]);
    let signers = soroban_sdk::vec![&env, admin.clone()];
    client.register_signer(&admin, &admin);
    let (salt, sig) = nonce_proof(&env, 0, b"upgrade-test");
    client.propose_upgrade(&wasm_hash, &admin, &signers, &0, &salt, &sig, &u64::MAX);
    let pending = client.get_pending_upgrade();
    assert!(pending.is_some());
}

#[test]
fn test_upgrade_cancel() {
    let (env, client, admin) = setup_env();
    let wasm_hash = BytesN::from_array(&env, &[0u8; 32]);
    let signers = soroban_sdk::vec![&env, admin.clone()];
    client.register_signer(&admin, &admin);
    let (salt, sig) = nonce_proof(&env, 0, b"upgrade-test");
    client.propose_upgrade(&wasm_hash, &admin, &signers, &0, &salt, &sig, &u64::MAX);
    client.cancel_upgrade(&admin);
    let pending = client.get_pending_upgrade();
    assert!(pending.is_none());
}

#[test]
fn test_upgrade_timelock_remaining() {
    let (env, client, admin) = setup_env();
    let wasm_hash = BytesN::from_array(&env, &[0u8; 32]);
    let signers = soroban_sdk::vec![&env, admin.clone()];
    client.register_signer(&admin, &admin);
    let (salt, sig) = nonce_proof(&env, 0, b"upgrade-test");
    client.propose_upgrade(&wasm_hash, &admin, &signers, &0, &salt, &sig, &u64::MAX);
    let remaining = client.get_upgrade_timelock_remaining();
    assert!(remaining.is_some());
}

// ═════════════════════════════════════════════════════════════════════════════
// Emergency Revocation Paths
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn test_emergency_revocation_propose_and_check() {
    let (env, client, admin) = setup_env();
    let signer = Address::generate(&env);
    client.register_signer(&signer, &admin);
    let replacement = Address::generate(&env);
    client.propose_emergency_revocation(&admin, &signer, &replacement, &0);
    assert!(client.has_active_revocation_proposal());
}

#[test]
fn test_emergency_revocation_get_proposal() {
    let (env, client, admin) = setup_env();
    let signer = Address::generate(&env);
    client.register_signer(&signer, &admin);
    let replacement = Address::generate(&env);
    client.propose_emergency_revocation(&admin, &signer, &replacement, &0);
    let proposal = client.get_emergency_revocation();
    assert!(proposal.is_some());
}

// ═════════════════════════════════════════════════════════════════════════════
// Node Profile Paths
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn test_node_profile_upsert_and_get_rate() {
    let (env, client, admin) = setup_env();
    let node = Address::generate(&env);
    client.upsert_node_profile(&admin, &node, &1000, &80);
    let rate = client.get_latest_rate(&node);
    assert_eq!(rate, Ok(1000));
}

// ═════════════════════════════════════════════════════════════════════════════
// Multi-asset Bundle Processing
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn test_bundle_process_single_asset() {
    let (env, client, admin) = setup_env();
    let node = Address::generate(&env);
    client.stake_and_register(&node, &2000);
    use stellarflow_contracts::validation::AssetPriceUpdate;
    let mut updates: Vec<AssetPriceUpdate> = Vec::new(&env);
    updates.push_back(AssetPriceUpdate {
        asset: stellarflow_contracts::symbol_to_asset_id(&symbol_short!("NGN")),
        price: 100_000,
        timestamp: env.ledger().timestamp() - 30,
    });
    let outcome = client.update_prices_bundle(&node, &updates);
    assert_eq!(outcome.total_assets, 1);
    assert_eq!(outcome.accepted, 1);
}

// ═════════════════════════════════════════════════════════════════════════════
// Slashing / Penalty Paths
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn test_slashing_get_fault_count_empty() {
    let (env, client, admin) = setup_env();
    let validator = Address::generate(&env);
    let asset = symbol_short!("XLM");
    let count = client.get_ingestion_fault_count(&validator, &asset);
    assert_eq!(count, 0);
}

#[test]
fn test_slashing_get_multiplier_zero_faults() {
    let (env, client, admin) = setup_env();
    let validator = Address::generate(&env);
    let asset = symbol_short!("XLM");
    let multiplier = client.get_ingestion_multiplier(&validator, &asset);
    assert_eq!(multiplier, 1);
}

// ═════════════════════════════════════════════════════════════════════════════
// Coordinator Nonce Paths
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn test_nonce_initial_zero() {
    let (env, client, admin) = setup_env();
    let coordinator = Address::generate(&env);
    let nonce = client.get_coordinator_nonce(&coordinator);
    assert_eq!(nonce, 0);
}

#[test]
fn test_nonce_increments_after_use() {
    let (env, client, admin) = setup_env();
    let (salt, sig) = nonce_proof(&env, 0, b"nonce-test");
    client.set_value(&99, &admin, &0, &salt, &sig, &u64::MAX);
    let nonce = client.get_coordinator_nonce(&admin);
    assert_eq!(nonce, 1);
}
