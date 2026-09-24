use soroban_sdk::{contracttype, Env};

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct InterestRateConfig {
    pub base_rate_bps: u32,
    pub multiplier_bps: u32,
    pub jump_multiplier_bps: u32,
    pub optimal_utilization_bps: u32,
    pub ledgers_per_year: u64,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct PoolState {
    pub cash: i128,
    pub borrows: i128,
    pub last_accrued_ledger: u32,
    pub accumulated_interest_index: u128,
}

pub struct InterestRateController;

impl InterestRateController {
    pub fn calculate_utilization(cash: i128, borrows: i128) -> u32 {
        let total_assets = cash + borrows;
        if total_assets == 0 {
            return 0;
        }
        ((borrows * 10_000) / total_assets) as u32
    }

    pub fn calculate_interest_rate(utilization: u32, config: &InterestRateConfig) -> u32 {
        if utilization <= config.optimal_utilization_bps {
            let slope = (utilization as u64 * config.multiplier_bps as u64) / 10_000;
            config.base_rate_bps + slope as u32
        } else {
            let base_slope = (config.optimal_utilization_bps as u64 * config.multiplier_bps as u64) / 10_000;
            let excess_utilization = utilization - config.optimal_utilization_bps;
            let excess_slope = (excess_utilization as u64 * config.jump_multiplier_bps as u64) / 10_000;
            config.base_rate_bps + base_slope as u32 + excess_slope as u32
        }
    }

    pub fn accrue_interest(
        env: &Env,
        pool: &mut PoolState,
        config: &InterestRateConfig,
    ) -> i128 {
        let current_ledger = env.ledger().sequence();
        if current_ledger <= pool.last_accrued_ledger {
            return 0;
        }

        let elapsed_ledgers = (current_ledger - pool.last_accrued_ledger) as u64;
        let utilization = Self::calculate_utilization(pool.cash, pool.borrows);
        let rate_bps = Self::calculate_interest_rate(utilization, config);

        let scale_1e18 = 1_000_000_000_000_000_000u128;
        let interest_factor = (rate_bps as u128 * elapsed_ledgers as u128 * scale_1e18) 
            / (10_000u128 * config.ledgers_per_year as u128);

        let accrued_interest_index_delta = (pool.accumulated_interest_index * interest_factor) / scale_1e18;
        let new_interest_index = pool.accumulated_interest_index + accrued_interest_index_delta;

        let interest_accrued = (pool.borrows * accrued_interest_index_delta as i128) / pool.accumulated_interest_index as i128;

        pool.borrows += interest_accrued;
        pool.accumulated_interest_index = new_interest_index;
        pool.last_accrued_ledger = current_ledger;

        interest_accrued
    }
}
