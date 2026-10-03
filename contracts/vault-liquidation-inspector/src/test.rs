use super::*;
use soroban_sdk::{contract, contractimpl, map, symbol_short, Map};

#[contract]
struct MockOracle;

#[contractimpl]
impl MockOracle {
    pub fn set_twap(env: Env, asset: Symbol, price: i128) {
        let mut prices: Map<Symbol, i128> = env
            .storage()
            .instance()
            .get(&symbol_short!("prices"))
            .unwrap_or_else(|| map![&env]);
        prices.set(asset, price);
        env.storage()
            .instance()
            .set(&symbol_short!("prices"), &prices);
    }

    pub fn get_twap(env: Env, asset: Symbol) -> Option<i128> {
        let prices: Map<Symbol, i128> = env.storage().instance().get(&symbol_short!("prices"))?;
        prices.get(asset)
    }
}

fn setup_oracle(env: &Env) -> Address {
    let oracle_id = env.register_contract(None, MockOracle);
    let oracle = MockOracleClient::new(env, &oracle_id);
    oracle.set_twap(&symbol_short!("USD"), &1);
    oracle.set_twap(&symbol_short!("RISK"), &10);
    oracle.set_twap(&symbol_short!("SAFE"), &10);
    oracle_id
}

#[test]
fn reports_healthy_basket_value_from_twaps() {
    let env = Env::default();
    let oracle = setup_oracle(&env);
    let monitor_id = env.register_contract(None, VaultHealthMonitor);
    let monitor = VaultHealthMonitorClient::new(&env, &monitor_id);
    let collateral = soroban_sdk::vec![
        &env,
        CollateralAsset {
            asset: symbol_short!("RISK"),
            amount: 100,
            multiplier_bps: 8_000,
        },
        CollateralAsset {
            asset: symbol_short!("SAFE"),
            amount: 100,
            multiplier_bps: 9_000,
        },
    ];

    let report = monitor.inspect(&oracle, &collateral, &symbol_short!("USD"), &1_700);

    assert_eq!(report.collateral_value, 1_700);
    assert_eq!(report.debt_value, 1_700);
    assert_eq!(report.health_factor_bps, 10_000);
    assert!(report.is_healthy);
    assert!(report.liquidation_plan.is_empty());
}

#[test]
fn plans_partial_liquidation_from_highest_risk_asset() {
    let env = Env::default();
    let oracle = setup_oracle(&env);
    let monitor_id = env.register_contract(None, VaultHealthMonitor);
    let monitor = VaultHealthMonitorClient::new(&env, &monitor_id);
    let collateral = soroban_sdk::vec![
        &env,
        CollateralAsset {
            asset: symbol_short!("SAFE"),
            amount: 100,
            multiplier_bps: 9_000,
        },
        CollateralAsset {
            asset: symbol_short!("RISK"),
            amount: 100,
            multiplier_bps: 5_000,
        },
    ];

    let report = monitor.inspect(&oracle, &collateral, &symbol_short!("USD"), &1_600);

    assert_eq!(report.collateral_value, 1_400);
    assert_eq!(report.health_factor_bps, 8_750);
    assert!(!report.is_healthy);
    assert_eq!(report.liquidation_plan.len(), 1);
    let step = report.liquidation_plan.get(0).unwrap();
    assert_eq!(step.asset, symbol_short!("RISK"));
    assert_eq!(step.collateral_amount, 40);
    assert_eq!(step.repayment_value, 400);
}
