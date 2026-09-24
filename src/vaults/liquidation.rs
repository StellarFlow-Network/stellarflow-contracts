use soroban_sdk::{contracttype, symbol_short, Address, Env, IntoVal, Symbol};

use crate::ContractError;

/// Basis-point denominator used by collateral ratios.
pub const BPS_DENOMINATOR: u128 = 10_000;
/// A vault is eligible for liquidation below 110% collateralization.
pub const DEFAULT_LIQUIDATION_THRESHOLD_BPS: u32 = 11_000;
/// Liquidators receive 5% of the confiscated collateral.
pub const LIQUIDATOR_BONUS_BPS: u32 = 500;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultPosition {
    pub owner: Address,
    /// Collateral amount, or its value when prices have already been applied.
    pub collateral_value: u128,
    /// Configured liquidation threshold in basis points. Zero uses 110%.
    pub liquidation_threshold_bps: u32,
    /// Debt amount, or its value when prices have already been applied.
    pub borrowed_value: u128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiquidationResult {
    pub liquidated: bool,
    /// Collateralization ratio in basis points (10_000 == 100%).
    pub health_factor: u128,
    pub liquidator_reward: u128,
    pub protocol_reserve: u128,
}

pub fn health_factor(position: &VaultPosition) -> Result<u128, ContractError> {
    if position.borrowed_value == 0 {
        return Ok(u128::MAX);
    }

    position
        .collateral_value
        .checked_mul(BPS_DENOMINATOR)
        .ok_or(ContractError::MathOverflow)?
        .checked_div(position.borrowed_value)
        .ok_or(ContractError::DivisionByZero)
}

fn threshold(position: &VaultPosition) -> u128 {
    if position.liquidation_threshold_bps == 0 {
        DEFAULT_LIQUIDATION_THRESHOLD_BPS as u128
    } else {
        position.liquidation_threshold_bps as u128
    }
}

pub fn liquidate(
    _env: &Env,
    position: &VaultPosition,
    purchase_collateral: u128,
) -> Result<LiquidationResult, ContractError> {
    let hf = health_factor(position)?;
    if hf >= threshold(position) {
        return Ok(LiquidationResult {
            liquidated: false,
            health_factor: hf,
            liquidator_reward: 0,
            protocol_reserve: 0,
        });
    }

    let reward = purchase_collateral
        .checked_mul(LIQUIDATOR_BONUS_BPS as u128)
        .ok_or(ContractError::MathOverflow)?
        .checked_div(BPS_DENOMINATOR)
        .ok_or(ContractError::DivisionByZero)?;
    let protocol_reserve = purchase_collateral
        .checked_sub(reward)
        .ok_or(ContractError::MathOverflow)?;

    Ok(LiquidationResult {
        liquidated: true,
        health_factor: hf,
        liquidator_reward: reward,
        protocol_reserve,
    })
}

/// Price a vault using the oracle's verified `get_twap(Symbol)` feed before
/// applying the liquidation rule. Missing, stale, or invalid feeds fail
/// closed; a caller cannot provide a fabricated price.
pub fn liquidate_at_twap(
    env: &Env,
    oracle: &Address,
    collateral_asset: &Symbol,
    debt_asset: &Symbol,
    position: &VaultPosition,
    purchase_collateral: u128,
) -> Result<LiquidationResult, ContractError> {
    let collateral_price = read_twap(env, oracle, collateral_asset)?;
    let debt_price = read_twap(env, oracle, debt_asset)?;
    if collateral_price <= 0 || debt_price <= 0 {
        return Err(ContractError::NotInitialized);
    }

    let collateral_value = position
        .collateral_value
        .checked_mul(collateral_price as u128)
        .ok_or(ContractError::MathOverflow)?;
    let borrowed_value = position
        .borrowed_value
        .checked_mul(debt_price as u128)
        .ok_or(ContractError::MathOverflow)?;
    let priced_position = VaultPosition {
        collateral_value,
        borrowed_value,
        ..position.clone()
    };

    liquidate(env, &priced_position, purchase_collateral)
}

fn read_twap(env: &Env, oracle: &Address, asset: &Symbol) -> Result<i128, ContractError> {
    let result: Result<Option<i128>, soroban_sdk::Error> = env.invoke_contract(
        oracle,
        &symbol_short!("get_twap"),
        soroban_sdk::vec![env, asset.into_val(env)],
    );
    match result {
        Ok(Some(price)) => Ok(price),
        _ => Err(ContractError::NotInitialized),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    fn position(env: &Env, collateral: u128, debt: u128) -> VaultPosition {
        VaultPosition {
            owner: Address::generate(env),
            collateral_value: collateral,
            liquidation_threshold_bps: DEFAULT_LIQUIDATION_THRESHOLD_BPS,
            borrowed_value: debt,
        }
    }

    #[test]
    fn calculates_ratio_without_integer_truncation() {
        let env = Env::default();
        assert_eq!(health_factor(&position(&env, 109, 100)).unwrap(), 10_900);
        assert_eq!(health_factor(&position(&env, 110, 100)).unwrap(), 11_000);
    }

    #[test]
    fn liquidates_below_110_percent_and_splits_five_percent_bonus() {
        let env = Env::default();
        let result = liquidate(&env, &position(&env, 109, 100), 100).unwrap();
        assert!(result.liquidated);
        assert_eq!(result.health_factor, 10_900);
        assert_eq!(result.liquidator_reward, 5);
        assert_eq!(result.protocol_reserve, 95);
    }

    #[test]
    fn does_not_liquidate_at_or_above_threshold() {
        let env = Env::default();
        let result = liquidate(&env, &position(&env, 110, 100), 100).unwrap();
        assert!(!result.liquidated);
        assert_eq!(result.liquidator_reward, 0);
    }
}
