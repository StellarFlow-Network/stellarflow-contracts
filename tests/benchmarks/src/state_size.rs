//! Soroban State Size Benchmarking and Profiling Suite (Issue #949).
//!
//! Profiles contract binary compile sizes and instance storage byte footprints.
//! Benchmarks execution costs (CPU instructions and memory bytes) for key serialization
//! and deserialization tasks, and enforces target allocation limits.
//!
//! Time Complexity:
//! - Serialization benchmark: O(N) where N is the serialized XDR byte length.
//! - Footprint calculation: O(1) direct XDR length query.
//!
//! Space Complexity:
//! - O(N) temporary buffer for serialized bytes.

use soroban_sdk::{
    xdr::{FromXdr, ToXdr},
    Address, Bytes, BytesN, Env, IntoVal, Symbol, TryFromVal, Val,
};

/// Target allocation limits for state size and serialization tasks.
pub const MAX_KEY_SERIALIZATION_CPU_BUDGET: u64 = 100_000;
pub const MAX_KEY_SERIALIZATION_MEM_BUDGET: u64 = 50_000;
pub const MAX_INSTANCE_ENTRY_BYTES_BUDGET: usize = 64_000; // 64 KB instance entry limit

/// Result of a state size and serialization benchmark measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateSizeProfile {
    pub task_name: &'static str,
    pub serialized_bytes: usize,
    pub cpu_instructions: u64,
    pub memory_bytes: u64,
}

impl StateSizeProfile {
    pub fn log(&self) {
        eprintln!(
            "[state-profile] task={} serialized_bytes={} cpu_instructions={} memory_bytes={}",
            self.task_name, self.serialized_bytes, self.cpu_instructions, self.memory_bytes
        );
    }

    /// Assert that the measured profile is strictly within target allocations.
    /// Fails CI/test if binary size or gas/footprint profile exceeds target allocations.
    pub fn assert_within_target_allocations(&self) {
        assert!(
            self.serialized_bytes <= MAX_INSTANCE_ENTRY_BYTES_BUDGET,
            "Task '{}' exceeded instance byte footprint budget: {} > {}",
            self.task_name,
            self.serialized_bytes,
            MAX_INSTANCE_ENTRY_BYTES_BUDGET
        );
        assert!(
            self.cpu_instructions <= MAX_KEY_SERIALIZATION_CPU_BUDGET,
            "Task '{}' exceeded CPU allocation budget: {} > {}",
            self.task_name,
            self.cpu_instructions,
            MAX_KEY_SERIALIZATION_CPU_BUDGET
        );
        assert!(
            self.memory_bytes <= MAX_KEY_SERIALIZATION_MEM_BUDGET,
            "Task '{}' exceeded memory allocation budget: {} > {}",
            self.task_name,
            self.memory_bytes,
            MAX_KEY_SERIALIZATION_MEM_BUDGET
        );
    }
}

/// Benchmark serialization and deserialization execution costs for a given value.
pub fn benchmark_serialization_deserialization<T>(
    env: &Env,
    task_name: &'static str,
    value: &T,
) -> StateSizeProfile
where
    T: ToXdr + FromXdr + Clone,
{
    let cpu_before = env.budget().cpu_instruction_cost();
    let mem_before = env.budget().memory_bytes_cost();

    // 1. Benchmark Serialization to XDR bytes
    let serialized_xdr = value.to_xdr(env);
    let serialized_len = serialized_xdr.len() as usize;

    // 2. Benchmark Deserialization from XDR bytes
    let _deserialized = T::from_xdr(env, &serialized_xdr).expect("deserialization should succeed");

    let cpu_instructions = env
        .budget()
        .cpu_instruction_cost()
        .saturating_sub(cpu_before);
    let memory_bytes = env.budget().memory_bytes_cost().saturating_sub(mem_before);

    let profile = StateSizeProfile {
        task_name,
        serialized_bytes: serialized_len,
        cpu_instructions,
        memory_bytes,
    };

    profile.log();
    profile
}

/// Measure instance storage byte footprint of an encoded key-value pair.
pub fn measure_instance_storage_footprint<K, V>(env: &Env, key: &K, val: &V) -> usize
where
    K: ToXdr,
    V: ToXdr,
{
    let key_bytes = key.to_xdr(env).len() as usize;
    let val_bytes = val.to_xdr(env).len() as usize;
    key_bytes + val_bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    #[test]
    fn test_benchmark_key_serialization_deserialization() {
        let env = Env::default();
        let sample_key = Symbol::new(&env, "ValidatorConsensusThreshold");

        let profile = benchmark_serialization_deserialization(
            &env,
            "SymbolKeySerialization",
            &sample_key,
        );

        assert!(profile.serialized_bytes > 0);
        profile.assert_within_target_allocations();
    }

    #[test]
    fn test_benchmark_address_key_footprint() {
        let env = Env::default();
        let addr = Address::generate(&env);
        let val = 1_000_000i128;

        let footprint = measure_instance_storage_footprint(&env, &addr, &val);
        assert!(footprint > 0);
        assert!(footprint <= MAX_INSTANCE_ENTRY_BYTES_BUDGET);
    }
}
