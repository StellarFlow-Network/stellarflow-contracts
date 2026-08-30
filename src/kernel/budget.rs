#![cfg(any(test, feature = "testutils"))]

use soroban_sdk::Env;

pub struct GasLogger;

impl GasLogger {
    pub fn log_budget(env: &Env, function_name: &str) {
        let budget = env.budget();
        let cpu = budget.cpu_instruction_cost();
        let mem = budget.memory_bytes_cost();

        // Print budget logs
        budget.print();

        // Highlight functions approaching block limits
        // CPU limit: 100,000,000
        // Memory limit: 41,943,040 bytes
        let cpu_pct = (cpu as f64 / 100_000_000.0) * 100.0;
        let mem_pct = (mem as f64 / 41_943_040.0) * 100.0;

        extern crate std;
        std::println!("\n==================================================");
        std::println!(" GAS CONSUMPTION LOG: {}", function_name);
        std::println!("--------------------------------------------------");
        std::println!("CPU Instructions:  {:>12} / 100,000,000 ({:.2}%)", cpu, cpu_pct);
        std::println!("Memory Allocation: {:>12} / 41,943,040  ({:.2}%)", mem, mem_pct);

        if cpu_pct >= 80.0 {
            std::println!("⚠️ WARNING: CPU instructions approach maximum Soroban block limit!");
        }
        if mem_pct >= 80.0 {
            std::println!("⚠️ WARNING: Memory allocation approaches maximum Soroban block limit!");
        }
        std::println!("==================================================\n");
    }
}
