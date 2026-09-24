//! Soroban network resource ceilings used for budget safety checks.
//!
//! Values align with default transaction resource limits on Stellar pubnet/testnet
//! (see Stellar protocol resource configuration).

/// Default per-transaction CPU instruction budget on Soroban.
pub const NETWORK_CPU_INSTRUCTION_LIMIT: u64 = 100_000_000;

/// Default per-transaction memory budget (40 MiB).
pub const NETWORK_MEMORY_BYTE_LIMIT: u64 = 41_943_040;

/// Require entrypoints to remain below this fraction of the network CPU cap.
pub const SAFE_CPU_UTILIZATION_RATIO: f64 = 0.80;

/// Require entrypoints to remain below this fraction of the network memory cap.
pub const SAFE_MEMORY_UTILIZATION_RATIO: f64 = 0.80;

pub fn safe_cpu_instruction_ceiling() -> u64 {
    (NETWORK_CPU_INSTRUCTION_LIMIT as f64 * SAFE_CPU_UTILIZATION_RATIO) as u64
}

pub fn safe_memory_byte_ceiling() -> u64 {
    (NETWORK_MEMORY_BYTE_LIMIT as f64 * SAFE_MEMORY_UTILIZATION_RATIO) as u64
}
