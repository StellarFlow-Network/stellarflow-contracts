#![cfg(test)]
use super::*;
use soroban_sdk::{
    testutils::Address as _, Address, Bytes, Env,
};

// ─── Helpers ────────────────────────────────────────────────────────────────

fn setup() -> (Env, Address, Address, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);

    // Deploy a stellar asset contract as the flash loan token
    let token_client_addr = Address::generate(&env);
    let token_id = env.register_stellar_asset_contract(token_client_addr.clone());

    // Deploy the flash loan contract
    let contract_addr = env.register_contract(None, FlashLoanEngine);

    // Mint tokens to the flash loan contract (liquidity pool)
    let mint_client = token::StellarAssetClient::new(&env, &token_id);
    mint_client.mint(&contract_addr, &1_000_000_000);

    (env, admin, token_id, treasury, contract_addr)
}

fn setup_initialized() -> (Env, Address, Address, Address, Address) {
    let (env, admin, token_id, treasury, contract_addr) = setup();
    let client = FlashLoanEngineClient::new(&env, &contract_addr);

    client.initialize(&admin, &token_id, &treasury);

    (env, admin, token_id, treasury, contract_addr)
}

// ─── Initialization Tests ───────────────────────────────────────────────────

#[test]
fn test_initialize_sets_state() {
    let (env, admin, token_id, treasury, contract_addr) = setup();
    let client = FlashLoanEngineClient::new(&env, &contract_addr);

    client.initialize(&admin, &token_id, &treasury);

    assert_eq!(client.get_admin(), admin);
    assert_eq!(client.get_token(), token_id);
    assert_eq!(client.get_treasury(), treasury);

    let fee_params = client.get_fee_params();
    assert_eq!(fee_params.base_fee_bps, 9);
    assert_eq!(fee_params.protocol_fee_bps, 1);
    assert_eq!(fee_params.max_discount_bps, 500);
    assert_eq!(fee_params.min_fee, 1);

    // Default tiers should be configured
    assert_eq!(client.get_tier_count(), 4);
}

#[test]
fn test_double_initialize_fails() {
    let (env, admin, token_id, treasury, contract_addr) = setup();
    let client = FlashLoanEngineClient::new(&env, &contract_addr);

    client.initialize(&admin, &token_id, &treasury);

    let result = client.try_initialize(&admin, &token_id, &treasury);
    assert_eq!(result, Err(Ok(Error::AlreadyInitialized)));
}

// ─── Fee Parameter Tests ────────────────────────────────────────────────────

#[test]
fn test_set_fee_params_by_admin() {
    let (env, admin, _token_id, _treasury, contract_addr) = setup_initialized();
    let client = FlashLoanEngineClient::new(&env, &contract_addr);

    let params = client.set_fee_params(&admin, &50, &5, &30, &10);
    assert_eq!(params.base_fee_bps, 50);
    assert_eq!(params.protocol_fee_bps, 5);
    assert_eq!(params.max_discount_bps, 30);
    assert_eq!(params.min_fee, 10);

    let stored = client.get_fee_params();
    assert_eq!(stored.base_fee_bps, 50);
    assert_eq!(stored.protocol_fee_bps, 5);
}

#[test]
fn test_set_fee_params_rejects_zero_base() {
    let (env, admin, _token_id, _treasury, contract_addr) = setup_initialized();
    let client = FlashLoanEngineClient::new(&env, &contract_addr);

    let result = client.try_set_fee_params(&admin, &0, &0, &0, &0);
    assert_eq!(result, Err(Ok(Error::InvalidFeeParams)));
}

#[test]
fn test_set_fee_params_rejects_protocol_exceeds_base() {
    let (env, admin, _token_id, _treasury, contract_addr) = setup_initialized();
    let client = FlashLoanEngineClient::new(&env, &contract_addr);

    let result = client.try_set_fee_params(&admin, &10, &20, &5, &1);
    assert_eq!(result, Err(Ok(Error::InvalidFeeParams)));
}

#[test]
fn test_set_fee_params_rejects_discount_exceeds_base() {
    let (env, admin, _token_id, _treasury, contract_addr) = setup_initialized();
    let client = FlashLoanEngineClient::new(&env, &contract_addr);

    let result = client.try_set_fee_params(&admin, &10, &5, &15, &1);
    assert_eq!(result, Err(Ok(Error::InvalidFeeParams)));
}

#[test]
fn test_set_fee_params_rejects_negative_min_fee() {
    let (env, admin, _token_id, _treasury, contract_addr) = setup_initialized();
    let client = FlashLoanEngineClient::new(&env, &contract_addr);

    let result = client.try_set_fee_params(&admin, &10, &5, &5, &-1);
    assert_eq!(result, Err(Ok(Error::InvalidFeeParams)));
}

#[test]
fn test_non_admin_cannot_set_fee_params() {
    let (env, _admin, _token_id, _treasury, contract_addr) = setup_initialized();
    let client = FlashLoanEngineClient::new(&env, &contract_addr);

    let non_admin = Address::generate(&env);
    let result = client.try_set_fee_params(&non_admin, &50, &5, &300, &10);
    assert_eq!(result, Err(Ok(Error::NotAdmin)));
}

// ─── Tier Configuration Tests ───────────────────────────────────────────────

#[test]
fn test_default_tiers_are_configured() {
    let (env, _admin, _token_id, _treasury, contract_addr) = setup_initialized();
    let client = FlashLoanEngineClient::new(&env, &contract_addr);

    let bronze = client.get_tier_config(&DiscountTier::Bronze).unwrap();
    assert_eq!(bronze.min_volume, 10_000);
    assert_eq!(bronze.discount_bps, 100);

    let silver = client.get_tier_config(&DiscountTier::Silver).unwrap();
    assert_eq!(silver.min_volume, 100_000);
    assert_eq!(silver.discount_bps, 250);

    let gold = client.get_tier_config(&DiscountTier::Gold).unwrap();
    assert_eq!(gold.min_volume, 1_000_000);
    assert_eq!(gold.discount_bps, 400);

    let platinum = client.get_tier_config(&DiscountTier::Platinum).unwrap();
    assert_eq!(platinum.min_volume, 10_000_000);
    assert_eq!(platinum.discount_bps, 500);
}

#[test]
fn test_set_tier_config_by_admin() {
    let (env, admin, _token_id, _treasury, contract_addr) = setup_initialized();
    let client = FlashLoanEngineClient::new(&env, &contract_addr);

    let config = client.set_tier_config(
        &admin,
        &DiscountTier::Gold,
        &500_000,
        &350,
        &350,
    );
    assert_eq!(config.min_volume, 500_000);
    assert_eq!(config.discount_bps, 350);

    let stored = client.get_tier_config(&DiscountTier::Gold).unwrap();
    assert_eq!(stored.min_volume, 500_000);
}

#[test]
fn test_set_tier_config_rejects_discount_exceeds_max() {
    let (env, admin, _token_id, _treasury, contract_addr) = setup_initialized();
    let client = FlashLoanEngineClient::new(&env, &contract_addr);

    let result = client.try_set_tier_config(
        &admin,
        &DiscountTier::Gold,
        &500_000,
        &400,
        &300,
    );
    assert_eq!(result, Err(Ok(Error::InvalidTierConfig)));
}

#[test]
fn test_non_admin_cannot_set_tier_config() {
    let (env, _admin, _token_id, _treasury, contract_addr) = setup_initialized();
    let client = FlashLoanEngineClient::new(&env, &contract_addr);

    let non_admin = Address::generate(&env);
    let result = client.try_set_tier_config(
        &non_admin,
        &DiscountTier::Gold,
        &500_000,
        &350,
        &350,
    );
    assert_eq!(result, Err(Ok(Error::NotAdmin)));
}

// ─── Borrower Profile Tests ─────────────────────────────────────────────────

#[test]
fn test_new_borrower_starts_at_none_tier() {
    let (env, _admin, _token_id, _treasury, contract_addr) = setup_initialized();
    let client = FlashLoanEngineClient::new(&env, &contract_addr);

    let borrower = Address::generate(&env);
    let profile = client.get_borrower_profile(&borrower);

    assert_eq!(profile.tier, DiscountTier::None);
    assert_eq!(profile.total_volume, 0);
    assert_eq!(profile.borrow_count, 0);
    assert_eq!(profile.last_borrow_ts, 0);
}

#[test]
fn test_effective_tier_starts_at_none() {
    let (env, _admin, _token_id, _treasury, contract_addr) = setup_initialized();
    let client = FlashLoanEngineClient::new(&env, &contract_addr);

    let borrower = Address::generate(&env);
    assert_eq!(client.get_effective_tier(&borrower), DiscountTier::None);
}

#[test]
fn test_quote_fee_for_new_borrower_uses_base_rate() {
    let (env, _admin, _token_id, _treasury, contract_addr) = setup_initialized();
    let client = FlashLoanEngineClient::new(&env, &contract_addr);

    let borrower = Address::generate(&env);
    let fee = client.quote_fee(&borrower, &100_000);

    // Base fee: 100_000 * 9 / 10_000 = 90
    // No discount for None tier
    assert_eq!(fee, 90);
}

#[test]
fn test_quote_fee_respects_min_fee() {
    let (env, _admin, _token_id, _treasury, contract_addr) = setup_initialized();
    let client = FlashLoanEngineClient::new(&env, &contract_addr);

    let borrower = Address::generate(&env);
    // Very small amount: 100 * 9 / 10_000 = 0, but min_fee is 1
    let fee = client.quote_fee(&borrower, &100);
    assert_eq!(fee, 1);
}

// ─── Flash Loan Execution Tests ─────────────────────────────────────────────

#[test]
fn test_flash_loan_count_starts_at_zero() {
    let (env, _admin, _token_id, _treasury, contract_addr) = setup_initialized();
    let client = FlashLoanEngineClient::new(&env, &contract_addr);

    assert_eq!(client.get_flash_loan_count(), 0);
}

#[test]
fn test_flash_loan_rejects_zero_amount() {
    let (env, admin, token_id, treasury, contract_addr) = setup();
    let borrower_contract = env.register_contract(None, FlashLoanEngine);
    let client = FlashLoanEngineClient::new(&env, &contract_addr);

    client.initialize(&admin, &token_id, &treasury);

    let result = client.try_flash_borrow(&borrower_contract, &0, &Bytes::new(&env));
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

#[test]
fn test_flash_loan_rejects_negative_amount() {
    let (env, admin, token_id, treasury, contract_addr) = setup();
    let borrower_contract = env.register_contract(None, FlashLoanEngine);
    let client = FlashLoanEngineClient::new(&env, &contract_addr);

    client.initialize(&admin, &token_id, &treasury);

    let result = client.try_flash_borrow(&borrower_contract, &-100, &Bytes::new(&env));
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

#[test]
fn test_flash_loan_rejects_when_paused() {
    let (env, admin, _token_id, _treasury, contract_addr) = setup_initialized();
    let client = FlashLoanEngineClient::new(&env, &contract_addr);
    let borrower_contract = env.register_contract(None, FlashLoanEngine);

    client.set_paused(&admin, &true);

    let result = client.try_flash_borrow(&borrower_contract, &1000, &Bytes::new(&env));
    assert_eq!(result, Err(Ok(Error::ContractPaused)));
}

// ─── Pause Controls Tests ───────────────────────────────────────────────────

#[test]
fn test_set_paused_by_admin() {
    let (env, admin, _token_id, _treasury, contract_addr) = setup_initialized();
    let client = FlashLoanEngineClient::new(&env, &contract_addr);

    assert!(!client.is_paused());
    client.set_paused(&admin, &true);
    assert!(client.is_paused());
    client.set_paused(&admin, &false);
    assert!(!client.is_paused());
}

#[test]
fn test_non_admin_cannot_set_paused() {
    let (env, _admin, _token_id, _treasury, contract_addr) = setup_initialized();
    let client = FlashLoanEngineClient::new(&env, &contract_addr);

    let non_admin = Address::generate(&env);
    let result = client.try_set_paused(&non_admin, &true);
    assert_eq!(result, Err(Ok(Error::NotAdmin)));
}

// ─── Admin Transfer Tests ───────────────────────────────────────────────────

#[test]
fn test_transfer_admin() {
    let (env, admin, _token_id, _treasury, contract_addr) = setup_initialized();
    let client = FlashLoanEngineClient::new(&env, &contract_addr);

    let new_admin = Address::generate(&env);
    client.transfer_admin(&admin, &new_admin);

    assert_eq!(client.get_admin(), new_admin);
}

#[test]
fn test_non_admin_cannot_transfer_admin() {
    let (env, _admin, _token_id, _treasury, contract_addr) = setup_initialized();
    let client = FlashLoanEngineClient::new(&env, &contract_addr);

    let non_admin = Address::generate(&env);
    let result = client.try_transfer_admin(&non_admin, &Address::generate(&env));
    assert_eq!(result, Err(Ok(Error::NotAdmin)));
}

// ─── Fee Computation Unit Tests ─────────────────────────────────────────────

#[test]
fn test_compute_fee_base_rate() {
    let params = FeeParams {
        base_fee_bps: 9,
        protocol_fee_bps: 1,
        max_discount_bps: 500,
        min_fee: 1,
    };

    // 100_000 * 9 / 10_000 = 90
    let fee = FlashLoanEngine::compute_fee(&params, 100_000, 0);
    assert_eq!(fee, 90);
}

#[test]
fn test_compute_fee_with_discount() {
    let params = FeeParams {
        base_fee_bps: 9,
        protocol_fee_bps: 1,
        max_discount_bps: 500,
        min_fee: 1,
    };

    // Base fee = 90, discount = 100 bps = 1%
    // Discount amount = 90 * 100 / 10_000 = 0 (integer math)
    let fee = FlashLoanEngine::compute_fee(&params, 100_000, 100);
    assert_eq!(fee, 90);
}

#[test]
fn test_compute_fee_with_large_discount() {
    let params = FeeParams {
        base_fee_bps: 9,
        protocol_fee_bps: 1,
        max_discount_bps: 500,
        min_fee: 1,
    };

    // Base fee = 1_000_000 * 9 / 10_000 = 900
    // 500 bps discount: 900 * 500 / 10_000 = 45
    let fee = FlashLoanEngine::compute_fee(&params, 1_000_000, 500);
    assert_eq!(fee, 855);
}

#[test]
fn test_compute_fee_discount_capped() {
    let params = FeeParams {
        base_fee_bps: 9,
        protocol_fee_bps: 1,
        max_discount_bps: 200,
        min_fee: 1,
    };

    // Even if we pass 500 bps discount, it's capped at 200
    // Base fee = 1_000_000 * 9 / 10_000 = 900
    // Discount (capped 200): 900 * 200 / 10_000 = 18
    let fee = FlashLoanEngine::compute_fee(&params, 1_000_000, 500);
    assert_eq!(fee, 882);
}

#[test]
fn test_compute_fee_enforces_minimum() {
    let params = FeeParams {
        base_fee_bps: 9,
        protocol_fee_bps: 1,
        max_discount_bps: 500,
        min_fee: 10,
    };

    // Base fee = 1 * 9 / 10_000 = 0, but min_fee is 10
    let fee = FlashLoanEngine::compute_fee(&params, 1, 0);
    assert_eq!(fee, 10);
}

// ─── Tier Computation Tests ─────────────────────────────────────────────────

#[test]
fn test_tier_ordinal_mapping() {
    assert_eq!(FlashLoanEngine::tier_to_ordinal(DiscountTier::None), 0);
    assert_eq!(FlashLoanEngine::tier_to_ordinal(DiscountTier::Bronze), 0);
    assert_eq!(FlashLoanEngine::tier_to_ordinal(DiscountTier::Silver), 1);
    assert_eq!(FlashLoanEngine::tier_to_ordinal(DiscountTier::Gold), 2);
    assert_eq!(FlashLoanEngine::tier_to_ordinal(DiscountTier::Platinum), 3);
}

// ─── Integration: Flash Loan Full Flow ──────────────────────────────────────

#[test]
fn test_flash_loan_insufficient_liquidity() {
    let (env, admin, token_id, treasury, contract_addr) = setup();
    let client = FlashLoanEngineClient::new(&env, &contract_addr);
    client.initialize(&admin, &token_id, &treasury);

    let borrower_contract = env.register_contract(None, FlashLoanEngine);

    // Try to borrow more than the contract holds
    let result = client.try_flash_borrow(&borrower_contract, &2_000_000_000, &Bytes::new(&env));
    assert_eq!(result, Err(Ok(Error::InsufficientLiquidity)));
}

// ─── Edge Case Tests ────────────────────────────────────────────────────────

#[test]
fn test_get_tier_config_for_bronze_returns_some() {
    let (env, _admin, _token_id, _treasury, contract_addr) = setup_initialized();
    let client = FlashLoanEngineClient::new(&env, &contract_addr);

    let bronze = client.get_tier_config(&DiscountTier::Bronze);
    assert!(bronze.is_some());
}

#[test]
fn test_flash_loan_record_not_found() {
    let (env, _admin, _token_id, _treasury, contract_addr) = setup_initialized();
    let client = FlashLoanEngineClient::new(&env, &contract_addr);

    let record = client.get_flash_loan_record(&999);
    assert!(record.is_none());
}

#[test]
fn test_fee_params_default_values() {
    let (env, _admin, _token_id, _treasury, contract_addr) = setup_initialized();
    let client = FlashLoanEngineClient::new(&env, &contract_addr);

    let params = client.get_fee_params();
    assert_eq!(params.base_fee_bps, 9);
    assert_eq!(params.protocol_fee_bps, 1);
    assert_eq!(params.max_discount_bps, 500);
    assert_eq!(params.min_fee, 1);
}
