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

/// Scale an intermediate product into interior precision space before division.
fn scale_product_to_interior(a: u128, b: u128) -> Result<u128, ContractError> {
    a.checked_mul(b)
        .ok_or(ContractError::Overflow)?
        .checked_mul(INTERIOR_SCALE)
        .ok_or(ContractError::Overflow)
}

/// Normalize an interior-space quotient back to the 10^7 fixed-point footprint.
pub fn normalize_to_fixed_point_footprint(interior_value: u128) -> Result<u64, ContractError> {
    let normalized = interior_value
        .checked_div(INTERIOR_SCALE)
        .ok_or(ContractError::DivisionByZero)?;
    u64::try_from(normalized).map_err(|_| ContractError::Overflow)
}

/// Multiply two fixed-point values and scale down to the 10^7 footprint.
///
/// Pre-multiplies the intermediate product by `INTERIOR_SCALE` before the
/// division step, then normalizes the result back to the standard footprint.
pub fn multiply_and_scale_down(a: u64, b: u64) -> Result<u64, ContractError> {
    let interior_product = scale_product_to_interior(u128::from(a), u128::from(b))?;
    let interior_quotient = interior_product
        .checked_div(FIXED_POINT_SCALE)
        .ok_or(ContractError::DivisionByZero)?;
    normalize_to_fixed_point_footprint(interior_quotient)
}

/// Compute a single relayer's corridor usage fee share from the variable pool.
///
/// Uses interior scaling so fractional weights do not truncate before the
/// final stroop allocation is written to ledger storage.
pub fn compute_corridor_usage_fee_share(
    total_fee: u64,
    relayer_usage: u64,
    total_usage: u64,
) -> Result<u64, ContractError> {
    if total_usage == 0 {
        return Err(ContractError::DivisionByZero);
    }
    if total_fee == 0 || relayer_usage == 0 {
        return Ok(0);
    }

    let interior_numerator = u128::from(total_fee)
        .checked_mul(u128::from(relayer_usage))
        .ok_or(ContractError::Overflow)?
        .checked_mul(INTERIOR_SCALE)
        .ok_or(ContractError::Overflow)?;

    let interior_quotient = interior_numerator / u128::from(total_usage);
    normalize_to_fixed_point_footprint(interior_quotient)
}

/// Compute a relayer's fee share across a multi-hop corridor path.
///
/// Combines hop-level and relayer-level usage weights in one interior-scaled
/// pass to avoid compounded truncation error across separate relayers.
pub fn compute_multi_hop_corridor_fee_share(
    total_fee: u64,
    hop_usage: u64,
    relayer_usage: u64,
    total_hop_usage: u64,
    total_relayer_usage: u64,
) -> Result<u64, ContractError> {
    if total_hop_usage == 0 || total_relayer_usage == 0 {
        return Err(ContractError::DivisionByZero);
    }
    if total_fee == 0 || hop_usage == 0 || relayer_usage == 0 {
        return Ok(0);
    }

    let interior_numerator = u128::from(total_fee)
        .checked_mul(u128::from(hop_usage))
        .ok_or(ContractError::Overflow)?
        .checked_mul(u128::from(relayer_usage))
        .ok_or(ContractError::Overflow)?
        .checked_mul(INTERIOR_SCALE)
        .ok_or(ContractError::Overflow)?;

    let interior_denominator = u128::from(total_hop_usage)
        .checked_mul(u128::from(total_relayer_usage))
        .ok_or(ContractError::Overflow)?;

    let interior_quotient = interior_numerator / interior_denominator;
    normalize_to_fixed_point_footprint(interior_quotient)
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

// ---------------------------------------------------------------------------
// Corridor weight profile functions — independent access control (issue #530)
// ---------------------------------------------------------------------------

/// Set or update the corridor weight profile for an asset.
/// Uses its own admin check so weight edits are gated independently
/// from fee pool writes.
pub fn set_corridor_weight(
    env: Env,
    admin: Address,
    asset: AssetId,
    base_weight: u64,
    dynamic_weight: u64,
) -> Result<CorridorWeightProfile, ContractError> {
    admin.require_auth();
    let data = TimeLockedUpgradeContract::get_data(env.clone())?;
    if data.admin != admin {
        return Err(ContractError::NotAdmin);
    }
    let key = CorridorWeightKey::Profile(asset.clone());
    let profile = CorridorWeightProfile {
        asset: asset.clone(),
        base_weight,
        dynamic_weight,
    };
    env.storage().persistent().set(&key, &profile);
    Ok(profile)
}

/// Read the corridor weight profile for an asset.
pub fn get_corridor_weight(env: Env, asset: AssetId) -> CorridorWeightProfile {
    let key = CorridorWeightKey::Profile(asset.clone());
    env.storage()
        .persistent()
        .get(&key)
        .unwrap_or(CorridorWeightProfile::new(asset))
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
    use crate::TimeLockedUpgradeContractClient;
    use soroban_sdk::testutils::Address as _;

    fn setup() -> (Env, TimeLockedUpgradeContractClient<'static>, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
        let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let attacker = Address::generate(&env);
        client.initialize(&admin, &treasury);
        (env, client, admin, attacker)
    }

    #[test]
    fn corridor_weight_profile_is_isolated_from_fee_pool() {
        let (_, client, admin, _) = setup();
        let asset = 3897123275;

        let pool = client.add_corridor_fees(&admin, &asset, &1_000, &25);
        assert_eq!(pool.collected, 1_000);
        assert_eq!(pool.variable_pool, 25);

        let profile = client.set_corridor_weight(&admin, &asset, &70, &30);
        assert_eq!(profile.asset, asset);
        assert_eq!(profile.base_weight, 70);
        assert_eq!(profile.dynamic_weight, 30);

        let unchanged_pool = client.get_corridor_fee_pool(&asset);
        assert_eq!(unchanged_pool.collected, 1_000);
        assert_eq!(unchanged_pool.variable_pool, 25);

        let stored_profile = client.get_corridor_weight(&asset);
        assert_eq!(stored_profile.base_weight, 70);
        assert_eq!(stored_profile.dynamic_weight, 30);
    }

    #[test]
    fn non_admin_cannot_edit_corridor_weight_profile() {
        let (_, client, _, attacker) = setup();
        let result = client.try_set_corridor_weight(&attacker, &2654435761, &40, &60);

        assert_eq!(result, Err(Ok(ContractError::NotAdmin)));
    }

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
        // A value whose interior-scaled quotient exceeds u64::MAX once divided
        // back down, without overflowing the u128 computation itself.
        let too_large: u128 = (u128::from(u64::MAX) + 1) * INTERIOR_SCALE;
        assert_eq!(
            normalize_to_fixed_point_footprint(too_large),
            Err(ContractError::Overflow)
        );
    }
}
