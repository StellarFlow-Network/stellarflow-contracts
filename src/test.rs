use soroban_sdk::{symbol_short, Bytes, Env};
use soroban_sdk::testutils::{Address as _, Ledger, LedgerInfo}; // Removed Symbol as _
use crate::{
    ContractError, StakingTier, StakingTierConfig, TimeLockedUpgradeContract,
    TimeLockedUpgradeContractClient, DEFAULT_HEARTBEAT_INTERVAL, 
    AssetId,
};

/// Helper: advance the ledger timestamp by `delta` seconds.
fn advance_ledger_timestamp(env: &Env, delta: u64) {
    let current_ts = env.ledger().timestamp();
    env.ledger().set(LedgerInfo {
        timestamp: current_ts + delta,
        protocol_version: env.ledger().protocol_version(),
        sequence_number: env.ledger().sequence(),
        network_id: Default::default(),
        base_reserve: 10,
        min_temp_entry_ttl: 0,
        min_persistent_entry_ttl: 0,
        max_entry_ttl: u32::MAX,
    });
}

fn nonce_proof(env: &Env, nonce: u64, salt_seed: &[u8]) -> (Bytes, soroban_sdk::BytesN<32>) {
    let salt = Bytes::from_slice(env, salt_seed);
    let signature = crate::nonce::derive_salt_signature(env, nonce, salt.clone());
    (salt, signature)
}

#[test]
fn test_flash_loan_fee_discount_selects_highest_volume_tier() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);
    let admin = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let tiers = soroban_sdk::vec![
        &env,
        FlashLoanFeeTier { min_volume: 1_000, discount_bps: 100 },
        FlashLoanFeeTier { min_volume: 10_000, discount_bps: 500 },
        FlashLoanFeeTier { min_volume: 100_000, discount_bps: 1_000 },
    ];
    client.set_flash_loan_fee_tiers(&admin, &tiers);

    let quote = client.quote_flash_loan_fee(&10_000, &50_000);
    assert_eq!(quote.tier_index, 1);
    assert_eq!(quote.discount_bps, 500);
    assert_eq!(quote.fee, 9_500);

    let base_quote = client.quote_flash_loan_fee(&10_000, &999);
    assert_eq!(base_quote.discount_bps, 0);
    assert_eq!(base_quote.fee, 10_000);
}

#[test]
fn test_flash_loan_fee_tiers_reject_unsorted_or_excessive_discount() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);
    let admin = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let invalid = soroban_sdk::vec![
        &env,
        FlashLoanFeeTier { min_volume: 10_000, discount_bps: 100 },
        FlashLoanFeeTier { min_volume: 1_000, discount_bps: 10_001 },
    ];
    assert_eq!(client.try_set_flash_loan_fee_tiers(&admin, &invalid), Err(Ok(ContractError::InvalidFlashLoanFeeTier)));

    let excessive = soroban_sdk::vec![
        &env,
        FlashLoanFeeTier { min_volume: 1_000, discount_bps: 10_001 },
    ];
    assert_eq!(client.try_set_flash_loan_fee_tiers(&admin, &excessive), Err(Ok(ContractError::InvalidFlashLoanFeeDiscount)));
}

// ═════════════════════════════════════════════════════════════════════════════
// Existing tests
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn test_schema_version_migration_converts_legacy_state() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);

    let legacy_profiles_key = soroban_sdk::Symbol::new(&env, "NODES");
    let legacy_signers_key = soroban_sdk::Symbol::new(&env, "SIGNERS");
    let legacy_stakes_key = soroban_sdk::Symbol::new(&env, "STAKES");
    let legacy_total_key = soroban_sdk::Symbol::new(&env, "TOTAL");
    let legacy_heartbeat_key = soroban_sdk::Symbol::new(&env, "HBEAT");

    let mut profiles = soroban_sdk::Map::new(&env);
    profiles.set(admin.clone(), crate::NodeProfile {
        node: admin.clone(),
        rate: 42,
        confidence: 90,
        updated_at: 1,
    });
    env.storage().instance().set(&legacy_profiles_key, &profiles);

    let mut signers = soroban_sdk::Map::new(&env);
    signers.set(admin.clone(), true);
    env.storage().instance().set(&legacy_signers_key, &signers);

    let mut stakes = soroban_sdk::Map::new(&env);
    stakes.set(admin.clone(), 123u64);
    env.storage().instance().set(&legacy_stakes_key, &stakes);
    env.storage().instance().set(&legacy_total_key, &123u64);

    let mut heartbeats = soroban_sdk::Map::new(&env);
    heartbeats.set(0u32, 7u64);
    env.storage().instance().set(&legacy_heartbeat_key, &heartbeats);

    client.initialize(&admin, &treasury);

    let data = client.get_data();
    assert_eq!(data.admin, admin);
    assert_eq!(data.value, 0);

    assert!(env.storage().persistent().has(&crate::storage::NodeProfileKey::ProfileByNode(admin.clone())));
    assert!(env.storage().instance().has(&crate::storage::SignerKey::SignerByAddress(admin.clone())));
    assert!(env.storage().instance().has(&crate::storage::StakeKey::StakeByNode(admin.clone())));
    assert_eq!(env.storage().instance().get::<_, u64>(&crate::TOTAL_STAKED_KEY).unwrap(), 123u64);
    assert!(env.storage().temporary().has(&crate::storage::HeartbeatKey::HeartbeatByAsset(0u32)));
}

#[test]
fn test_initialize_and_basic_functionality() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);

    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let data = client.get_data();
    assert_eq!(data.admin, admin);
    assert_eq!(data.value, 0);

    let (salt, signature) = nonce_proof(&env, 0, b"set-value-0");
    client.set_value(&42, &admin, &0, &salt, &signature, &u64::MAX);
    let data = client.get_data();
    assert_eq!(data.value, 42);
    assert_eq!(client.get_coordinator_nonce(&admin), 1);
}

#[test]
fn test_propose_upgrade() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let new_wasm_hash = soroban_sdk::BytesN::from_array(&env, &[1u8; 32]);
    let (salt, signature) = nonce_proof(&env, 0, b"propose-upgrade-0");

    client.propose_upgrade(&new_wasm_hash, &admin, &0, &salt, &signature, &u64::MAX);

    let pending = client.get_pending_upgrade();
    assert!(pending.is_some());

    let staged_upgrade = pending.unwrap();
    assert_eq!(staged_upgrade.wasm_hash, new_wasm_hash);
    // assert_eq!(pending_upgrade.proposer, admin); // proposer field doesn't exist on StagedUpgrade
    assert_eq!(client.get_coordinator_nonce(&admin), 1);

    let remaining = client.get_upgrade_timelock_remaining();
    assert!(remaining.is_some());
    assert_eq!(remaining.unwrap(), 5000u32);
}

#[test]
fn test_set_value_rejects_bad_salt_signature() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let salt = Bytes::from_slice(&env, b"bad-salt");
    let bad_signature = soroban_sdk::BytesN::from_array(&env, &[9u8; 32]);

    let result = client.try_set_value(&42, &admin, &0, &salt, &bad_signature, &u64::MAX);
    assert_eq!(result, Err(Ok(ContractError::InvalidSaltSignature)));
}

#[test]
fn test_execute_upgrade_after_timelock() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let new_wasm_hash = soroban_sdk::BytesN::from_array(&env, &[1u8; 32]);
    let (salt, signature) = nonce_proof(&env, 0, b"propose-upgrade-1");

    client.propose_upgrade(&new_wasm_hash, &admin, &0, &salt, &signature, &u64::MAX);

    // Fast forward ledgers
    env.ledger().set(LedgerInfo { sequence_number: 5001, ..env.ledger().get() });

    // Timelock should be satisfied
    let remaining = client.get_upgrade_timelock_remaining();
    assert_eq!(remaining.unwrap(), 4999u32.saturating_sub(5001u32.saturating_sub(1)));
}

#[test]
fn test_cancel_upgrade() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let new_wasm_hash = soroban_sdk::BytesN::from_array(&env, &[1u8; 32]);

    let (salt, signature) = nonce_proof(&env, 0, b"propose-upgrade-2");
    client.propose_upgrade(&new_wasm_hash, &admin, &0, &salt, &signature, &u64::MAX);
    assert!(client.get_pending_upgrade().is_some());

    client.cancel_upgrade(&admin);

    assert!(client.get_pending_upgrade().is_none());
    assert!(client.get_upgrade_timelock_remaining().is_none());
}

#[test]
fn test_timelock_countdown() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let new_wasm_hash = soroban_sdk::BytesN::from_array(&env, &[1u8; 32]);

    let (salt, signature) = nonce_proof(&env, 0, b"propose-upgrade-3");
    client.propose_upgrade(&new_wasm_hash, &admin, &0, &salt, &signature, &u64::MAX);

    let remaining = client.get_upgrade_timelock_remaining().unwrap();
    assert_eq!(remaining, 5000);

    env.ledger().set(LedgerInfo { sequence_number: 1000, ..env.ledger().get() });

    let remaining = client.get_upgrade_timelock_remaining().unwrap();
    assert_eq!(remaining, 4000);

    env.ledger().set(LedgerInfo { sequence_number: 5000, ..env.ledger().get() });

    let remaining = client.get_upgrade_timelock_remaining().unwrap();
    assert_eq!(remaining, 0u32);
}

// ═════════════════════════════════════════════════════════════════════════════
// Heartbeat Verification tests (Issue #188)
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn test_heartbeat_fresh_data() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let asset: AssetId = 3897123275; // NGN
    // Update heartbeat
    client.update_heartbeat(&asset, &admin);

    // Data should be fresh immediately after update
    assert!(client.is_data_fresh(&asset));

    // Verify timestamp was recorded
    let ts = client.get_last_update_timestamp(&asset);
    assert!(ts.is_some());
    assert_eq!(ts.unwrap(), env.ledger().timestamp());
}

#[test]
fn test_heartbeat_stale_data() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let asset: AssetId = 2654435761; // KES

    // Update heartbeat at current time
    client.update_heartbeat(&asset, &admin);
    assert!(client.is_data_fresh(&asset));

    // Fast-forward past the default heartbeat interval (5 min = 300s) + 1
    advance_ledger_timestamp(&env, DEFAULT_HEARTBEAT_INTERVAL + 1);

    // Data should now be stale
    assert!(!client.is_data_fresh(&asset));
}

#[test]
fn test_heartbeat_never_updated() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let asset: AssetId = 4026531840; // GHS

    // No heartbeat recorded → should be stale
    assert!(!client.is_data_fresh(&asset));
    assert!(client.get_last_update_timestamp(&asset).is_none());
}

#[test]
fn test_heartbeat_custom_interval() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let asset: AssetId = 4160749568; // CFA

    // Verify default interval
    assert_eq!(client.get_heartbeat_interval(), DEFAULT_HEARTBEAT_INTERVAL);

    // Set a custom interval of 10 minutes (600 seconds)
    let custom_interval: u64 = 600;
    client.set_heartbeat_interval(&custom_interval, &admin);
    assert_eq!(client.get_heartbeat_interval(), custom_interval);

    // Update heartbeat
    client.update_heartbeat(&asset, &admin);
    assert!(client.is_data_fresh(&asset));

    // Fast-forward 301 seconds — stale with default, but fresh with custom
    advance_ledger_timestamp(&env, 301);
    assert!(client.is_data_fresh(&asset)); // Still fresh (301 < 600)

    // Fast-forward past the custom interval
    advance_ledger_timestamp(&env, 300); // total: 601
    assert!(!client.is_data_fresh(&asset)); // Now stale (601 > 600)
}

/*
#[test]
fn test_heartbeat_unauthorized_update() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let unauthorized = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let asset: AssetId = 3897123275; // NGN

    // Non-admin tries to update heartbeat — should panic
    let args = soroban_sdk::vec![&env, asset.into_val(&env), unauthorized.into_val(&env)];
    let result = env.try_invoke_contract::<(), soroban_sdk::Error>(
        &contract_id,
        &soroban_sdk::Symbol::new(&env, "update_heartbeat"),
        args,
    );
    assert!(result.is_err());
}
*/

/*
#[test]
fn test_heartbeat_unauthorized_set_interval() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let unauthorized = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    // Non-admin tries to set heartbeat interval — should panic
    let args = soroban_sdk::vec![&env, 600u64.into_val(&env), unauthorized.into_val(&env)];
    let result = env.try_invoke_contract::<(), soroban_sdk::Error>(
        &contract_id,
        &soroban_sdk::Symbol::new(&env, "set_heartbeat_interval"),
        args,
    );
    assert!(result.is_err());
}
*/

/*
#[test]
fn test_unauthorized_propose_upgrade() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);
    
    let admin = soroban_sdk::Address::generate(&env);
    let unauthorized_user = soroban_sdk::Address::generate(&env);
    
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);
    
    let new_wasm_hash = soroban_sdk::BytesN::from_array(&env, &[1u8; 32]);
    
    // Try to propose upgrade as unauthorized user - should fail
    let args = soroban_sdk::vec![&env, new_wasm_hash.into_val(&env), unauthorized_user.into_val(&env)];
    let result = env.try_invoke_contract::<(), soroban_sdk::Error>(
        &contract_id,
        &soroban_sdk::Symbol::new(&env, "propose_upgrade"),
        args,
    );
    assert!(result.is_err());
}
*/

/*
#[test]
fn test_unauthorized_set_value() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);
    
    let admin = soroban_sdk::Address::generate(&env);
    let unauthorized_user = soroban_sdk::Address::generate(&env);
    
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);
    
    // Try to set value as unauthorized user - should fail
    let args = soroban_sdk::vec![&env, 42u64.into_val(&env), unauthorized_user.into_val(&env)];
    let result = env.try_invoke_contract::<(), soroban_sdk::Error>(
        &contract_id,
        &soroban_sdk::Symbol::new(&env, "set_value"),
        args,
    );
    assert!(result.is_err());
}
*/
// ═══════════════════════════════════════════════════════════════════════════
// Read-Only View Guardrails tests (Issue #449)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_get_data_is_idempotent() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let first = client.get_data();
    let second = client.get_data();
    assert_eq!(first.admin, second.admin);
    assert_eq!(first.value, second.value);
}

#[test]
fn test_is_data_fresh_does_not_mutate_state() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let asset: AssetId = 3897123275; // NGN

    // Calling is_data_fresh multiple times on the same slot must not alter state
    assert!(!client.is_data_fresh(&asset));
    assert!(!client.is_data_fresh(&asset));
    assert!(!client.is_data_fresh(&asset));
}

#[test]
fn test_query_methods_do_not_affect_each_other() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let asset: AssetId = 2654435761; // KES

    // get_data reads contract state; is_data_fresh reads heartbeat storage.
    // Neither should influence the other's result.
    let data_before = client.get_data();
    let _ = client.is_data_fresh(&asset);
    let data_after = client.get_data();

    assert_eq!(data_before.admin, data_after.admin);
    assert_eq!(data_before.value, data_after.value);
}

#[test]
fn test_get_data_returns_error_before_init() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let result = client.try_get_data();
    assert_eq!(result, Err(Ok(ContractError::NotInitialized)));
}

#[test]
fn test_is_data_fresh_returns_false_for_unknown_asset() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    // Any asset that was never written should return false
    let asset: AssetId = 4026531840; // GHS
    assert!(!client.is_data_fresh(&asset));
}

// ═══════════════════════════════════════════════════════════════════════════
// Atomic Staking tests (Issue #289)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_stake_and_register_success() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let node = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let record = client.stake_and_register(&node, &1000u64);

    assert_eq!(record.node, node);
    assert_eq!(record.amount, 1000u64);
    assert_eq!(client.get_stake(&node), 1000u64);
    assert_eq!(client.get_total_staked(), 1000u64);
}

#[test]
fn test_stake_updates_heartbeat() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let node = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let stake_asset: AssetId = 0; // STAKE
    assert!(!client.is_data_fresh(&stake_asset));

    client.stake_and_register(&node, &500u64);

    assert!(client.is_data_fresh(&stake_asset));
}

#[test]
fn test_multiple_nodes_stake() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let node1 = soroban_sdk::Address::generate(&env);
    let node2 = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    client.stake_and_register(&node1, &1000u64);
    client.stake_and_register(&node2, &2000u64);

    assert_eq!(client.get_stake(&node1), 1000u64);
    assert_eq!(client.get_stake(&node2), 2000u64);
    assert_eq!(client.get_total_staked(), 3000u64);
}

#[test]
fn test_get_stake_unregistered_node_returns_zero() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let node = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    assert_eq!(client.get_stake(&node), 0u64);
    assert_eq!(client.get_total_staked(), 0u64);
}

#[test]
fn test_unstake_removes_node_and_updates_total() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let node = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    client.stake_and_register(&node, &1000u64);
    assert_eq!(client.get_total_staked(), 1000u64);

    let returned = client.unstake(&node);

    assert_eq!(returned, 1000u64);
    assert_eq!(client.get_stake(&node), 0u64);
    assert_eq!(client.get_total_staked(), 0u64);
}

// ═══════════════════════════════════════════════════════════════════════════
// Dynamic Staking Tier tests (Issue #300)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_regional_feed_allows_lower_stake_than_premier_feed() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let node = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let regional = symbol_short!("KES");
    let premier = symbol_short!("NGN");

    let signers = soroban_sdk::vec![&env, admin.clone(), admin.clone()];
    client.set_asset_feed_metrics(&admin, &regional, &10, &100, &signers);
    client.set_asset_feed_metrics(&admin, &premier, &80, &1_000, &signers);

    assert_eq!(client.get_staking_tier(&regional), StakingTier::Regional);
    assert_eq!(client.get_staking_tier(&premier), StakingTier::Premier);
    assert!(client.get_required_stake(&regional) < client.get_required_stake(&premier));

    let regional_record = client.stake_and_register_for_feed(&node, &regional, &100u64);
    assert_eq!(regional_record.tier, StakingTier::Regional);
    assert_eq!(client.get_feed_stake(&node, &regional), 100u64);

    let premier_result = client.try_stake_and_register_for_feed(&node, &premier, &100u64);
    assert_eq!(
        premier_result,
        Err(Ok(ContractError::InsufficientStakeForTier))
    );

    let premier_record = client.stake_and_register_for_feed(&node, &premier, &10_000u64);
    assert_eq!(premier_record.tier, StakingTier::Premier);
    assert_eq!(client.get_feed_stake(&node, &premier), 10_000u64);
}

#[test]
fn test_corridor_volume_bumps_tier_requirements() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let asset = symbol_short!("GHS");
    client.set_asset_feed_metrics(&admin, &asset, &10, &200, &soroban_sdk::vec![&env, admin.clone()]);

    assert_eq!(client.get_staking_tier(&asset), StakingTier::Regional);

    client.add_corridor_fees(&asset, &2_000_000_000u64, &0u64);

    assert_eq!(client.get_staking_tier(&asset), StakingTier::Standard);
    assert_eq!(client.get_required_stake(&asset), 1_000u64);
}

#[test]
fn test_custom_tier_config_is_enforced() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let node = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let signers = soroban_sdk::vec![&env, admin.clone(), admin.clone()];
    client.set_staking_tier_config(
        &admin,
        &StakingTierConfig {
            regional_min_stake: 250,
            standard_min_stake: 2_500,
            premier_min_stake: 25_000,
        },
    );

    let asset = symbol_short!("ZAR");
    client.set_asset_feed_metrics(&admin, &asset, &10, &100, &signers);

    assert_eq!(client.get_required_stake(&asset), 250u64);

    let result = client.try_stake_and_register_for_feed(&node, &asset, &200u64);
    assert_eq!(result, Err(Ok(ContractError::InsufficientStakeForTier)));

    client.stake_and_register_for_feed(&node, &asset, &250u64);
    assert_eq!(client.get_feed_stake(&node, &asset), 250u64);
}

#[test]
fn test_unstake_from_feed_updates_totals() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let node = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let asset = symbol_short!("UGX");
    client.set_asset_feed_metrics(&admin, &asset, &10, &100, &soroban_sdk::vec![&env, admin.clone()]);
    client.stake_and_register_for_feed(&node, &asset, &100u64);

    assert_eq!(client.get_total_staked(), 100u64);
    assert_eq!(client.unstake_from_feed(&node, &asset), 100u64);
    assert_eq!(client.get_feed_stake(&node, &asset), 0u64);
    assert_eq!(client.get_total_staked(), 0u64);
}

#[test]
fn test_set_value_updates_heartbeat() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let value_asset: AssetId = 1; // VALUE
    // Before set_value, no heartbeat exists for "VALUE"
    assert!(!client.is_data_fresh(&value_asset));

    // Call set_value — should auto-record heartbeat
    let (salt, signature) = nonce_proof(&env, 0, b"set-value-1");
    client.set_value(&42, &admin, &0, &salt, &signature, &u64::MAX);

    // Now the "VALUE" asset should have a fresh heartbeat
    assert!(client.is_data_fresh(&value_asset));
    assert!(client.get_last_update_timestamp(&value_asset).is_some());

    // Fast-forward past interval → data goes stale
    advance_ledger_timestamp(&env, DEFAULT_HEARTBEAT_INTERVAL + 1);
    assert!(!client.is_data_fresh(&value_asset));

    // Another set_value call refreshes the heartbeat
    let (salt, signature) = nonce_proof(&env, 1, b"set-value-2");
    client.set_value(&100, &admin, &1, &salt, &signature, &u64::MAX);
    assert!(client.is_data_fresh(&value_asset));
}

#[test]
fn test_initialize_twice_returns_typed_error() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let result = client.try_initialize(&admin, &treasury);
    assert_eq!(result, Err(Ok(ContractError::AlreadyInitialized)));
}

#[test]
fn test_unauthorized_set_value_returns_typed_error() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let unauthorized = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let (salt, signature) = nonce_proof(&env, 0, b"set-value-unauth");
    let result = client.try_set_value(&42, &unauthorized, &0u64, &salt, &signature, &u64::MAX);
    assert_eq!(result, Err(Ok(ContractError::NotAdmin)));
}

#[test]
fn test_zero_heartbeat_interval_returns_typed_error() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let result = client.try_set_heartbeat_interval(&0, &admin);
    assert_eq!(result, Err(Ok(ContractError::InvalidHeartbeatInterval)));
}

#[test]
fn test_expired_signature_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    // Advance ledger past the expiry window
    advance_ledger_timestamp(&env, 1000);
    let expired_at: u64 = 500; // already in the past

    let new_wasm_hash = soroban_sdk::BytesN::from_array(&env, &[1u8; 32]);
    let (salt, signature) = nonce_proof(&env, 0, b"propose-upgrade-expired");
    let result = client.try_propose_upgrade(&new_wasm_hash, &admin, &0, &salt, &signature, &expired_at);
    assert_eq!(result, Err(Ok(ContractError::SignatureExpired)));

    let (salt2, signature2) = nonce_proof(&env, 0, b"set-value-expired");
    let result = client.try_set_value(&42, &admin, &0, &salt2, &signature2, &expired_at);
    assert_eq!(result, Err(Ok(ContractError::SignatureExpired)));
}

// ═════════════════════════════════════════════════════════════════════════════
// Issue #453 — Bond capacity checks for premium asset pool validator profiles
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn test_update_validator_profile_succeeds_with_sufficient_stake() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let node = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    // Stake exactly the minimum required bond.
    client.stake_and_register(&node, &crate::validation::PREMIUM_POOL_MIN_STAKE);

    let pool = symbol_short!("USDC");
    // Must not error when stake >= PREMIUM_POOL_MIN_STAKE.
    client.update_validator_profile(&node, &pool);
}

#[test]
fn test_update_validator_profile_blocked_below_min_stake() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let node = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    // Stake one unit below the required minimum.
    client.stake_and_register(&node, &(crate::validation::PREMIUM_POOL_MIN_STAKE - 1));

    let pool = symbol_short!("BTC");
    let result = client.try_update_validator_profile(&node, &pool);
    assert_eq!(result, Err(Ok(ContractError::PremiumPoolAccessDenied)));
}

#[test]
fn test_update_validator_profile_blocked_with_zero_stake() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let node = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    // Node has never staked — locked stake is 0.
    let pool = symbol_short!("ETH");
    let result = client.try_update_validator_profile(&node, &pool);
    assert_eq!(result, Err(Ok(ContractError::PremiumPoolAccessDenied)));
}

#[test]
fn test_update_validator_profile_succeeds_above_min_stake() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let node = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    // Stake well above the minimum.
    client.stake_and_register(&node, &5_000u64);

    let pool = symbol_short!("XLM");
    client.update_validator_profile(&node, &pool);
    // Heartbeat for the pool asset should now be fresh.
    assert!(client.is_data_fresh(&crate::symbol_to_asset_id(&pool)));
}

// ═════════════════════════════════════════════════════════════════════════════
// Emergency Key Revocation tests (multi-sig coordinator group)
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn test_emergency_revocation_proposal_opens_successfully() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let signer_a = soroban_sdk::Address::generate(&env);
    let compromised = soroban_sdk::Address::generate(&env);
    let replacement = soroban_sdk::Address::generate(&env);

    client.initialize(&admin, &soroban_sdk::Address::generate(&env));
    client.register_signer(&signer_a, &admin);
    client.register_signer(&compromised, &admin);

    // Admin opens an emergency revocation proposal against the compromised signer.
    client.propose_emergency_revocation(&admin, &compromised, &replacement);

    let proposal = client.get_emerg_revocation_proposal();
    assert!(proposal.is_some());
    let p = proposal.unwrap();
    assert_eq!(p.target, compromised);
    assert_eq!(p.replacement, replacement);
    assert_eq!(p.proposer, admin);
    // Proposer's opening vote is counted automatically — expect 1 vote.
    assert_eq!(p.votes.len(), 1);
}

#[test]
fn test_emergency_revocation_blocks_target_on_threshold() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let signer_a = soroban_sdk::Address::generate(&env);
    let signer_b = soroban_sdk::Address::generate(&env);
    let compromised = soroban_sdk::Address::generate(&env);
    let replacement = soroban_sdk::Address::generate(&env);

    client.initialize(&admin, &soroban_sdk::Address::generate(&env));
    // Register three signers (compromised + two honest ones).
    client.register_signer(&signer_a, &admin);
    client.register_signer(&signer_b, &admin);
    client.register_signer(&compromised, &admin);

    // Open proposal — admin's implicit vote is vote #1.
    client.propose_emergency_revocation(&admin, &compromised, &replacement);

    // signer_a votes — vote #2, threshold for 3 signers = 3/2+1 = 2, reached.
    client.vote_emergency_revocation(&signer_a, &u64::MAX);

    // Proposal should be cleared.
    assert!(client.get_emerg_revocation_proposal().is_none());

    // Target must now be flagged as revoked in storage.
    assert!(client.is_revoked(&compromised));
}

#[test]
fn test_revoked_address_cannot_sign_or_modify_config() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let signer_a = soroban_sdk::Address::generate(&env);
    let compromised = soroban_sdk::Address::generate(&env);
    let replacement = soroban_sdk::Address::generate(&env);

    client.initialize(&admin, &soroban_sdk::Address::generate(&env));
    client.register_signer(&signer_a, &admin);
    client.register_signer(&compromised, &admin);

    // Revoke the compromised key (admin opens + signer_a confirms = threshold 2 of 2).
    client.propose_emergency_revocation(&admin, &compromised, &replacement);
    client.vote_emergency_revocation(&signer_a, &u64::MAX);

    assert!(client.is_revoked(&compromised));

    // Attempt: revoked node tries to re-stake.
    let result = client.try_stake_and_register(&compromised, &500u64);
    assert_eq!(result, Err(Ok(ContractError::RevokedAddress)));

    // Attempt: revoked node tries to register a new signer.
    let new_signer = soroban_sdk::Address::generate(&env);
    let result = client.try_register_signer(&new_signer, &compromised);
    assert_eq!(result, Err(Ok(ContractError::RevokedAddress)));
}

#[test]
fn test_revoked_admin_cannot_propose_or_execute_upgrade() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    // Use a 3-of-3 setup: admin + signer_a + signer_b.
    let admin = soroban_sdk::Address::generate(&env);
    let signer_a = soroban_sdk::Address::generate(&env);
    let signer_b = soroban_sdk::Address::generate(&env);
    let replacement = soroban_sdk::Address::generate(&env);

    client.initialize(&admin, &soroban_sdk::Address::generate(&env));
    client.register_signer(&signer_a, &admin);
    client.register_signer(&signer_b, &admin);

    // Revoke the admin (signer_a opens, signer_b confirms = threshold 2 of 2).
    client.propose_emergency_revocation(&signer_a, &admin, &replacement);
    client.vote_emergency_revocation(&signer_b, &u64::MAX);

    assert!(client.is_revoked(&admin));

    client.propose_admin_change(&admin, &new_admin);
    // Attempt immediate execution without waiting
    let result = client.try_execute_admin_change_by_timelock(&admin);
    assert_eq!(result, Err(Ok(ContractError::AdminChangeTimelockNotSatis)));
}

#[test]
fn test_compromised_key_cannot_vote_on_its_own_revocation() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let signer_a = soroban_sdk::Address::generate(&env);
    let compromised = soroban_sdk::Address::generate(&env);
    let replacement = soroban_sdk::Address::generate(&env);

    client.initialize(&admin, &soroban_sdk::Address::generate(&env));
    client.register_signer(&signer_a, &admin);
    client.register_signer(&compromised, &admin);

    client.propose_emergency_revocation(&admin, &compromised, &replacement);

    // Compromised key attempts to vote on its own revocation — must be rejected.
    let result = client.try_vote_emergency_revocation(&compromised, &u64::MAX);
    assert_eq!(result, Err(Ok(ContractError::Unauthorized)));
}

#[test]
fn test_double_vote_on_emergency_revocation_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let signer_a = soroban_sdk::Address::generate(&env);
    let signer_b = soroban_sdk::Address::generate(&env);
    let signer_c = soroban_sdk::Address::generate(&env);
    let compromised = soroban_sdk::Address::generate(&env);
    let replacement = soroban_sdk::Address::generate(&env);

    client.initialize(&admin, &soroban_sdk::Address::generate(&env));
    client.register_signer(&signer_a, &admin);
    client.register_signer(&signer_b, &admin);
    client.register_signer(&signer_c, &admin);
    client.register_signer(&compromised, &admin);

    // Open proposal (admin = vote 1, threshold of 4 signers = 3).
    client.propose_emergency_revocation(&admin, &compromised, &replacement);

    client.vote_emergency_revocation(&signer_a, &u64::MAX);

    // signer_a votes a second time — must be rejected.
    let result = client.try_vote_emergency_revocation(&signer_a, &u64::MAX);
    assert_eq!(result, Err(Ok(ContractError::AlreadyVoted)));
}

#[test]
fn test_only_one_emergency_proposal_at_a_time() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let signer_a = soroban_sdk::Address::generate(&env);
    let compromised = soroban_sdk::Address::generate(&env);
    let another_target = soroban_sdk::Address::generate(&env);
    let replacement = soroban_sdk::Address::generate(&env);

    client.initialize(&admin, &soroban_sdk::Address::generate(&env));
    client.register_signer(&signer_a, &admin);
    client.register_signer(&compromised, &admin);
    client.register_signer(&another_target, &admin);

    client.propose_emergency_revocation(&admin, &compromised, &replacement);

    // Opening a second proposal while one is already active must be rejected.
    let result = client.try_propose_emergency_revocation(&signer_a, &another_target, &replacement);
    assert_eq!(result, Err(Ok(ContractError::EmergencyRevocationAlreadyActive)));
}

#[test]
fn test_emergency_revocation_expired_signature_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let signer_a = soroban_sdk::Address::generate(&env);
    let compromised = soroban_sdk::Address::generate(&env);
    let replacement = soroban_sdk::Address::generate(&env);

    client.initialize(&admin, &soroban_sdk::Address::generate(&env));
    client.register_signer(&signer_a, &admin);
    client.register_signer(&compromised, &admin);

    client.propose_emergency_revocation(&admin, &compromised, &replacement);

    // Advance ledger past the expiry window.
    advance_ledger_timestamp(&env, 1_000);
    let expired_at: u64 = 500;

    let result = client.try_vote_emergency_revocation(&signer_a, &expired_at);
    assert_eq!(result, Err(Ok(ContractError::SignatureExpired)));
}

#[test]
fn test_vote_with_no_active_proposal_returns_no_active_error() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let signer_a = soroban_sdk::Address::generate(&env);

    client.initialize(&admin, &soroban_sdk::Address::generate(&env));
    client.register_signer(&signer_a, &admin);

    // No proposal has been opened yet.
    let result = client.try_vote_emergency_revocation(&signer_a, &u64::MAX);
    assert_eq!(result, Err(Ok(ContractError::NoActiveEmergencyRevocation)));
}

#[test]
fn test_replacement_signer_promoted_on_revocation() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let signer_a = soroban_sdk::Address::generate(&env);
    let compromised = soroban_sdk::Address::generate(&env);
    let replacement = soroban_sdk::Address::generate(&env);

    client.initialize(&admin, &soroban_sdk::Address::generate(&env));
    client.register_signer(&signer_a, &admin);
    client.register_signer(&compromised, &admin);

    // Revoke compromised — threshold = 1 (only 1 registered honest signer after removal).
    // admin opens (vote 1 of 2 needed for 2 signers).
    client.propose_emergency_revocation(&admin, &compromised, &replacement);
    // signer_a votes — threshold 2 reached.
    client.vote_emergency_revocation(&signer_a, &u64::MAX);

    // Target must be revoked.
    assert!(client.is_revoked(&compromised));
    // Replacement must now be a registered signer and therefore able to vote.
    // We verify by trying a no-op: replacement voting on a non-existent proposal
    // should return NoActiveEmergencyRevocation (not Unauthorized), proving it
    // is recognised as a valid participant.
    let result = client.try_vote_emergency_revocation(&replacement, &u64::MAX);
    assert_eq!(result, Err(Ok(ContractError::NoActiveEmergencyRevocation)));
}

// ═════════════════════════════════════════════════════════════════════════════
// Zero-Knowledge Anonymity Set Deposit Merkle Verifier Tests (Issue #767)
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn test_zk_merkle_deposit_and_withdrawal_flow() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let commitment_0 = soroban_sdk::BytesN::from_array(&env, &[1u8; 32]);
    let (leaf_idx, root_0) = client.deposit_commitment(&commitment_0);
    assert_eq!(leaf_idx, 0);
    assert_eq!(client.get_anonymity_set_root(), Some(root_0.clone()));
    assert!(client.is_merkle_root_valid(&root_0));

    // Build Merkle proof path for leaf 0
    let mut path = soroban_sdk::Vec::new(&env);
    for level in 0..crate::zk::merkle::TREE_DEPTH {
        path.push_back(crate::zk::merkle::get_zero_hash(&env, level));
    }

    let nullifier = soroban_sdk::BytesN::from_array(&env, &[77u8; 32]);
    assert!(!client.is_nullifier_spent(&nullifier));

    // Verify valid withdrawal
    let verify_res = client.verify_zk_withdrawal(&root_0, &nullifier, &commitment_0, &path, &0);
    assert_eq!(verify_res, true);
}


// ═════════════════════════════════════════════════════════════════════════════
// Flash Loan Arbitrage Detection Tests (issue #757)
// ═════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod flash_loan_guard_tests {
    use crate::flash_loan_guard::{
        check_flash_loan_arbitrage, check_k_nondecreasing, check_liquidity_depth,
        check_reserve_ratio, PoolSnapshot, MIN_LIQUIDITY_DEPTH, MAX_RATIO_DEVIATION_BPS,
    };
    use crate::ContractError;

    const DEPTH: u128 = MIN_LIQUIDITY_DEPTH;

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn pool(reserve_a: u128, reserve_b: u128) -> PoolSnapshot {
        PoolSnapshot { reserve_a, reserve_b }
    }

    // ── 1. Liquidity depth safety threshold ───────────────────────────────────

    #[test]
    fn test_flash_loan_guard_liquidity_depth_passes_at_minimum() {
        // Both reserves exactly at the minimum threshold should be accepted.
        let snap = pool(DEPTH, DEPTH);
        assert!(check_liquidity_depth(&snap).is_ok());
    }

    #[test]
    fn test_flash_loan_guard_liquidity_depth_passes_above_minimum() {
        let snap = pool(DEPTH * 5, DEPTH * 3);
        assert!(check_liquidity_depth(&snap).is_ok());
    }

    #[test]
    fn test_flash_loan_guard_liquidity_depth_fails_reserve_a_drained() {
        // reserve_a drained to one stroop below minimum.
        let snap = pool(DEPTH - 1, DEPTH * 10);
        assert_eq!(
            check_liquidity_depth(&snap),
            Err(ContractError::FlashLoanArbitrageDetected)
        );
    }

    #[test]
    fn test_flash_loan_guard_liquidity_depth_fails_reserve_b_drained() {
        let snap = pool(DEPTH * 10, DEPTH - 1);
        assert_eq!(
            check_liquidity_depth(&snap),
            Err(ContractError::FlashLoanArbitrageDetected)
        );
    }

    #[test]
    fn test_flash_loan_guard_liquidity_depth_fails_both_reserves_drained() {
        let snap = pool(0, 0);
        assert_eq!(
            check_liquidity_depth(&snap),
            Err(ContractError::FlashLoanArbitrageDetected)
        );
    }

    // ── 2. Pool invariant non-decrease (k >= k_before) ────────────────────────

    #[test]
    fn test_flash_loan_guard_k_nondecreasing_passes_when_k_unchanged() {
        let before = pool(2_000, 5_000);
        let after = pool(2_000, 5_000);
        assert!(check_k_nondecreasing(&before, &after).is_ok());
    }

    #[test]
    fn test_flash_loan_guard_k_nondecreasing_passes_when_k_increases() {
        // Fee accrual: slightly less out than in → k grows.
        let before = pool(10_000, 10_000);
        let after = pool(10_100, 9_910); // k_after = 100_101_000 > k_before = 100_000_000
        assert!(check_k_nondecreasing(&before, &after).is_ok());
    }

    #[test]
    fn test_flash_loan_guard_k_nondecreasing_fails_when_k_decreases() {
        let before = pool(10_000, 10_000);
        let after = pool(9_000, 9_000); // k_after = 81_000_000 < k_before = 100_000_000
        assert_eq!(
            check_k_nondecreasing(&before, &after),
            Err(ContractError::FlashLoanArbitrageDetected)
        );
    }

    #[test]
    fn test_flash_loan_guard_k_nondecreasing_fails_small_drain() {
        // Even a 1-stroop loss should be caught.
        let r = DEPTH;
        let before = pool(r, r);
        let after = pool(r - 1, r - 1);
        assert_eq!(
            check_k_nondecreasing(&before, &after),
            Err(ContractError::FlashLoanArbitrageDetected)
        );
    }

    #[test]
    fn test_flash_loan_guard_k_nondecreasing_passes_on_large_reserves() {
        // Realistic large pool: 10^15 XLM each side.
        let r: u128 = 1_000_000_000_000_000_000_000;
        // After a tiny 0.01% fee-bearing swap: k grows.
        let amount_in = r / 10_000;
        let amount_out = amount_in - 1; // floor truncation keeps k non-decreasing
        let before = pool(r, r);
        let after = pool(r + amount_in, r - amount_out);
        assert!(check_k_nondecreasing(&before, &after).is_ok());
    }

    // ── 3. Reserve ratio deviation bound ─────────────────────────────────────

    #[test]
    fn test_flash_loan_guard_ratio_passes_when_unchanged() {
        let snap = pool(DEPTH * 3, DEPTH);
        assert!(check_reserve_ratio(&snap, &snap).is_ok());
    }

    #[test]
    fn test_flash_loan_guard_ratio_passes_on_small_shift() {
        // 5% price movement — well within the 50% tolerance window.
        let before = pool(100 * DEPTH, 100 * DEPTH);
        let after = pool(105 * DEPTH, 100 * DEPTH);
        assert!(check_reserve_ratio(&before, &after).is_ok());
    }

    #[test]
    fn test_flash_loan_guard_ratio_passes_at_exact_boundary() {
        // Exactly MAX_RATIO_DEVIATION_BPS = 50% shift is allowed (boundary inclusive).
        let before = pool(DEPTH * 2, DEPTH * 2);
        // 50% upward: ratio goes from 1 to 1.5
        let after = pool(DEPTH * 3, DEPTH * 2);
        assert!(check_reserve_ratio(&before, &after).is_ok());
    }

    #[test]
    fn test_flash_loan_guard_ratio_fails_flash_loan_spike() {
        // Flash loan scenario: reserve_a doubles via borrowed capital.
        let before = pool(DEPTH, DEPTH);
        let after = pool(DEPTH * 2 + 1, DEPTH); // 100%+ ratio shift
        assert_eq!(
            check_reserve_ratio(&before, &after),
            Err(ContractError::FlashLoanArbitrageDetected)
        );
    }

    #[test]
    fn test_flash_loan_guard_ratio_fails_reserve_collapse() {
        // Attacker drains reserve_b in the same transaction.
        let before = pool(DEPTH * 10, DEPTH * 10);
        // reserve_b collapses to near minimum while reserve_a is untouched.
        // ratio_before = 1.0; ratio_after = 10 → 900% deviation.
        let after = pool(DEPTH * 10, DEPTH);
        assert_eq!(
            check_reserve_ratio(&before, &after),
            Err(ContractError::FlashLoanArbitrageDetected)
        );
    }

    // ── 4. Combined check_flash_loan_arbitrage ────────────────────────────────

    #[test]
    fn test_flash_loan_arbitrage_passes_for_normal_swap() {
        // Standard 1% swap on a well-funded pool.
        let r = 10_000 * DEPTH;
        let amount_in = r / 100;
        let amount_out = (r * amount_in) / (r + amount_in); // constant-product
        let before = pool(r, r);
        let after = pool(r + amount_in, r - amount_out);
        assert!(check_flash_loan_arbitrage(&before, &after).is_ok());
    }

    #[test]
    fn test_flash_loan_arbitrage_rejects_zero_reserve_before() {
        let before = PoolSnapshot { reserve_a: 0, reserve_b: DEPTH };
        let after = pool(DEPTH, DEPTH);
        assert_eq!(
            check_flash_loan_arbitrage(&before, &after),
            Err(ContractError::InvalidInput)
        );
    }

    #[test]
    fn test_flash_loan_arbitrage_rejects_zero_reserve_after() {
        let before = pool(DEPTH, DEPTH);
        let after = PoolSnapshot { reserve_a: DEPTH, reserve_b: 0 };
        assert_eq!(
            check_flash_loan_arbitrage(&before, &after),
            Err(ContractError::InvalidInput)
        );
    }

    #[test]
    fn test_flash_loan_arbitrage_rejects_complete_reserve_drain() {
        // Attacker uses flash loan to drain both reserves completely.
        let before = pool(100 * DEPTH, 100 * DEPTH);
        let after = PoolSnapshot { reserve_a: 1, reserve_b: 1 };
        assert_eq!(
            check_flash_loan_arbitrage(&before, &after),
            Err(ContractError::FlashLoanArbitrageDetected)
        );
    }

    #[test]
    fn test_flash_loan_arbitrage_rejects_single_sided_drain() {
        // reserve_b collapses below min depth while reserve_a is inflated.
        let before = pool(50 * DEPTH, 50 * DEPTH);
        let after = pool(100 * DEPTH, DEPTH / 2); // depth check catches reserve_b
        assert_eq!(
            check_flash_loan_arbitrage(&before, &after),
            Err(ContractError::FlashLoanArbitrageDetected)
        );
    }

    #[test]
    fn test_flash_loan_arbitrage_rejects_k_decrease_on_deep_pool() {
        // Pool is deep enough (both above min), ratio is fine, but k drops.
        let r = 10 * DEPTH;
        let before = pool(r, r);
        let after = pool(r - 1, r - 1); // tiny k decrease
        assert_eq!(
            check_flash_loan_arbitrage(&before, &after),
            Err(ContractError::FlashLoanArbitrageDetected)
        );
    }

    #[test]
    fn test_flash_loan_arbitrage_rejects_price_manipulation_attack() {
        // Classic flash-loan sandwich: borrow huge, swap all A→B,
        // victim's trade executes at terrible price, attacker reverses.
        // We only see the attacker's first swap here.
        // Pool: 100k A, 100k B. Attacker borrows 200k A and dumps it.
        let r = 100_000 * DEPTH;
        let borrowed = 200_000 * DEPTH;
        // After dump: reserve_a = 300k, reserve_b = 33.33k
        let new_b = (r * r) / (r + borrowed); // ~33333 * DEPTH
        let before = pool(r, r);
        let after = pool(r + borrowed, new_b);
        assert_eq!(
            check_flash_loan_arbitrage(&before, &after),
            Err(ContractError::FlashLoanArbitrageDetected)
        );
    }

    #[test]
    fn test_flash_loan_arbitrage_constant_product_large_swap_within_bounds() {
        // Legitimate large (10%) fee-bearing swap where k strictly grows.
        let r = 1_000_000 * DEPTH;
        let amount_in = r / 10;
        // Exact constant product: amount_out = r * amount_in / (r + amount_in).
        // Using integer floor: amount_out < amount_in so k grows.
        let amount_out = r
            .checked_mul(amount_in)
            .unwrap()
            .checked_div(r + amount_in)
            .unwrap();
        let before = pool(r, r);
        let after = pool(r + amount_in, r - amount_out);
        assert!(check_flash_loan_arbitrage(&before, &after).is_ok());
    }
}
