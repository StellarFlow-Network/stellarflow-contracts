//! Integration tests for the concentrated-liquidity tick-splitting engine
//! (Issue #986), exercised through the contract's public `#[contractimpl]`
//! methods rather than calling `amm::positions` directly.

use soroban_sdk::{testutils::Address as _, Address, Env};

use stellarflow_contracts::{
    AssetId, ContractError, TimeLockedUpgradeContract, TimeLockedUpgradeContractClient,
};

fn setup() -> (Env, TimeLockedUpgradeContractClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury);
    (env, client, admin)
}

const POOL: AssetId = 1;

#[test]
fn open_then_split_position_through_the_contract_client() {
    let (env, client, _admin) = setup();
    client.amm_initialize_tick_pool(&POOL, &1);

    let owner = Address::generate(&env);
    let position = client.amm_open_position(&owner, &POOL, &-10, &10, &1000);
    assert_eq!(position.liquidity, 1000);
    assert_eq!(position.tick_lower, -10);
    assert_eq!(position.tick_upper, 10);

    let result = client.amm_split_position(&owner, &position.id, &0);
    assert_eq!(result.lower.liquidity + result.upper.liquidity, 1000);
    assert_eq!(result.lower.tick_lower, -10);
    assert_eq!(result.lower.tick_upper, 0);
    assert_eq!(result.upper.tick_lower, 0);
    assert_eq!(result.upper.tick_upper, 10);

    // The original receipt id no longer resolves to a position.
    let reload = client.try_amm_get_position(&position.id);
    assert_eq!(reload, Err(Ok(ContractError::PositionNotFound)));

    // Both new receipts do.
    assert_eq!(client.amm_get_position(&result.lower.id).liquidity, result.lower.liquidity);
    assert_eq!(client.amm_get_position(&result.upper.id).liquidity, result.upper.liquidity);
}

#[test]
fn split_position_rejects_a_caller_who_is_not_the_owner() {
    let (env, client, _admin) = setup();
    client.amm_initialize_tick_pool(&POOL, &1);

    let owner = Address::generate(&env);
    let intruder = Address::generate(&env);
    let position = client.amm_open_position(&owner, &POOL, &-10, &10, &1000);

    let result = client.try_amm_split_position(&intruder, &position.id, &0);
    assert_eq!(result, Err(Ok(ContractError::Unauthorized)));
}

#[test]
fn split_position_rejects_a_mid_tick_outside_the_range() {
    let (env, client, _admin) = setup();
    client.amm_initialize_tick_pool(&POOL, &1);

    let owner = Address::generate(&env);
    let position = client.amm_open_position(&owner, &POOL, &-10, &10, &1000);

    let result = client.try_amm_split_position(&owner, &position.id, &20);
    assert_eq!(result, Err(Ok(ContractError::InvalidSplitBoundary)));
}

#[test]
fn split_position_apportions_accrued_fees_and_preserves_pool_liquidity() {
    let (env, client, _admin) = setup();
    client.amm_initialize_tick_pool(&POOL, &1);

    let owner = Address::generate(&env);
    let position = client.amm_open_position(&owner, &POOL, &-10, &10, &1000);

    env.as_contract(&client.address, || {
        stellarflow_contracts::amm::ticks::accrue_fee_growth(&env, POOL, 1000).unwrap();
    });

    let result = client.amm_split_position(&owner, &position.id, &0);
    assert_eq!(result.lower.tokens_owed + result.upper.tokens_owed, 1000);

    // The pool's total liquidity is conserved across the split (500 + 500),
    // but active liquidity itself drops to 500: splitting exactly at the
    // current tick (0) means the lower sub-range [-10, 0) no longer
    // straddles the current price (its upper bound is exclusive), so only
    // the upper sub-range's share remains counted as active.
    env.as_contract(&client.address, || {
        let meta = stellarflow_contracts::amm::ticks::get_tick_index(&env, POOL).unwrap();
        assert_eq!(meta.active_liquidity, 500);
    });
}
