#![cfg(test)]
use super::*;
use soroban_sdk::{testutils::Address as _, Address, Env};

// ─── helpers ─────────────────────────────────────────────────────────────────

fn setup(mint_to: &Address, mint_amount: i128, env: &Env) -> (Address, Address, Address, Address) {
    let tank_id = env.register_contract(None, GasTank);
    let oracle = Address::generate(env);
    let relayer = Address::generate(env);
    let token_id = env.register_stellar_asset_contract(mint_to.clone());
    soroban_sdk::token::StellarAssetClient::new(env, &token_id).mint(mint_to, &mint_amount);
    (tank_id, token_id, oracle, relayer)
}

fn tc<'a>(env: &'a Env, token_id: &Address) -> soroban_sdk::token::Client<'a> {
    soroban_sdk::token::Client::new(env, token_id)
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[test]
fn test_initialize() {
    let env = Env::default();
    env.mock_all_auths();

    let consumer = Address::generate(&env);
    let (tank_id, token_id, oracle, _) = setup(&consumer, 0, &env);
    let tank = GasTankClient::new(&env, &tank_id);

    tank.initialize(&token_id, &oracle);

    assert_eq!(tank.get_token(), token_id);
    assert_eq!(tank.get_oracle(), oracle);
}

#[test]
fn test_double_initialize_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let consumer = Address::generate(&env);
    let (tank_id, token_id, oracle, _) = setup(&consumer, 0, &env);
    let tank = GasTankClient::new(&env, &tank_id);

    tank.initialize(&token_id, &oracle);

    match tank.try_initialize(&token_id, &oracle) {
        Err(Ok(Error::AlreadyInitialized)) => {}
        other => panic!("expected AlreadyInitialized error, got {:?}", other),
    }
}

#[test]
fn test_deposit_increases_balance() {
    let env = Env::default();
    env.mock_all_auths();

    let consumer = Address::generate(&env);
    let (tank_id, token_id, oracle, _) = setup(&consumer, 1000, &env);
    let tank = GasTankClient::new(&env, &tank_id);
    tank.initialize(&token_id, &oracle);

    tank.deposit(&consumer, &600);

    assert_eq!(tank.get_balance(&consumer), 600);
    assert_eq!(tc(&env, &token_id).balance(&consumer), 400);
    assert_eq!(tc(&env, &token_id).balance(&tank_id), 600);
}

#[test]
fn test_withdraw_decreases_balance() {
    let env = Env::default();
    env.mock_all_auths();

    let consumer = Address::generate(&env);
    let (tank_id, token_id, oracle, _) = setup(&consumer, 1000, &env);
    let tank = GasTankClient::new(&env, &tank_id);
    tank.initialize(&token_id, &oracle);

    tank.deposit(&consumer, &600);
    tank.withdraw(&consumer, &200);

    assert_eq!(tank.get_balance(&consumer), 400);
    assert_eq!(tc(&env, &token_id).balance(&consumer), 600);
    assert_eq!(tc(&env, &token_id).balance(&tank_id), 400);
}

#[test]
fn test_withdraw_overdraft_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let consumer = Address::generate(&env);
    let (tank_id, token_id, oracle, _) = setup(&consumer, 500, &env);
    let tank = GasTankClient::new(&env, &tank_id);
    tank.initialize(&token_id, &oracle);

    tank.deposit(&consumer, &100);
    match tank.try_withdraw(&consumer, &101) {
        Err(Ok(Error::InsufficientBalance)) => {}
        other => panic!("expected InsufficientBalance error, got {:?}", other),
    }
}

#[test]
fn test_set_and_get_allowance() {
    let env = Env::default();
    env.mock_all_auths();

    let consumer = Address::generate(&env);
    let (tank_id, token_id, oracle, relayer) = setup(&consumer, 0, &env);
    let tank = GasTankClient::new(&env, &tank_id);
    tank.initialize(&token_id, &oracle);

    tank.set_allowance(&consumer, &relayer, &50);
    assert_eq!(tank.get_allowance(&consumer, &relayer), 50);

    // Clearing allowance to 0 removes the funder from the list
    tank.set_allowance(&consumer, &relayer, &0);
    assert_eq!(tank.get_allowance(&consumer, &relayer), 0);
}

#[test]
fn test_reimburse_pays_relayer() {
    let env = Env::default();
    env.mock_all_auths();

    let consumer = Address::generate(&env);
    let (tank_id, token_id, oracle, relayer) = setup(&consumer, 1000, &env);
    let tank = GasTankClient::new(&env, &tank_id);
    tank.initialize(&token_id, &oracle);

    tank.deposit(&consumer, &500);
    tank.set_allowance(&consumer, &relayer, &50);

    // oracle calls reimburse (mock_all_auths handles oracle.require_auth)
    tank.reimburse(&relayer);

    assert_eq!(tank.get_balance(&consumer), 450);
    assert_eq!(tc(&env, &token_id).balance(&relayer), 50);
    assert_eq!(tc(&env, &token_id).balance(&tank_id), 450);
}

#[test]
fn test_reimburse_capped_by_balance() {
    let env = Env::default();
    env.mock_all_auths();

    let consumer = Address::generate(&env);
    let (tank_id, token_id, oracle, relayer) = setup(&consumer, 1000, &env);
    let tank = GasTankClient::new(&env, &tank_id);
    tank.initialize(&token_id, &oracle);

    // Deposit only 30 but allowance is 100 → charge must be capped at 30
    tank.deposit(&consumer, &30);
    tank.set_allowance(&consumer, &relayer, &100);

    tank.reimburse(&relayer);

    assert_eq!(tank.get_balance(&consumer), 0);
    assert_eq!(tc(&env, &token_id).balance(&relayer), 30);
}

#[test]
fn test_reimburse_no_funders_is_noop() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (tank_id, token_id, oracle, relayer) = setup(&admin, 0, &env);
    let tank = GasTankClient::new(&env, &tank_id);
    tank.initialize(&token_id, &oracle);

    // No consumers deposited → must succeed silently
    tank.reimburse(&relayer);
    assert_eq!(tc(&env, &token_id).balance(&relayer), 0);
}

#[test]
fn test_reimburse_multiple_consumers() {
    let env = Env::default();
    env.mock_all_auths();

    let consumer1 = Address::generate(&env);
    let consumer2 = Address::generate(&env);
    let (tank_id, token_id, oracle, relayer) = setup(&consumer1, 500, &env);
    let tank = GasTankClient::new(&env, &tank_id);

    // Mint also for consumer2
    soroban_sdk::token::StellarAssetClient::new(&env, &token_id).mint(&consumer2, &500);

    tank.initialize(&token_id, &oracle);
    tank.deposit(&consumer1, &200);
    tank.deposit(&consumer2, &300);

    // Each consumer funds 40 per update
    tank.set_allowance(&consumer1, &relayer, &40);
    tank.set_allowance(&consumer2, &relayer, &40);

    tank.reimburse(&relayer);

    // Both consumers should each have paid 40
    assert_eq!(tank.get_balance(&consumer1), 160);
    assert_eq!(tank.get_balance(&consumer2), 260);
    assert_eq!(tc(&env, &token_id).balance(&relayer), 80);
}
