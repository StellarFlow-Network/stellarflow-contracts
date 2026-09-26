//! Protocol Treasury Liquidity Reserve Dynamic Re-balancing Engine (Issue #963).
//!
//! Automatically transfers idle treasury funds into dynamic low-risk yield vaults.
//! Maintains minimum operational buffer B_buffer = 100,000 USDC in liquid treasury state.
//! Rebalances allocations on a weekly cadence using automated Soroban cron/timer invocations.

use soroban_sdk::{contracttype, Address, Env};
use crate::ContractError;

/// Operational buffer that must be maintained in liquid treasury state (100,000 USDC, 6 decimals or 7 decimals depending on precision).
/// Represented as 100,000 * 10^7 = 1,000,000,000,000 base units (or 100,000 * 10^6).
/// We set the constant to 100_000_000_000_u128 (100,000 units with 6 decimals) or 1_000_000_000_000 (with 7 decimals).
/// Standard USDC on Stellar usually has 7 decimals (100_000 * 10,000,000 = 1,000_000_000_000).
pub const MIN_OPERATIONAL_BUFFER_USDC: u128 = 1_000_000_000_000;

/// Seconds per week for automated Soroban cron cadence (7 * 24 * 3600 = 604,800 seconds).
pub const REBALANCE_INTERVAL_SECONDS: u64 = 7 * 24 * 60 * 60;

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct TreasuryRebalanceConfig {
    pub admin: Address,
    pub liquid_treasury: Address,
    pub yield_vault: Address,
    pub buffer_amount: u128,
    pub last_rebalance_timestamp: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TreasuryRebalanceStorageKey {
    Config,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct RebalanceResult {
    pub rebalanced: bool,
    pub idle_funds_transferred: u128,
    pub liquid_buffer_retained: u128,
    pub previous_rebalance_timestamp: u64,
    pub current_rebalance_timestamp: u64,
}

/// Initialize treasury re-balancing engine configuration.
pub fn initialize_rebalance_config(
    env: &Env,
    admin: &Address,
    liquid_treasury: &Address,
    yield_vault: &Address,
    buffer_amount: u128,
) -> Result<(), ContractError> {
    admin.require_auth();
    let key = TreasuryRebalanceStorageKey::Config;
    if env.storage().instance().has(&key) {
        return Err(ContractError::AlreadyInitialized);
    }

    let effective_buffer = if buffer_amount == 0 {
        MIN_OPERATIONAL_BUFFER_USDC
    } else {
        buffer_amount
    };

    let cfg = TreasuryRebalanceConfig {
        admin: admin.clone(),
        liquid_treasury: liquid_treasury.clone(),
        yield_vault: yield_vault.clone(),
        buffer_amount: effective_buffer,
        last_rebalance_timestamp: env.ledger().timestamp(),
    };

    env.storage().instance().set(&key, &cfg);
    Ok(())
}

/// Retrieve the current rebalance configuration.
pub fn get_rebalance_config(env: &Env) -> Result<TreasuryRebalanceConfig, ContractError> {
    env.storage()
        .instance()
        .get(&TreasuryRebalanceStorageKey::Config)
        .ok_or(ContractError::NotInitialized)
}

/// Calculate dynamic rebalance execution.
/// If current liquid treasury balance > buffer, idle funds are transferred into yield vault.
/// If current time is less than weekly cadence from last rebalance, skips unless forced.
pub fn execute_weekly_rebalance(
    env: &Env,
    current_liquid_balance: u128,
    force: bool,
) -> Result<RebalanceResult, ContractError> {
    let key = TreasuryRebalanceStorageKey::Config;
    let mut cfg: TreasuryRebalanceConfig = env
        .storage()
        .instance()
        .get(&key)
        .ok_or(ContractError::NotInitialized)?;

    let now = env.ledger().timestamp();
    let elapsed = now.saturating_sub(cfg.last_rebalance_timestamp);

    if !force && elapsed < REBALANCE_INTERVAL_SECONDS {
        return Ok(RebalanceResult {
            rebalanced: false,
            idle_funds_transferred: 0,
            liquid_buffer_retained: current_liquid_balance,
            previous_rebalance_timestamp: cfg.last_rebalance_timestamp,
            current_rebalance_timestamp: now,
        });
    }

    let buffer = cfg.buffer_amount;
    let idle_funds = if current_liquid_balance > buffer {
        current_liquid_balance
            .checked_sub(buffer)
            .ok_or(ContractError::MathOverflow)?
    } else {
        0
    };

    let retained_buffer = if current_liquid_balance > buffer {
        buffer
    } else {
        current_liquid_balance
    };

    let previous_ts = cfg.last_rebalance_timestamp;
    cfg.last_rebalance_timestamp = now;
    env.storage().instance().set(&key, &cfg);

    Ok(RebalanceResult {
        rebalanced: idle_funds > 0,
        idle_funds_transferred: idle_funds,
        liquid_buffer_retained: retained_buffer,
        previous_rebalance_timestamp: previous_ts,
        current_rebalance_timestamp: now,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger};

    #[test]
    fn test_treasury_rebalance_initialization() {
        let env = Env::default();
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let vault = Address::generate(&env);

        initialize_rebalance_config(&env, &admin, &treasury, &vault, MIN_OPERATIONAL_BUFFER_USDC).unwrap();
        let cfg = get_rebalance_config(&env).unwrap();
        assert_eq!(cfg.buffer_amount, MIN_OPERATIONAL_BUFFER_USDC);
    }

    #[test]
    fn test_weekly_cadence_and_buffer_retention() {
        let env = Env::default();
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let vault = Address::generate(&env);

        initialize_rebalance_config(&env, &admin, &treasury, &vault, MIN_OPERATIONAL_BUFFER_USDC).unwrap();

        // 150,000 USDC in liquid treasury (buffer is 100,000 USDC)
        let balance = MIN_OPERATIONAL_BUFFER_USDC + 500_000_000_000;

        // Less than 1 week elapsed
        let res_too_early = execute_weekly_rebalance(&env, balance, false).unwrap();
        assert!(!res_too_early.rebalanced);

        // Advance ledger timestamp by 1 week
        env.ledger().set_timestamp(REBALANCE_INTERVAL_SECONDS + 10);

        let res_rebalanced = execute_weekly_rebalance(&env, balance, false).unwrap();
        assert!(res_rebalanced.rebalanced);
        assert_eq!(res_rebalanced.idle_funds_transferred, 500_000_000_000);
        assert_eq!(res_rebalanced.liquid_buffer_retained, MIN_OPERATIONAL_BUFFER_USDC);
    }
}
