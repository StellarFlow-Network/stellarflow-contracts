#![cfg(test)]

use super::*;
use soroban_sdk::{
    contract, contractimpl, symbol_short, testutils::Address as _, testutils::Events,
    testutils::Ledger, Address, Env, Symbol, vec,
};

#[soroban_sdk::contractevent]
pub struct TokenTransferEvent {
    pub from: Address,
    pub to: Address,
    pub amount: i128,
}

#[contract]
pub struct DummyToken;

#[contractimpl]
impl DummyToken {
    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        from.require_auth();
        env.events()
            .publish_event(&TokenTransferEvent { from, to, amount });
    }
}

fn setup() -> (Env, Address, PriceOracleClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    (env, contract_id, client)
}

fn set_admin(env: &Env, contract_id: &Address, admin: &Address) {
    env.as_contract(contract_id, || {
        crate::auth::_set_admin(env, &soroban_sdk::vec![env, admin.clone()]);
    });
}

fn add_provider(env: &Env, contract_id: &Address, provider: &Address) {
    env.as_contract(contract_id, || {
        crate::auth::_add_provider(env, provider);
    });
}

#[test]
fn test_get_index_price() {
    let env = Env::default();
    env.mock_all_auths();
    
    // Setup Oracle & Admin...
    // Add assets: NGN, GHS, CFA...
    // Set prices for NGN, GHS, CFA...

    let components = soroban_sdk::vec![
        &env,
        AssetWeight { asset: symbol_short!("NGN"), weight: 4000 }, // 40%
        AssetWeight { asset: symbol_short!("GHS"), weight: 3000 }, // 30%
        AssetWeight { asset: symbol_short!("CFA"), weight: 3000 }, // 30%
    ];

    let index_price = client.get_index_price(&components);
    
    // Assert the index_price equals the expected mathematical weighted average
}

#[test]
fn test_initialize_sets_admin_and_assets() {
    let (env, contract_id, client) = setup();
    let admin = Address::generate(&env);
    let pairs = soroban_sdk::vec![&env, symbol_short!("NGN"), symbol_short!("KES")];

    client.initialize(&admin, &pairs);

    env.as_contract(&contract_id, || {
        let admins = crate::auth::_get_admin(&env);
        assert_eq!(admins.len(), 1);
        assert_eq!(admins.get(0).unwrap(), admin);
    });
    assert_eq!(client.get_all_assets(), pairs);
}

#[test]
#[should_panic]
fn test_init_admin_panics_when_called_twice() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let first_admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let second_admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    client.init_admin(&first_admin);
    // Second call should panic with Error::AlreadyInitialized
    client.init_admin(&second_admin);
}

#[test]
fn test_get_price_existing_asset() {
    let env = Env::default();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    env.ledger().set_timestamp(1_234_567_890);
    env.ledger().set_sequence_number(1);

    let asset = symbol_short!("XLM");
    client.set_price(&asset, &1_000_000_i128, &6u32, &3600u64);

    let retrieved_price = client.get_price(&asset, &true);
    assert_eq!(retrieved_price.price, 1_000_000_i128);
    assert_eq!(retrieved_price.timestamp, 1_234_567_890);
    assert_eq!(retrieved_price.decimals, 6u32);
    assert_eq!(retrieved_price.provider, contract_id);
}

#[test]
fn test_get_price_nonexistent_asset() {
    let env = Env::default();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let asset = symbol_short!("BTC");

    let result = client.try_get_price(&asset, &true);
    assert!(result.is_err());
}

#[test]
fn test_get_price_after_update() {
    let env = Env::default();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let asset = symbol_short!("XLM");

    env.ledger().set_timestamp(1_234_567_890);
    env.ledger().set_sequence_number(1);
    client
        .try_set_price(&asset, &1_000_000_i128, &6u32, &3600u64)
        .unwrap()
        .unwrap();

    let initial = client.try_get_price(&asset, &true).unwrap().unwrap();
    assert_eq!(initial.price, 1_000_000_i128);
    assert_eq!(initial.timestamp, 1_234_567_890);

    env.ledger().set_timestamp(1_234_567);
    client.set_price(&asset, &1_500_i128, &2u32, &3_600u64);

    let updated = client.try_get_price(&asset, &true).unwrap().unwrap();
    assert_eq!(updated.price, 1_200_000_i128);
    assert_eq!(updated.timestamp, 1_234_567_900);
}

#[test]
fn test_get_price_with_status_marks_stale_entries() {
    let (env, _, client) = setup();
    let asset = symbol_short!("NGN");

    env.ledger().set_timestamp(1_000);
    client.set_price(&asset, &1_500_i128, &2u32, &100u64);

    env.ledger().set_timestamp(1_101);
    let result = client.get_price_with_status(&asset);

    assert_eq!(result.data.price, 1_500_i128);
    assert!(result.is_stale);
}

#[test]
fn test_update_price_rejects_untracked_asset() {
    let (env, contract_id, client) = setup();
    let admin = Address::generate(&env);
    let provider = Address::generate(&env);

    set_admin(&env, &contract_id, &admin);
    add_provider(&env, &contract_id, &provider);

    let result = client.try_update_price(
        &provider,
        &symbol_short!("BTC"),
        &50_000_i128,
        &6u32,
        &100u32,
        &3_600u64,
    );
    match result {
        Err(Ok(err)) => assert_eq!(err, Error::InvalidAssetSymbol),
        other => panic!("expected InvalidAssetSymbol, got {:?}", other),
    }
}

#[test]
fn test_update_price_rejects_non_provider() {
    let (env, contract_id, client) = setup();
    let admin = Address::generate(&env);
    let provider = Address::generate(&env);

    set_admin(&env, &contract_id, &admin);
    client.add_asset(&admin, &symbol_short!("NGN"));

    let result = client.try_update_price(
        &provider,
        &symbol_short!("NGN"),
        &1_000_i128,
        &6u32,
        &100u32,
        &3_600u64,
    );
    match result {
        Err(Ok(err)) => assert_eq!(err, Error::NotAuthorized),
        other => panic!("expected NotAuthorized, got {:?}", other),
    }
}

#[test]
fn test_update_price_rejects_flash_crash() {
    let (env, contract_id, client) = setup();
    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("NGN");

    set_admin(&env, &contract_id, &admin);
    add_provider(&env, &contract_id, &provider);

    client.set_price(&asset, &1_000_i128, &2u32, &3_600u64);

    let result = client.try_update_price(&provider, &asset, &1_200_i128, &2u32, &100u32, &3_600u64);
    match result {
        Err(Ok(err)) => assert_eq!(err, Error::FlashCrashDetected),
        other => panic!("expected FlashCrashDetected, got {:?}", other),
    }
}

#[test]
fn test_set_and_get_price_bounds() {
    let (env, contract_id, client) = setup();
    let admin = Address::generate(&env);
    let asset = symbol_short!("NGN");

    set_admin(&env, &contract_id, &admin);
    client.set_price_bounds(&admin, &asset, &500_i128, &2_000_i128);

    let bounds = client.get_price_bounds(&asset).unwrap();
    assert_eq!(bounds.min_price, 500_i128);
    assert_eq!(bounds.max_price, 2_000_i128);
}

#[test]
fn test_update_price_rejects_price_outside_bounds() {
    let (env, contract_id, client) = setup();
    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("NGN");

    env.ledger().set_timestamp(1_700_000_123);
    env.ledger().set_sequence_number(77);
    client.set_price(&asset, &950_i128, &2u32, &3600u64);

    let stored = client.get_price(&asset, &true);
    assert_eq!(stored.price, 950_i128);
    assert_eq!(stored.timestamp, 1_700_000_123);
    client.add_asset(&admin, &asset);
    client.set_price_bounds(&admin, &asset, &500_i128, &2_000_i128);
}
#[test]
#[should_panic(expected = "HostError")]
fn test_set_price_rejects_zero_price() {
    let env = Env::default();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let asset = symbol_short!("NGN");

    let result = client.try_update_price(&provider, &asset, &250_i128, &2u32, &100u32, &3_600u64);
    match result {
        Err(Ok(err)) => assert_eq!(err, Error::PriceOutOfBounds),
        other => panic!("expected PriceOutOfBounds, got {:?}", other),
    }
}

#[test]
#[should_panic(expected = "HostError")]
fn test_set_price_rejects_negative_price() {
    let env = Env::default();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let asset = symbol_short!("NGN");

    set_admin(&env, &contract_id, &admin);
    client.set_price_floor(&admin, &asset, &700_i128);

    assert_eq!(client.get_price_floor(&asset), Some(700_i128));
}

#[test]
fn test_update_price_rejects_price_below_floor() {
    let (env, contract_id, client) = setup();
    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("NGN");

    set_admin(&env, &contract_id, &admin);
    add_provider(&env, &contract_id, &provider);

    client.add_asset(&admin, &asset);
    client.set_price_floor(&admin, &asset, &700_i128);

    env.ledger().set_timestamp(1_700_000_500);
    env.ledger().set_sequence_number(2);
    client.update_price(&provider, &asset, &1_500_000_i128, &6u32, &100u32, &3600u64);

    let stored = client.get_price(&asset, &true);
    assert_eq!(stored.price, 1_500_000_i128);
    assert_eq!(stored.timestamp, 1_700_000_500);
    assert_eq!(stored.provider, provider); // not contract_id
}

#[test]
#[should_panic]
fn test_set_price_rejects_price_below_floor() {
    let (env, contract_id, client) = setup();
    let admin = Address::generate(&env);
    let asset = symbol_short!("NGN");

    set_admin(&env, &contract_id, &admin);
    client.set_price_floor(&admin, &asset, &700_i128);
    client.set_price(&asset, &650_i128, &2u32, &3_600u64);
    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
        crate::auth::_add_provider(&env, &provider);
    });

    client.add_asset(&admin, &asset);
    env.ledger().set_sequence_number(1);

    client.update_price(&provider, &asset, &1_000_i128, &6u32, &100u32, &3600u64);
    env.ledger().set_sequence_number(2);
    client.update_price(&provider, &asset, &1_020_i128, &6u32, &100u32, &3600u64);

    let stored = client.get_price(&asset, &true);
    assert_eq!(stored.price, 1_020_i128);
}

#[test]
fn test_remove_asset_clears_related_configuration() {
    let (env, contract_id, client) = setup();
    let admin = Address::generate(&env);
    let asset = symbol_short!("NGN");

    set_admin(&env, &contract_id, &admin);
    client.set_price(&asset, &1_000_i128, &2u32, &3_600u64);
    client.set_price_floor(&admin, &asset, &700_i128);
    client.set_price_bounds(&admin, &asset, &500_i128, &2_000_i128);

    client.remove_asset(&admin, &asset);
    let asset = symbol_short!("NGN");
    client.add_asset(&admin, &asset);
    // Note: unauthorized_address is NOT added as a provider, so it should fail with NotAuthorized
    client.add_asset(&admin, &asset);

    assert_eq!(client.get_price_safe(&asset), None);
    assert_eq!(client.get_price_floor(&asset), None);
    assert_eq!(client.get_price_bounds(&asset), None);
    assert!(client.get_all_assets().is_empty());
}

#[test]
fn test_rescue_tokens_admin_can_recover_assets() {
    let (env, contract_id, client) = setup();
    let token_id = env.register(DummyToken, ());
    let admin = Address::generate(&env);
    let recipient = Address::generate(&env);

    set_admin(&env, &contract_id, &admin);
    client.rescue_tokens(&admin, &token_id, &recipient, &1_000_i128);

    let events = env.events().all();
    let debug_str = alloc::format!("{:?}", events);
    assert!(debug_str.contains("rescue_tokens_event"));
}

#[test]
#[should_panic(expected = "Unauthorised")]
fn test_rescue_tokens_rejects_non_admin() {
    let (env, contract_id, client) = setup();
    let token_id = env.register(DummyToken, ());
    let admin = Address::generate(&env);
    let non_admin = Address::generate(&env);
    let recipient = Address::generate(&env);

    set_admin(&env, &contract_id, &admin);
    client.rescue_tokens(&non_admin, &token_id, &recipient, &1_000_i128);
}

#[test]
fn try_try_subscribe_to_price_updates() {
    let (env, contract_id, client) = setup();
    let admin1 = Address::generate(&env);
    let admin2 = Address::generate(&env);

    client.init_admin(&admin1);
    env.as_contract(&contract_id, || {
        crate::auth::_add_authorized(&env, &admin2);
    });

    assert!(client.toggle_pause(&admin1, &admin2));
    assert!(!client.toggle_pause(&admin1, &admin2));
    let asset = symbol_short!("ETH");
    let price: i128 = 1_000_000;
    match client.try_update_price(&provider, &asset, &price, &6u32, &100u32, &3600u64) {
        Err(Ok(e)) => assert_eq!(e, Error::InvalidAssetSymbol),
        other => panic!("expected InvalidAssetSymbol, got {:?}", other),
    }
}

#[test]
fn test_update_price_emits_event() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let provider = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let asset = symbol_short!("NGN");
    let price: i128 = 1_500_000;

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
        crate::auth::_add_provider(&env, &provider);
    });

    client.add_asset(&admin, &asset);

    env.ledger().set_timestamp(1_700_000_000);
    env.ledger().set_sequence_number(1);
    client.update_price(&provider, &asset, &price, &6u32, &100u32, &3600u64);

    let events = env.events().all();
    let debug_str = alloc::format!("{:?}", events);
    assert!(debug_str.contains("price_updated_event"));
}

#[test]
fn test_update_price_emits_cross_call_event_on_5pct_move() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let provider = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let asset = symbol_short!("NGN");

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
        crate::auth::_add_provider(&env, &provider);
    });

    client.add_asset(&admin, &asset);

    // Set a baseline price via set_price (bypasses provider check)
    // Use small values so the delta check (>50) doesn't fire on the update
    let old_price: i128 = 40;
    let new_price: i128 = 43; // 7.5% increase (750 bps > 500 threshold), delta=3 ≤ 50

    client.set_price(&asset, &old_price, &6u32, &3600u64);

    client.update_price(&provider, &asset, &new_price, &6u32, &100u32, &3600u64);

    let events = env.events().all();
    let debug_str = alloc::format!("{:?}", events);
    // "cross_call" topic must be present
    assert!(debug_str.contains("cross_call"), "expected cross_call event, got: {}", debug_str);
}

#[test]
fn test_update_price_no_cross_call_event_below_5pct() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let provider = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let asset = symbol_short!("NGN");

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
        crate::auth::_add_provider(&env, &provider);
    });

    client.add_asset(&admin, &asset);

    // 2% move — below 5% threshold, delta=1 ≤ 50
    let old_price: i128 = 50;
    let new_price: i128 = 51; // 2% increase

    client.set_price(&asset, &old_price, &6u32, &3600u64);
    client.update_price(&provider, &asset, &new_price, &6u32, &100u32, &3600u64);

    let events = env.events().all();
    let debug_str = alloc::format!("{:?}", events);
    assert!(!debug_str.contains("cross_call"), "cross_call should NOT fire below 5%");
}

#[test]
fn test_update_price_delta_limit_rejection_emits_anomaly_event() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let provider = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let asset = symbol_short!("NGN");

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
        crate::auth::_add_provider(&env, &provider);
    });

    client.add_asset(&admin, &asset);

    env.ledger().set_timestamp(1_700_100_000);
    env.ledger().set_sequence_number(1);
    client.update_price(&provider, &asset, &1_000_i128, &6u32, &100u32, &3600u64);

    env.ledger().set_timestamp(1_700_100_010);
    env.ledger().set_sequence_number(2);
    let result = client.try_update_price(&provider, &asset, &1_100_i128, &6u32, &100u32, &3600u64);
    assert!(result.is_ok());

    let events = env.events().all();
    let debug_str = alloc::format!("{:?}", events);
    assert!(debug_str.contains("price_anomaly_event"));

    let stored = client.get_price(&asset, &true);
    assert_eq!(stored.price, 1_000_i128);
}

#[test]
fn test_calculate_percentage_change_bps_for_increase() {
    assert_eq!(
        calculate_percentage_change_bps(1_000_000, 1_200_000),
        Some(2_000)
    );
}

#[test]
fn test_calculate_percentage_change_bps_for_drop() {
    assert_eq!(
        calculate_percentage_change_bps(1_000_000, 800_000),
        Some(-2_000)
    );
}

#[test]
fn test_calculate_percentage_difference_bps_is_absolute() {
    assert_eq!(
        calculate_percentage_difference_bps(1_000_000, 800_000),
        Some(2_000)
    );
    assert_eq!(
        calculate_percentage_difference_bps(1_000_000, 1_250_000),
        Some(2_500)
    );
}

#[test]
fn test_calculate_percentage_change_returns_none_for_zero_baseline() {
    assert_eq!(calculate_percentage_change_bps(0, 1_000_000), None);
    assert_eq!(calculate_percentage_difference_bps(0, 1_000_000), None);
}

#[test]
fn test_flash_crash_protection_rejects_large_increase() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("NGN");
    let old_price: i128 = 1_000_000;
    let new_price: i128 = 1_200_000; // 20% increase > 10% threshold

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
        crate::auth::_add_provider(&env, &provider);
    });

    client.set_price(&asset, &old_price, &6u32, &3600u64);

    // Should reject 20% increase (exceeds 10% MAX_PERCENT_CHANGE)
    match client.try_update_price(&provider, &asset, &new_price, &6u32, &100u32, &3600u64) {
        Err(Ok(e)) => assert_eq!(e, Error::FlashCrashDetected),
        other => panic!("expected FlashCrashDetected, got {:?}", other),
    }
}

// ============================================================================
// calculate_price_volatility tests (Circuit Breaker helper)
// ============================================================================

#[test]
fn test_price_volatility_increase() {
    assert_eq!(
        calculate_price_volatility(1_000_000, 1_200_000),
        Some(200_000)
    );
}

#[test]
fn test_twap_buffer_limits_to_10_entries_and_calculates_average() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let asset = symbol_short!("NGN");

    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
    });

    client.add_asset(&admin, &asset);

    // Initial TWAP is None
    assert_eq!(client.get_twap(&asset), None);

    // Push 15 prices
    for i in 1..=15 {
        env.ledger().set_timestamp(1_000_000 + i * 10);
        client.set_price(&asset, &(i as i128 * 100), &6, &3600);
    }

    // Since max entries is 10, it should only keep the last 10 entries.
    // The prices kept should be: 600, 700, 800, 900, 1000, 1100, 1200, 1300, 1400, 1500
    // Sum = 10500. Average = 1050
    assert_eq!(client.get_twap(&asset), Some(1050));
}

#[test]
fn test_price_volatility_decrease() {
    assert_eq!(
        calculate_price_volatility(1_200_000, 1_000_000),
        Some(200_000)
    );
}

#[test]
fn test_price_volatility_no_change() {
    assert_eq!(
        calculate_price_volatility(500_000, 500_000),
        Some(0)
    );
}

#[test]
fn test_price_volatility_from_zero() {
    assert_eq!(
        calculate_price_volatility(0, 1_000_000),
        Some(1_000_000)
    );
}

#[test]
fn test_price_volatility_to_zero() {
    assert_eq!(
        calculate_price_volatility(1_000_000, 0),
        Some(1_000_000)
    );
}

#[test]
fn test_is_stale_with_mocked_ledger_time() {
    // Test case: ledger_time=2000, stored_timestamp=1000, ttl=500
    // Expected: 2000 >= (1000 + 500) = 2000 >= 1500 = true (stale)
    let current_time = 2000u64;
    let stored_timestamp = 1000u64;
    let ttl = 500u64;
    
    assert!(is_stale(current_time, stored_timestamp, ttl), "Price should be stale");
    
    // Additional test: not stale case
    // current_time < stored_timestamp + ttl should return false
    assert!(!is_stale(1400u64, 1000u64, 500u64), "Price should not be stale when within TTL");
    
    // Edge case: exactly at expiration boundary
    assert!(is_stale(1500u64, 1000u64, 500u64), "Price should be stale at expiration boundary");
}

// ============================================================================
// Cross-Contract Tests - Dummy Consumer calling the Oracle
// ============================================================================

// ============================================================================
// remove_asset tests
// ============================================================================

#[test]
fn test_remove_asset_deletes_price_entry() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
    });

    let asset = symbol_short!("NGN");
    client.set_price(&asset, &1_000_i128, &2u32, &3600u64);

    // Confirm it exists
    assert!(client.get_price_safe(&asset).is_some());

    // Remove it
    client.remove_asset(&admin, &asset);

    // Should be gone
    assert!(client.get_price_safe(&asset).is_none());
}

#[test]
fn test_remove_asset_not_in_get_all_assets() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
    });

    let ngn = symbol_short!("NGN");
    let kes = symbol_short!("KES");
    client.set_price(&ngn, &1_000_i128, &2u32, &3600u64);
    client.set_price(&kes, &500_i128, &2u32, &3600u64);

    client.remove_asset(&admin, &ngn);

    let assets = client.get_all_assets();
    assert_eq!(assets.len(), 1);
    assert!(!assets.contains(&ngn));
    assert!(assets.contains(&kes));
}

#[test]
fn test_remove_asset_nonexistent_returns_error() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
    });

    let result = client.try_remove_asset(&admin, &symbol_short!("NGN"));
    match result {
        Err(Ok(e)) => assert_eq!(e, Error::AssetNotFound),
        other => panic!("expected AssetNotFound, got {:?}", other),
    }
}

#[test]
fn test_flash_crash_protection_rejects_large_drop() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("NGN");
    let old_price: i128 = 1_000_000;
    let new_price: i128 = 800_000; // 20% drop > 10% threshold

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
        crate::auth::_add_provider(&env, &provider);
    });

    client.set_price(&asset, &old_price, &6u32, &3600u64);

    // Should reject 20% drop (exceeds 10% MAX_PERCENT_CHANGE)
    match client.try_update_price(&provider, &asset, &new_price, &6u32, &100u32, &3600u64) {
        Err(Ok(e)) => assert_eq!(e, Error::FlashCrashDetected),
        other => panic!("expected FlashCrashDetected, got {:?}", other),
    }
}

#[test]
fn test_remove_asset_non_admin_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let non_admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
    });

    let asset = symbol_short!("NGN");
    client.set_price(&asset, &1_000_i128, &2u32, &3600u64);

    let result = client.try_remove_asset(&non_admin, &asset);
    assert!(result.is_err());
}

#[test]
fn test_clear_assets_removes_persistent_price_keys() {
    let env = Env::default();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let ngn = symbol_short!("NGN");
    let kes = symbol_short!("KES");

    env.as_contract(&contract_id, || {
        env.storage().persistent().set(
            &DataKey::Price(ngn.clone()),
            &PriceData {
                price: 1_000,
                timestamp: 10,
                provider: env.current_contract_address(),
                decimals: 2,
                confidence_score: 100,
                ttl: 60,
            },
        );
        env.storage().persistent().set(
            &DataKey::Price(kes.clone()),
            &PriceData {
                price: 2_000,
                timestamp: 10,
                provider: env.current_contract_address(),
                decimals: 2,
                confidence_score: 100,
                ttl: 60,
            },
        );
    });

    let assets = soroban_sdk::vec![&env, ngn.clone(), kes.clone()];
    client.clear_assets(&assets);

    env.as_contract(&contract_id, || {
        assert!(!env.storage().persistent().has(&DataKey::Price(ngn)));
        assert!(!env.storage().persistent().has(&DataKey::Price(kes)));
    });
}

#[test]
fn test_clear_assets_rejects_batches_above_limit_atomically() {
    let env = Env::default();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let protected = symbol_short!("NGN");
    env.as_contract(&contract_id, || {
        env.storage().persistent().set(
            &DataKey::Price(protected.clone()),
            &PriceData {
                price: 1_000,
                timestamp: 10,
                provider: env.current_contract_address(),
                decimals: 2,
                confidence_score: 100,
                ttl: 60,
            },
        );
    });

    let mut assets = soroban_sdk::Vec::new(&env);
    for _ in 0..21 {
        assets.push_back(protected.clone());
    }

    let result = client.try_clear_assets(&assets);
    match result {
        Err(Ok(e)) => assert_eq!(e, Error::TooManyAssets),
        other => panic!("expected TooManyAssets, got {:?}", other),
    }

    env.as_contract(&contract_id, || {
        assert!(env.storage().persistent().has(&DataKey::Price(protected)));
    });
}

// ============================================================================
// Cross-Contract Tests - Dummy Consumer calling the Oracle
// ============================================================================

#[test]
fn test_dummy_consumer_calls_oracle_successfully() {
    let env = Env::default();

    // Register the price oracle contract
    let oracle_id = env.register(PriceOracle, ());
    let oracle_client = PriceOracleClient::new(&env, &oracle_id);

    // Register the dummy consumer contract
    let dummy_id = env.register(DummyConsumer, ());
    let dummy_client = DummyConsumerClient::new(&env, &dummy_id);

    // Set up the oracle with some prices
    let ngn = symbol_short!("NGN");
    let price = 1_500_000_i128;
    env.ledger().set_timestamp(1_234_567_890);
    env.ledger().set_sequence_number(1);
    oracle_client.set_price(&ngn, &price, &2u32, &3600u64);

    // The Dummy contract calls the Oracle to get the price
    let fetched_price = dummy_client.get_oracle_price(&oracle_id, &ngn);

    assert_eq!(fetched_price, price, "Dummy contract should receive correct price from Oracle");
}

#[test]
fn test_dummy_consumer_gets_all_assets() {
    let env = Env::default();

    let oracle_id = env.register(PriceOracle, ());
    let oracle_client = PriceOracleClient::new(&env, &oracle_id);

    let dummy_id = env.register(DummyConsumer, ());
    let dummy_client = DummyConsumerClient::new(&env, &dummy_id);

    // Add multiple prices to the oracle
    let ngn = symbol_short!("NGN");
    let kes = symbol_short!("KES");
    let ghs = symbol_short!("GHS");

    oracle_client.set_price(&ngn, &1_500_i128, &2u32, &3600u64);
    oracle_client.set_price(&kes, &800_i128, &2u32, &3600u64);
    oracle_client.set_price(&ghs, &5_000_i128, &2u32, &3600u64);

    // The Dummy contract fetches all available assets
    let assets = dummy_client.get_all_available_assets(&oracle_id);

    assert_eq!(assets.len(), 3, "Should have 3 assets");
    assert!(assets.contains(&ngn));
    assert!(assets.contains(&kes));
    assert!(assets.contains(&ghs));
}

#[test]
fn test_dummy_consumer_safe_price_fetch() {
    let env = Env::default();

    let oracle_id = env.register(PriceOracle, ());
    let oracle_client = PriceOracleClient::new(&env, &oracle_id);

    let dummy_id = env.register(DummyConsumer, ());
    let dummy_client = DummyConsumerClient::new(&env, &dummy_id);

    // Add a price to the oracle
    let ngn = symbol_short!("NGN");
    let btc = symbol_short!("BTC"); // Not added to oracle
    let price = 1_500_000_i128;

    env.ledger().set_timestamp(1_234_567_890);
    env.ledger().set_sequence_number(1);
    oracle_client.set_price(&ngn, &price, &2u32, &3600u64);

    // Safely fetch existing price
    let existing_price = dummy_client.try_get_oracle_price_data(&oracle_id, &ngn);
    assert!(existing_price.is_some(), "Should find existing price");
    assert_eq!(
        existing_price.unwrap().price,
        price,
        "Price data should match"
    );

    // Safely fetch non-existing price (should return None, not panic)
    let missing_price = dummy_client.try_get_oracle_price_data(&oracle_id, &btc);
    assert!(missing_price.is_none(), "Should return None for non-existent asset");
}

#[test]
fn test_dummy_consumer_multiple_price_fetches() {
    let env = Env::default();

    let oracle_id = env.register(PriceOracle, ());
    let oracle_client = PriceOracleClient::new(&env, &oracle_id);

    let dummy_id = env.register(DummyConsumer, ());
    let dummy_client = DummyConsumerClient::new(&env, &dummy_id);

    // Set up initial prices
    let ngn = symbol_short!("NGN");
    let kes = symbol_short!("KES");
    env.ledger().set_timestamp(1_000_000);
    env.ledger().set_sequence_number(1);
    oracle_client.set_price(&ngn, &1_000_000_i128, &2u32, &3600u64);
    oracle_client.set_price(&kes, &500_000_i128, &2u32, &3600u64);

    // First call - verify prices
    let ngn_price_1 = dummy_client.get_oracle_price(&oracle_id, &ngn);
    let kes_price_1 = dummy_client.get_oracle_price(&oracle_id, &kes);
    assert_eq!(ngn_price_1, 1_000_000_i128);
    assert_eq!(kes_price_1, 500_000_i128);

    // Update prices
    env.ledger().set_timestamp(2_000_000);
    env.ledger().set_sequence_number(2);
    oracle_client.set_price(&ngn, &1_200_000_i128, &2u32, &3600u64);
    oracle_client.set_price(&kes, &450_000_i128, &2u32, &3600u64);

    // Second call - verify updated prices
    let ngn_price_2 = dummy_client.get_oracle_price(&oracle_id, &ngn);
    let kes_price_2 = dummy_client.get_oracle_price(&oracle_id, &kes);
    assert_eq!(ngn_price_2, 1_200_000_i128);
    assert_eq!(kes_price_2, 450_000_i128);
}

// ============================================================================
// Upgrade tests
// ============================================================================

/// A real Soroban WASM blob used to satisfy the host's WASM validation
/// when testing `upload_contract_wasm` in the upgrade happy-path test.
const TEST_WASM: &[u8] = include_bytes!("../test_fixtures/test_contract_data.wasm");

#[test]
fn test_upgrade_admin_only() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    client.init_admin(&admin);

    let new_wasm_hash = env.deployer().upload_contract_wasm(TEST_WASM);
    // Should not panic – admin is authorised
    client.upgrade(&admin, &new_wasm_hash);
}

#[test]
#[should_panic(expected = "Unauthorised: caller is not in the authorized admin list")]
fn test_upgrade_rejects_non_admin() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let non_admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    client.init_admin(&admin);

    // Auth check runs before the hash is used, so any 32-byte value is fine here.
    let dummy_hash = soroban_sdk::BytesN::from_array(&env, &[0u8; 32]);
    // Must panic – non_admin is not the admin
    client.upgrade(&non_admin, &dummy_hash);
}

// ============================================================================
// Bulk get_prices Tests
// ============================================================================

#[test]
fn test_get_prices_returns_all_requested_assets() {
    let env = Env::default();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let ngn = symbol_short!("NGN");
    let kes = symbol_short!("KES");
    let ghs = symbol_short!("GHS");

    env.ledger().set_timestamp(1_000_000);
    env.ledger().set_sequence_number(1);
    client.set_price(&ngn, &1_500_i128, &2u32, &3600u64);
    client.set_price(&kes, &800_i128, &4u32, &3600u64);
    client.set_price(&ghs, &5_000_i128, &6u32, &3600u64);

    let assets = soroban_sdk::vec![&env, ngn.clone(), kes.clone(), ghs.clone()];
    let results = client.get_prices(&assets);

    assert_eq!(results.len(), 3);
    assert_eq!(results.get(0).unwrap().unwrap().price, 1_500_i128);
    assert_eq!(results.get(0).unwrap().unwrap().decimals, 2u32);
    assert_eq!(results.get(1).unwrap().unwrap().price, 800_i128);
    assert_eq!(results.get(1).unwrap().unwrap().decimals, 4u32);
    assert_eq!(results.get(2).unwrap().unwrap().price, 5_000_i128);
    assert_eq!(results.get(2).unwrap().unwrap().decimals, 6u32);
}

#[test]
fn test_get_prices_returns_none_for_missing_asset() {
    let env = Env::default();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let ngn = symbol_short!("NGN");
    let btc = symbol_short!("BTC"); // not stored

    env.ledger().set_timestamp(1_000_000);
    env.ledger().set_sequence_number(1);
    client.set_price(&ngn, &1_500_i128, &2u32, &3600u64);

    let assets = soroban_sdk::vec![&env, ngn.clone(), btc.clone()];
    let results = client.get_prices(&assets);

    assert_eq!(results.len(), 2);
    assert!(results.get(0).unwrap().is_some());
    assert!(results.get(1).unwrap().is_none()); // BTC missing → None
}

#[test]
fn test_get_prices_returns_none_for_stale_asset() {
    let env = Env::default();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let ngn = symbol_short!("NGN");

    // Store price with a short TTL of 100 seconds
    env.ledger().set_timestamp(1_000_000);
    env.ledger().set_sequence_number(1);
    client.set_price(&ngn, &1_500_i128, &2u32, &100u64);

    // Advance time past TTL
    env.ledger().set_timestamp(1_000_200);
    env.ledger().set_sequence_number(2);

    let assets = soroban_sdk::vec![&env, ngn.clone()];
    let results = client.get_prices(&assets);

    assert_eq!(results.len(), 1);
    assert!(results.get(0).unwrap().is_none()); // stale → None
}

#[test]
fn test_get_prices_preserves_order() {
    let env = Env::default();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let ngn = symbol_short!("NGN");
    let kes = symbol_short!("KES");

    env.ledger().set_timestamp(1_000_000);
    env.ledger().set_sequence_number(1);
    client.set_price(&ngn, &111_i128, &2u32, &3600u64);
    client.set_price(&kes, &222_i128, &2u32, &3600u64);

    // Request in reverse order
    let assets = soroban_sdk::vec![&env, kes.clone(), ngn.clone()];
    let results = client.get_prices(&assets);

    assert_eq!(results.get(0).unwrap().unwrap().price, 222_i128); // KES first
    assert_eq!(results.get(1).unwrap().unwrap().price, 111_i128); // NGN second
}

#[test]
fn test_get_prices_empty_input_returns_empty_vec() {
    let env = Env::default();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let assets: soroban_sdk::Vec<Symbol> = soroban_sdk::vec![&env];
    let results = client.get_prices(&assets);

    assert_eq!(results.len(), 0);
}

#[test]
fn test_get_prices_with_status_marks_stale_entry() {
    let env = Env::default();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let ngn = symbol_short!("NGN");

    env.ledger().set_timestamp(1_000_000);
    env.ledger().set_sequence_number(1);
    client.set_price(&ngn, &1_500_i128, &2u32, &100u64);

    env.ledger().set_timestamp(1_000_200);
    env.ledger().set_sequence_number(2);

    let assets = soroban_sdk::vec![&env, ngn.clone()];
    let results = client.get_prices_with_status(&assets);

    assert_eq!(results.len(), 1);
    let entry = results.get(0).unwrap().unwrap();
    assert_eq!(entry.price, 1_500_i128);
    assert!(entry.is_stale);
}

#[test]
fn test_get_prices_with_status_returns_none_for_missing_asset() {
    let env = Env::default();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let ngn = symbol_short!("NGN");
    let btc = symbol_short!("BTC");

    env.ledger().set_timestamp(1_000_000);
    env.ledger().set_sequence_number(1);
    client.set_price(&ngn, &1_500_i128, &2u32, &3600u64);

    let assets = soroban_sdk::vec![&env, ngn.clone(), btc.clone()];
    let results = client.get_prices_with_status(&assets);

    assert_eq!(results.len(), 2);
    assert!(results.get(0).unwrap().is_some());
    assert!(results.get(1).unwrap().is_none());
}

// ============================================================================
// Min/Max Price Bounds Tests
// ============================================================================

#[test]
fn test_set_price_bounds_and_get() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
    });

    let asset = symbol_short!("NGN");
    client.set_price_bounds(&admin, &asset, &500_i128, &2_000_i128);

    let bounds = client.get_price_bounds(&asset).unwrap();
    assert_eq!(bounds.min_price, 500_i128);
    assert_eq!(bounds.max_price, 2_000_i128);
}

#[test]
fn test_get_price_bounds_returns_none_when_not_set() {
    let env = Env::default();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let bounds = client.get_price_bounds(&symbol_short!("NGN"));
    assert!(bounds.is_none());
}

#[test]
fn test_update_price_within_bounds_succeeds() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("NGN");
    // Use small prices so delta stays ≤ 50 (delta check threshold)
    let old_price: i128 = 1_000;
    let new_price: i128 = 1_040; // ~4% increase, delta=40 ≤ 50

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
        crate::auth::_add_provider(&env, &provider);
    });

    client.set_price(&asset, &old_price, &6u32, &3600u64);

    // Should allow ~4% increase (within 10% MAX_PERCENT_CHANGE, delta ≤ 50)
    client.update_price(&provider, &asset, &new_price, &6u32, &100u32, &3600u64);

    let price_data = client.get_price(&asset, &true);
    assert_eq!(price_data.price, new_price);
}

#[test]
fn test_flash_crash_protection_allows_exact_threshold() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("NGN");
    // Use small prices so delta stays ≤ 50 (delta check threshold)
    let old_price: i128 = 500;
    let new_price: i128 = 550; // Exactly 10% increase = threshold, delta=50 ≤ 50

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
        crate::auth::_add_provider(&env, &provider);
    });

    client.set_price(&asset, &old_price, &6u32, &3600u64);

    // Should allow exactly 10% increase (at threshold, not exceeding), delta=50 ≤ 50
    client.update_price(&provider, &asset, &new_price, &6u32, &100u32, &3600u64);

    let price_data = client.get_price(&asset, &true);
    assert_eq!(price_data.price, new_price);
}

#[test]
fn test_update_price_below_min_bound_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("NGN");

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
        crate::auth::_add_provider(&env, &provider);
    });

    client.add_asset(&admin, &asset);
    client.set_price_bounds(&admin, &asset, &500_i128, &2_000_i128);

    let result = client.try_update_price(&provider, &asset, &100_i128, &6u32, &100u32, &3600u64);
    match result {
        Err(Ok(e)) => assert_eq!(e, Error::PriceOutOfBounds),
        other => panic!("expected PriceOutOfBounds, got {:?}", other),
    }
}

#[test]
fn test_flash_crash_protection_allows_first_price_update() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("NGN");

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
        crate::auth::_add_provider(&env, &provider);
    });

    // Track the asset first, then do first price update (no previous price)
    client.add_asset(&admin, &asset);
    client.update_price(&provider, &asset, &1_000_i128, &6u32, &100u32, &3600u64);

    let price_data = client.get_price(&asset, &true);
    assert_eq!(price_data.price, 1_000_i128);
}

#[test]
fn test_update_price_above_max_bound_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let provider = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let asset = symbol_short!("NGN");

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
        crate::auth::_add_provider(&env, &provider);
    });

    client.add_asset(&admin, &asset);

    // Set bounds: 500 to 2000
    client.set_price_bounds(&admin, &asset, &500_i128, &2_000_i128);

    // Price above max should be rejected
    let result = client.try_update_price(&provider, &asset, &5_000_i128, &6u32, &100u32, &3600u64);
    match result {
        Err(Ok(e)) => assert_eq!(e, Error::PriceOutOfBounds),
        other => panic!("expected PriceOutOfBounds, got {:?}", other),
    }
}

#[test]
fn test_update_price_at_exact_bounds_succeeds() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("NGN");
    let price: i128 = 1_500_000;

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
        crate::auth::_add_provider(&env, &provider);
    });

    // Track asset first, then first price update (no previous price) should always be allowed
    client.add_asset(&admin, &asset);
    client.update_price(&provider, &asset, &price, &6u32, &100u32, &3600u64);

    let price_data = client.get_price(&asset, &true);
    assert_eq!(price_data.price, price);
}

#[test]
fn test_flash_crash_protection_rejects_just_over_threshold() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("NGN");
    let old_price: i128 = 1_000_000;
    let new_price: i128 = 1_101_000; // Just over 10% (1010 bps > 1000 bps threshold)

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
        crate::auth::_add_provider(&env, &provider);
    });

    client.set_price(&asset, &old_price, &6u32, &3600u64);

    match client.try_update_price(&provider, &asset, &new_price, &6u32, &100u32, &3600u64) {
        Err(Ok(e)) => assert_eq!(e, Error::FlashCrashDetected),
        other => panic!("expected FlashCrashDetected, got {:?}", other),
    }
}

#[test]
fn test_update_price_no_bounds_set_allows_any_valid_price() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("NGN");

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
        crate::auth::_add_provider(&env, &provider);
    });

    client.add_asset(&admin, &asset);

    // No bounds set — should accept any positive price
    let result = client.try_update_price(&provider, &asset, &999_999_999_i128, &6u32, &100u32, &3600u64);
    assert!(result.is_ok());
}

#[test]
#[should_panic(expected = "min_price must be <= max_price")]
fn test_set_price_bounds_min_greater_than_max_panics() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
    });

    // min > max should panic
    client.set_price_bounds(&admin, &symbol_short!("NGN"), &2_000_i128, &500_i128);
}

#[test]
#[should_panic(expected = "Unauthorised")]
fn test_set_price_bounds_non_admin_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let non_admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
    });

    // non_admin should be rejected
    client.set_price_bounds(&non_admin, &symbol_short!("NGN"), &500_i128, &2_000_i128);
}

// ============================================================================
// AssetAdded Event Tests
// ============================================================================

#[test]
fn test_set_price_emits_asset_added_event_on_first_add() {
    let env = Env::default();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let asset = symbol_short!("NGN");

    // Set price for a new asset
    client.set_price(&asset, &1_500_i128, &2u32, &3600u64);

    // Verify AssetAdded event was emitted
    let events = env.events().all();
    let debug_str = alloc::format!("{:?}", events);
    assert!(debug_str.contains("asset_added_event"), "AssetAdded event should be emitted for new asset");
    assert!(debug_str.contains("symbol"), "Event should contain symbol field");
    assert!(debug_str.contains("NGN"), "Event should contain the correct asset symbol");
}

#[test]
fn test_set_price_does_not_emit_asset_added_event_on_update() {
    let env = Env::default();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let asset = symbol_short!("NGN");

    // First set - should emit AssetAdded
    client.set_price(&asset, &1_500_i128, &2u32, &3600u64);

    // Verify first event was emitted
    let events_after_first = env.events().all();
    let debug_str_first = alloc::format!("{:?}", events_after_first);
    assert!(debug_str_first.contains("asset_added_event"), "Should emit AssetAdded on first set");

    // Second set (update) - should NOT emit AssetAdded
    env.ledger().set_timestamp(1_234_567_900);
    client.set_price(&asset, &1_600_i128, &2u32, &3600u64);

    // Verify no AssetAdded event on update (only the update event should be present if any)
    let events_after_second = env.events().all();
    let debug_str_second = alloc::format!("{:?}", events_after_second);
    // Should NOT contain asset_added_event on update
    assert!(!debug_str_second.contains("asset_added_event"), "Should NOT emit AssetAdded on update");
}

#[test]
fn test_multiple_assets_added_sequentially_each_emits_event() {
    let env = Env::default();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let ngn = symbol_short!("NGN");
    let kes = symbol_short!("KES");
    let ghs = symbol_short!("GHS");

    // Add NGN - should emit AssetAdded
    client.set_price(&ngn, &1_500_i128, &2u32, &3600u64);
    let events_ngn = env.events().all();
    let debug_ngn = alloc::format!("{:?}", events_ngn);
    assert!(debug_ngn.contains("asset_added_event"), "Should emit AssetAdded for NGN");
    assert!(debug_ngn.contains("NGN"), "Should contain NGN symbol");

    // Add KES - should emit AssetAdded
    client.set_price(&kes, &800_i128, &2u32, &3600u64);
    let events_kes = env.events().all();
    let debug_kes = alloc::format!("{:?}", events_kes);
    assert!(debug_kes.contains("asset_added_event"), "Should emit AssetAdded for KES");
    assert!(debug_kes.contains("KES"), "Should contain KES symbol");

    // Add GHS - should emit AssetAdded
    client.set_price(&ghs, &5_000_i128, &2u32, &3600u64);
    let events_ghs = env.events().all();
    let debug_ghs = alloc::format!("{:?}", events_ghs);
    assert!(debug_ghs.contains("asset_added_event"), "Should emit AssetAdded for GHS");
    assert!(debug_ghs.contains("GHS"), "Should contain GHS symbol");
}

#[test]
fn test_mixed_add_and_update_emits_correct_events() {
    let env = Env::default();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let ngn = symbol_short!("NGN");
    let kes = symbol_short!("KES");

    // Add NGN (new asset) - should emit AssetAdded
    client.set_price(&ngn, &1_500_i128, &2u32, &3600u64);
    let events_ngn = env.events().all();
    let debug_ngn = alloc::format!("{:?}", events_ngn);
    assert!(debug_ngn.contains("asset_added_event"), "Should emit AssetAdded for NGN");

    // Add KES (new asset) - should emit AssetAdded
    client.set_price(&kes, &800_i128, &2u32, &3600u64);
    let events_kes = env.events().all();
    let debug_kes = alloc::format!("{:?}", events_kes);
    assert!(debug_kes.contains("asset_added_event"), "Should emit AssetAdded for KES");

    // Update NGN (existing asset) - should NOT emit AssetAdded
    env.ledger().set_timestamp(1_234_567_900);
    client.set_price(&ngn, &1_600_i128, &2u32, &3600u64);
    let events_update = env.events().all();
    let debug_update = alloc::format!("{:?}", events_update);
    assert!(!debug_update.contains("asset_added_event"), "Should NOT emit AssetAdded on update");

    // Add GHS (new asset) - should emit AssetAdded
    let ghs = symbol_short!("GHS");
    client.set_price(&ghs, &5_000_i128, &2u32, &3600u64);
    let events_ghs = env.events().all();
    let debug_ghs = alloc::format!("{:?}", events_ghs);
    assert!(debug_ghs.contains("asset_added_event"), "Should emit AssetAdded for GHS");
}

#[test]
fn test_asset_added_event_contains_correct_symbol() {
    let env = Env::default();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let asset = symbol_short!("NGN");

    client.set_price(&asset, &1_500_i128, &2u32, &3600u64);

    // Verify event structure contains the correct symbol
    let events = env.events().all();
    let debug_str = alloc::format!("{:?}", events);
    assert!(debug_str.contains("asset_added_event"), "Should emit AssetAdded event");
    assert!(debug_str.contains("NGN"), "Event should contain the correct asset symbol");
}

#[test]
fn test_get_last_n_events_sliding_window() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let ngn = symbol_short!("NGN");
    let kes = symbol_short!("KES");
    let ghs = symbol_short!("GHS");

    // Push 6 events - oldest should be dropped
    client.set_price(&ngn, &100_i128, &2u32, &3600u64); // 1 (dropped)
    client.set_price(&kes, &200_i128, &2u32, &3600u64); // 2
    client.set_price(&ghs, &300_i128, &2u32, &3600u64); // 3
    client.set_price(&ngn, &110_i128, &2u32, &3600u64); // 4
    client.set_price(&kes, &210_i128, &2u32, &3600u64); // 5 
    client.set_price(&ghs, &310_i128, &2u32, &3600u64); // 6 (newest)

    let events = client.get_last_n_events(&5);
    assert_eq!(events.len(), 5);

    // Newest first (index 0) is an update because ghs was already added
    assert_eq!(events.get(0).unwrap().asset, ghs);
    assert_eq!(events.get(0).unwrap().price, 310_i128);
    assert_eq!(events.get(0).unwrap().event_type, Symbol::new(&env, "price_updated"));

    assert_eq!(events.get(1).unwrap().asset, kes);
    assert_eq!(events.get(1).unwrap().price, 210_i128);

    // Oldest in the log (index 4) should be the 2nd event pushed
    assert_eq!(events.get(4).unwrap().asset, kes);
    assert_eq!(events.get(4).unwrap().price, 200_i128);
}

// ============================================================================
// Zero-Write Optimisation Tests (#132)
// ============================================================================

#[test]
fn test_set_price_identical_value_only_updates_timestamp() {
    let env = Env::default();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let asset = symbol_short!("NGN");

    // Initial write
    env.ledger().set_timestamp(1_000_000);
    env.ledger().set_sequence_number(1);
    client.set_price(&asset, &1_500_i128, &2u32, &3600u64);

    let first = client.get_price(&asset, &true);
    assert_eq!(first.price, 1_500_i128);
    assert_eq!(first.timestamp, 1_000_000);

    // Second call with the same price — only timestamp should advance
    env.ledger().set_timestamp(1_001_000);
    env.ledger().set_sequence_number(2);
    client.set_price(&asset, &1_500_i128, &2u32, &3600u64);

    let second = client.get_price(&asset, &true);
    assert_eq!(second.price, 1_500_i128, "price must remain unchanged");
    assert_eq!(second.timestamp, 1_001_000, "timestamp must be refreshed");
}

#[test]
fn test_set_price_different_value_writes_new_price() {
    let env = Env::default();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let asset = symbol_short!("KES");

    env.ledger().set_timestamp(2_000_000);
    env.ledger().set_sequence_number(1);
    client.set_price(&asset, &800_i128, &2u32, &3600u64);

    env.ledger().set_timestamp(2_001_000);
    env.ledger().set_sequence_number(2);
    client.set_price(&asset, &850_i128, &2u32, &3600u64);

    let stored = client.get_price(&asset, &true);
    assert_eq!(stored.price, 850_i128, "new price must be stored");
    assert_eq!(stored.timestamp, 2_001_000);
}

#[test]
fn test_set_price_identical_value_still_emits_price_updated_event() {
    let env = Env::default();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let asset = symbol_short!("GHS");

    env.ledger().set_timestamp(3_000_000);
    env.ledger().set_sequence_number(1);
    client.set_price(&asset, &5_000_i128, &2u32, &3600u64);

    // Clear events by reading them, then do the identical-price call
    env.ledger().set_timestamp(3_001_000);
    env.ledger().set_sequence_number(2);
    client.set_price(&asset, &5_000_i128, &2u32, &3600u64);

    // Verify the price is still correct after identical update
    let price_data = client.get_price(&asset);
    assert_eq!(price_data.price, 5_000_i128);
    assert_eq!(price_data.timestamp, 3_001_000);
}

// ============================================================================
// Renounce Ownership Tests
// ============================================================================

#[test]
fn test_renounce_ownership_removes_admin_permanently() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    client.init_admin(&admin);
    assert!(client.is_admin(&admin));

    client.renounce_ownership(&admin);

    assert!(!client.is_admin(&admin));
    env.as_contract(&contract_id, || {
        assert!(!crate::auth::_has_admin(&env));
    });
}

#[test]
fn test_renounce_ownership_emits_event() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    client.init_admin(&admin);
    client.renounce_ownership(&admin);

    let events = env.events().all();
    let debug_str = alloc::format!("{:?}", events);
    assert!(debug_str.contains("ownership_renounced_event"));
}

#[test]
#[should_panic(expected = "Unauthorised: caller is not in the authorized admin list")]
fn test_renounce_ownership_rejects_non_admin() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let non_admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    client.init_admin(&admin);
    client.renounce_ownership(&non_admin);
}

#[test]
#[should_panic(expected = "Unauthorised: caller is not in the authorized admin list")]
fn test_renounce_ownership_blocks_admin_functions_after_renouncement() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    client.init_admin(&admin);
    client.renounce_ownership(&admin);

    // Any admin-only function should now fail — upgrade is a convenient test target
    let dummy_hash = soroban_sdk::BytesN::from_array(&env, &[0u8; 32]);
    client.upgrade(&admin, &dummy_hash);
}

// ============================================================================
// Multi-Signature Pause Tests
// ============================================================================

#[test]
#[should_panic]
fn test_toggle_pause_fails_with_same_admin_twice() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin1 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let admin2 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    client.init_admin(&admin1);
    env.as_contract(&contract_id, || {
        crate::auth::_add_authorized(&env, &admin2);
    });

    // Should fail when using the same admin twice
    client.toggle_pause(&admin1, &admin1);
}

#[test]
#[should_panic]
fn test_toggle_pause_fails_with_non_admin() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin1 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let admin2 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let non_admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    client.init_admin(&admin1);
    env.as_contract(&contract_id, || {
        crate::auth::_add_authorized(&env, &admin2);
    });

    // Should fail when one signer is not an admin
    client.toggle_pause(&admin1, &non_admin);
}

#[test]
#[should_panic]
fn test_toggle_pause_fails_with_only_one_admin() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin1 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    // Initialize with only one admin
    client.init_admin(&admin1);

    // Should fail when only one admin exists
    let fake_admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    client.toggle_pause(&admin1, &fake_admin);
}

#[test]
fn test_register_and_remove_admin_updates_count() {
    let (env, contract_id, client) = setup();
    let admin1 = Address::generate(&env);
    let admin2 = Address::generate(&env);
    let admin3 = Address::generate(&env);

    client.init_admin(&admin1);
    env.as_contract(&contract_id, || {
        crate::auth::_add_authorized(&env, &admin2);
    });

    assert_eq!(client.get_admin_count(), 2);
    client.register_admin(&admin1, &admin2, &admin3);
    assert_eq!(client.get_admin_count(), 3);
    assert!(client.is_admin(&admin3));

    client.remove_admin(&admin1, &admin2, &admin3);
    assert_eq!(client.get_admin_count(), 2);
    assert!(!client.is_admin(&admin3));
}

#[test]
#[should_panic(expected = "Unauthorised")]
fn test_renounce_ownership_blocks_admin_calls() {
    let (env, _, client) = setup();
    let admin = Address::generate(&env);

    client.init_admin(&admin);
    client.renounce_ownership(&admin);
    client.add_asset(&admin, &symbol_short!("NGN"));
    assert!(client.is_admin(&admin1));
    assert!(client.is_admin(&admin2));
}

#[test]
#[should_panic]
fn test_remove_admin_fails_if_last_admin() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin1 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    // Initialize with only one admin
    client.init_admin(&admin1);

    // Should fail when trying to remove the last admin
    let fake_admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    client.remove_admin(&admin1, &fake_admin, &admin1);
}

#[test]
fn test_multi_sig_pause_emits_event() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin1 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let admin2 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    client.init_admin(&admin1);
    env.as_contract(&contract_id, || {
        crate::auth::_add_authorized(&env, &admin2);
    });

    client.toggle_pause(&admin1, &admin2);

    let events = env.events().all();
    let debug_str = alloc::format!("{:?}", events);
    assert!(debug_str.contains("pause_toggled"));
}

#[test]
fn test_register_admin_emits_event() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin1 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let admin2 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let admin3 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    client.init_admin(&admin1);
    env.as_contract(&contract_id, || {
        crate::auth::_add_authorized(&env, &admin2);
    });

    client.register_admin(&admin1, &admin2, &admin3);

    let events = env.events().all();
    let debug_str = alloc::format!("{:?}", events);
    assert!(debug_str.contains("admin_registered"));
}

#[test]
fn test_remove_admin_emits_event() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin1 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let admin2 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let admin3 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    client.init_admin(&admin1);
    env.as_contract(&contract_id, || {
        crate::auth::_add_authorized(&env, &admin2);
        crate::auth::_add_authorized(&env, &admin3);
    });

    client.remove_admin(&admin1, &admin2, &admin3);

    let events = env.events().all();
    let debug_str = alloc::format!("{:?}", events);
    assert!(debug_str.contains("admin_removed"));
}

#[test]
fn test_get_admin_count_returns_correct_value() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin1 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let admin2 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    // Initially 1 admin
    client.init_admin(&admin1);
    assert_eq!(client.get_admin_count(), 1);

    // Add second admin
    env.as_contract(&contract_id, || {
        crate::auth::_add_authorized(&env, &admin2);
    });
    assert_eq!(client.get_admin_count(), 2);
}

#[test]
fn test_full_multi_sig_workflow() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    
    // Start with 2 admins
    let admin1 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let admin2 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let admin3 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    client.init_admin(&admin1);
    env.as_contract(&contract_id, || {
        crate::auth::_add_authorized(&env, &admin2);
    });

    // Step 1: Register third admin
    client.register_admin(&admin1, &admin2, &admin3);
    assert_eq!(client.get_admin_count(), 3);

    // Step 2: Toggle pause with admin1 and admin3
    let paused = client.toggle_pause(&admin1, &admin3);
    assert_eq!(paused, true);

    // Step 3: Remove admin2 with admin1 and admin3
    client.remove_admin(&admin1, &admin3, &admin2);
    assert_eq!(client.get_admin_count(), 2);
    assert!(!client.is_admin(&admin2));

    // Step 4: Toggle unpause with remaining admins
    let paused = client.toggle_pause(&admin1, &admin3);
    assert_eq!(paused, false);
}

// ============================================================================
// Self-Destruct Tests
// ============================================================================

#[test]
fn test_self_destruct_requires_two_admins() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin1 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let admin2 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    client.init_admin(&admin1);
    env.as_contract(&contract_id, || {
        crate::auth::_add_authorized(&env, &admin2);
    });

    client.self_destruct(&admin1, &admin2);

    // Verify contract is destroyed — no admins remain
    env.as_contract(&contract_id, || {
        assert!(!crate::auth::_has_admin(&env));
        assert!(env.storage().instance().has(&DataKey::Destroyed));
    });
}

#[test]
fn test_self_destruct_fails_with_same_admin_twice() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin1 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let admin2 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    client.init_admin(&admin1);
    env.as_contract(&contract_id, || {
        crate::auth::_add_authorized(&env, &admin2);
    });

    let result = client.try_self_destruct(&admin1, &admin1);
    match result {
        Err(Ok(e)) => assert_eq!(e, Error::MultiSigValidationFailed),
        other => panic!("expected MultiSigValidationFailed, got {:?}", other),
    }
}

#[test]
fn test_self_destruct_fails_with_non_admin() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin1 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let admin2 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let non_admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    client.init_admin(&admin1);
    env.as_contract(&contract_id, || {
        crate::auth::_add_authorized(&env, &admin2);
    });

    let result = client.try_self_destruct(&admin1, &non_admin);
    match result {
        Err(Ok(e)) => assert_eq!(e, Error::MultiSigValidationFailed),
        other => panic!("expected MultiSigValidationFailed, got {:?}", other),
    }
}

#[test]
fn test_self_destruct_fails_with_only_one_admin() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin1 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let fake_admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    client.init_admin(&admin1);

    let result = client.try_self_destruct(&admin1, &fake_admin);
    match result {
        Err(Ok(e)) => assert_eq!(e, Error::MultiSigValidationFailed),
        other => panic!("expected MultiSigValidationFailed, got {:?}", other),
    }
}

#[test]
#[should_panic(expected = "Error(ContractDestroyed)")]
fn test_self_destruct_blocks_admin_functions() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin1 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let admin2 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    client.init_admin(&admin1);
    env.as_contract(&contract_id, || {
        crate::auth::_add_authorized(&env, &admin2);
    });

    // Destroy the contract
    client.self_destruct(&admin1, &admin2);

    // Any admin-only function should now fail with ContractDestroyed
    let dummy_hash = soroban_sdk::BytesN::from_array(&env, &[0u8; 32]);
    client.upgrade(&admin1, &dummy_hash);
}

#[test]
fn test_self_destruct_emits_event() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin1 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let admin2 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    client.init_admin(&admin1);
    env.as_contract(&contract_id, || {
        crate::auth::_add_authorized(&env, &admin2);
    });

    client.self_destruct(&admin1, &admin2);

    let events = env.events().all();
    let debug_str = alloc::format!("{:?}", events);
    assert!(debug_str.contains("contract_destroyed"), "Should emit contract_destroyed event");
    assert!(debug_str.contains(&format!("{:?}", admin1)), "Event should contain admin1");
    assert!(debug_str.contains(&format!("{:?}", admin2)), "Event should contain admin2");
}

#[test]
#[should_panic(expected = "Error(ContractDestroyed)")]
fn test_self_destruct_prevents_double_destruct() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin1 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let admin2 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    client.init_admin(&admin1);
    env.as_contract(&contract_id, || {
        crate::auth::_add_authorized(&env, &admin2);
    });

    client.self_destruct(&admin1, &admin2);

    // Second call should panic with ContractDestroyed
    client.self_destruct(&admin1, &admin2);
}

// ─────────────────────────────────────────────────────────────────────────────
// Cross-Contract Callback Tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_try_subscribe_to_price_updates() {
    let (env, _contract_id, client) = setup();
    let callback_contract = Address::generate(&env);

    // Should successfully subscribe
    let result = client.try_subscribe_to_price_updates(&callback_contract);
    assert_eq!(result, Ok(()));

    // Verify subscriber is in the list
    let subscribers = client.get_price_update_subscribers();
    assert_eq!(subscribers.len(), 1);
    assert_eq!(subscribers.get(0).unwrap(), callback_contract);
}

#[test]
fn test_subscribe_duplicate_fails() {
    let (env, _contract_id, client) = setup();
    let callback_contract = Address::generate(&env);

    // First subscription succeeds
    assert_eq!(client.try_subscribe_to_price_updates(&callback_contract), Ok(()));

    // Duplicate subscription should fail
    let result = client.try_subscribe_to_price_updates(&callback_contract);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("already subscribed"));
}

#[test]
fn test_multiple_subscribers() {
    let (env, _contract_id, client) = setup();
    let callback1 = Address::generate(&env);
    let callback2 = Address::generate(&env);
    let callback3 = Address::generate(&env);

    // Subscribe multiple contracts
    assert_eq!(client.try_subscribe_to_price_updates(&callback1), Ok(()));
    assert_eq!(client.try_subscribe_to_price_updates(&callback2), Ok(()));
    assert_eq!(client.try_subscribe_to_price_updates(&callback3), Ok(()));

    // Verify all are subscribed
    let subscribers = client.get_price_update_subscribers();
    assert_eq!(subscribers.len(), 3);
    assert!(subscribers.iter().any(|s| s == &callback1));
    assert!(subscribers.iter().any(|s| s == &callback2));
    assert!(subscribers.iter().any(|s| s == &callback3));
}

#[test]
fn test_try_unsubscribe_from_price_updates() {
    let (env, _contract_id, client) = setup();
    let callback1 = Address::generate(&env);
    let callback2 = Address::generate(&env);

    // Subscribe both
    client.try_subscribe_to_price_updates(&callback1).unwrap();
    client.try_subscribe_to_price_updates(&callback2).unwrap();
    assert_eq!(client.get_price_update_subscribers().len(), 2);

    // Unsubscribe first
    let result = client.try_unsubscribe_from_price_updates(&callback1);
    assert_eq!(result, Ok(()));

    // Verify only callback2 remains
    let subscribers = client.get_price_update_subscribers();
    assert_eq!(subscribers.len(), 1);
    assert_eq!(subscribers.get(0).unwrap(), callback2);
}

#[test]
fn test_unsubscribe_nonexistent_fails() {
    let (env, _contract_id, client) = setup();
    let callback1 = Address::generate(&env);
    let callback2 = Address::generate(&env);

    // Subscribe only callback1
    client.try_subscribe_to_price_updates(&callback1).unwrap();

    // Try to unsubscribe callback2 (not subscribed)
    let result = client.try_unsubscribe_from_price_updates(&callback2);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not found"));

    // callback1 should still be subscribed
    let subscribers = client.get_price_update_subscribers();
    assert_eq!(subscribers.len(), 1);
    assert_eq!(subscribers.get(0).unwrap(), callback1);
}

#[test]
fn test_get_empty_subscriber_list() {
    let (_env, _contract_id, client) = setup();

    // No subscribers initially
    let subscribers = client.get_price_update_subscribers();
    assert_eq!(subscribers.len(), 0);
}

#[test]
fn test_subscribe_unsubscribe_cycle() {
    let (env, _contract_id, client) = setup();
    let callback = Address::generate(&env);

    // Subscribe
    assert_eq!(client.try_subscribe_to_price_updates(&callback), Ok(()));
    assert_eq!(client.get_price_update_subscribers().len(), 1);

    // Unsubscribe
    assert_eq!(client.try_unsubscribe_from_price_updates(&callback), Ok(()));
    assert_eq!(client.get_price_update_subscribers().len(), 0);

    // Subscribe again should work
    assert_eq!(client.try_subscribe_to_price_updates(&callback), Ok(()));
    assert_eq!(client.get_price_update_subscribers().len(), 1);
}

#[test]
fn test_update_price_does_not_crash_with_subscribers() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let subscriber = Address::generate(&env);
    let asset = symbol_short!("NGN");

    // Initialize
    let pairs = soroban_sdk::vec![&env, asset];
    client.initialize(&admin, &pairs);

    // Add provider
    env.as_contract(&contract_id, || {
        crate::auth::_add_provider(&env, &provider);
    });

    // Subscribe a contract
    assert_eq!(client.try_subscribe_to_price_updates(&subscriber), Ok(()));

    // Update price should not crash even with subscribers
    // (The callback will fail because subscriber doesn't implement on_price_update, but update should succeed)
    env.ledger().set_timestamp(1_000_000);
    let result = client.update_price(&provider, &asset, &1_500_000_i128, &6u32, &90u32, &3600u64);
    
    // The update should succeed even if the callback fails
    assert!(result.is_ok(), "Price update should succeed even with subscribers");

    // Verify price was stored
    let price = client.get_price(&asset, &true);
    assert_eq!(price.price, 1_500_000_i128);
}

#[test]
fn test_set_price_with_subscribers() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let subscriber = Address::generate(&env);
    let asset = symbol_short!("KES");

    // Subscribe a contract
    assert_eq!(client.try_subscribe_to_price_updates(&subscriber), Ok(()));

    // Set price should not crash even with subscribers
    env.ledger().set_timestamp(2_000_000);
    client.set_price(&asset, &2_000_000_i128, &6u32, &3600u64);

    // Verify price was stored
    let price = client.get_price(&asset, &true);
    assert_eq!(price.price, 2_000_000_i128);
}
