#![cfg(test)]

//! Benchmarking and profiling suite for state size, key serialization, and deserialization (Issue #949).

use soroban_sdk::{
    testutils::Address as _,
    Address, BytesN, Env, Symbol,
};
use stellarflow_benchmarks::state_size::{
    benchmark_serialization_deserialization, measure_instance_storage_footprint,
    MAX_INSTANCE_ENTRY_BYTES_BUDGET,
};

#[test]
fn test_benchmark_storage_keys_serialization() {
    let env = Env::default();

    // 1. Symbol key benchmark
    let symbol_key = Symbol::new(&env, "VALIDATOR_THRESHOLD");
    let profile_sym = benchmark_serialization_deserialization(
        &env,
        "StorageKey::Symbol",
        &symbol_key,
    );
    profile_sym.assert_within_target_allocations();

    // 2. Address key benchmark
    let addr_key = Address::generate(&env);
    let profile_addr = benchmark_serialization_deserialization(
        &env,
        "StorageKey::Address",
        &addr_key,
    );
    profile_addr.assert_within_target_allocations();

    // 3. BytesN key benchmark (e.g. validator 32-byte pubkey)
    let bytes_key = BytesN::from_array(&env, &[42u8; 32]);
    let profile_bytes = benchmark_serialization_deserialization(
        &env,
        "StorageKey::BytesN32",
        &bytes_key,
    );
    profile_bytes.assert_within_target_allocations();
}

#[test]
fn test_profile_instance_storage_footprints() {
    let env = Env::default();
    let validator_pubkey = BytesN::from_array(&env, &[15u8; 32]);
    let stake_amount = 50_000_000i128;

    let footprint = measure_instance_storage_footprint(&env, &validator_pubkey, &stake_amount);
    eprintln!("[footprint-profile] validator_stake_entry_bytes={}", footprint);

    assert!(footprint > 0);
    assert!(
        footprint <= MAX_INSTANCE_ENTRY_BYTES_BUDGET,
        "Validator stake entry exceeded instance byte footprint budget"
    );
}
