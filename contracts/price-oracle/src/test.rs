#![cfg(test)]
extern crate alloc;

use super::*;
use soroban_sdk::{
    contract, contractimpl, symbol_short, testutils::Address as _, testutils::Events,
    testutils::Ledger, Address, Env, Symbol,
};

use crate::{
    calculate_percentage_change_bps, calculate_percentage_difference_bps,
    calculate_price_volatility, is_stale,
    StellarFlowClient, Error,
};

// ============================================================================
// Dummy Contract - Example implementation for cross-contract price fetching
// ============================================================================

/// A simple example contract that demonstrates how to consume prices from StellarFlow.
/// This contract shows the minimal implementation needed to call the price oracle.
#[contract]
pub struct DummyConsumer;

#[contractimpl]
impl DummyConsumer {
    /// Fetch the price of an asset from the StellarFlow price oracle.
    ///
    /// # Arguments
    /// * `oracle_address` - The address of the StellarFlow price oracle contract
    /// * `asset` - The symbol of the asset to fetch (e.g., "NGN", "KES", "GHS")
    ///
    /// # Returns
    /// The current price for the asset, or panics if not found
    pub fn get_oracle_price(env: Env, oracle_address: Address, asset: Symbol) -> i128 {
        // Use the public cross-contract interface client that downstream contracts consume.
        let oracle_client = StellarFlowClient::new(&env, &oracle_address);

        // Call get_last_price which is optimized for minimal gas usage
        oracle_client.get_last_price(&asset)
    }

    /// Fetch the full price data from the oracle, demonstrating the safe getter.
    ///
    /// Returns `None` if the asset doesn't exist, allowing graceful degradation.
    pub fn try_get_oracle_price_data(env: Env, oracle_address: Address, asset: Symbol) -> Option<PriceData> {
        let oracle_client = StellarFlowClient::new(&env, &oracle_address);
        oracle_client.get_price_safe(&asset)
    }

    /// Example function showing how to fetch all available assets from the oracle.
    pub fn get_all_available_assets(env: Env, oracle_address: Address) -> soroban_sdk::Vec<Symbol> {
        let oracle_client = StellarFlowClient::new(&env, &oracle_address);
        oracle_client.get_all_assets()
    }
}

#[test]
fn test_initialize_success() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let pairs = soroban_sdk::vec![&env, symbol_short!("NGN"), symbol_short!("KES")];
    client.initialize(&admin, &pairs);

    let events = env.events().all();
    let debug_str = alloc::format!("{:?}", events);
    assert!(debug_str.contains("AdminChanged"));

    // Must be inside as_contract to access instance storage
    env.as_contract(&contract_id, || {
        let stored_admins: soroban_sdk::Vec<Address> = env.storage().instance().get(&DataKey::Admin).unwrap();
        assert!(stored_admins.contains(&admin));

        let stored_pairs: soroban_sdk::Vec<Symbol> = env
            .storage()
            .instance()
            .get(&DataKey::BaseCurrencyPairs)
            .unwrap();
        assert_eq!(stored_pairs, pairs);
    });
}

#[test]
#[should_panic]
fn test_initialize_double_panics() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let pairs = soroban_sdk::vec![&env, symbol_short!("NGN")];
    client.initialize(&admin, &pairs);
    // Second call should panic with Error::AlreadyInitialized
    client.initialize(&admin, &pairs);
}

fn setup() -> (Env, PriceOracleClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    (env, client)
}

#[test]
fn test_init_admin_sets_admin_once() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    client.init_admin(&admin);

    let events = env.events().all();
    let debug_str = alloc::format!("{:?}", events);
    assert!(debug_str.contains("AdminChanged"));

    env.as_contract(&contract_id, || {
        assert!(crate::auth::_has_admin(&env));
        assert!(crate::auth::_get_admin(&env).contains(&admin));
    });
}

#[test]
fn test_get_admin_reader_returns_current_admin() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    client.init_admin(&admin);

    assert_eq!(client.get_admin(), admin);
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

    let retrieved_price = client.get_price(&asset);
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

    let result = client.try_get_price(&asset);
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

    let initial = client.try_get_price(&asset).unwrap().unwrap();
    assert_eq!(initial.price, 1_000_000_i128);
    assert_eq!(initial.timestamp, 1_234_567_890);

    env.ledger().set_timestamp(1_234_567_900);
    env.ledger().set_sequence_number(2);
    client
        .try_set_price(&asset, &1_200_000_i128, &6u32, &3600u64)
        .unwrap()
        .unwrap();

    let updated = client.try_get_price(&asset).unwrap().unwrap();
    assert_eq!(updated.price, 1_200_000_i128);
    assert_eq!(updated.timestamp, 1_234_567_900);
}

#[test]
fn test_get_price_safe_nonexistent_returns_none() {
    let (_, client) = setup();
    assert_eq!(client.get_price_safe(&symbol_short!("NGN")), None);
}

#[test]
fn test_get_all_assets_returns_tracked_symbols() {
    let (_, client) = setup();
    let ngn = symbol_short!("NGN");
    let kes = symbol_short!("KES");

    client.set_price(&ngn, &1_500_i128, &2u32, &3600u64);
    client.set_price(&kes, &800_i128, &2u32, &14400u64);

    let assets = client.get_all_assets();
    assert_eq!(assets.len(), 2);
    assert!(assets.contains(&ngn));
    assert!(assets.contains(&kes));
}

#[test]
fn test_set_price_uses_current_ledger_timestamp() {
    let env = Env::default();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let asset = symbol_short!("NGN");

    env.ledger().set_timestamp(1_700_000_123);
    env.ledger().set_sequence_number(77);
    client.set_price(&asset, &950_i128, &2u32, &3600u64);

    let stored = client.get_price(&asset);
    assert_eq!(stored.price, 950_i128);
    assert_eq!(stored.timestamp, 1_700_000_123);
}

#[test]
fn test_update_price_provider_can_store_new_price() {
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

    env.ledger().set_timestamp(1_700_000_500);
    env.ledger().set_sequence_number(2);
    client.update_price(&provider, &asset, &1_500_000_i128, &6u32, &100u32, &3600u64);

    let stored = client.get_price(&asset);
    assert_eq!(stored.price, 1_500_000_i128);
    assert_eq!(stored.timestamp, 1_700_000_500);
    assert_eq!(stored.provider, provider); // not contract_id
}

#[test]
fn test_update_price_multiple_updates() {
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

    client.update_price(&provider, &asset, &1_000_i128, &6u32, &100u32, &3600u64);
    client.update_price(&provider, &asset, &1_020_i128, &6u32, &100u32, &3600u64);

    let stored = client.get_price(&asset);
    assert_eq!(stored.price, 1_020_i128);
}

#[test]
fn test_update_price_admin_authority() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let unauthorized_address = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
    });

    let result = client.try_update_price(
        &unauthorized_address,
        &symbol_short!("NGN"),
        &50_000_000_000_i128,
        &8u32,
        &100u32,
        &3600u64,
    );
    match result {
        Err(Ok(e)) => assert_eq!(e, Error::NotAuthorized),
        other => panic!("expected NotAuthorized, got {:?}", other),
    }
}

#[test]
fn test_update_price_rejects_unapproved_symbol() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let provider = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
        crate::auth::_add_provider(&env, &provider);
    });

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

    env.ledger().set_timestamp(1_700_000_000);
    env.ledger().set_sequence_number(1);
    client.update_price(&provider, &asset, &price, &6u32, &100u32, &3600u64);

    let events = env.events().all();
    let debug_str = alloc::format!("{:?}", events);
    assert!(debug_str.contains("price_updated_event"));
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

    let stored = client.get_price(&asset);
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

    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let provider = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let asset = symbol_short!("NGN");

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
        crate::auth::_add_provider(&env, &provider);
    });

    // Set bounds: 500 to 2000
    client.set_price_bounds(&admin, &asset, &500_i128, &2_000_i128);

    // Price within bounds should succeed
    let result = client.try_update_price(&provider, &asset, &1_000_i128, &6u32, &100u32, &3600u64);
    assert!(result.is_ok());

    let stored = client.get_price(&asset);
    assert_eq!(stored.price, 1_000_i128);
}

#[test]
fn test_update_price_below_min_bound_rejected() {
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

    // Set bounds: 500 to 2000
    client.set_price_bounds(&admin, &asset, &500_i128, &2_000_i128);

    // Price below min should be rejected
    let result = client.try_update_price(&provider, &asset, &100_i128, &6u32, &100u32, &3600u64);
    match result {
        Err(Ok(e)) => assert_eq!(e, Error::PriceOutOfBounds),
        other => panic!("expected PriceOutOfBounds, got {:?}", other),
    }
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

    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let provider = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let asset = symbol_short!("NGN");

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
        crate::auth::_add_provider(&env, &provider);
    });

    // Set bounds: 500 to 2000
    client.set_price_bounds(&admin, &asset, &500_i128, &2_000_i128);

    // Price at exact min
    let result = client.try_update_price(&provider, &asset, &500_i128, &6u32, &100u32, &3600u64);
    assert!(result.is_ok());

    // Price at exact max (delta check may interfere, so set first via set_price to get close)
    // Use set_price to reset, then update_price at exact max
    client.set_price(&asset, &1_990_i128, &6u32, &3600u64);
    let result = client.try_update_price(&provider, &asset, &2_000_i128, &6u32, &100u32, &3600u64);
    assert!(result.is_ok());
}

#[test]
fn test_update_price_no_bounds_set_allows_any_valid_price() {
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
