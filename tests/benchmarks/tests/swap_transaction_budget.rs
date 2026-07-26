//! Profiles oracle read entrypoints used before cross-currency swap execution.

use price_oracle::{ContractError as OracleError, PriceOracle, PriceOracleClient};
use soroban_sdk::{symbol_short, vec, Env, Symbol};
use stellarflow_benchmarks::profile::{assert_swap_path_within_limits, measure_entrypoint, EntrypointUsage};

const PRICE_DECIMALS: u32 = 9;
const PRICE_TTL_LEDGERS: u64 = 3_600;

fn setup_oracle_with_swap_pair(env: &Env) -> (PriceOracleClient<'static>, Symbol, Symbol) {
    env.mock_all_auths();
    let contract_id = env.register_contract(None, PriceOracle);
    let client = PriceOracleClient::new(env, &contract_id);
    let source = symbol_short!("NGN");
    let destination = symbol_short!("GHS");
    client.set_price(&source, &1_000_000_000_i128, &PRICE_DECIMALS, &PRICE_TTL_LEDGERS);
    client.set_price(
        &destination,
        &50_000_000_i128,
        &PRICE_DECIMALS,
        &PRICE_TTL_LEDGERS,
    );
    (client, source, destination)
}

#[test]
fn swap_oracle_entrypoints_log_resources_and_stay_within_budget() {
    let env = Env::default();
    env.budget().reset_default();

    let cpu_path_start = env.budget().cpu_instruction_cost();
    let mem_path_start = env.budget().memory_bytes_cost();

    let (client, source, destination) = setup_oracle_with_swap_pair(&env);

    let mut usages: Vec<EntrypointUsage> = Vec::new();

    usages.push(measure_entrypoint(&env, "get_price:source", || {
        let price = client
            .try_get_price(&source, &true)
            .expect("get_price should succeed")
            .expect("price data should be present");
        assert!(price.price > 0);
    }));

    usages.push(measure_entrypoint(&env, "get_price:destination", || {
        let price = client
            .try_get_price(&destination, &true)
            .expect("get_price should succeed")
            .expect("price data should be present");
        assert!(price.price > 0);
    }));

    usages.push(measure_entrypoint(&env, "get_prices:batch", || {
        let assets = vec![&env, source.clone(), destination.clone()];
        let batch = client.get_prices(&assets);
        assert_eq!(batch.len(), 2);
    }));

    usages.push(measure_entrypoint(&env, "get_price_with_status:source", || {
        let with_status = client.get_price_with_status(&source);
        assert!(with_status.data.price > 0);
    }));

    let total_cpu = env
        .budget()
        .cpu_instruction_cost()
        .saturating_sub(cpu_path_start);
    let total_mem = env
        .budget()
        .memory_bytes_cost()
        .saturating_sub(mem_path_start);
    assert_swap_path_within_limits(&usages, total_cpu, total_mem);
}

#[test]
fn swap_price_reads_do_not_exhaust_default_cpu_meter() {
    let env = Env::default();
    env.budget().reset_default();

    let (client, source, _) = setup_oracle_with_swap_pair(&env);

    for _ in 0..8 {
        let result = client.try_get_price(&source, &true);
        assert!(matches!(result, Ok(Ok(_))));
    }

    let cpu_used = env.budget().cpu_instruction_cost();
    eprintln!("[resource-profile] repeated_get_price cpu_instructions={cpu_used}");
    assert!(
        cpu_used < stellarflow_benchmarks::limits::safe_cpu_instruction_ceiling(),
        "repeated swap price reads exhausted the safe CPU budget"
    );
}

#[test]
fn missing_swap_asset_fails_without_budget_spike() {
    let env = Env::default();
    env.budget().reset_default();

    let (client, _, _) = setup_oracle_with_swap_pair(&env);
    let missing = symbol_short!("ZAR");

    let usage = measure_entrypoint(&env, "get_price:missing_asset", || {
        let err = client
            .try_get_price(&missing, &true)
            .expect("host should return a contract result")
            .expect_err("missing asset should error");
        assert_eq!(err, OracleError::AssetNotFound);
    });

    usage.assert_within_safe_network_limits();
}
