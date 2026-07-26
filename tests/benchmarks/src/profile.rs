//! Helpers for measuring and logging Soroban host resource usage in tests.

use soroban_sdk::Env;

use crate::limits::{safe_cpu_instruction_ceiling, safe_memory_byte_ceiling};

/// Resource consumption attributed to a single contract entrypoint invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntrypointUsage {
    pub entrypoint: &'static str,
    pub cpu_instructions: u64,
    pub memory_bytes: u64,
}

impl EntrypointUsage {
    pub fn log(&self) {
        eprintln!(
            "[resource-profile] entrypoint={} cpu_instructions={} memory_bytes={}",
            self.entrypoint, self.cpu_instructions, self.memory_bytes
        );
    }

    pub fn assert_within_safe_network_limits(&self) {
        let cpu_ceiling = safe_cpu_instruction_ceiling();
        let mem_ceiling = safe_memory_byte_ceiling();
        assert!(
            self.cpu_instructions <= cpu_ceiling,
            "entrypoint {} exceeded safe CPU budget: {} > {} (80% of network limit)",
            self.entrypoint,
            self.cpu_instructions,
            cpu_ceiling
        );
        assert!(
            self.memory_bytes <= mem_ceiling,
            "entrypoint {} exceeded safe memory budget: {} > {} (80% of network limit)",
            self.entrypoint,
            self.memory_bytes,
            mem_ceiling
        );
    }
}

/// Measure incremental CPU and memory charged by `invoke` on the test `Env`.
pub fn measure_entrypoint<F>(env: &Env, entrypoint: &'static str, invoke: F) -> EntrypointUsage
where
    F: FnOnce(),
{
    let cpu_before = env.budget().cpu_instruction_cost();
    let mem_before = env.budget().memory_bytes_cost();
    invoke();
    let usage = EntrypointUsage {
        entrypoint,
        cpu_instructions: env
            .budget()
            .cpu_instruction_cost()
            .saturating_sub(cpu_before),
        memory_bytes: env
            .budget()
            .memory_bytes_cost()
            .saturating_sub(mem_before),
    };
    usage.log();
    usage
}

/// Log and assert limits for a full multi-step swap-style transaction path.
pub fn assert_swap_path_within_limits(usages: &[EntrypointUsage], total_cpu: u64, total_mem: u64) {
    for usage in usages {
        usage.assert_within_safe_network_limits();
    }

    let cpu_ceiling = safe_cpu_instruction_ceiling();
    let mem_ceiling = safe_memory_byte_ceiling();

    eprintln!(
        "[resource-profile] swap_path_total cpu_instructions={} memory_bytes={}",
        total_cpu, total_mem
    );

    assert!(
        total_cpu <= cpu_ceiling,
        "swap path exceeded safe CPU budget: {} > {}",
        total_cpu,
        cpu_ceiling
    );
    assert!(
        total_mem <= mem_ceiling,
        "swap path exceeded safe memory budget: {} > {}",
        total_mem,
        mem_ceiling
    );
}
