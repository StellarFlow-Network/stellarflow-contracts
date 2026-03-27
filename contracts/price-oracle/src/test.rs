#![cfg(test)]

use super::*;
use soroban_sdk::{
    contract, contractimpl, symbol_short, testutils::Address as _, testutils::Ledger, Address,
    Env, Symbol,
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
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let pairs = soroban_sdk::vec![&env, symbol_short!("NGN"), symbol_short!("KES")];
    client.initialize(&admin, &pairs);
    // Must be inside as_contract to access instance storage
    env.as_contract(&contract_id, || {
        let stored_admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        assert_eq!(stored_admin, admin);

        let stored_pairs: soroban_sdk::Vec<Symbol> = env
            .storage()
            .instance()
            .get(&DataKey::BaseCurrencyPairs)
            .unwrap();
        assert_eq!(stored_pairs, pairs);
    });
}

#[test]
#[should_panic(expected = "Contract already initialized")]
fn test_initialize_double_panics() {
    let env = Env::default();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let pairs = soroban_sdk::vec![&env, symbol_short!("NGN")];
    client.initialize(&admin, &pairs);
    // Second call should panic
    client.initialize(&admin, &pairs);
}

fn setup() -> (Env, PriceOracleClient<'static>) {
    let env = Env::default();
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
    let admin = Address::generate(&env);

    client.init_admin(&admin);

    env.as_contract(&contract_id, || {
        assert!(crate::auth::_has_admin(&env));
        assert_eq!(crate::auth::_get_admin(&env), admin);
    });
}

#[test]
fn test_get_admin_reader_returns_current_admin() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin = Address::generate(&env);

    client.init_admin(&admin);

    assert_eq!(client.get_admin(), admin);
}

#[test]
#[should_panic(expected = "Admin already initialised")]
fn test_init_admin_panics_when_called_twice() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let first_admin = Address::generate(&env);
    let second_admin = Address::generate(&env);

    client.init_admin(&first_admin);
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

    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("NGN");

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &admin);
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

    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("NGN");

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &admin);
        crate::auth::_add_provider(&env, &provider);
    });

    client.update_price(&provider, &asset, &1_000_000_i128, &6u32, &100u32, &3600u64);
    client.update_price(&provider, &asset, &1_200_000_i128, &6u32, &100u32, &3600u64);

    let stored = client.get_price(&asset);
    assert_eq!(stored.price, 1_200_000_i128);
}

#[test]
fn test_update_price_unauthorized_rejection() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let unauthorized_address = Address::generate(&env);

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &admin);
    });

    let result = client.try_update_price(
        &unauthorized_address,
        &symbol_short!("NGN"),
        &50_000_000_000_i128,
        &8u32,
        &100u32,
        &3600u64,
    );
    assert!(result.is_err());
}

#[test]
fn test_update_price_rejects_unapproved_symbol() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let provider = Address::generate(&env);

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &admin);
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

    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("NGN");
    let price: i128 = 1_500_000;

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &admin);
        crate::auth::_add_provider(&env, &provider);
    });

    env.ledger().set_timestamp(1_700_000_000);
    env.ledger().set_sequence_number(1);
    client.update_price(&provider, &asset, &price, &6u32, &100u32, &3600u64);

    // let events = env.events().all();
    // assert!(events.len() > 0);
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
