use super::*;
use soroban_sdk::{
    testutils::{Address as _, Events, Ledger},
    Env,
};

#[test]
fn test_stable_vault_health_decoupling_and_threshold() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, VaultHealthMonitor);
    let client = VaultHealthMonitorClient::new(&env, &contract_id);
    let vault = Address::generate(&env);
    let account = Address::generate(&env);
    client.initialize(&vault);

    // Test USDC/USDT backed position with threshold 0.95 (9500 bps)
    // collateral_value = 11_052, debt_value = 10_000 => 11052 * 9500 / 10000 = 10499.4 (~10500 / 1.05)
    let hf = client.assess_stable_vault_health(&vault, &account, &11_053, &10_000);
    assert_eq!(hf, 10_500);

    let events = env.events().all();
    assert_eq!(events.len(), 1);
    let event_debug = alloc::format!("{:?}", events.get(0).unwrap());
    assert!(event_debug.contains("VaultHealthWarning"));
    assert!(event_debug.contains("10500"));
}
