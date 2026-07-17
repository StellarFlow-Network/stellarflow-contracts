//! High-precision fee arithmetic for multi-hop corridor pools.
//!
//! Fractional corridor usage fee splits scale intermediate products by
//! `INTERIOR_SCALE` (10^14) before division, then normalize back to the
//! standard 10^7 fixed-point footprint prior to ledger mutations.

use crate::{AssetId, ContractError, TimeLockedUpgradeContract};
use soroban_sdk::{contracttype, Address, Env, Vec};

pub const STANDARD_FIXED_POINT_SCALE: i128 = 10_000_000;
pub const INTERIOR_FEE_PRECISION_SCALE: i128 = 100_000_000_000_000;

// ---------------------------------------------------------------------------
// Asset pricing storage (general — unchanged)
// ---------------------------------------------------------------------------

/// Interior scaling coefficient applied before division steps (10^14).
pub const INTERIOR_SCALE: u128 = 100_000_000_000_000;

/// System standard fixed-point footprint (10^7).
pub const FIXED_POINT_SCALE: u128 = 10_000_000;

#[contracttype]
#[derive(Clone)]
pub struct CorridorFeePool {
    pub asset: AssetId,
    pub collected: u64,
    pub variable_pool: u64,
}

#[contracttype]
pub enum FeesStorageKey {
    CorridorPool(AssetId),
}

impl CorridorFeePool {
    fn new(asset: AssetId) -> Self {
        Self {
            asset,
            collected: 0,
            variable_pool: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Corridor weight profile — separated from asset pricing entries (issue #530)
// ---------------------------------------------------------------------------

/// Dedicated profile holding dynamic corridor weight variables.
/// Kept in its own storage key so audits and state updates never
/// touch the general asset pricing block.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct CorridorWeightProfile {
    pub asset: AssetId,
    pub base_weight: u64,
    pub dynamic_weight: u64,
}

/// Separate storage namespace for corridor weight profiles.
#[contracttype]
pub enum CorridorWeightKey {
    Profile(AssetId),
}

impl CorridorWeightProfile {
    fn new(asset: AssetId) -> Self {
        Self {
            asset,
            base_weight: 0,
            dynamic_weight: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Fee pool functions (unchanged behaviour)
// ---------------------------------------------------------------------------

pub fn add_corridor_fees(
    env: Env,
    admin: Address,
    asset: AssetId,
    collected: u64,
    variable_fee: u64,
) -> Result<CorridorFeePool, ContractError> {
    admin.require_auth();
    let data = TimeLockedUpgradeContract::get_data(env.clone())?;
    if data.admin != admin {
        return Err(ContractError::NotAdmin);
    }
    let key = FeesStorageKey::CorridorPool(asset.clone());
    let mut pool: CorridorFeePool = env
        .storage()
        .instance()
        .get(&key)
        .unwrap_or(CorridorFeePool::new(asset.clone()));
    pool.collected = pool
        .collected
        .checked_add(collected)
        .ok_or(ContractError::Overflow)?;
    pool.variable_pool = pool
        .variable_pool
        .checked_add(variable_fee)
        .ok_or(ContractError::Overflow)?;
    env.storage().instance().set(&key, &pool);
    Ok(pool)
}

pub fn get_corridor_fee_pool(env: Env, asset: AssetId) -> CorridorFeePool {
    let key = FeesStorageKey::CorridorPool(asset.clone());
    env.storage()
        .instance()
        .get(&key)
        .unwrap_or(CorridorFeePool::new(asset))
}

pub fn distribute_variable_fee_pool(
    env: &Env,
    variable_pool: u64,
    relayer_weights: Vec<u64>,
) -> Result<Vec<u64>, ContractError> {
    let total_weight = relayer_weights
        .iter()
        .try_fold(0_i128, |acc, weight| {
            acc.checked_add(weight as i128)
                .ok_or(ContractError::Overflow)
        })?;

    let mut profiles = Vec::new(env);
    if total_weight == 0 || relayer_weights.len() == 0 {
        return Ok(profiles);
    }

    let pool_profile = (variable_pool as i128)
        .checked_mul(STANDARD_FIXED_POINT_SCALE)
        .ok_or(ContractError::Overflow)?;
    let interior_pool_profile = pool_profile
        .checked_mul(INTERIOR_FEE_PRECISION_SCALE)
        .ok_or(ContractError::Overflow)?;

    let last_index = relayer_weights.len() - 1;
    let mut assigned_profile = 0_i128;

    for index in 0..relayer_weights.len() {
        let profile = if index == last_index {
            pool_profile
                .checked_sub(assigned_profile)
                .ok_or(ContractError::Overflow)?
        } else {
            let weight = relayer_weights
                .get(index)
                .ok_or(ContractError::Overflow)? as i128;
            let interior_share = interior_pool_profile
                .checked_mul(weight)
                .ok_or(ContractError::Overflow)?
                .checked_div(total_weight)
                .ok_or(ContractError::DivisionByZero)?;
            interior_share
                .checked_div(INTERIOR_FEE_PRECISION_SCALE)
                .ok_or(ContractError::DivisionByZero)?
        };

        assigned_profile = assigned_profile
            .checked_add(profile)
            .ok_or(ContractError::Overflow)?;
        profiles.push_back(profile.try_into().map_err(|_| ContractError::Overflow)?);
    }

    Ok(profiles)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fee_distribution_normalizes_to_standard_fixed_point_footprint() {
        let env = Env::default();
        let mut weights = Vec::new(&env);
        weights.push_back(1);
        weights.push_back(1);
        weights.push_back(1);

        let profiles = distribute_variable_fee_pool(&env, 1, weights).unwrap();

        assert_eq!(profiles.get(0), Some(3_333_333));
        assert_eq!(profiles.get(1), Some(3_333_333));
        assert_eq!(profiles.get(2), Some(3_333_334));
        assert_eq!(
            profiles.iter().fold(0_u64, |acc, value| acc + value),
            STANDARD_FIXED_POINT_SCALE as u64
        );
    }

    #[test]
    fn test_corridor_usage_fee_share_three_way_split_preserves_total() {
        let total_fee = 10_000u64;
        let usages = [3_333_333u64, 3_333_333u64, 3_333_334u64];
        let total_usage: u64 = 10_000_000;

        let mut allocated = 0u64;
        for (index, usage) in usages.iter().enumerate() {
            let share = if index == usages.len() - 1 {
                total_fee - allocated
            } else {
                compute_corridor_usage_fee_share(total_fee, *usage, total_usage).unwrap()
            };
            allocated += share;
        }

        assert_eq!(allocated, total_fee);
    }

    #[test]
    fn test_multi_hop_fee_share_single_pass_matches_chained_low_precision() {
        let total_fee = 1_000_000u64;
        let hop_usage = 4_000_000u64;
        let relayer_usage = 2_500_000u64;
        let total_hop_usage = 10_000_000u64;
        let total_relayer_usage = 10_000_000u64;

        let high_precision = compute_multi_hop_corridor_fee_share(
            total_fee,
            hop_usage,
            relayer_usage,
            total_hop_usage,
            total_relayer_usage,
        )
        .unwrap();

        // Low-precision chained division: ((fee * hop / total_hop) * relayer / total_relayer)
        let hop_share = total_fee * hop_usage / total_hop_usage;
        let low_precision = hop_share * relayer_usage / total_relayer_usage;

        assert!(high_precision >= low_precision);
        assert_eq!(high_precision, 100_000);
    }

    #[test]
    fn test_compute_corridor_usage_fee_share_zero_total_usage() {
        assert_eq!(
            compute_corridor_usage_fee_share(100, 1, 0),
            Err(ContractError::DivisionByZero)
        );
    }

    #[test]
    fn test_normalize_to_fixed_point_footprint_overflow() {
        let too_large = u128::from(u64::MAX) * u128::from(u64::MAX) * INTERIOR_SCALE;
        assert_eq!(
            normalize_to_fixed_point_footprint(too_large),
            Err(ContractError::Overflow)
        );
    }
}
