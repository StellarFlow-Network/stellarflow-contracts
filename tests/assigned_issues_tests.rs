#![cfg(test)]

use soroban_sdk::{testutils::Address as _, Address, Env, Symbol, Vec, Val, IntoVal};
use stellarflow_contracts::{
    TimeLockedUpgradeContract, TimeLockedUpgradeContractClient,
    vaults::interest::{InterestRateConfig, PoolState},
    orders::limit::AssetPair,
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
        base_rate_bps: 200, // 2%
        multiplier_bps: 1000, // 10%
        jump_multiplier_bps: 5000, // 50%
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
    let result = client.try_execute_isolated_call(&client.address, &Symbol::new(&env, "get_recovery_key"), &args);
    assert!(result.is_ok());
}
