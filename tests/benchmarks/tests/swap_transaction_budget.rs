//! Profiles dynamic-router swap entrypoints (quote + execute).
//!
//! Retains the safe-network-limit assertions for individual and repeated
//! swap-path reads/writes. The 30%-of-block-budget acceptance check and the
//! broader entrypoint coverage live in `budget_allocation_suite.rs`.

use stellarflow_benchmarks::limits::safe_cpu_instruction_ceiling;
use stellarflow_benchmarks::profile::{measure_entrypoint, EntrypointUsage};
use stellarflow_contracts::{router::dynamic::PoolEdge, TimeLockedUpgradeContractClient};
use soroban_sdk::{testutils::Address as _, Address, Env};
use std::vec::Vec;

fn register_pools(env: &Env, client: &TimeLockedUpgradeContractClient, admin: &Address) {
    let edge = |a: u32, b: u32| PoolEdge {
        pool: Address::generate(env),
        asset_in: a,
        asset_out: b,
        reserve_in: 1_000_000_000,
        reserve_out: 1_000_000_000,
        fee_bps: 30,
    };
    client.register_amm_pool(&admin, &edge(1, 2));
    client.register_amm_pool(&admin, &edge(2, 3));
}

fn setup() -> (Env, TimeLockedUpgradeContractClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, stellarflow_contracts::TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    client.initialize(&admin, &Address::generate(&env));
    register_pools(&env, &client, &admin);
    (env, client, admin)
}

#[test]
fn swap_quote_and_execute_entrypoints_log_resources_and_stay_within_budget() {
    let (env, client, admin) = setup();
    env.budget().reset_default();

    let mut usages: Vec<EntrypointUsage> = Vec::new();

    usages.push(measure_entrypoint(&env, "swap:quote_best_route", || {
        let quote = client
            .quote_best_swap_route(&1u32, &3u32, &10_000u64, &2u32);
        assert_eq!(quote.hops.len(), 2);
    }));

    usages.push(measure_entrypoint(&env, "swap:execute_dynamic_swap", || {
        let trader = Address::generate(&env);
        client
            .execute_dynamic_swap(&trader, &1u32, &3u32, &10_000u64, &1u64, &500u32);
    }));

    let _ = admin;
    for usage in usages.iter() {
        usage.assert_within_safe_network_limits();
    }
}

#[test]
fn repeated_swap_quotes_do_not_exhaust_default_cpu_meter() {
    let (env, client, _admin) = setup();
    env.budget().reset_default();

    for _ in 0..8 {
        let _ = client
            .quote_best_swap_route(&1u32, &3u32, &1_000u64, &2u32);
    }

    let cpu_used = env.budget().cpu_instruction_cost();
    eprintln!("[resource-profile] repeated_swap_quotes cpu_instructions={cpu_used}");
    assert!(
        cpu_used < safe_cpu_instruction_ceiling(),
        "repeated swap quotes exhausted the safe CPU budget"
    );
}

#[test]
fn missing_route_fails_without_budget_spike() {
    let (env, client, _admin) = setup();

    let usage = measure_entrypoint(&env, "swap:quote_missing_pair", || {
        let res = client.try_quote_best_swap_route(&1u32, &99u32, &10_000u64, &2u32);
        match res {
            Err(Ok(stellarflow_contracts::ContractError::PoolNotFound)) => {}
            other => panic!("missing pair should fail with PoolNotFound, got {:?}", other),
        }
    });

    usage.assert_within_safe_network_limits();
}
