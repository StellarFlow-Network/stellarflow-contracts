use soroban_sdk::{symbol_short, testutils::Address as _, Address, Env, Symbol, TryFromVal};
use stellarflow_benchmarks::events::{
    emit_compact, emit_governance_compact, emit_raw, emit_vault_compact, publish_topics,
    AmmRawEvent, GovernanceCompactEvent, VaultCompactEvent, EVENT_NAMESPACE,
};
use price_oracle::PriceOracle;

#[derive(Debug, Clone, Copy)]
struct Usage {
    cpu: u64,
    memory: u64,
}

fn measure_raw(env: &Env, trader: &Address) -> Usage {
    let cpu_before = env.budget().cpu_instruction_cost();
    let memory_before = env.budget().memory_bytes_cost();
    let contract_id = env.register_contract(None, PriceOracle);
    env.as_contract(&contract_id, || {
        emit_raw(
            env,
            symbol_short!("swap"),
            symbol_short!("pool"),
            AmmRawEvent {
                trader: trader.clone(),
                input_asset: symbol_short!("NGN"),
                output_asset: symbol_short!("GHS"),
                amount_in: 1_000_000,
                amount_out: 49_000,
            },
        );
    });
    Usage {
        cpu: env.budget().cpu_instruction_cost() - cpu_before,
        memory: env.budget().memory_bytes_cost() - memory_before,
    }
}

fn measure_compact(env: &Env, trader: &Address) -> Usage {
    let cpu_before = env.budget().cpu_instruction_cost();
    let memory_before = env.budget().memory_bytes_cost();
    let contract_id = env.register_contract(None, PriceOracle);
    env.as_contract(&contract_id, || {
        emit_compact(
            env,
            symbol_short!("swap"),
            symbol_short!("pool"),
            (
                trader.clone(),
                symbol_short!("NGN"),
                symbol_short!("GHS"),
                1_000_000,
                49_000,
            ),
        );
    });
    Usage {
        cpu: env.budget().cpu_instruction_cost() - cpu_before,
        memory: env.budget().memory_bytes_cost() - memory_before,
    }
}

#[test]
fn compact_tuple_is_no_more_expensive_than_raw_struct() {
    let raw_env = Env::default();
    let compact_env = Env::default();
    let raw_trader = Address::generate(&raw_env);
    let compact_trader = Address::generate(&compact_env);
    let raw = measure_raw(&raw_env, &raw_trader);
    let compact = measure_compact(&compact_env, &compact_trader);

    eprintln!(
        "[event-profile] raw cpu={} memory={} compact cpu={} memory={}",
        raw.cpu, raw.memory, compact.cpu, compact.memory
    );
    assert!(compact.cpu <= raw.cpu, "compact tuple increased CPU cost");
    assert!(compact.memory <= raw.memory, "compact tuple increased memory cost");
}

#[test]
fn compact_event_topics_and_payload_are_indexer_compatible() {
    let env = Env::default();
    let trader = Address::generate(&env);
    let contract_id = env.register_contract(None, PriceOracle);
    env.as_contract(&contract_id, || {
        emit_compact(
            &env,
            symbol_short!("swap"),
            symbol_short!("pool"),
            (
                trader.clone(),
                symbol_short!("NGN"),
                symbol_short!("GHS"),
                1_000_000,
                49_000,
            ),
        );
    });

    let (_, topics, data) = env.events().all().get(0).expect("event should be emitted");
    let expected_topics = soroban_sdk::vec![
        &env,
        Symbol::new(&env, EVENT_NAMESPACE).into_val(&env),
        symbol_short!("swap").into_val(&env),
        symbol_short!("pool").into_val(&env),
    ];
    assert_eq!(topics, expected_topics);

    let payload: (Address, Symbol, Symbol, i128, i128) =
        TryFromVal::try_from_val(&env, &data).expect("RPC payload must decode");
    assert_eq!(payload.0, trader);
    assert_eq!(payload.1, symbol_short!("NGN"));
    assert_eq!(payload.2, symbol_short!("GHS"));
    assert_eq!(payload.3, 1_000_000);
    assert_eq!(payload.4, 49_000);
}

#[test]
fn amm_vault_and_governance_use_the_same_topic_order() {
    let env = Env::default();
    let expected_prefix = Symbol::new(&env, EVENT_NAMESPACE);

    assert_eq!(
        publish_topics(&env, symbol_short!("swap"), symbol_short!("pool")).0,
        expected_prefix
    );
    assert_eq!(
        publish_topics(&env, symbol_short!("harvest"), symbol_short!("vault")).0,
        expected_prefix
    );
    assert_eq!(
        publish_topics(&env, symbol_short!("proposal"), symbol_short!("gov")).0,
        expected_prefix
    );

    let keeper = Address::generate(&env);
    let contract_id = env.register_contract(None, PriceOracle);
    env.as_contract(&contract_id, || {
        emit_vault_compact(
            &env,
            symbol_short!("harvest"),
            symbol_short!("vault"),
            (keeper.clone(), 100, 5, 95, 1_095),
        );
        emit_governance_compact(
            &env,
            symbol_short!("proposal"),
            symbol_short!("gov"),
            (keeper, 7, symbol_short!("upgrade"), 2, 2),
        );
    });

    let events = env.events().all();
    assert_eq!(events.len(), 2);
    let (_, vault_topics, vault_data) = events.get(0).unwrap();
    let (_, governance_topics, governance_data) = events.get(1).unwrap();
    assert_eq!(vault_topics.len(), 3);
    assert_eq!(governance_topics.len(), 3);
    let _: VaultCompactEvent = TryFromVal::try_from_val(&env, &vault_data).unwrap();
    let _: GovernanceCompactEvent = TryFromVal::try_from_val(&env, &governance_data).unwrap();
}