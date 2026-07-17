use soroban_sdk::{Bytes, Env, symbol_short};
use soroban_sdk::testutils::{Address as _, Ledger, LedgerInfo};
use crate::{
    ContractError, StakingTier, StakingTierConfig, TimeLockedUpgradeContract,
    TimeLockedUpgradeContractClient, DEFAULT_HEARTBEAT_INTERVAL, UPGRADE_DELAY_SECONDS,
    AssetId,
};

/// Helper: advance the ledger timestamp by `delta` seconds.
fn advance_ledger_timestamp(env: &Env, delta: u64) {
    let current_ts = env.ledger().timestamp();
    env.ledger().set(LedgerInfo {
        timestamp: current_ts + delta,
        protocol_version: env.ledger().protocol_version(),
        sequence_number: env.ledger().sequence() + (delta / 5) as u32,
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

// ═════════════════════════════════════════════════════════════════════════════
// Existing tests
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn test_initialize_and_basic_functionality() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);

    client.initialize(&admin, &admin);

    let data = client.get_data();
    assert_eq!(data.admin, admin);
    assert_eq!(data.value, 0);

    let (salt, signature) = nonce_proof(&env, 0, b"set-value-0");
    client.set_value(&42, &admin, &0, &salt, &signature, &u64::MAX, &1u64);
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
    client.initialize(&admin, &admin);

    let new_wasm_hash = soroban_sdk::BytesN::from_array(&env, &[1u8; 32]);
    let (salt, signature) = nonce_proof(&env, 0, b"propose-upgrade-0");

    client.propose_upgrade(&new_wasm_hash, &admin, &0, &salt, &signature, &u64::MAX);

    let pending = client.get_pending_upgrade();
    assert!(pending.is_some());

    let pending_upgrade = pending.unwrap();
    assert_eq!(pending_upgrade.wasm_hash, new_wasm_hash);
    assert_eq!(client.get_coordinator_nonce(&admin), 1);

    let remaining = client.get_upgrade_timelock_remaining();
    assert!(remaining.is_some());
    assert_eq!(remaining.unwrap(), 5000);
}

#[test]
fn test_set_value_rejects_bad_salt_signature() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &admin);

    let salt = Bytes::from_slice(&env, b"bad-salt");
    let bad_signature = soroban_sdk::BytesN::from_array(&env, &[9u8; 32]);

    let result = client.try_set_value(&42, &admin, &0, &salt, &bad_signature, &u64::MAX, &1u64);
    assert_eq!(result, Err(Ok(ContractError::InvalidSaltSignature)));
}

#[test]
fn test_execute_upgrade_after_timelock() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &admin);

    let new_wasm_hash = soroban_sdk::BytesN::from_array(&env, &[1u8; 32]);
    let (salt, signature) = nonce_proof(&env, 0, b"propose-upgrade-1");

    client.propose_upgrade(&new_wasm_hash, &admin, &0, &salt, &signature, &u64::MAX);

    // Fast forward time by 48 hours
    advance_ledger_timestamp(&env, UPGRADE_DELAY_SECONDS);

    // Timelock should be satisfied
    let remaining = client.get_upgrade_timelock_remaining();
    assert_eq!(remaining.unwrap(), 0);
}

#[test]
fn test_cancel_upgrade() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &admin);

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
    client.initialize(&admin, &admin);

    let new_wasm_hash = soroban_sdk::BytesN::from_array(&env, &[1u8; 32]);

    let (salt, signature) = nonce_proof(&env, 0, b"propose-upgrade-3");
    client.propose_upgrade(&new_wasm_hash, &admin, &0, &salt, &signature, &u64::MAX);

    let remaining = client.get_upgrade_timelock_remaining().unwrap();
    assert_eq!(remaining, 5000);

    // Advance by half the time (2500 ledgers * 5 seconds = 12500 seconds)
    advance_ledger_timestamp(&env, 12500);

    let remaining = client.get_upgrade_timelock_remaining().unwrap();
    assert_eq!(remaining, 2501);

    // Advance the rest
    advance_ledger_timestamp(&env, 12500);

    let remaining = client.get_upgrade_timelock_remaining().unwrap();
    assert_eq!(remaining, 1);
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
    client.initialize(&admin, &admin);

    let asset = symbol_short!("NGN");

    client.add_corridor_fees(&asset, &crate::validation::MIN_POOL_VOLUME_DEPTH, &0u64);

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
    client.initialize(&admin, &admin);

    let asset = symbol_short!("KES");

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
    client.initialize(&admin, &admin);

    let asset = symbol_short!("GHS");

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
    client.initialize(&admin, &admin);

    let asset = symbol_short!("CFA");

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
    client.initialize(&admin, &admin);

    let asset = symbol_short!("NGN");

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
    client.initialize(&admin, &admin);

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
    
    client.initialize(&admin, &admin);
    
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
    
    client.initialize(&admin, &admin);
    
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
    client.initialize(&admin, &admin);

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
    client.initialize(&admin, &admin);

    let asset = symbol_short!("NGN");

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
    client.initialize(&admin, &admin);

    let asset = symbol_short!("KES");

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
    client.initialize(&admin, &admin);

    // Any asset that was never written should return false
    let asset = symbol_short!("GHS");
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
    client.initialize(&admin, &admin);

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
    client.initialize(&admin, &admin);

    let stake_asset = symbol_short!("STAKE");
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
    client.initialize(&admin, &admin);

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
    client.initialize(&admin, &admin);

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
    client.initialize(&admin, &admin);

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
    env.mock_all_auths_allowing_non_root_auth();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let signer2 = soroban_sdk::Address::generate(&env);
    let node = soroban_sdk::Address::generate(&env);
    let signer1 = soroban_sdk::Address::generate(&env);
    let signer2 = soroban_sdk::Address::generate(&env);
    client.initialize(&admin);
    client.register_signer(&signer1, &admin);
    client.register_signer(&signer2, &admin);

    let regional: Symbol = symbol_short!("KES");
    let premier: Symbol = symbol_short!("NGN");

    let signers = soroban_sdk::vec![&env, signer1.clone(), signer2.clone()];
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
    env.mock_all_auths_allowing_non_root_auth();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let signer1 = soroban_sdk::Address::generate(&env);
    let signer2 = soroban_sdk::Address::generate(&env);
    client.initialize(&admin);
    client.register_signer(&signer1, &admin);
    client.register_signer(&signer2, &admin);

    let asset: AssetId = 4026531840; // GHS
    let signers = soroban_sdk::vec![&env, signer1.clone(), signer2.clone()];
    client.set_asset_feed_metrics(&admin, &asset, &10, &200, &signers);

    assert_eq!(client.get_staking_tier(&asset), StakingTier::Regional);

    client.add_corridor_fees(&asset, &2_000_000_000u64, &0u64);

    assert_eq!(client.get_staking_tier(&asset), StakingTier::Standard);
    assert_eq!(client.get_required_stake(&asset), 1_000u64);
}

#[test]
fn test_custom_tier_config_is_enforced() {
    let env = Env::default();
    env.mock_all_auths_allowing_non_root_auth();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let signer2 = soroban_sdk::Address::generate(&env);
    let node = soroban_sdk::Address::generate(&env);
    let signer1 = soroban_sdk::Address::generate(&env);
    let signer2 = soroban_sdk::Address::generate(&env);
    client.initialize(&admin);
    client.register_signer(&signer1, &admin);
    client.register_signer(&signer2, &admin);

    let signers = soroban_sdk::vec![&env, signer1.clone(), signer2.clone()];
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
    env.mock_all_auths_allowing_non_root_auth();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let signer2 = soroban_sdk::Address::generate(&env);
    let node = soroban_sdk::Address::generate(&env);
    let signer1 = soroban_sdk::Address::generate(&env);
    let signer2 = soroban_sdk::Address::generate(&env);
    client.initialize(&admin);
    client.register_signer(&signer1, &admin);
    client.register_signer(&signer2, &admin);

    let asset: AssetId = 2863311530; // UGX
    let signers = soroban_sdk::vec![&env, signer1.clone(), signer2.clone()];
    client.set_asset_feed_metrics(&admin, &asset, &10, &100, &signers);
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
    client.initialize(&admin, &admin);

    let value_asset = symbol_short!("VALUE");

    // Before set_value, no heartbeat exists for "VALUE"
    assert!(!client.is_data_fresh(&value_asset));

    // Call set_value — should auto-record heartbeat
    let (salt, signature) = nonce_proof(&env, 0, b"set-value-1");
    client.set_value(&42, &admin, &0, &salt, &signature, &u64::MAX, &1u64);

    // Now the "VALUE" asset should have a fresh heartbeat
    assert!(client.is_data_fresh(&value_asset));
    assert!(client.get_last_update_timestamp(&value_asset).is_some());

    // Fast-forward past interval → data goes stale
    advance_ledger_timestamp(&env, DEFAULT_HEARTBEAT_INTERVAL + 1);
    assert!(!client.is_data_fresh(&value_asset));

    // Another set_value call refreshes the heartbeat
    let (salt, signature) = nonce_proof(&env, 1, b"set-value-2");
    client.set_value(&100, &admin, &1, &salt, &signature, &u64::MAX, &2u64);
    assert!(client.is_data_fresh(&value_asset));
}

#[test]
fn test_initialize_twice_returns_typed_error() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &admin);

    let result = client.try_initialize(&admin, &admin);
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
    client.initialize(&admin, &admin);

    let (salt, signature) = nonce_proof(&env, 0, b"set-value-unauth");
    let result = client.try_set_value(&42, &unauthorized, &0u64, &salt, &signature, &u64::MAX, &1u64);
    assert_eq!(result, Err(Ok(ContractError::NotAdmin)));
}

#[test]
fn test_zero_heartbeat_interval_returns_typed_error() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &admin);

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
    client.initialize(&admin, &admin);

    // Advance ledger past the expiry window
    advance_ledger_timestamp(&env, 1000);
    let expired_at: u64 = 500; // already in the past

    let new_wasm_hash = soroban_sdk::BytesN::from_array(&env, &[1u8; 32]);
    let (salt, signature) = nonce_proof(&env, 0, b"propose-upgrade-expired");
    let result = client.try_propose_upgrade(&new_wasm_hash, &admin, &0, &salt, &signature, &expired_at);
    assert_eq!(result, Err(Ok(ContractError::SignatureExpired)));

    let (salt2, signature2) = nonce_proof(&env, 0, b"set-value-expired");
    let result = client.try_set_value(&42, &admin, &0, &salt2, &signature2, &expired_at, &1u64);
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
    client.initialize(&admin, &admin);

    // Stake exactly the minimum required bond.
    client.stake_and_register(&node, &crate::validation::PREMIUM_POOL_MIN_STAKE);

    let pool = symbol_short!("USDC");
    let asset = crate::symbol_to_asset_id(&pool);
    client.add_corridor_fees(&asset, &crate::validation::MIN_POOL_VOLUME_DEPTH, &0u64);
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
    client.initialize(&admin, &admin);

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
    client.initialize(&admin, &admin);

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
    client.initialize(&admin, &admin);

    // Stake well above the minimum.
    client.stake_and_register(&node, &5_000u64);

    let pool = symbol_short!("XLM");
    let asset = crate::symbol_to_asset_id(&pool);
    client.add_corridor_fees(&asset, &crate::validation::MIN_POOL_VOLUME_DEPTH, &0u64);
    client.update_validator_profile(&node, &pool);
    let pool_id = crate::symbol_to_asset_id(&pool);
    assert!(client.is_data_fresh(&pool_id));
}



// ═════════════════════════════════════════════════════════════════════════════
// Liquidity depth gate tests - flash-loan manipulation protection
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn test_telemetry_update_rejects_thin_pool_depth() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let asset: AssetId = 3897123275; // NGN
    client.add_corridor_fees(
        &asset,
        &(crate::validation::MIN_POOL_VOLUME_DEPTH - 1),
        &0u64,
    );

    let result = client.try_update_heartbeat(&asset, &admin);
    assert_eq!(result, Err(Ok(ContractError::InsufficientLiquidityDepth)));
    assert!(client.get_last_update_timestamp(&asset).is_none());
}

#[test]
fn test_telemetry_update_accepts_sufficient_pool_depth() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let asset: AssetId = 2654435761; // KES
    client.add_corridor_fees(
        &asset,
        &crate::validation::MIN_POOL_VOLUME_DEPTH,
        &0u64,
    );

    client.update_heartbeat(&asset, &admin);
    assert!(client.is_data_fresh(&asset));
}

#[test]
fn test_validator_profile_rejects_thin_pool_even_with_sufficient_bond() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let node = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    client.stake_and_register(&node, &crate::validation::PREMIUM_POOL_MIN_STAKE);
    let pool = symbol_short!("USDC");
    let asset = crate::symbol_to_asset_id(&pool);
    client.add_corridor_fees(
        &asset,
        &(crate::validation::MIN_POOL_VOLUME_DEPTH - 1),
        &0u64,
    );

    let before = client.get_last_update_timestamp(&asset);
    let result = client.try_update_validator_profile(&node, &pool);
    assert_eq!(result, Err(Ok(ContractError::InsufficientLiquidityDepth)));
    assert_eq!(client.get_last_update_timestamp(&asset), before);
}

#[test]
fn test_configured_volume_metrics_can_satisfy_liquidity_gate() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    let asset: AssetId = 4026531840; // GHS
    client.set_asset_feed_metrics(
        &admin,
        &asset,
        &crate::validation::MIN_POOL_VOLUME_SCORE,
        &200,
        &soroban_sdk::vec![&env, admin.clone()],
    );

    client.update_heartbeat(&asset, &admin);
    assert!(client.is_data_fresh(&asset));
}

// ═════════════════════════════════════════════════════════════════════════════
// Emergency Key Revocation tests (multi-sig coordinator group)
// ═════════════════════════════════════════════════════════════════════════════

// ═══════════════════════════════════════════════════════════════════════════
// Ephemeral Ballot Lifecycle tests (Issue #484)
// ═══════════════════════════════════════════════════════════════════════════


#[test]
fn test_propose_creates_ballot_in_temp_storage() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let target = soroban_sdk::Address::generate(&env);
    let replacement = soroban_sdk::Address::generate(&env);
    client.initialize(&admin);
    client.register_signer(&admin, &admin);

    assert!(client.get_revocation_ballot().is_none());
    client.propose_revocation(&admin, &target, &replacement);
    assert!(client.get_revocation_ballot().is_some());
}

#[test]
fn test_duplicate_proposal_blocked() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let target = soroban_sdk::Address::generate(&env);
    let replacement = soroban_sdk::Address::generate(&env);
    client.initialize(&admin);
    client.register_signer(&admin, &admin);

    client.propose_revocation(&admin, &target, &replacement);
    let result = client.try_propose_revocation(&admin, &target, &replacement);
    assert_eq!(result, Err(Ok(ContractError::ProposalAlreadyActive)));
}

#[test]
fn test_vote_without_proposal_returns_error() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    client.initialize(&admin);

    let result = client.try_vote_revocation(&admin, &u64::MAX);
    assert_eq!(result, Err(Ok(ContractError::NoActiveProposal)));
}

#[test]
fn test_vote_records_in_temp_storage() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let voter = soroban_sdk::Address::generate(&env);
    let voter2 = soroban_sdk::Address::generate(&env);
    let target = soroban_sdk::Address::generate(&env);
    let replacement = soroban_sdk::Address::generate(&env);
    client.initialize(&admin);
    // Register two signers so threshold = 2; one vote leaves ballot open
    client.register_signer(&voter, &admin);
    client.register_signer(&voter2, &admin);

    client.propose_revocation(&admin, &target, &replacement);
    client.vote_revocation(&voter, &u64::MAX);

    let ballot = client.get_revocation_ballot().unwrap();
    assert!(ballot.votes.contains_key(voter));
}

#[test]
fn test_double_vote_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let voter = soroban_sdk::Address::generate(&env);
    let voter2 = soroban_sdk::Address::generate(&env);
    let target = soroban_sdk::Address::generate(&env);
    let replacement = soroban_sdk::Address::generate(&env);
    client.initialize(&admin);
    // Register two signers so threshold = 2; single vote leaves ballot open
    client.register_signer(&voter, &admin);
    client.register_signer(&voter2, &admin);

    client.propose_revocation(&admin, &target, &replacement);
    client.vote_revocation(&voter, &u64::MAX);
    let result = client.try_vote_revocation(&voter, &u64::MAX);
    assert_eq!(result, Err(Ok(ContractError::AlreadyVoted)));
}

#[test]
fn test_finalize_consensus_removes_ballot() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let target = soroban_sdk::Address::generate(&env);
    let replacement = soroban_sdk::Address::generate(&env);
    client.initialize(&admin);
    client.register_signer(&admin, &admin);

    client.propose_revocation(&admin, &target, &replacement);
    assert!(client.get_revocation_ballot().is_some());

    client.finalize_consensus();
    assert!(client.get_revocation_ballot().is_none());
}

#[test]
fn test_finalize_consensus_safe_with_no_ballot() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    client.initialize(&admin);

    // finalize_consensus when no ballot exists must not panic
    client.finalize_consensus();
    assert!(client.get_revocation_ballot().is_none());
}

// ═══════════════════════════════════════════════════════════════════════════
// Two-Phase Admin Key Change tests (Issue #493)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_propose_admin_change_creates_pending_record() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let new_admin = soroban_sdk::Address::generate(&env);
    client.initialize(&admin);

    assert!(client.get_pending_admin_change().is_none());
    client.propose_admin_change(&admin, &new_admin);
    let proposal = client.get_pending_admin_change().unwrap();
    assert_eq!(proposal.new_admin, new_admin);
    assert_eq!(proposal.proposer, admin);
}

#[test]
fn test_duplicate_admin_change_proposal_blocked() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let new_admin = soroban_sdk::Address::generate(&env);
    client.initialize(&admin);

    client.propose_admin_change(&admin, &new_admin);
    let result = client.try_propose_admin_change(&admin, &new_admin);
    assert_eq!(result, Err(Ok(ContractError::AdminChangePending)));
}

#[test]
fn test_non_admin_cannot_propose_admin_change() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let attacker = soroban_sdk::Address::generate(&env);
    let new_admin = soroban_sdk::Address::generate(&env);
    client.initialize(&admin);

    let result = client.try_propose_admin_change(&attacker, &new_admin);
    assert_eq!(result, Err(Ok(ContractError::NotAdmin)));
}

#[test]
fn test_countersign_executes_admin_change_immediately() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let new_admin = soroban_sdk::Address::generate(&env);
    let cosigner = soroban_sdk::Address::generate(&env);
    client.initialize(&admin);
    client.register_signer(&cosigner, &admin);

    client.propose_admin_change(&admin, &new_admin);
    client.countersign_admin_change(&cosigner);

    // Admin should now be updated
    let data = client.get_data();
    assert_eq!(data.admin, new_admin);
    // Pending proposal should be cleared
    assert!(client.get_pending_admin_change().is_none());
}

#[test]
fn test_cosigner_cannot_be_proposer() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let new_admin = soroban_sdk::Address::generate(&env);
    client.initialize(&admin);

    client.propose_admin_change(&admin, &new_admin);
    // Admin tries to countersign their own proposal — must be rejected
    let result = client.try_countersign_admin_change(&admin);
    assert_eq!(result, Err(Ok(ContractError::CosignerCannotBeProposer)));
}

#[test]
fn test_timelock_path_rejected_before_delay() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let new_admin = soroban_sdk::Address::generate(&env);
    client.initialize(&admin);

    client.propose_admin_change(&admin, &new_admin);
    // Attempt immediate execution without waiting
    let result = client.try_execute_admin_change_by_timelock(&admin);
    assert_eq!(result, Err(Ok(ContractError::AdminChangeTimelockNotSatisfied)));
}

#[test]
fn test_timelock_path_succeeds_after_delay() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let new_admin = soroban_sdk::Address::generate(&env);
    client.initialize(&admin);

    client.propose_admin_change(&admin, &new_admin);

    // Fast-forward 24 hours
    advance_ledger_timestamp(&env, 24 * 60 * 60);

    client.execute_admin_change_by_timelock(&admin);

    let data = client.get_data();
    assert_eq!(data.admin, new_admin);
    assert!(client.get_pending_admin_change().is_none());
}

#[test]
fn test_cancel_admin_change_clears_proposal() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let new_admin = soroban_sdk::Address::generate(&env);
    client.initialize(&admin);

    client.propose_admin_change(&admin, &new_admin);
    assert!(client.get_pending_admin_change().is_some());

    client.cancel_admin_change(&admin);
    assert!(client.get_pending_admin_change().is_none());
    // Admin key is unchanged
    assert_eq!(client.get_data().admin, admin);
}

#[test]
fn test_execute_timelock_without_pending_returns_error() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    client.initialize(&admin);

    let result = client.try_execute_admin_change_by_timelock(&admin);
    assert_eq!(result, Err(Ok(ContractError::NoAdminChangePending)));
}

#[test]
fn test_countersign_without_pending_returns_error() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let cosigner = soroban_sdk::Address::generate(&env);
    client.initialize(&admin);
    client.register_signer(&cosigner, &admin);

    let result = client.try_countersign_admin_change(&cosigner);
    assert_eq!(result, Err(Ok(ContractError::NoAdminChangePending)));
}

#[test]
fn test_node_profile_ttl_extension() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    let node = soroban_sdk::Address::generate(&env);
    let treasury = soroban_sdk::Address::generate(&env);
    client.initialize(&admin, &treasury);

    // Upsert the profile
    client.upsert_node_profile(&admin, &node, &100, &99);

    // Retrieve the profiles map and check that it was successfully retrieved.
    let rate = client.get_latest_rate(&node);
    assert_eq!(rate, 100);
}

