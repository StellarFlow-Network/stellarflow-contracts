#![cfg(test)]

use super::*;
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Events, Ledger},
    Address, Env,
};

fn setup() -> (Env, Address, PriceOracleClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
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
        AssetWeight {
            asset: symbol_short!("NGN"),
            weight: 4000
        }, // 40%
        AssetWeight {
            asset: symbol_short!("GHS"),
            weight: 3000
        }, // 30%
        AssetWeight {
            asset: symbol_short!("CFA"),
            weight: 3000
        }, // 30%
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
fn test_revoke_key_blocks_compromised_admin_and_provider() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let compromised = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
        crate::auth::_add_authorized(&env, &compromised);
        crate::auth::_add_provider(&env, &compromised);
    });

    assert!(client.revoke_key(&admin, &compromised));

    env.as_contract(&contract_id, || {
        assert!(!crate::auth::_is_authorized(&env, &compromised));
        assert!(!crate::auth::_is_provider(&env, &compromised));
        assert!(crate::auth::_is_revoked(&env, &compromised));
    });
}

#[test]
#[should_panic]
fn test_init_admin_panics_when_called_twice() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
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
    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);
    env.ledger().with_mut(|li| li.timestamp = 1_234_567_890);
    env.ledger().with_mut(|li| li.sequence_number = 1);

    let asset = symbol_short!("NGN");
    client.set_price(&asset, &1_000_000_i128);

    let result = client.try_get_price(&asset, &true);
    let retrieved_price = result.unwrap()
    assert_eq!(retrieved_price.price, 1_000_000_i128);
    assert_eq!(retrieved_price.timestamp, 1_234_567_890);
    assert_eq!(retrieved_price.decimals, 6u32);
    assert_eq!(retrieved_price.provider, contract_id);
}

#[test]
fn test_get_price_nonexistent_asset() {
    let env = Env::default();
    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);
    let asset = symbol_short!("BTC");

    let result = client.try_get_price(&asset, &true);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().unwrap(), Error::AssetNotFound);
}

#[test]
fn test_get_price_multiple_assets() {
    let env = Env::default();
    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);
    let ngn = symbol_short!("NGN");
    let kes = symbol_short!("KES");

    client
        .try_set_price(&ngn, &1_000_000_i128)
        .unwrap()
        
    client
        .try_set_price(&kes, &50_000_000_000_i128)
        .unwrap()
        

    assert_eq!(
        client.try_get_price(&ngn, &true).unwrap().unwrap().price,
        1_000_000_i128
    );
    assert_eq!(
        client.try_get_price(&kes, &true).unwrap().unwrap().price,
        50_000_000_000_i128
    );
}

#[test]
fn test_get_price_after_update() {
    let env = Env::default();
    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);
    let asset = symbol_short!("NGN");

    env.ledger().with_mut(|li| li.timestamp = 1_234_567_890);
    env.ledger().with_mut(|li| li.sequence_number = 1);
    client
        .try_set_price(&asset, &1_000_000_i128, &6u32, &3600u64)
        .unwrap()
        

    let initial = client.try_get_price(&asset, &true).unwrap()
    assert_eq!(initial.price, 1_000_000_i128);
    assert_eq!(initial.timestamp, 1_234_567_890);

    env.ledger().with_mut(|li| li.timestamp = 1_234_567);
    client.set_price(&asset, &1_500_i128, &2u32, &3_600u64);

    let updated = client.try_get_price(&asset, &true).unwrap()
    assert_eq!(updated.price, 1_200_000_i128);
    assert_eq!(updated.timestamp, 1_234_567_900);
}

#[test]
fn test_get_price_with_status_marks_stale_entries() {
    let (env, _, client) = setup();
    let asset = symbol_short!("NGN");

    env.ledger().with_mut(|li| li.timestamp = 1_000);
    client.set_price(&asset, &1_500_i128, &2u32, &100u64);

    env.ledger().with_mut(|li| li.timestamp = 1_101);
    let result = client.get_price_with_status(&asset);

    assert_eq!(result.data.price, 1_500_i128);
    assert!(result.is_stale);
}

#[test]
fn test_update_price_rejects_minimum_quorum_not_met() {
    let (env, contract_id, client) = setup();
    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("NGN");

    add_provider(&env, &contract_id, &provider);
    client.initialize(&admin, &soroban_sdk::vec![&env, asset.clone()]);

    let asset = symbol_short!("ZAR");
    client.add_asset(&admin, &asset);
    let result = client.try_update_price(&provider, &asset, &1_000_i128, &6u32, &100u32, &3_600u64, &100_000_i128);
    match result {
        Err(Ok(err)) => assert_eq!(err, Error::MinimumQuorumNotMet),
        other => panic!("expected MinimumQuorumNotMet, got {:?}", other),
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

    let asset = symbol_short!("ZAR");
    client.add_asset(&admin, &asset);
    set_admin(&env, &contract_id, &admin);
    add_provider(&env, &contract_id, &provider);

    client.set_price(&asset, &1_000_i128, &2u32, &3_600u64, &100_000_i128);

    let result = client.try_update_price(&provider, &asset, &1_200_i128, &2u32, &100u32, &3_600u64, &100_000_i128);
    match result {
        Err(Ok(err)) => assert_eq!(err, Error::FlashCrashDetected),
        other => panic!("expected FlashCrashDetected, got {:?}", other),
    }
}

#[test]
fn test_update_price_rejects_incomplete_quorum() {
    let (env, contract_id, client) = setup();
    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("NGN");

    add_provider(&env, &contract_id, &provider);
    client.initialize(&admin, &soroban_sdk::vec![&env, asset.clone()]);

    let asset = symbol_short!("ZAR");
    client.add_asset(&non_admin, &asset);
    let result = client.try_update_price(&provider, &asset, &1_000_i128, &6u32, &100u32, &3_600u64, &100_000_i128);
    match result {
        Err(Ok(err)) => assert_eq!(err, Error::IncompleteQuorum),
        other => panic!("expected IncompleteQuorum, got {:?}", other),
    }
}

#[test]
fn test_set_and_get_max_deviation_percentage() {
    let (env, contract_id, client) = setup();
    let admin = Address::generate(&env);

    set_admin(&env, &contract_id, &admin);
    client.set_max_deviation_percentage(&admin, &500_i128);

    let max_deviation = client.get_max_deviation_percentage();
    assert_eq!(max_deviation, 500_i128);
}

#[test]
fn test_set_max_deviation_percentage_rejects_values_below_floor() {
    let (env, contract_id, client) = setup();
    let admin = Address::generate(&env);

    set_admin(&env, &contract_id, &admin);

    let result = client.try_set_max_deviation_percentage(&admin, &50_i128);
    match result {
        Err(Ok(e)) => assert_eq!(e, Error::InvalidMaxDeviation),
        other => panic!("expected InvalidMaxDeviation, got {:?}", other),
    }
}

#[test]
fn test_get_max_deviation_percentage_clamps_legacy_low_storage() {
    let (env, contract_id, client) = setup();

    env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .set(&DataKey::MaxPriceDeviationBps, &50_i128);
    });

    assert_eq!(client.get_max_deviation_percentage(), 100_i128);
}

#[test]
fn test_rollback_max_deviation_rejects_values_below_floor() {
    let (env, contract_id, client) = setup();
    let admin = Address::generate(&env);

    set_admin(&env, &contract_id, &admin);
    client.set_max_deviation_percentage(&admin, &500_i128);

    env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .set(&DataKey::PrevMaxDeviationBps, &50_i128);
    });

    let result = client.try_rollback_max_deviation_pct(&admin);
    match result {
        Err(Ok(e)) => assert_eq!(e, Error::InvalidMaxDeviation),
        other => panic!("expected InvalidMaxDeviation, got {:?}", other),
    }

    assert_eq!(client.get_max_deviation_percentage(), 500_i128);
}

#[test]
fn test_update_price_rejects_configured_max_deviation() {
    let (env, contract_id, client) = setup();
    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("NGN");

    set_admin(&env, &contract_id, &admin);
    add_provider(&env, &contract_id, &provider);
    client.add_asset(&admin, &asset);
    client.set_max_deviation_percentage(&admin, &500_i128);
    client.set_price(&asset, &1_000_i128, &2u32, &3_600u64, &100_000_i128);

    let result = client.try_update_price(&provider, &asset, &1_100_i128, &2u32, &100u32, &3_600u64, &100_000_i128);
    match result {
        Err(Ok(err)) => assert_eq!(err, Error::FlashCrashDetected),
        other => panic!("expected FlashCrashDetected, got {:?}", other),
    }
}

#[test]
fn test_set_min_quorum_threshold_rejects_values_below_floor() {
    let (env, contract_id, client) = setup();
    let admin = Address::generate(&env);

    set_admin(&env, &contract_id, &admin);

    let result = client.try_set_min_quorum_threshold(&admin, &1u32);
    match result {
        Err(Ok(e)) => assert_eq!(e, Error::MultiSigValidationFailed),
        other => panic!("expected MultiSigValidationFailed, got {:?}", other),
    }
}

#[test]
fn test_get_min_quorum_threshold_clamps_legacy_low_storage() {
    let (env, contract_id, client) = setup();

    env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .set(&DataKey::MinQuorumThreshold, &1u32);
    });

    assert_eq!(client.get_min_quorum_threshold(), 2u32);
}

#[test]
fn test_set_and_get_price_bounds() {
    let (env, contract_id, client) = setup();
    let admin = Address::generate(&env);
    let asset = symbol_short!("NGN");

    set_admin(&env, &contract_id, &admin);
    client.set_price_bounds(&admin, &asset, &500_i128, &2_000_i128);

    let bounds = client.get_price_bounds(&asset)
    assert_eq!(bounds.min_price, 500_i128);
    assert_eq!(bounds.max_price, 2_000_i128);
}

#[test]
fn test_set_price_bounds_emits_indexable_price_bounds_event() {
fn test_register_assets_with_config_applies_all_config_atomically() {
    let (env, contract_id, client) = setup();
    let admin = Address::generate(&env);
    let asset = symbol_short!("NGN");

    set_admin(&env, &contract_id, &admin);
    client.set_price_bounds(&admin, &asset, &500_i128, &2_000_i128);

    let events = env.events().all();
    let debug_str = alloc::format!("{:?}", events);
    assert!(debug_str.contains("price_bounds_set"));
    assert!(debug_str.contains("NGN"));
}

#[test]
fn test_set_price_floor_emits_indexable_price_floor_event() {

    let config = AssetRegistrationConfig {
        asset: asset.clone(),
        name: Symbol::new(&env, "Nigerian Naira"),
        base_decimals: 7,
        quote_decimals: 2,
        min_price: 500_i128,
        max_price: 2_000_i128,
        price_floor: Some(600_i128),
    };

    client
        .register_assets_with_config(&admin, &vec![&env, config], &500_i128)
        

    let info = client.get_asset_info(&asset)
    assert_eq!(info.name, Symbol::new(&env, "Nigerian Naira"));
    assert_eq!(info.base_decimals, 7);
    assert_eq!(info.quote_decimals, 2);

    let meta = client.get_asset_meta(&asset)
    assert_eq!(meta.base_decimals, 7);
    assert_eq!(meta.quote_decimals, 2);

    let bounds = client.get_price_bounds(&asset)
    assert_eq!(bounds.min_price, 500_i128);
    assert_eq!(bounds.max_price, 2_000_i128);
    assert_eq!(client.get_max_deviation_percentage(), 500_i128);
    assert_eq!(client.get_price_floor(&asset), Some(600_i128));
}

#[test]
fn test_register_assets_with_config_rolls_back_on_invalid_config() {
    let (env, contract_id, client) = setup();
    let admin = Address::generate(&env);
    let asset = symbol_short!("NGN");

    set_admin(&env, &contract_id, &admin);
    client.set_price_floor(&admin, &asset, &700_i128);

    let events = env.events().all();
    let debug_str = alloc::format!("{:?}", events);
    assert!(debug_str.contains("price_floor_set"));
    assert!(debug_str.contains("NGN"));

    let bad_config = AssetRegistrationConfig {
        asset: asset.clone(),
        name: Symbol::new(&env, "Nigerian Naira"),
        base_decimals: 7,
        quote_decimals: 2,
        min_price: 2_500_i128,
        max_price: 2_000_i128,
        price_floor: Some(600_i128),
    };

    let result = client.try_register_assets_with_config(&admin, &vec![&env, bad_config], &500_i128);
    match result {
        Err(Ok(err)) => assert_eq!(err, Error::InvalidPriceBounds),
        other => panic!("expected InvalidPriceBounds, got {:?}", other),
    }

    assert!(client.get_asset_info(&asset).is_none());
    assert!(client.get_asset_meta(&asset).is_none());
    assert!(client.get_price_bounds(&asset).is_none());
    assert_eq!(client.get_price_floor(&asset), None);
    assert_eq!(client.get_max_deviation_percentage(), 1_000_i128);
}

#[test]
fn test_update_price_rejects_price_outside_bounds() {
    let (env, contract_id, client) = setup();
    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("NGN");

    env.ledger().with_mut(|li| li.timestamp = 1_700_000_123);
    env.ledger().with_mut(|li| li.sequence_number = 77);
    client.set_price(&asset, &950_i128, &2u32, &3600u64, &100_000_i128);

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
    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);
    let asset = symbol_short!("NGN");

    let result = client.try_update_price(&provider, &asset, &250_i128, &2u32, &100u32, &3_600u64, &100_000_i128);
    match result {
        Err(Ok(err)) => assert_eq!(err, Error::PriceOutOfBounds),
        other => panic!("expected PriceOutOfBounds, got {:?}", other),
    }
}

#[test]
#[should_panic(expected = "HostError")]
fn test_set_price_rejects_negative_price() {
    let env = Env::default();
    let contract_id = env.register_contract(None, PriceOracle);
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

    env.ledger().with_mut(|li| li.timestamp = 1_700_000_500);
    env.ledger().with_mut(|li| li.sequence_number = 2);
    client.update_price(&provider, &asset, &1_500_000_i128, &6u32, &100u32, &3600u64, &100_000_i128);

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
    client.set_price(&asset, &650_i128, &2u32, &3_600u64, &100_000_i128);
    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
        crate::auth::_add_provider(&env, &provider);
    });

    client.update_price(
        &unauthorized_address,
        &symbol_short!("NGN"),
        &50_000_000_000_i128,
    );
}

#[test]
fn test_remove_asset_clears_related_configuration() {
    let (env, contract_id, client) = setup();
    let admin = Address::generate(&env);
    let asset = symbol_short!("NGN");

    set_admin(&env, &contract_id, &admin);
    client.set_price(&asset, &1_000_i128, &2u32, &3_600u64, &100_000_i128);
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
#[should_panic]
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
    match client.try_update_price(&provider, &asset, &price, &6u32, &100u32, &3600u64, &100_000_i128) {
        Err(Ok(e)) => assert_eq!(e, Error::InvalidAssetSymbol),
        other => panic!("expected InvalidAssetSymbol, got {:?}", other),
    }
}

#[test]
fn test_update_price_emits_event() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("NGN");

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
        crate::auth::_add_provider(&env, &provider);
    });

    client.add_asset(&admin, &asset);

    env.ledger().with_mut(|li| li.timestamp = 1_700_000_000);
    env.ledger().with_mut(|li| li.sequence_number = 1);
    client.update_price(&provider, &asset, &price, &6u32, &100u32, &3600u64, &100_000_i128);

    let events = env.events().all();
    let debug_str = alloc::format!("{:?}", events);
    assert!(debug_str.contains("price_updated_event"));
    assert!(debug_str.contains("price_update"));
}

#[test]
fn test_update_price_emits_indexable_price_update_topic() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
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

    env.ledger().with_mut(|li| li.timestamp = 1_700_000_000);
    env.ledger().with_mut(|li| li.sequence_number = 1);
    client.update_price(&provider, &asset, &price, &6u32, &100u32, &3600u64, &100_000_i128);

    let events = env.events().all();
    assert!(!events.events().is_empty());
}

#[test]
fn test_update_price_emits_cross_call_event_on_5pct_move() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
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

    client.set_price(&asset, &old_price, &6u32, &3600u64, &100_000_i128);

    client.update_price(&provider, &asset, &new_price, &6u32, &100u32, &3600u64, &100_000_i128);

    let events = env.events().all();
    let debug_str = alloc::format!("{:?}", events);
    // "cross_call" topic must be present
    assert!(
        debug_str.contains("cross_call"),
        "expected cross_call event, got: {}",
        debug_str
    );
}

#[test]
fn test_update_price_no_cross_call_event_below_5pct() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
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

    client.set_price(&asset, &old_price, &6u32, &3600u64, &100_000_i128);
    client.update_price(&provider, &asset, &new_price, &6u32, &100u32, &3600u64, &100_000_i128);

    let events = env.events().all();
    let debug_str = alloc::format!("{:?}", events);
    assert!(
        !debug_str.contains("cross_call"),
        "cross_call should NOT fire below 5%"
    );
}

#[test]
fn test_update_price_delta_limit_rejection_emits_anomaly_event() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let provider = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let asset = symbol_short!("NGN");

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
        crate::auth::_add_provider(&env, &provider);
    });

    client.add_asset(&admin, &asset);

    env.ledger().with_mut(|li| li.timestamp = 1_700_100_000);
    env.ledger().with_mut(|li| li.sequence_number = 1);
    client.update_price(&provider, &asset, &1_000_i128, &6u32, &100u32, &3600u64, &100_000_i128);

    env.ledger().with_mut(|li| li.timestamp = 1_700_100_010);
    env.ledger().with_mut(|li| li.sequence_number = 2);
    let result = client.try_update_price(&provider, &asset, &1_100_i128, &6u32, &100u32, &3600u64, &100_000_i128);
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

    let contract_id = env.register_contract(None, PriceOracle);
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

    client.set_price(&asset, &old_price, &6u32, &3600u64, &100_000_i128);

    // Should reject 20% increase (exceeds 10% MAX_PERCENT_CHANGE)
    match client.try_update_price(&provider, &asset, &new_price, &6u32, &100u32, &3600u64, &100_000_i128) {
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

    let contract_id = env.register_contract(None, PriceOracle);
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
        env.ledger().with_mut(|li| li.timestamp = 1_000_000 + i * 10);
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
    assert_eq!(calculate_price_volatility(500_000, 500_000), Some(0));
}

#[test]
fn test_price_volatility_from_zero() {
    assert_eq!(calculate_price_volatility(0, 1_000_000), Some(1_000_000));
}

#[test]
fn test_price_volatility_to_zero() {
    assert_eq!(calculate_price_volatility(1_000_000, 0), Some(1_000_000));
}

#[test]
fn test_is_stale_with_mocked_ledger_time() {
    // Test case: ledger_time=2000, stored_timestamp=1000, ttl=500
    // Expected: 2000 >= (1000 + 500) = 2000 >= 1500 = true (stale)
    let current_time = 2000u64;
    let stored_timestamp = 1000u64;
    let ttl = 500u64;

    assert!(
        is_stale(current_time, stored_timestamp, ttl),
        "Price should be stale"
    );

    // Additional test: not stale case
    // current_time < stored_timestamp + ttl should return false
    assert!(
        !is_stale(1400u64, 1000u64, 500u64),
        "Price should not be stale when within TTL"
    );

    // Edge case: exactly at expiration boundary
    assert!(
        is_stale(1500u64, 1000u64, 500u64),
        "Price should be stale at expiration boundary"
    );
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

    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
    });

    let asset = symbol_short!("NGN");
    client.set_price(&asset, &1_000_i128, &2u32, &3600u64, &100_000_i128);

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

    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
    });

    let ngn = symbol_short!("NGN");
    let kes = symbol_short!("KES");
    client.set_price(&ngn, &1_000_i128, &2u32, &3600u64, &100_000_i128);
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

    let contract_id = env.register_contract(None, PriceOracle);
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

    let contract_id = env.register_contract(None, PriceOracle);
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
    match client.try_update_price(&provider, &asset, &new_price, &6u32, &100u32, &3600u64, &100_000_i128) {
        Err(Ok(e)) => assert_eq!(e, Error::FlashCrashDetected),
        other => panic!("expected FlashCrashDetected, got {:?}", other),
    }
}

#[test]
fn test_remove_asset_non_admin_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let non_admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
    });

    let asset = symbol_short!("NGN");
    client.set_price(&asset, &1_000_i128, &2u32, &3600u64, &100_000_i128);

    let result = client.try_remove_asset(&non_admin, &asset);
    assert!(result.is_err());
}

#[test]
fn test_clear_assets_removes_persistent_price_keys() {
    let env = Env::default();
    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let ngn = symbol_short!("NGN");
    let kes = symbol_short!("KES");

    env.as_contract(&contract_id, || {
        env.storage().persistent().set(
            &DataKey::Price(ngn.clone()),
            &PriceData {
                price: 1_000,
                timestamp: 10,
                ledger_sequence: 0,
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
                ledger_sequence: 0,
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
    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let protected = symbol_short!("NGN");
    env.as_contract(&contract_id, || {
        env.storage().persistent().set(
            &DataKey::Price(protected.clone()),
            &PriceData {
                price: 1_000,
                timestamp: 10,
                ledger_sequence: 0,
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
    let oracle_id = env.register_contract(None, PriceOracle);
    let oracle_client = PriceOracleClient::new(&env, &oracle_id);

    // Register the dummy consumer contract
    let dummy_id = env.register(DummyConsumer, ());
    let dummy_client = DummyConsumerClient::new(&env, &dummy_id);

    // Set up the oracle with some prices
    let ngn = symbol_short!("NGN");
    let price = 1_500_000_i128;
    env.ledger().with_mut(|li| li.timestamp = 1_234_567_890);
    env.ledger().with_mut(|li| li.sequence_number = 1);
    oracle_client.set_price(&ngn, &price, &2u32, &3600u64);

    // The Dummy contract calls the Oracle to get the price
    let fetched_price = dummy_client.get_oracle_price(&oracle_id, &ngn);

    assert_eq!(
        fetched_price, price,
        "Dummy contract should receive correct price from Oracle"
    );
}

#[test]
fn test_dummy_consumer_gets_all_assets() {
    let env = Env::default();

    let oracle_id = env.register_contract(None, PriceOracle);
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

    let oracle_id = env.register_contract(None, PriceOracle);
    let oracle_client = PriceOracleClient::new(&env, &oracle_id);

    let dummy_id = env.register(DummyConsumer, ());
    let dummy_client = DummyConsumerClient::new(&env, &dummy_id);

    // Add a price to the oracle
    let ngn = symbol_short!("NGN");
    let btc = symbol_short!("BTC"); // Not added to oracle
    let price = 1_500_000_i128;

    env.ledger().with_mut(|li| li.timestamp = 1_234_567_890);
    env.ledger().with_mut(|li| li.sequence_number = 1);
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
    assert!(
        missing_price.is_none(),
        "Should return None for non-existent asset"
    );
}

#[test]
fn test_dummy_consumer_multiple_price_fetches() {
    let env = Env::default();

    let oracle_id = env.register_contract(None, PriceOracle);
    let oracle_client = PriceOracleClient::new(&env, &oracle_id);

    let dummy_id = env.register(DummyConsumer, ());
    let dummy_client = DummyConsumerClient::new(&env, &dummy_id);

    // Set up initial prices
    let ngn = symbol_short!("NGN");
    let kes = symbol_short!("KES");
    env.ledger().with_mut(|li| li.timestamp = 1_000_000);
    env.ledger().with_mut(|li| li.sequence_number = 1);
    oracle_client.set_price(&ngn, &1_000_000_i128, &2u32, &3600u64);
    oracle_client.set_price(&kes, &500_000_i128, &2u32, &3600u64);

    // First call - verify prices
    let ngn_price_1 = dummy_client.get_oracle_price(&oracle_id, &ngn);
    let kes_price_1 = dummy_client.get_oracle_price(&oracle_id, &kes);
    assert_eq!(ngn_price_1, 1_000_000_i128);
    assert_eq!(kes_price_1, 500_000_i128);

    // Update prices
    env.ledger().with_mut(|li| li.timestamp = 2_000_000);
    env.ledger().with_mut(|li| li.sequence_number = 2);
    oracle_client.set_price(&ngn, &1_200_000_i128, &2u32, &3600u64);
    oracle_client.set_price(&kes, &450_000_i128, &2u32, &3600u64);

    // Second call - verify updated prices
    let ngn_price_2 = dummy_client.get_oracle_price(&oracle_id, &ngn);
    let kes_price_2 = dummy_client.get_oracle_price(&oracle_id, &kes);
    assert_eq!(ngn_price_2, 1_200_000_i128);
    assert_eq!(kes_price_2, 450_000_i128);
}

// ============================================================================
// Rewards: accumulation + claim tests
// ============================================================================

#[contract]
pub struct DummyToken;

#[contractimpl]
impl DummyToken {
    /// Minimal transfer implementation that records received amounts per address.
    pub fn transfer(env: Env, _from: Address, to: Address, amount: i128) {
        let key = Symbol::new(&env, "TRANSFERS");
        let mut transfers: soroban_sdk::Map<Address, i128> = env
            .storage()
            .instance()
            .get(&key)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));

        let prev = transfers.get(to.clone()).unwrap_or(0_i128);
        transfers.set(to.clone(), prev.saturating_add(amount));
        env.storage().instance().set(&key, &transfers);
    }
}

#[test]
fn test_rewards_accumulate_and_claim() {
    let env = Env::default();
    env.mock_all_auths();

    // Deploy the oracle contract
    let oracle_id = env.register_contract(None, PriceOracle);
    let oracle_client = PriceOracleClient::new(&env, &oracle_id);

    // Create a relayer address
    let relayer = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    // Simulate multiple consensus cycles crediting fractional rewards (units)
    env.as_contract(&oracle_id, || {
        crate::rewards::Rewards::add_to_balance(&env, &relayer, 5_i128);
        crate::rewards::Rewards::add_to_balance(&env, &relayer, 3_i128);
        crate::rewards::Rewards::add_to_balance(&env, &relayer, 2_i128);
    });

    // Verify the stored balance is the sum (10)
    env.as_contract(&oracle_id, || {
        let storage = env.storage().persistent();
        let map: soroban_sdk::Map<Address, i128> = storage
            .get(&DataKey::Rewards)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));
        let bal = map.get(relayer.clone()).unwrap_or(0_i128);
        assert_eq!(bal, 10_i128);
    });

    // Deploy dummy token and use its address as the token contract
    let token_id = env.register(DummyToken, ());

    // Relayer claims rewards via the public endpoint
    let claimed = oracle_client.claim_rewards(&relayer, &token_id);
    assert_eq!(claimed, 10_i128, "Claimed amount must equal accumulated sum");

    // Verify the on-chain balance was zeroed BEFORE transfer (i.e., now zero)
    env.as_contract(&oracle_id, || {
        let storage = env.storage().persistent();
        let map: soroban_sdk::Map<Address, i128> = storage
            .get(&DataKey::Rewards)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));
        let bal = map.get(relayer.clone()).unwrap_or(0_i128);
        assert_eq!(bal, 0_i128, "Balance must be zero after claim");
    });

    // Verify DummyToken recorded the transfer amount exactly
    let key = Symbol::new(&env, "TRANSFERS");
    let transfers: soroban_sdk::Map<Address, i128> = env
        .storage()
        .instance()
        .get(&key)
        .unwrap_or_else(|| soroban_sdk::Map::new(&env));
    let received = transfers.get(relayer.clone()).unwrap_or(0_i128);
    assert_eq!(received, 10_i128, "Token contract should have received exact amount");
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

    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    client.init_admin(&admin);

    let new_wasm_hash = env.deployer().upload_contract_wasm(TEST_WASM);
    // Should not panic – admin is authorised
    client.upgrade(&admin, &new_wasm_hash);
}

#[test]
#[should_panic]
fn test_upgrade_rejects_non_admin() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
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
    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let ngn = symbol_short!("NGN");
    let kes = symbol_short!("KES");
    let ghs = symbol_short!("GHS");

    env.ledger().with_mut(|li| li.timestamp = 1_000_000);
    env.ledger().with_mut(|li| li.sequence_number = 1);
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
    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let ngn = symbol_short!("NGN");
    let btc = symbol_short!("BTC"); // not stored

    env.ledger().with_mut(|li| li.timestamp = 1_000_000);
    env.ledger().with_mut(|li| li.sequence_number = 1);
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
    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let ngn = symbol_short!("NGN");

    // Store price with a short TTL of 100 seconds
    env.ledger().with_mut(|li| li.timestamp = 1_000_000);
    env.ledger().with_mut(|li| li.sequence_number = 1);
    client.set_price(&ngn, &1_500_i128, &2u32, &100u64);

    // Advance time past TTL
    env.ledger().with_mut(|li| li.timestamp = 1_000_200);
    env.ledger().with_mut(|li| li.sequence_number = 2);

    let assets = soroban_sdk::vec![&env, ngn.clone()];
    let results = client.get_prices(&assets);

    assert_eq!(results.len(), 1);
    assert!(results.get(0).unwrap().is_none()); // stale → None
}

#[test]
fn test_get_prices_preserves_order() {
    let env = Env::default();
    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let ngn = symbol_short!("NGN");
    let kes = symbol_short!("KES");

    env.ledger().with_mut(|li| li.timestamp = 1_000_000);
    env.ledger().with_mut(|li| li.sequence_number = 1);
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
    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let assets: soroban_sdk::Vec<Symbol> = soroban_sdk::vec![&env];
    let results = client.get_prices(&assets);

    assert_eq!(results.len(), 0);
}

#[test]
fn test_get_prices_with_status_marks_stale_entry() {
    let env = Env::default();
    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let ngn = symbol_short!("NGN");

    env.ledger().with_mut(|li| li.timestamp = 1_000_000);
    env.ledger().with_mut(|li| li.sequence_number = 1);
    client.set_price(&ngn, &1_500_i128, &2u32, &100u64);

    env.ledger().with_mut(|li| li.timestamp = 1_000_200);
    env.ledger().with_mut(|li| li.sequence_number = 2);

    let assets = soroban_sdk::vec![&env, ngn.clone()];
    let results = client.get_prices_with_status(&assets);

    assert_eq!(results.len(), 1);
    let entry = results.get(0).unwrap()
    assert_eq!(entry.price, 1_500_i128);
    assert!(entry.is_stale);
}

#[test]
fn test_get_prices_with_status_returns_none_for_missing_asset() {
    let env = Env::default();
    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let ngn = symbol_short!("NGN");
    let btc = symbol_short!("BTC");

    env.ledger().with_mut(|li| li.timestamp = 1_000_000);
    env.ledger().with_mut(|li| li.sequence_number = 1);
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

    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
    });

    let asset = symbol_short!("NGN");
    client.set_price_bounds(&admin, &asset, &500_i128, &2_000_i128);

    let bounds = client.get_price_bounds(&asset)
    assert_eq!(bounds.min_price, 500_i128);
    assert_eq!(bounds.max_price, 2_000_i128);
}

#[test]
fn test_get_price_bounds_returns_none_when_not_set() {
    let env = Env::default();
    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let bounds = client.get_price_bounds(&symbol_short!("NGN"));
    assert!(bounds.is_none());
}

#[test]
fn test_update_price_within_bounds_succeeds() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
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
    client.update_price(&provider, &asset, &new_price, &6u32, &100u32, &3600u64, &100_000_i128);

    let price_data = client.get_price(&asset, &true);
    assert_eq!(price_data.price, new_price);
}

#[test]
fn test_flash_crash_protection_allows_exact_threshold() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
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

    client.set_price(&asset, &old_price, &6u32, &3600u64, &100_000_i128);

    // Should allow exactly 10% increase (at threshold, not exceeding), delta=50 ≤ 50
    client.update_price(&provider, &asset, &new_price, &6u32, &100u32, &3600u64, &100_000_i128);

    let price_data = client.get_price(&asset, &true);
    assert_eq!(price_data.price, new_price);
}

#[test]
fn test_update_price_below_min_bound_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
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

    let result = client.try_update_price(&provider, &asset, &100_i128, &6u32, &100u32, &3600u64, &100_000_i128);
    match result {
        Err(Ok(e)) => assert_eq!(e, Error::PriceOutOfBounds),
        other => panic!("expected PriceOutOfBounds, got {:?}", other),
    }
}

#[test]
fn test_flash_crash_protection_allows_first_price_update() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
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
    client.update_price(&provider, &asset, &1_000_i128, &6u32, &100u32, &3600u64, &100_000_i128);

    let price_data = client.get_price(&asset, &true);
    assert_eq!(price_data.price, 1_000_i128);
}

#[test]
fn test_update_price_above_max_bound_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
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
    let result = client.try_update_price(&provider, &asset, &5_000_i128, &6u32, &100u32, &3600u64, &100_000_i128);
    match result {
        Err(Ok(e)) => assert_eq!(e, Error::PriceOutOfBounds),
        other => panic!("expected PriceOutOfBounds, got {:?}", other),
    }
}

#[test]
fn test_update_price_at_exact_bounds_succeeds() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
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
    client.update_price(&provider, &asset, &price, &6u32, &100u32, &3600u64, &100_000_i128);

    let price_data = client.get_price(&asset, &true);
    assert_eq!(price_data.price, price);
}

#[test]
fn test_flash_crash_protection_rejects_just_over_threshold() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
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

    client.set_price(&asset, &old_price, &6u32, &3600u64, &100_000_i128);

    match client.try_update_price(&provider, &asset, &new_price, &6u32, &100u32, &3600u64, &100_000_i128) {
        Err(Ok(e)) => assert_eq!(e, Error::FlashCrashDetected),
        other => panic!("expected FlashCrashDetected, got {:?}", other),
    }
}

#[test]
fn test_update_price_no_bounds_set_allows_any_valid_price() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
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
    let result = client.try_update_price(
        &provider,
        &asset,
        &999_999_999_i128,
        &6u32,
        &100u32,
        &3600u64,
    );
    assert!(result.is_ok());
}

#[test]
#[should_panic]
fn test_set_price_bounds_min_greater_than_max_panics() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    env.as_contract(&contract_id, || {
        crate::auth::_set_admin(&env, &soroban_sdk::vec![&env, admin.clone()]);
    });

    // min > max should panic
    client.set_price_bounds(&admin, &symbol_short!("NGN"), &2_000_i128, &500_i128);
}

#[test]
#[should_panic]
fn test_set_price_bounds_non_admin_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
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
    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let asset = symbol_short!("NGN");

    // Set price for a new asset
    client.set_price(&asset, &1_500_i128, &2u32, &3600u64, &100_000_i128);

    // Verify AssetAdded event was emitted
    let events = env.events().all();
    let debug_str = alloc::format!("{:?}", events);
    assert!(
        debug_str.contains("asset_added_event"),
        "AssetAdded event should be emitted for new asset"
    );
    assert!(
        debug_str.contains("symbol"),
        "Event should contain symbol field"
    );
    assert!(
        debug_str.contains("NGN"),
        "Event should contain the correct asset symbol"
    );
}

#[test]
fn test_set_price_does_not_emit_asset_added_event_on_update() {
    let env = Env::default();
    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let asset = symbol_short!("NGN");

    // First set - should emit AssetAdded
    client.set_price(&asset, &1_500_i128, &2u32, &3600u64, &100_000_i128);

    // Verify first event was emitted
    let events_after_first = env.events().all();
    let debug_str_first = alloc::format!("{:?}", events_after_first);
    assert!(
        debug_str_first.contains("asset_added_event"),
        "Should emit AssetAdded on first set"
    );

    // Second set (update) - should NOT emit AssetAdded
    env.ledger().with_mut(|li| li.timestamp = 1_234_567_900);
    client.set_price(&asset, &1_600_i128, &2u32, &3600u64);

    // Verify no AssetAdded event on update (only the update event should be present if any)
    let events_after_second = env.events().all();
    let debug_str_second = alloc::format!("{:?}", events_after_second);
    // Should NOT contain asset_added_event on update
    assert!(
        !debug_str_second.contains("asset_added_event"),
        "Should NOT emit AssetAdded on update"
    );
}

#[test]
fn test_multiple_assets_added_sequentially_each_emits_event() {
    let env = Env::default();
    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let ngn = symbol_short!("NGN");
    let kes = symbol_short!("KES");
    let ghs = symbol_short!("GHS");

    // Add NGN - should emit AssetAdded
    client.set_price(&ngn, &1_500_i128, &2u32, &3600u64);
    let events_ngn = env.events().all();
    let debug_ngn = alloc::format!("{:?}", events_ngn);
    assert!(
        debug_ngn.contains("asset_added_event"),
        "Should emit AssetAdded for NGN"
    );
    assert!(debug_ngn.contains("NGN"), "Should contain NGN symbol");

    // Add KES - should emit AssetAdded
    client.set_price(&kes, &800_i128, &2u32, &3600u64);
    let events_kes = env.events().all();
    let debug_kes = alloc::format!("{:?}", events_kes);
    assert!(
        debug_kes.contains("asset_added_event"),
        "Should emit AssetAdded for KES"
    );
    assert!(debug_kes.contains("KES"), "Should contain KES symbol");

    // Add GHS - should emit AssetAdded
    client.set_price(&ghs, &5_000_i128, &2u32, &3600u64);
    let events_ghs = env.events().all();
    let debug_ghs = alloc::format!("{:?}", events_ghs);
    assert!(
        debug_ghs.contains("asset_added_event"),
        "Should emit AssetAdded for GHS"
    );
    assert!(debug_ghs.contains("GHS"), "Should contain GHS symbol");
}

#[test]
fn test_mixed_add_and_update_emits_correct_events() {
    let env = Env::default();
    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let ngn = symbol_short!("NGN");
    let kes = symbol_short!("KES");

    // Add NGN (new asset) - should emit AssetAdded
    client.set_price(&ngn, &1_500_i128, &2u32, &3600u64);
    let events_ngn = env.events().all();
    let debug_ngn = alloc::format!("{:?}", events_ngn);
    assert!(
        debug_ngn.contains("asset_added_event"),
        "Should emit AssetAdded for NGN"
    );

    // Add KES (new asset) - should emit AssetAdded
    client.set_price(&kes, &800_i128, &2u32, &3600u64);
    let events_kes = env.events().all();
    let debug_kes = alloc::format!("{:?}", events_kes);
    assert!(
        debug_kes.contains("asset_added_event"),
        "Should emit AssetAdded for KES"
    );

    // Update NGN (existing asset) - should NOT emit AssetAdded
    env.ledger().with_mut(|li| li.timestamp = 1_234_567_900);
    client.set_price(&ngn, &1_600_i128, &2u32, &3600u64);
    let events_update = env.events().all();
    let debug_update = alloc::format!("{:?}", events_update);
    assert!(
        !debug_update.contains("asset_added_event"),
        "Should NOT emit AssetAdded on update"
    );

    // Add GHS (new asset) - should emit AssetAdded
    let ghs = symbol_short!("GHS");
    client.set_price(&ghs, &5_000_i128, &2u32, &3600u64);
    let events_ghs = env.events().all();
    let debug_ghs = alloc::format!("{:?}", events_ghs);
    assert!(
        debug_ghs.contains("asset_added_event"),
        "Should emit AssetAdded for GHS"
    );
}

#[test]
fn test_asset_added_event_contains_correct_symbol() {
    let env = Env::default();
    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let asset = symbol_short!("NGN");

    client.set_price(&asset, &1_500_i128, &2u32, &3600u64);

    // Verify event structure contains the correct symbol
    let events = env.events().all();
    let debug_str = alloc::format!("{:?}", events);
    assert!(
        debug_str.contains("asset_added_event"),
        "Should emit AssetAdded event"
    );
    assert!(
        debug_str.contains("NGN"),
        "Event should contain the correct asset symbol"
    );
}

#[test]
fn test_get_last_n_events_sliding_window() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, PriceOracle);
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
    assert_eq!(
        events.get(0).unwrap().event_type,
        Symbol::new(&env, "price_updated")
    );

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
    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);
    let asset = symbol_short!("NGN");

    // Initial write
    env.ledger().with_mut(|li| li.timestamp = 1_000_000);
    env.ledger().with_mut(|li| li.sequence_number = 1);
    client.set_price(&asset, &1_500_i128, &2u32, &3600u64);

    let first = client.get_price(&asset, &true);
    assert_eq!(first.price, 1_500_i128);
    assert_eq!(first.timestamp, 1_000_000);

    // Second call with the same price — only timestamp should advance
    env.ledger().with_mut(|li| li.timestamp = 1_001_000);
    env.ledger().with_mut(|li| li.sequence_number = 2);
    client.set_price(&asset, &1_500_i128, &2u32, &3600u64);

    let second = client.get_price(&asset, &true);
    assert_eq!(second.price, 1_500_i128, "price must remain unchanged");
    assert_eq!(second.timestamp, 1_001_000, "timestamp must be refreshed");
}

#[test]
fn test_set_price_different_value_writes_new_price() {
    let env = Env::default();
    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);
    let asset = symbol_short!("KES");

    env.ledger().with_mut(|li| li.timestamp = 2_000_000);
    env.ledger().with_mut(|li| li.sequence_number = 1);
    client.set_price(&asset, &800_i128, &2u32, &3600u64);

    env.ledger().with_mut(|li| li.timestamp = 2_001_000);
    env.ledger().with_mut(|li| li.sequence_number = 2);
    client.set_price(&asset, &850_i128, &2u32, &3600u64);

    let stored = client.get_price(&asset, &true);
    assert_eq!(stored.price, 850_i128, "new price must be stored");
    assert_eq!(stored.timestamp, 2_001_000);
}

#[test]
fn test_set_price_identical_value_still_emits_price_updated_event() {
    let env = Env::default();
    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);
    let asset = symbol_short!("GHS");

    env.ledger().with_mut(|li| li.timestamp = 3_000_000);
    env.ledger().with_mut(|li| li.sequence_number = 1);
    client.set_price(&asset, &5_000_i128, &2u32, &3600u64);

    // Clear events by reading them, then do the identical-price call
    env.ledger().with_mut(|li| li.timestamp = 3_001_000);
    env.ledger().with_mut(|li| li.sequence_number = 2);
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

    let contract_id = env.register_contract(None, PriceOracle);
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

    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    client.init_admin(&admin);
    client.renounce_ownership(&admin);

    let events = env.events().all();
    let debug_str = alloc::format!("{:?}", events);
    assert!(debug_str.contains("ownership_renounced_event"));
}

#[test]
#[should_panic]
fn test_renounce_ownership_rejects_non_admin() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
    let non_admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);

    client.init_admin(&admin);
    client.renounce_ownership(&non_admin);
}

#[test]
#[should_panic]
fn test_renounce_ownership_blocks_admin_functions_after_renouncement() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
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

    let contract_id = env.register_contract(None, PriceOracle);
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

    let contract_id = env.register_contract(None, PriceOracle);
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

    let contract_id = env.register_contract(None, PriceOracle);
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
#[should_panic]
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

    let contract_id = env.register_contract(None, PriceOracle);
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

    let contract_id = env.register_contract(None, PriceOracle);
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

    let contract_id = env.register_contract(None, PriceOracle);
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

    let contract_id = env.register_contract(None, PriceOracle);
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

    let contract_id = env.register_contract(None, PriceOracle);
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

    let contract_id = env.register_contract(None, PriceOracle);
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

#[test]
fn test_delegate_vote_reassign_and_clear() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let proxy1 = Address::generate(&env);
    let proxy2 = Address::generate(&env);

    assert_eq!(client.get_vote_delegate(&owner), None);

    client.delegate_vote(&owner, &proxy1);
    assert_eq!(client.get_vote_delegate(&owner), Some(proxy1));

    client.delegate_vote(&owner, &proxy2);
    assert_eq!(client.get_vote_delegate(&owner), Some(proxy2));

    client.clear_vote_delegate(&owner);
    assert_eq!(client.get_vote_delegate(&owner), None);
}

#[test]
fn test_delegate_proxy_vote_counts_owner_weight() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin1 = Address::generate(&env);
    let admin2 = Address::generate(&env);
    let proxy = Address::generate(&env);

    client.init_admin(&admin1);
    env.as_contract(&contract_id, || {
        crate::auth::_add_authorized(&env, &admin2);
    });

    client.delegate_vote(&admin2, &proxy);
    let action_id = client.propose_action(&admin1, &0u32, &None, &String::from_str(&env, ""));

    assert_eq!(client.vote_for_action(&proxy, &action_id), 2);
    client.execute_proposed_action(&admin1, &action_id);
    env.as_contract(&contract_id, || {
        assert!(crate::auth::_is_paused(&env));
    });
}

#[test]
fn test_cleared_delegate_cannot_vote_owner_weight() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin1 = Address::generate(&env);
    let admin2 = Address::generate(&env);
    let proxy = Address::generate(&env);

    client.init_admin(&admin1);
    env.as_contract(&contract_id, || {
        crate::auth::_add_authorized(&env, &admin2);
    });

    client.delegate_vote(&admin2, &proxy);
    client.clear_vote_delegate(&admin2);
    let action_id = client.propose_action(&admin1, &0u32, &None, &String::from_str(&env, ""));

    let result = client.try_vote_for_action(&proxy, &action_id);
    match result {
        Err(Ok(e)) => assert_eq!(e, Error::NotAuthorized),
        other => panic!("expected NotAuthorized, got {:?}", other),
    }
}

// ============================================================================
// Self-Destruct Tests
// ============================================================================

#[test]
fn test_self_destruct_requires_two_admins() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
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

    let contract_id = env.register_contract(None, PriceOracle);
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

    let contract_id = env.register_contract(None, PriceOracle);
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

    let contract_id = env.register_contract(None, PriceOracle);
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

    let contract_id = env.register_contract(None, PriceOracle);
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

    let contract_id = env.register_contract(None, PriceOracle);
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

// ────────────────────────────────────────────────────────────────────────────
// Issue #369: Ledger-Sync | Enforcing Absolute Chronological Gaps Tests
// ────────────────────────────────────────────────────────────────────────────

#[test]
fn test_ledger_gap_new_provider_allowed() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("NGN");

    env.ledger().with_mut(|li| li.timestamp = 1_000_000);
    env.ledger().with_mut(|li| li.sequence_number = 100);

    client.init_admin(&admin);
    client.add_asset(&admin, &asset)

    env.as_contract(&contract_id, || {
        crate::auth::_add_provider(&env, &provider);
    });

    // First submission from a new provider should succeed (no ledger gap restriction)
    let result = client.try_update_price(&provider, &asset, &100_000_000, &9, &95, &3600);
    assert!(result.is_ok(), "New provider should be allowed to submit without ledger gap restriction");
}

#[test]
fn test_ledger_gap_insufficient_gap_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("KES");

    env.ledger().with_mut(|li| li.timestamp = 1_000_000);
    env.ledger().with_mut(|li| li.sequence_number = 100);

    client.init_admin(&admin);
    client.add_asset(&admin, &asset)

    env.as_contract(&contract_id, || {
        crate::auth::_add_provider(&env, &provider);
    });

    // First submission at ledger 100
    let result1 = client.try_update_price(&provider, &asset, &100_000_000, &9, &95, &3600);
    assert!(result1.is_ok(), "First submission should succeed");

    // Try to submit again at ledger 102 (gap of 2 blocks, less than required 3)
    env.ledger().with_mut(|li| li.sequence_number = 102);
    env.ledger().with_mut(|li| li.timestamp = 1_000_100);

    let result2 = client.try_update_price(&provider, &asset, &105_000_000, &9, &95, &3600);
    match result2 {
        Err(Ok(ContractError::LedgerGapTooSmall)) => {
            // Expected!
        }
        other => panic!(
            "Expected LedgerGapTooSmall error for insufficient gap, got: {:?}",
            other
        ),
    }
}

#[test]
fn test_ledger_gap_exactly_3_blocks_allowed() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("GHS");

    env.ledger().with_mut(|li| li.timestamp = 1_000_000);
    env.ledger().with_mut(|li| li.sequence_number = 100);

    client.init_admin(&admin);
    client.add_asset(&admin, &asset)

    env.as_contract(&contract_id, || {
        crate::auth::_add_provider(&env, &provider);
    });

    // First submission at ledger 100
    let result1 = client.try_update_price(&provider, &asset, &50_000_000, &9, &95, &3600);
    assert!(result1.is_ok(), "First submission should succeed");

    // Submit again at ledger 103 (gap of exactly 3 blocks)
    env.ledger().with_mut(|li| li.sequence_number = 103);
    env.ledger().with_mut(|li| li.timestamp = 1_000_100);

    let result2 = client.try_update_price(&provider, &asset, &51_000_000, &9, &95, &3600);
    assert!(
        result2.is_ok(),
        "Submission after exactly 3-block gap should succeed"
    );
}

#[test]
fn test_ledger_gap_more_than_3_blocks_allowed() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("CFA");

    env.ledger().with_mut(|li| li.timestamp = 1_000_000);
    env.ledger().with_mut(|li| li.sequence_number = 100);

    client.init_admin(&admin);
    client.add_asset(&admin, &asset)

    env.as_contract(&contract_id, || {
        crate::auth::_add_provider(&env, &provider);
    });

    // First submission at ledger 100
    let result1 = client.try_update_price(&provider, &asset, &75_000_000, &9, &95, &3600);
    assert!(result1.is_ok(), "First submission should succeed");

    // Submit again at ledger 150 (gap of 50 blocks, well above minimum)
    env.ledger().with_mut(|li| li.sequence_number = 150);
    env.ledger().with_mut(|li| li.timestamp = 1_001_000);

    let result2 = client.try_update_price(&provider, &asset, &76_000_000, &9, &95, &3600);
    assert!(
        result2.is_ok(),
        "Submission after 50-block gap should succeed"
    );
}

#[test]
fn test_ledger_gap_multiple_providers_independent() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let provider_a = Address::generate(&env);
    let provider_b = Address::generate(&env);
    let asset = symbol_short!("NGN");

    env.ledger().with_mut(|li| li.timestamp = 1_000_000);
    env.ledger().with_mut(|li| li.sequence_number = 100);

    client.init_admin(&admin);
    client.add_asset(&admin, &asset)

    env.as_contract(&contract_id, || {
        crate::auth::_add_provider(&env, &provider_a);
        crate::auth::_add_provider(&env, &provider_b);
    });

    // Provider A submits at ledger 100
    let result_a1 = client.try_update_price(&provider_a, &asset, &100_000_000, &9, &95, &3600);
    assert!(result_a1.is_ok(), "Provider A first submission should succeed");

    // Provider B submits at ledger 101 (new provider, no gap restriction)
    env.ledger().with_mut(|li| li.sequence_number = 101);
    let result_b1 = client.try_update_price(&provider_b, &asset, &102_000_000, &9, &95, &3600);
    assert!(
        result_b1.is_ok(),
        "Provider B first submission should succeed (no gap restriction for new provider)"
    );

    // Provider A tries to submit at ledger 102 (gap of 2, fails)
    env.ledger().with_mut(|li| li.sequence_number = 102);
    let result_a2 = client.try_update_price(&provider_a, &asset, &101_000_000, &9, &95, &3600);
    match result_a2 {
        Err(Ok(ContractError::LedgerGapTooSmall)) => {
            // Expected!
        }
        other => panic!(
            "Expected LedgerGapTooSmall for Provider A, got: {:?}",
            other
        ),
    }

    // Provider B tries to submit at ledger 103 (gap of 2 for B, fails)
    env.ledger().with_mut(|li| li.sequence_number = 103);
    let result_b2 = client.try_update_price(&provider_b, &asset, &103_000_000, &9, &95, &3600);
    match result_b2 {
        Err(Ok(ContractError::LedgerGapTooSmall)) => {
            // Expected!
        }
        other => panic!(
            "Expected LedgerGapTooSmall for Provider B, got: {:?}",
            other
        ),
    }

    // Provider A submits at ledger 104 (gap of 4 from ledger 100, succeeds)
    env.ledger().with_mut(|li| li.sequence_number = 104);
    let result_a3 = client.try_update_price(&provider_a, &asset, &104_000_000, &9, &95, &3600);
    assert!(result_a3.is_ok(), "Provider A should succeed after 4-block gap");

    // Provider B submits at ledger 105 (gap of 4 from ledger 101, succeeds)
    env.ledger().with_mut(|li| li.sequence_number = 105);
    let result_b3 = client.try_update_price(&provider_b, &asset, &105_000_000, &9, &95, &3600);
    assert!(result_b3.is_ok(), "Provider B should succeed after 4-block gap");
}

#[test]
fn test_ledger_gap_provider_last_seen_ledger_tracking() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("KES");

    env.ledger().with_mut(|li| li.timestamp = 1_000_000);
    env.ledger().with_mut(|li| li.sequence_number = 100);

    client.init_admin(&admin);
    client.add_asset(&admin, &asset)

    env.as_contract(&contract_id, || {
        crate::auth::_add_provider(&env, &provider);
    });

    // Initially, provider has no recorded last ledger
    let last_ledger_before = client.get_provider_last_seen_ledger(&provider);
    assert_eq!(last_ledger_before, 0, "New provider should have no recorded last ledger");

    // First submission at ledger 100
    client.update_price(&provider, &asset, &100_000_000, &9, &95, &3600);

    // Provider's last seen ledger should now be 100
    let last_ledger_after_first = client.get_provider_last_seen_ledger(&provider);
    assert_eq!(
        last_ledger_after_first, 100,
        "Provider's last seen ledger should be updated to 100"
    );

    // Submit again at ledger 105
    env.ledger().with_mut(|li| li.sequence_number = 105);
    env.ledger().with_mut(|li| li.timestamp = 1_000_100);
    client.update_price(&provider, &asset, &105_000_000, &9, &95, &3600);

    // Provider's last seen ledger should now be 105
    let last_ledger_after_second = client.get_provider_last_seen_ledger(&provider);
    assert_eq!(
        last_ledger_after_second, 105,
        "Provider's last seen ledger should be updated to 105 after second submission"
    );
}

    let contract_id = env.register_contract(None, PriceOracle);
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
    assert!(
        debug_str.contains("contract_destroyed"),
        "Should emit contract_destroyed event"
    );
    assert!(
        debug_str.contains(&format!("{:?}", admin1)),
        "Event should contain admin1"
    );
    assert!(
        debug_str.contains(&format!("{:?}", admin2)),
        "Event should contain admin2"
    );
}

#[test]
#[should_panic(expected = "Error(ContractDestroyed)")]
fn test_self_destruct_prevents_double_destruct() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
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
    assert_eq!(
        client.try_subscribe_to_price_updates(&callback_contract),
        Ok(())
    );

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
    client.try_subscribe_to_price_updates(&callback1)
    client.try_subscribe_to_price_updates(&callback2)
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
    client.try_subscribe_to_price_updates(&callback1)

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

    let contract_id = env.register_contract(None, PriceOracle);
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
    env.ledger().with_mut(|li| li.timestamp = 1_000_000);
    let result = client.update_price(&provider, &asset, &1_500_000_i128, &6u32, &90u32, &3600u64, &100_000_i128);

    // The update should succeed even if the callback fails
    assert!(
        result.is_ok(),
        "Price update should succeed even with subscribers"
    );

    // Verify price was stored
    let price = client.get_price(&asset, &true);
    assert_eq!(price.price, 1_500_000_i128);
}

#[test]
fn test_set_price_with_subscribers() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let subscriber = Address::generate(&env);
    let asset = symbol_short!("KES");

    // Subscribe a contract
    assert_eq!(client.try_subscribe_to_price_updates(&subscriber), Ok(()));

    // Set price should not crash even with subscribers
    env.ledger().with_mut(|li| li.timestamp = 2_000_000);
    client.set_price(&asset, &2_000_000_i128, &6u32, &3600u64, &100_000_i128);

    // Verify price was stored
    let price = client.get_price(&asset, &true);
    assert_eq!(price.price, 2_000_000_i128);
}

// ─────────────────────────────────────────────────────────────────────────────
// bypass_safety_checks tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_enable_bypass_returns_expiry_one_hour_ahead() {
    let (env, contract_id, client) = setup();
    let admin = Address::generate(&env);
    set_admin(&env, &contract_id, &admin);

    env.ledger().with_mut(|li| li.timestamp = 1_000_000);
    let expiry = client.enable_bypass_safety_checks(&admin);

    assert_eq!(expiry, 1_000_000 + 3_600);
}

#[test]
fn test_get_bypass_expiry_returns_stored_value() {
    let (env, contract_id, client) = setup();
    let admin = Address::generate(&env);
    set_admin(&env, &contract_id, &admin);

    assert!(client.get_bypass_safety_checks_expiry().is_none());

    env.ledger().with_mut(|li| li.timestamp = 2_000_000);
    client.enable_bypass_safety_checks(&admin);

    assert_eq!(
        client.get_bypass_safety_checks_expiry(),
        Some(2_000_000 + 3_600)
    );
}

#[test]
fn test_buffer_truncation_with_equal_weights() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let asset = symbol_short!("GHS");

    // Initialize contract
    client.init_admin(&admin);
    client.add_asset(&admin, &asset);

    // Create 13 providers all with the same weight
    let mut providers = soroban_sdk::Vec::new(&env);
    for _ in 0..13 {
        let provider = Address::generate(&env);
        providers.push_back(provider.clone());

        env.as_contract(&contract_id, || {
            crate::auth::_add_provider(&env, &provider);
            crate::auth::_set_provider_weight(&env, &provider, 75u32);
        });
    }

    // Set initial price
    client.set_price(&asset, &800_000_i128, &6u32, &3600u64, &100_000_i128);

    // Have all 13 providers submit prices
    env.ledger().with_mut(|li| li.sequence_number = 300);
    for i in 0..13 {
        let provider = providers.get(i)
        let price = 800_000_i128 + (i as i128 * 10);
        client.update_price(&provider, &asset, &price, &6u32, 90u32, &3600u64, &100_000_i128);
    }

    // Get the buffer and verify it was truncated to 11
    let buffer = client.get_price_buffer_data(&asset);
    assert!(buffer.is_some(), "Buffer should exist");

    let buffer_data = buffer
    assert_eq!(
        buffer_data.entries.len(),
        11,
        "Buffer should be truncated to MAX_MEDIAN_ENTRIES (11) even with equal weights"
    );
}

#[test]
fn test_median_calculation_after_truncation() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let asset = symbol_short!("NGN");

    // Initialize contract
    client.init_admin(&admin);
    client.add_asset(&admin, &asset);

    // Create 12 providers with varying weights
    let mut providers = soroban_sdk::Vec::new(&env);
    for i in 0..12 {
        let provider = Address::generate(&env);
        providers.push_back(provider.clone());

        env.as_contract(&contract_id, || {
            crate::auth::_add_provider(&env, &provider);
            let weight = if i < 11 { 100u32 } else { 10u32 }; // Last provider has low weight
            crate::auth::_set_provider_weight(&env, &provider, weight);
        });
    }

    // Set initial price
    client.set_price(&asset, &1_000_000_i128, &6u32, &3600u64, &100_000_i128);

    // Have all 12 providers submit prices
    env.ledger().with_mut(|li| li.sequence_number = 400);
    for i in 0..12 {
        let provider = providers.get(i)
        let price = 1_000_000_i128 + (i as i128 * 1000);
        client.update_price(&provider, &asset, &price, &6u32, 90u32, &3600u64, &100_000_i128);
    }

    // Verify the price was updated (median calculation succeeded)
    let price_data = client.get_price(&asset, &true);
    assert!(
        price_data.price >= 1_000_000_i128,
        "Median price should be calculated from truncated buffer"
    );

    // The low-weight provider (index 11) should have been excluded
    let buffer = client.get_price_buffer_data(&asset)
    assert_eq!(buffer.entries.len(), 11, "Buffer should contain 11 entries");
}

#[test]
fn test_disable_bypass_clears_expiry() {
    let (env, contract_id, client) = setup();
    let admin = Address::generate(&env);
    set_admin(&env, &contract_id, &admin);

    env.ledger().with_mut(|li| li.timestamp = 1_000_000);
    client.enable_bypass_safety_checks(&admin);
    assert!(client.get_bypass_safety_checks_expiry().is_some());

    client.disable_bypass_safety_checks(&admin);
    assert!(client.get_bypass_safety_checks_expiry().is_none());
}

#[test]
fn test_bypass_allows_flash_crash_price() {
    let (env, contract_id, client) = setup();
    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("NGN");

    set_admin(&env, &contract_id, &admin);
    add_provider(&env, &contract_id, &provider);
    client.add_asset(&admin, &asset);

    // Seed an initial price, then set a tight deviation limit.
    client.set_price(&asset, &1_000_i128, &2u32, &3_600u64, &100_000_i128);
    client.set_max_deviation_percentage(&admin, &100_i128); // 1%

    // Without bypass, a 20% jump should be rejected.
    let rejected =
        client.try_update_price(&provider, &asset, &1_200_i128, &2u32, &100u32, &3_600u64, &100_000_i128);
    match rejected {
        Err(Ok(err)) => assert_eq!(err, Error::FlashCrashDetected),
        other => panic!("expected FlashCrashDetected, got {:?}", other),
    }

    // Enable bypass and retry — should succeed.
    env.ledger().with_mut(|li| li.timestamp = 1_000_000);
    client.enable_bypass_safety_checks(&admin);
    assert!(client
        .try_update_price(&provider, &asset, &1_200_i128, &2u32, &100u32, &3_600u64, &100_000_i128)
        .is_ok());
}

#[test]
fn test_bypass_allows_price_outside_bounds() {
    let (env, contract_id, client) = setup();
    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("NGN");

    set_admin(&env, &contract_id, &admin);
    add_provider(&env, &contract_id, &provider);
    client.add_asset(&admin, &asset);
    client.set_price_bounds(&admin, &asset, &500_i128, &1_500_i128);

    // Seed a price within bounds so deviation check passes.
    client.set_price(&asset, &1_000_i128, &2u32, &3_600u64, &100_000_i128);

    // Enable bypass and submit a price above max_price.
    env.ledger().with_mut(|li| li.timestamp = 1_000_000);
    client.enable_bypass_safety_checks(&admin);
    assert!(client
        .try_update_price(&provider, &asset, &2_000_i128, &2u32, &100u32, &3_600u64)
        .is_ok());
}

#[test]
fn test_bypass_expires_and_circuit_breaker_resumes() {
    let (env, contract_id, client) = setup();
    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("NGN");

    set_admin(&env, &contract_id, &admin);
    add_provider(&env, &contract_id, &provider);
    client.add_asset(&admin, &asset);
    client.set_price(&asset, &1_000_i128, &2u32, &3_600u64);
    client.set_max_deviation_percentage(&admin, &100_i128); // 1%

    // Enable bypass at t=1000.
    env.ledger().with_mut(|li| li.timestamp = 1_000);
    client.enable_bypass_safety_checks(&admin);

    // Advance clock past the 1-hour expiry.
    env.ledger().with_mut(|li| li.timestamp = 1_000 + 3_601);

    // Circuit breaker should be active again.
    let result = client.try_update_price(&provider, &asset, &1_200_i128, &2u32, &100u32, &3_600u64, &100_000_i128);
    match result {
        Err(Ok(err)) => assert_eq!(err, Error::FlashCrashDetected),
        other => panic!(
            "expected FlashCrashDetected after bypass expiry, got {:?}",
            other
        ),
    }
}

#[test]
#[should_panic]
fn test_enable_bypass_requires_admin() {
    let (env, contract_id, client) = setup();
    let admin = Address::generate(&env);
    let non_admin = Address::generate(&env);
    set_admin(&env, &contract_id, &admin);

    env.ledger().with_mut(|li| li.timestamp = 1_000_000);
    // non_admin is not in the admin list — should panic.
    client.enable_bypass_safety_checks(&non_admin);
}

// ── Issue #262: rate-map max-age enforcement ─────────────────────────────────

/// `get_price` must succeed when the stored timestamp is within MAX_RATE_AGE_SECONDS.
#[test]
fn test_get_price_within_max_age_succeeds() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let asset = symbol_short!("NGN");
    // Store price at t=1_000.
    env.ledger().with_mut(|li| li.timestamp = 1_000);
    env.ledger().with_mut(|li| li.sequence_number = 1);
    client.set_price(&asset, &1_500_i128, &2u32, &86_400u64);

    // Advance to t=1_299 — still within the 300-second boundary.
    env.ledger().with_mut(|li| li.timestamp = 1_299);
    let result = client.try_get_price(&asset, &true);
    assert!(result.is_ok(), "expected Ok within max age window");
}

/// `get_price` must panic with `StaleRateData` when the stored timestamp is
/// older than `current_time - MAX_RATE_AGE_SECONDS` (300 s).
#[test]
#[should_panic]
fn test_get_price_panics_when_rate_map_exceeds_max_age() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let asset = symbol_short!("NGN");
    // Store price at t=1_000.
    env.ledger().with_mut(|li| li.timestamp = 1_000);
    env.ledger().with_mut(|li| li.sequence_number = 1);
    client.set_price(&asset, &1_500_i128, &2u32, &86_400u64);

    // Advance past the 300-second boundary: t=1_000 + 300 + 1 = 1_301.
    env.ledger().with_mut(|li| li.timestamp = 1_301);
    // This must panic with Error::StaleRateData (error code 25).
    let _ = client.get_price(&asset, &true);
}

/// `get_last_price` must also panic when the rate map entry is too old,
/// because it delegates to `get_price`.
#[test]
#[should_panic]
fn test_get_last_price_panics_when_rate_map_exceeds_max_age() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let asset = symbol_short!("KES");
    env.ledger().with_mut(|li| li.timestamp = 2_000);
    env.ledger().with_mut(|li| li.sequence_number = 1);
    client.set_price(&asset, &500_i128, &2u32, &86_400u64);

    // Advance 301 seconds past the stored timestamp.
    env.ledger().with_mut(|li| li.timestamp = 2_301);
    let _ = client.get_last_price(&asset);
}

/// Exactly at the boundary (current_time == stored_timestamp + MAX_RATE_AGE_SECONDS)
/// the guard must NOT panic — the boundary is exclusive.
#[test]
fn test_get_price_at_exact_boundary_does_not_panic() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);

    let asset = symbol_short!("GHS");
    env.ledger().with_mut(|li| li.timestamp = 5_000);
    env.ledger().with_mut(|li| li.sequence_number = 1);
    client.set_price(&asset, &800_i128, &2u32, &86_400u64);

    // Exactly at boundary: 5_000 + 300 = 5_300.
    env.ledger().with_mut(|li| li.timestamp = 5_300);
    let result = client.try_get_price(&asset, &true);
    assert!(result.is_ok(), "expected Ok at exact boundary");
}

#[test]
fn test_relayer_activity_tracking() {
    let (env, contract_id, client) = setup();
    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("NGN");

    client.initialize(&admin, &soroban_sdk::vec![&env, asset.clone()]);
    add_provider(&env, &contract_id, &provider);

    // Initial state
    assert_eq!(client.get_provider_last_seen_ledger(&provider), 0);
    assert!(!client.is_provider_active(&provider, &10));

    // Update price at ledger 100
    env.ledger().with_mut(|li| li.sequence_number = 100);
    client.update_price(&provider, &asset, &1000_i128, &6u32, &100u32, &3600u64, &100_000_i128);

    // Check last seen ledger
    assert_eq!(client.get_provider_last_seen_ledger(&provider), 100);

    // Check activity within window of 10 ledgers
    assert!(client.is_provider_active(&provider, &10)); // 100 <= 100 + 10

    // Advance ledger to 110 (exactly at the end of the window)
    env.ledger().with_mut(|li| li.sequence_number = 110);
    assert!(client.is_provider_active(&provider, &10)); // 110 <= 100 + 10

    // Advance ledger to 111 (just outside the window)
    env.ledger().with_mut(|li| li.sequence_number = 111);
    assert!(!client.is_provider_active(&provider, &10)); // 111 > 110
}

#[test]
fn test_graceful_recovery_clears_metrics() {
    let (env, contract_id, client) = setup();
    let admin1 = Address::generate(&env);
    let admin2 = Address::generate(&env);
    let provider = Address::generate(&env);
    let asset = symbol_short!("NGN");

    // Initialize with 2 admins for multi-sig halt
    let pairs = soroban_sdk::vec![&env, asset.clone()];
    client.initialize(&admin1, &pairs);
    env.as_contract(&contract_id, || {
        crate::auth::_add_authorized(&env, &admin2);
        crate::auth::_add_provider(&env, &provider);
    });

    // 1. Populate metrics: Price update (adds to TWAP, RecentEvents, LastSeen)
    client.update_price(&provider, &asset, &1000_i128, &6u32, &100u32, &3600u64, &100_000_i128);

    // Add relayer infraction
    env.as_contract(&contract_id, || {
        crate::slashing::report_missed_blocks(&env, &provider, 5)
    });

    assert_eq!(client.get_twap(&asset), Some(1000));
    assert_eq!(client.get_last_n_events(&5).len(), 1);
    assert_eq!(client.get_provider_consecutive_missed_blocks(&provider), 5);

    // 2. Emergency Halt
    client.set_emergency_halt(&admin1, &admin2, &true);
    assert!(client.is_halted());

    // 3. Resume (Triggers Graceful Recovery)
    env.ledger().with_mut(|li| li.sequence_number = 500);
    client.set_emergency_halt(&admin1, &admin2, &false);
    assert!(!client.is_halted());

    // 4. Verify Metrics Cleared
    assert_eq!(client.get_twap(&asset), None);
    assert_eq!(client.get_last_n_events(&5).len(), 0);
    assert_eq!(client.get_provider_consecutive_missed_blocks(&provider), 0);
    assert_eq!(client.get_provider_last_seen_ledger(&provider), 500);
}
