#![cfg(test)]

use soroban_sdk::{
    testutils::Address as _, testutils::Ledger as _, Address, Env, IntoVal, Symbol, Val, Vec,
};
use stellarflow_contracts::{
    orders::limit::AssetPair,
    vaults::interest::{InterestRateConfig, PoolState},
    TimeLockedUpgradeContract, TimeLockedUpgradeContractClient,
};

fn setup_env() -> (Env, TimeLockedUpgradeContractClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    client.initialize(&admin, &admin);
    (env, client, admin)
}

#[test]
fn test_interest_rate_controller_utilization() {
    let (_, client, _) = setup_env();
    let u = client.calculate_utilization(&80, &20);
    assert_eq!(u, 2000); // 20%
}

#[test]
fn test_interest_rate_controller_rates() {
    let (_, client, _) = setup_env();
    let config = InterestRateConfig {
        base_rate_bps: 200,            // 2%
        multiplier_bps: 1000,          // 10%
        jump_multiplier_bps: 5000,     // 50%
        optimal_utilization_bps: 8000, // 80%
        ledgers_per_year: 6307200,
    };

    // Below optimal: 50% utilization
    let rate_50 = client.calculate_interest_rate(&5000, &config);
    assert_eq!(rate_50, 700); // 2% + 5% = 7%

    // Above optimal: 90% utilization
    let rate_90 = client.calculate_interest_rate(&9000, &config);
    assert_eq!(rate_90, 1500); // 2% + 8% (base slope) + 5% (jump slope) = 15%
}

#[test]
fn test_interest_rate_controller_accrue() {
    let (env, client, _) = setup_env();
    let config = InterestRateConfig {
        base_rate_bps: 200,
        multiplier_bps: 1000,
        jump_multiplier_bps: 5000,
        optimal_utilization_bps: 8000,
        ledgers_per_year: 6307200,
    };

    let pool = PoolState {
        cash: 20,
        borrows: 80,
        last_accrued_ledger: 0,
        accumulated_interest_index: 1_000_000_000_000_000_000, // 1.0 scaled
    };

    env.ledger().set_sequence(1000);
    let (updated_pool, accrued) = client.accrue_interest(&pool, &config);
    assert!(accrued > 0);
    assert_eq!(updated_pool.last_accrued_ledger, 1000);
}

#[test]
fn test_bytesn_optimization_hashing() {
    let (env, client, _) = setup_env();
    let addr = Address::generate(&env);
    let hashed_addr = client.optimize_address(&addr);
    assert_eq!(hashed_addr.len(), 32);

    let s = soroban_sdk::String::from_str(&env, "event_topic");
    let hashed_str = client.optimize_string(&s);
    assert_eq!(hashed_str.len(), 32);
}

#[test]
fn test_liquidity_depth_lifecycle() {
    let (env, client, _) = setup_env();
    let sell_issuer = Address::generate(&env);
    let buy_issuer = Address::generate(&env);
    let sell_asset = env.register_stellar_asset_contract(sell_issuer);
    let buy_asset = env.register_stellar_asset_contract(buy_issuer);

    let maker = Address::generate(&env);
    soroban_sdk::token::StellarAssetClient::new(&env, &sell_asset).mint(&maker, &2_000);

    let pair = AssetPair {
        sell_asset: sell_asset.clone(),
        buy_asset: buy_asset.clone(),
    };

    // Ask order
    let order = client.place_limit_order(&maker, &pair, &10_000_000, &1_000);
    let is_bid = sell_asset > buy_asset;
    let depth = client.get_liquidity_depth(&pair, &is_bid);
    assert_eq!(depth.len(), 1);
    assert_eq!(depth.get(0).unwrap().volume, 1_000);

    client.cancel_limit_order(&maker, &order.id);
    let depth_after = client.get_liquidity_depth(&pair, &is_bid);
    assert_eq!(depth_after.len(), 0);
}

#[test]
fn test_auth_context_isolation_guard() {
    let (env, client, _) = setup_env();
    let expected = Address::generate(&env);
    client.enforce_auth_isolation(&expected);

    // Call execute_isolated_call towards self
    let args: Vec<Val> = Vec::new(&env);
    let result = client.try_execute_isolated_call(
        &client.address,
        &Symbol::new(&env, "get_recovery_key"),
        &args,
    );
    assert!(result.is_ok());
}

#[test]
fn test_bridge_validator_double_sign_slashing() {
    use ed25519_dalek::{Signer, SigningKey};
    use soroban_sdk::BytesN;
    use stellarflow_contracts::bridge::slashing::{
        AttestationPayload, DoubleSignProof,
    };
    use stellarflow_contracts::bridge::relayer::bridge_message_digest;

    let (env, client, admin) = setup_env();

    let signing_key = SigningKey::from_bytes(&[99u8; 32]);
    let validator_pubkey = BytesN::from_array(&env, &signing_key.verifying_key().to_bytes());

    // Register bridge validator
    client.add_bridge_validator(&admin, &validator_pubkey);

    // Stake 10_000 collateral deposit
    let collateral = 10_000i128;
    client.stake_bridge_validator(&validator_pubkey, &collateral);
    assert_eq!(client.get_bridge_validator_collateral(&validator_pubkey), collateral);

    let source_chain_id = 1u32;
    let nonce = 42u64;
    let recipient_1 = Address::generate(&env);
    let recipient_2 = Address::generate(&env);

    let payload_1 = AttestationPayload {
        proof_hash: BytesN::from_array(&env, &[1u8; 32]),
        recipient: recipient_1,
        amount: 500,
    };
    let digest_1 = bridge_message_digest(
        &env,
        source_chain_id,
        nonce,
        &payload_1.proof_hash,
        &payload_1.recipient,
        payload_1.amount,
    );
    let sig_1 = signing_key.sign(&digest_1.to_array());
    let signature_1 = BytesN::from_array(&env, &sig_1.to_bytes());

    let payload_2 = AttestationPayload {
        proof_hash: BytesN::from_array(&env, &[2u8; 32]),
        recipient: recipient_2,
        amount: 1_000,
    };
    let digest_2 = bridge_message_digest(
        &env,
        source_chain_id,
        nonce,
        &payload_2.proof_hash,
        &payload_2.recipient,
        payload_2.amount,
    );
    let sig_2 = signing_key.sign(&digest_2.to_array());
    let signature_2 = BytesN::from_array(&env, &sig_2.to_bytes());

    let proof = DoubleSignProof {
        validator: validator_pubkey.clone(),
        source_chain_id,
        nonce,
        payload_1,
        signature_1,
        payload_2,
        signature_2,
    };

    // Slash 100% of offending validator's staked collateral
    let slashed = client.submit_double_sign_proof(&proof);
    assert_eq!(slashed, collateral);

    // Collateral is now 0 (100% slashed)
    assert_eq!(client.get_bridge_validator_collateral(&validator_pubkey), 0);

    // Offending validator is permanently removed and cannot be re-added
    assert!(client.try_add_bridge_validator(&admin, &validator_pubkey).is_err());
}

#[test]
fn test_zk_public_input_verification_guard() {
    use soroban_sdk::BytesN;
    use stellarflow_contracts::zk::public_input_guard::{
        DepositNotePublicInputs, SubmittedDepositParameters,
    };
    use stellarflow_contracts::zk::merkle::insert_deposit;
    use stellarflow_contracts::ContractError;

    let (env, client, _) = setup_env();

    let recipient = Address::generate(&env);
    let commitment = BytesN::from_array(&env, &[33u8; 32]);
    let (_, legitimate_root) = insert_deposit(&env, commitment).unwrap();

    let nullifier = BytesN::from_array(&env, &[77u8; 32]);
    let fee = 100i128;

    let public_inputs = DepositNotePublicInputs {
        root: legitimate_root.clone(),
        nullifier_hash: nullifier.clone(),
        recipient: recipient.clone(),
        fee,
    };

    let submitted_params = SubmittedDepositParameters {
        expected_root: legitimate_root.clone(),
        expected_nullifier_hash: nullifier.clone(),
        expected_recipient: recipient.clone(),
        expected_fee: fee,
    };

    // Valid inputs strictly match contract state parameters
    assert!(client.try_verify_zk_deposit_public_inputs(&public_inputs, &submitted_params).is_ok());

    // Mismatched root must revert with ContractError::InvalidZKPublicInputs (code 84)
    let fake_root = BytesN::from_array(&env, &[99u8; 32]);
    let bad_public_inputs = DepositNotePublicInputs {
        root: fake_root,
        nullifier_hash: nullifier.clone(),
        recipient: recipient.clone(),
        fee,
    };
    let result = client.try_verify_zk_deposit_public_inputs(&bad_public_inputs, &submitted_params);
    assert_eq!(result, Err(Ok(ContractError::InvalidZKPublicInputs)));

    // Mismatched fee must revert with ContractError::InvalidZKPublicInputs
    let bad_fee_inputs = DepositNotePublicInputs {
        root: legitimate_root.clone(),
        nullifier_hash: nullifier.clone(),
        recipient: recipient.clone(),
        fee: 999,
    };
    let result_fee = client.try_verify_zk_deposit_public_inputs(&bad_fee_inputs, &submitted_params);
    assert_eq!(result_fee, Err(Ok(ContractError::InvalidZKPublicInputs)));
}

#[test]
fn test_adaptive_swap_fee_calculation() {
    let (_, client, _) = setup_env();

    // fbase = 25 BPS, Vsigma = 10, fscalar = 4 -> fswap = 25 + 40 = 65 BPS
    let fee = client.compute_adaptive_swap_fee(&25, &10, &4);
    assert_eq!(fee, 65);

    // High volatility: fbase = 30 BPS, Vsigma = 20, fscalar = 10 -> 30 + 200 = 230 BPS
    // Constrained to fswap <= 0.01 (100 BPS)
    let capped_fee = client.compute_adaptive_swap_fee(&30, &20, &10);
    assert_eq!(capped_fee, 100);
}

