#![cfg(test)]

use crate::{ContractError, PriceOracle, PriceOracleClient};
use soroban_sdk::{testutils::Address as _, vec, Address, Env, Symbol};

fn setup() -> (Env, PriceOracleClient, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    client.initialize(&admin, &vec![&env, Symbol::new(&env, "USD")]);
    (env, client, admin)
}

#[test]
fn test_admin_can_assign_delegate() {
    let (env, client, admin) = setup();
    let delegate = Address::generate(&env);

    client.assign_delegate(&admin, &delegate);

    assert_eq!(client.get_delegate(&admin), Some(delegate));
}

#[test]
fn test_admin_can_replace_delegate() {
    let (env, client, admin) = setup();
    let delegate1 = Address::generate(&env);
    let delegate2 = Address::generate(&env);

    client.assign_delegate(&admin, &delegate1);
    assert_eq!(client.get_delegate(&admin), Some(delegate1));

    client.assign_delegate(&admin, &delegate2);
    assert_eq!(client.get_delegate(&admin), Some(delegate2));
}

#[test]
fn test_admin_can_revoke_delegate() {
    let (env, client, admin) = setup();
    let delegate = Address::generate(&env);

    client.assign_delegate(&admin, &delegate);
    client.revoke_delegate(&admin);

    assert_eq!(client.get_delegate(&admin), None);
}

#[test]
fn test_delegate_can_submit_prices() {
    let (env, client, admin) = setup();
    let delegate = Address::generate(&env);
    let asset = Symbol::new(&env, "USD");

    // Assign delegate
    client.assign_delegate(&admin, &delegate);

    // Delegate submits price
    // Note: update_price(env, source, asset, price, decimals, confidence, ttl)
    client.update_price(&delegate, &asset, &100_i128, &2_u32, &100_u32, &86400_u64);

    // Verify price was updated
    let price_data = client.get_price(&asset, &true).unwrap();
    assert_eq!(price_data.price, 100);
}

#[test]
fn test_random_wallet_cannot_submit_prices() {
    let (env, client, _admin) = setup();
    let random = Address::generate(&env);
    let asset = Symbol::new(&env, "USD");

    let result = client.try_update_price(&random, &asset, &100_i128, &2_u32, &100_u32, &86400_u64);

    assert!(result.is_err());
    // ContractError::NotAuthorized = 6 (Actually update_price returns ContractError::NotAuthorized)
    // Wait, let's check ContractError enum. NotAuthorized = 6.
}

#[test]
fn test_delegate_cannot_call_admin_functions() {
    let (env, client, admin) = setup();
    let delegate = Address::generate(&env);
    let new_asset = Symbol::new(&env, "EUR");

    client.assign_delegate(&admin, &delegate);

    // Delegate tries to add an asset
    let result = client.try_add_asset(&delegate, &new_asset);

    assert!(result.is_err());
}

#[test]
fn test_revoked_delegate_loses_access() {
    let (env, client, admin) = setup();
    let delegate = Address::generate(&env);
    let asset = Symbol::new(&env, "USD");

    client.assign_delegate(&admin, &delegate);
    client.revoke_delegate(&admin);

    let result =
        client.try_update_price(&delegate, &asset, &100_i128, &2_u32, &100_u32, &86400_u64);

    assert!(result.is_err());
}

#[test]
fn test_delegate_of_removed_admin_loses_access() {
    let (env, client, admin1) = setup();
    let admin2 = Address::generate(&env);
    let delegate = Address::generate(&env);
    let asset = Symbol::new(&env, "USD");

    // Add second admin so we can remove the first one
    client.register_admin(&admin1, &admin1, &admin2);

    // Admin1 assigns delegate
    client.assign_delegate(&admin1, &delegate);

    // Remove Admin1
    client.remove_admin(&admin1, &admin2, &admin1);

    // Delegate tries to submit price
    let result =
        client.try_update_price(&delegate, &asset, &100_i128, &2_u32, &100_u32, &86400_u64);

    assert!(result.is_err());
}

#[test]
fn test_admin_cannot_delegate_to_self() {
    let (env, client, admin) = setup();

    let result = client.try_assign_delegate(&admin, &admin);

    assert!(result.is_err());
}
