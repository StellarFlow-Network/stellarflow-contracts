#![no_std]

use soroban_sdk::{
    contract, contractclient, contracterror, contractimpl, contracttype, Address, Env, Symbol, Vec,
};

const BPS_DENOMINATOR: i128 = 10_000;

#[contractclient(name = "PriceOracleClient")]
pub trait PriceOracle {
    fn get_twap(env: Env, asset: Symbol) -> Option<i128>;
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollateralAsset {
    pub asset: Symbol,
    pub amount: i128,
    pub multiplier_bps: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiquidationStep {
    pub asset: Symbol,
    pub collateral_amount: i128,
    pub repayment_value: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HealthReport {
    pub collateral_value: i128,
    pub debt_value: i128,
    pub health_factor_bps: i128,
    pub is_healthy: bool,
    pub liquidation_plan: Vec<LiquidationStep>,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    InvalidAmount = 1,
    InvalidMultiplier = 2,
    MissingTwap = 3,
    InvalidPrice = 4,
    ArithmeticOverflow = 5,
}

#[contract]
pub struct VaultHealthMonitor;

#[contractimpl]
impl VaultHealthMonitor {
    /// Inspect a vault snapshot using oracle TWAPs and plan liquidation if unhealthy.
    /// Amounts and prices must use compatible units; multipliers are collateral factors in
    /// basis points in (0, 10000]. Lower factors are treated as higher risk.
    pub fn inspect(
        env: Env,
        oracle: Address,
        collateral: Vec<CollateralAsset>,
        debt_asset: Symbol,
        debt_amount: i128,
    ) -> Result<HealthReport, Error> {
        if debt_amount <= 0 || collateral.is_empty() {
            return Err(Error::InvalidAmount);
        }

        let oracle_client = PriceOracleClient::new(&env, &oracle);
        let debt_price = oracle_client
            .get_twap(&debt_asset)
            .ok_or(Error::MissingTwap)?;
        if debt_price <= 0 {
            return Err(Error::InvalidPrice);
        }
        let debt_value = debt_amount
            .checked_mul(debt_price)
            .ok_or(Error::ArithmeticOverflow)?;

        let mut ordered: Vec<CollateralAsset> = Vec::new(&env);
        let mut collateral_value = 0_i128;
        for item in collateral.iter() {
            if item.amount <= 0 {
                return Err(Error::InvalidAmount);
            }
            if item.multiplier_bps <= 0 || item.multiplier_bps > BPS_DENOMINATOR {
                return Err(Error::InvalidMultiplier);
            }

            let price = oracle_client
                .get_twap(&item.asset)
                .ok_or(Error::MissingTwap)?;
            if price <= 0 {
                return Err(Error::InvalidPrice);
            }

            let adjusted_value = item
                .amount
                .checked_mul(price)
                .and_then(|value| value.checked_mul(item.multiplier_bps))
                .and_then(|value| value.checked_div(BPS_DENOMINATOR))
                .ok_or(Error::ArithmeticOverflow)?;
            collateral_value = collateral_value
                .checked_add(adjusted_value)
                .ok_or(Error::ArithmeticOverflow)?;

            let mut index = ordered.len();
            for (existing_index, existing) in ordered.iter().enumerate() {
                if item.multiplier_bps < existing.multiplier_bps {
                    index = existing_index as u32;
                    break;
                }
            }
            ordered.insert(index, item);
        }

        let health_factor_bps = collateral_value
            .checked_mul(BPS_DENOMINATOR)
            .and_then(|value| value.checked_div(debt_value))
            .ok_or(Error::ArithmeticOverflow)?;
        let is_healthy = collateral_value >= debt_value;
        let mut liquidation_plan = Vec::new(&env);

        if !is_healthy {
            let mut deficit = debt_value - collateral_value;
            let mut remaining_debt = debt_value;
            for item in ordered.iter() {
                if deficit <= 0 || remaining_debt <= 0 {
                    break;
                }

                let price = oracle_client
                    .get_twap(&item.asset)
                    .ok_or(Error::MissingTwap)?;
                let health_gap_per_unit = price
                    .checked_mul(BPS_DENOMINATOR - item.multiplier_bps)
                    .ok_or(Error::ArithmeticOverflow)?;
                if health_gap_per_unit == 0 {
                    continue;
                }

                let requested_amount = deficit
                    .checked_mul(BPS_DENOMINATOR)
                    .and_then(|value| value.checked_add(health_gap_per_unit - 1))
                    .and_then(|value| value.checked_div(health_gap_per_unit))
                    .ok_or(Error::ArithmeticOverflow)?;
                let max_amount_for_debt = remaining_debt / price;
                let collateral_amount = requested_amount.min(item.amount).min(max_amount_for_debt);
                if collateral_amount == 0 {
                    continue;
                }

                let repayment_value = collateral_amount
                    .checked_mul(price)
                    .ok_or(Error::ArithmeticOverflow)?;
                let adjusted_value = collateral_amount
                    .checked_mul(price)
                    .and_then(|value| value.checked_mul(item.multiplier_bps))
                    .and_then(|value| value.checked_div(BPS_DENOMINATOR))
                    .ok_or(Error::ArithmeticOverflow)?;
                let health_gap_reduction = repayment_value - adjusted_value;

                if health_gap_reduction > 0 {
                    liquidation_plan.push_back(LiquidationStep {
                        asset: item.asset.clone(),
                        collateral_amount,
                        repayment_value,
                    });
                    env.events().publish(
                        (Symbol::new(&env, "liquidation"), item.asset.clone()),
                        (collateral_amount, repayment_value, health_factor_bps),
                    );
                    deficit = if health_gap_reduction >= deficit {
                        0
                    } else {
                        deficit - health_gap_reduction
                    };
                    remaining_debt -= repayment_value;
                }
            }
        }

        Ok(HealthReport {
            collateral_value,
            debt_value,
            health_factor_bps,
            is_healthy,
            liquidation_plan,
        })
    }
}

#[cfg(test)]
mod test;
