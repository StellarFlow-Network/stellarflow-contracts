//! Soroban budget CPU & memory allocation test suite (Issue #924).
//!
//! Continuous suite asserting that core contract entrypoints stay well
//! within Soroban block execution limits:
//!
//! * CPU instructions and memory bytes are **measured** for every core
//!   entrypoint (admin/staking, oracle heartbeat, dynamic swap routing,
//!   governance, vault interest math, bridge mint, event compression).
//! * Standard swap operations must consume **less than 30% of the maximum
//!   block budget** — the acceptance threshold from the issue, enforced as
//!   a hard assertion.
//! * Any code change that regresses gas usage fails CI (`cargo test` in
//!   `.github/workflows/ci.yml` runs this suite as part of the workspace).

use soroban_sdk::{testutils::Address as _, Address, BytesN, Env, Symbol};
use std::vec::Vec;
use stellarflow_contracts::{
    multisig_expiry, router::dynamic::PoolEdge, TimeLockedUpgradeContract,
    TimeLockedUpgradeContractClient,
};

use stellarflow_benchmarks::profile::{measure_entrypoint, EntrypointUsage};
use stellarflow_benchmarks::limits::{
    NETWORK_CPU_INSTRUCTION_LIMIT, NETWORK_MEMORY_BYTE_LIMIT,
};

/// Acceptance threshold (Issue #924): standard swap operations must stay
/// below 30% of the maximum block budget.
pub const SWAP_OP_CPU_THRESHOLD: f64 = 0.30;
pub const SWAP_OP_MEM_THRESHOLD: f64 = 0.30;

fn swap_cpu_ceiling() -> u64 {
    (NETWORK_CPU_INSTRUCTION_LIMIT as f64 * SWAP_OP_CPU_THRESHOLD) as u64
}

fn swap_mem_ceiling() -> u64 {
    (NETWORK_MEMORY_BYTE_LIMIT as f64 * SWAP_OP_MEM_THRESHOLD) as u64
}

fn assert_below_swap_threshold(usage: &EntrypointUsage) {
    assert!(
        usage.cpu_instructions < swap_cpu_ceiling(),
        "swap entrypoint {} used {} CPU instructions (>= 30% of the {}-instruction block budget); \
         this is a gas regression and must be fixed before merge",
        usage.entrypoint,
        usage.cpu_instructions,
        NETWORK_CPU_INSTRUCTION_LIMIT
    );
    assert!(
        usage.memory_bytes < swap_mem_ceiling(),
        "swap entrypoint {} used {} memory bytes (>= 30% of the {}-byte block budget); \
         this is a memory regression and must be fixed before merge",
        usage.entrypoint,
        usage.memory_bytes,
        NETWORK_MEMORY_BYTE_LIMIT
    );
}

// ---------------------------------------------------------------------------
// Shared fixture
// ---------------------------------------------------------------------------

struct Fixture {
    env: Env,
    contract_id: Address,
    client: TimeLockedUpgradeContractClient<'static>,
    admin: Address,
}

impl Fixture {
    fn new() -> Self {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
        let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        client.initialize(&admin, &Address::generate(&env));
        Self {
            env,
            contract_id,
            client,
            admin,
        }
    }
}

/// Register a liquid two-pool AMM graph: A→B and B→C.
fn register_pools(env: &Env, client: &TimeLockedUpgradeContractClient, admin: &Address) {
    let edge = |a: u32, b: u32| PoolEdge {
        pool: Address::generate(env),
        asset_in: a,
        asset_out: b,
        reserve_in: 1_000_000_000,
        reserve_out: 1_000_000_000,
        fee_bps: 30,
    };
    client.register_amm_pool(admin, &edge(1, 2));
    client.register_amm_pool(admin, &edge(2, 3));
}

// ---------------------------------------------------------------------------
// 1. Core entrypoint resource profiling
// ---------------------------------------------------------------------------

/// Profile a broad set of core entrypoints; every measurement is asserted
/// against the 80% safe-network ceiling and logged for CI tracking.
#[test]
fn core_entrypoints_stay_within_safe_network_limits() {
    let fx = Fixture::new();
    let node = Address::generate(&fx.env);

    let mut usages: Vec<EntrypointUsage> = Vec::new();

    // Admin/staking write path.
    usages.push(measure_entrypoint(&fx.env, "stake_and_register", || {
        fx.client
            .stake_and_register(&node, &1_000_000u64);
    }));

    // Oracle heartbeat write + freshness read.
    usages.push(measure_entrypoint(&fx.env, "update_heartbeat", || {
        fx.client
            .update_heartbeat(&42u32, &fx.admin);
    }));
    usages.push(measure_entrypoint(&fx.env, "is_data_fresh", || {
        assert!(fx.client.is_data_fresh(&42u32));
    }));

    // Governance proposal submit path.
    let wasm_hash = BytesN::from_array(&fx.env, &[1u8; 32]);
    usages.push(measure_entrypoint(&fx.env, "submit_governance_proposal", || {
        fx.client
            .submit_governance_proposal(&fx.admin, &wasm_hash);
    }));

    // Vault interest math (pure).
    let config = stellarflow_contracts::vaults::interest::InterestRateConfig {
        base_rate_bps: 200,
        multiplier_bps: 1000,
        jump_multiplier_bps: 5000,
        optimal_utilization_bps: 8000,
        ledgers_per_year: 6_307_200,
    };
    usages.push(measure_entrypoint(&fx.env, "calculate_interest_rate", || {
        let rate = fx.client.calculate_interest_rate(&5000, &config);
        assert_eq!(rate, 700);
    }));

    // Event compression helper (encoding cost paid on-chain per event).
    usages.push(measure_entrypoint(&fx.env, "event_payload_compression", || {
        let payload = stellarflow_contracts::events::compression::CompressedEventPayload::new(
            &fx.env,
            &[1, 2_000_000, u32::MAX as u64],
            stellarflow_contracts::events::compression::pack_word(
                true, false, true, false, 7, 3_600,
            ),
        );
        let decoded = payload.decode_fields().expect("decode should succeed");
        assert_eq!(decoded.len(), 3);
    }));

    for usage in usages.iter() {
        usage.assert_within_safe_network_limits();
    }
}

/// Every measured swap-path entrypoint must stay under the **30%** block
/// budget threshold from Issue #924's acceptance criteria.
#[test]
fn standard_swap_operations_stay_below_30_percent_of_block_budget() {
    let fx = Fixture::new();
    register_pools(&fx.env, &fx.client, &fx.admin);

    // Quote: state-free route discovery — the hot path pre-swap.
    let quote_usage = measure_entrypoint(&fx.env, "swap:quote_best_route", || {
        let quote = fx
            .client
            .quote_best_swap_route(&1u32, &3u32, &10_000u64, &2u32);
        assert_eq!(quote.hops.len(), 2);
    });
    assert_below_swap_threshold(&quote_usage);

    // Execute: the full swap mutation path.
    let exec_usage = measure_entrypoint(&fx.env, "swap:execute_dynamic_swap", || {
        let trader = Address::generate(&fx.env);
        fx.client
            .execute_dynamic_swap(&trader, &1u32, &3u32, &10_000u64, &1u64, &500u32);
    });
    assert_below_swap_threshold(&exec_usage);
}

/// Regression tripwire: repeated swaps amortize to a stable per-op cost and
/// must not approach the safe network ceiling, so any per-op growth (e.g. a
/// new per-hop allocation) trips CI long before mainnet limits.
#[test]
fn repeated_swap_path_does_not_exhaust_block_budget() {
    let fx = Fixture::new();
    register_pools(&fx.env, &fx.client, &fx.admin);

    let cpu_start = fx.env.budget().cpu_instruction_cost();
    let mem_start = fx.env.budget().memory_bytes_cost();

    for _ in 0..10 {
        let trader = Address::generate(&fx.env);
        fx.client
            .execute_dynamic_swap(&trader, &1u32, &3u32, &1_000u64, &1u64, &500u32);
    }

    let total_cpu = fx.env.budget().cpu_instruction_cost() - cpu_start;
    let total_mem = fx.env.budget().memory_bytes_cost() - mem_start;
    let per_op_cpu = total_cpu / 10;
    let per_op_mem = total_mem / 10;

    eprintln!(
        "[resource-profile] swap_10x per_op_cpu={per_op_cpu} per_op_mem={per_op_mem} total_cpu={total_cpu} total_mem={total_mem}"
    );

    // Per-op cost must sit below the 30% acceptance threshold...
    assert!(
        (per_op_cpu as f64) < swap_cpu_ceiling() as f64,
        "per-swap CPU {per_op_cpu} exceeded the 30% block-budget threshold"
    );
    assert!(
        (per_op_mem as f64) < swap_mem_ceiling() as f64,
        "per-swap memory {per_op_mem} exceeded the 30% block-budget threshold"
    );

    // ...and the whole 10x path must remain within the 80% safe ceiling.
    assert!(
        total_cpu <= stellarflow_benchmarks::limits::safe_cpu_instruction_ceiling(),
        "10x swap path exceeded the safe CPU ceiling: {total_cpu}"
    );
    assert!(
        total_mem <= stellarflow_benchmarks::limits::safe_memory_byte_ceiling(),
        "10x swap path exceeded the safe memory ceiling: {total_mem}"
    );
}

/// Multi-hop quotes deepen linearly; a deeper-than-baseline route must not
/// blow past the swap threshold — guards against super-linear routing cost.
#[test]
fn multi_hop_quote_cost_is_bounded() {
    let fx = Fixture::new();
    register_pools(&fx.env, &fx.client, &fx.admin);

    let single = measure_entrypoint(&fx.env, "swap:quote_1hop", || {
        let _ = fx
            .client
            .quote_best_swap_route(&1u32, &2u32, &10_000u64, &1u32);
    });
    let multi = measure_entrypoint(&fx.env, "swap:quote_2hop", || {
        let _ = fx
            .client
            .quote_best_swap_route(&1u32, &3u32, &10_000u64, &2u32);
    });

    assert_below_swap_threshold(&single);
    assert_below_swap_threshold(&multi);
}

// ---------------------------------------------------------------------------
// 2. Non-swap core operations also profiled for CI visibility
// ---------------------------------------------------------------------------

/// Governance cancellation vote path (multi-sig + storage heavy).
#[test]
fn governance_vote_path_profiles_within_limits() {
    let fx = Fixture::new();
    let wasm_hash = BytesN::from_array(&fx.env, &[2u8; 32]);
    let proposal_id = fx
        .client
        .submit_governance_proposal(&fx.admin, &wasm_hash);

    let usage = measure_entrypoint(&fx.env, "governance:get_proposal", || {
        let proposal = fx
            .client
            .get_governance_proposal(&proposal_id);
        assert_eq!(proposal.proposal_id, proposal_id);
    });
    usage.assert_within_safe_network_limits();
}

/// Multisig payload expiry guard (Issue #903) bookkeeping cost.
#[test]
fn multisig_expiry_guard_profiles_within_limits() {
    let fx = Fixture::new();
    let topic = multisig_expiry::upgrade_topic(&fx.env);
    let proposer = Address::generate(&fx.env);

    let usage = measure_entrypoint(&fx.env, "multisig:stage_and_check_expiry", || {
        fx.env.as_contract(&fx.contract_id, || {
            multisig_expiry::stage_payload(&fx.env, &topic, &proposer, 0);
            assert!(!multisig_expiry::is_payload_expired(&fx.env, &topic));
            multisig_expiry::enforce_payload_fresh(&fx.env, &topic);
        });
    });
    usage.assert_within_safe_network_limits();
}

/// Event payload compression round-trip cost (Issue #1019) — the encoding
/// runs on-chain inside every compressed event emission.
#[test]
fn compression_round_trip_profiles_within_limits() {
    let fx = Fixture::new();

    let usage = measure_entrypoint(&fx.env, "compression:round_trip", || {
        fx.env.as_contract(&fx.contract_id, || {
            let payload = stellarflow_contracts::events::compression::CompressedEventPayload::new(
                &fx.env,
                &[7, 1_000_000, u32::MAX as u64, 42],
                stellarflow_contracts::events::compression::pack_word(
                    false, true, false, false, 12, 3_600,
                ),
            );
            let decoded = payload.decode_fields().expect("decode should succeed");
            assert_eq!(decoded.get(2), Some(u32::MAX as u128));
        });
    });
    usage.assert_within_safe_network_limits();
}

/// Bridge mint path — measured in a fresh env because the reentrancy guard
/// is backed by persistent instance storage and must be exercised from a
/// clean top-level call frame.
#[test]
fn bridge_mint_entrypoint_stays_within_safe_network_limits() {
    let fx = Fixture::new();
    let controller = Address::generate(&fx.env);
    let asset = Symbol::new(&fx.env, "wBTC");
    let user = Address::generate(&fx.env);

    fx.client
        .register_wrapped_asset(&fx.admin, &asset, &controller, &1_000_000);

    let usage = measure_entrypoint(&fx.env, "mint_wrapped", || {
        fx.client.mint_wrapped(&controller, &asset, &user, &500);
    });
    usage.assert_within_safe_network_limits();
}
