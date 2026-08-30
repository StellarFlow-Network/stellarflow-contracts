//! High-precision fee arithmetic for multi-hop corridor pools.
//!
//! Fractional corridor usage fee splits scale intermediate products by
//! `INTERIOR_SCALE` (10^14) before division, then normalize back to the
//! standard 10^7 fixed-point footprint prior to ledger mutations.

use crate::{AssetId, ContractData, ContractError, TimeLockedUpgradeContract, DATA_KEY};
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
    VolumeHistory(AssetId),
    DynamicFee(AssetId),
    FlashLoanPool(AssetId),
}

/// Separate fee tracking pool for flash loan revenue.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct FlashLoanFeePool {
    pub asset: AssetId,
    pub accumulated_fees: u64,
    pub total_lp_distributed: u64,
    pub total_treasury_distributed: u64,
}

impl FlashLoanFeePool {
    pub fn new(asset: AssetId) -> Self {
        Self {
            asset,
            accumulated_fees: 0,
            total_lp_distributed: 0,
            total_treasury_distributed: 0,
        }
    }
}

/// Historical volume tracking to calculate volume delta
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct VolumeHistory {
    pub previous_period_volume: u64,
    pub current_period_volume: u64,
    pub last_updated: u64, // timestamp when period was last rotated
}

impl VolumeHistory {
    fn new() -> Self {
        Self {
            previous_period_volume: 0,
            current_period_volume: 0,
            last_updated: 0,
        }
    }
}

/// Dynamic fee configuration and current state
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct DynamicFeeState {
    pub min_fee_bps: u32,  // 5 = 0.05%
    pub max_fee_bps: u32,  // 30 = 0.30%
    pub current_fee_bps: u32,
    pub period_seconds: u64, // how often to recalculate (default: 3600 = 1 hour)
}

impl DynamicFeeState {
    fn new() -> Self {
        Self {
            min_fee_bps: 5,    // 0.05%
            max_fee_bps: 30,   // 0.30%
            current_fee_bps: 5, // start at minimum
            period_seconds: 3600, // 1 hour recalculation period
        }
    }
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
    // Reject dust deposits that fall below the minimum transfer threshold.
    crate::validation::dust::check_min_transfer(collected)?;
    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;
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
        .ok_or(ContractError::MathOverflow)?;
    pool.variable_pool = pool
        .variable_pool
        .checked_add(variable_fee)
        .ok_or(ContractError::MathOverflow)?;
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

/// Update volume history and recalculate dynamic fee if period has elapsed
pub fn update_volume_and_adjust_fee(env: &Env, asset: AssetId, trade_volume: u64) -> Result<u32, ContractError> {
    let volume_key = FeesStorageKey::VolumeHistory(asset.clone());
    let fee_key = FeesStorageKey::DynamicFee(asset.clone());
    
    let mut volume_history: VolumeHistory = env.storage()
        .instance()
        .get(&volume_key)
        .unwrap_or(VolumeHistory::new());
    
    let mut dynamic_fee: DynamicFeeState = env.storage()
        .instance()
        .get(&fee_key)
        .unwrap_or(DynamicFeeState::new());
    
    let current_timestamp = env.ledger().timestamp();
    
    // Check if we need to rotate to a new period
    if current_timestamp >= volume_history.last_updated + dynamic_fee.period_seconds {
        // Move current volume to previous, reset current
        volume_history.previous_period_volume = volume_history.current_period_volume;
        volume_history.current_period_volume = trade_volume;
        volume_history.last_updated = current_timestamp;
        
        // Calculate volume delta and adjust fee
        let new_fee = calculate_dynamic_fee(&volume_history, &dynamic_fee)?;
        dynamic_fee.current_fee_bps = new_fee;
    } else {
        // Still in the same period, just add to current volume
        volume_history.current_period_volume = volume_history.current_period_volume
            .checked_add(trade_volume)
            .ok_or(ContractError::MathOverflow)?;
    }
    
    // Save updated state
    env.storage().instance().set(&volume_key, &volume_history);
    env.storage().instance().set(&fee_key, &dynamic_fee);
    
    Ok(dynamic_fee.current_fee_bps)
}

/// Calculate volume delta between periods and adjust fee within bounds
fn calculate_dynamic_fee(volume_history: &VolumeHistory, dynamic_fee: &DynamicFeeState) -> Result<u32, ContractError> {
    // If no previous volume, keep current fee
    if volume_history.previous_period_volume == 0 {
        return Ok(dynamic_fee.current_fee_bps);
    }
    
    // Calculate volume change ratio (current / previous)
    let volume_delta = volume_history.current_period_volume as f64 / volume_history.previous_period_volume as f64;
    
    // Adjust fee based on volume changes:
    // - Volume spiked > 50%: increase fee to reduce congestion
    // - Volume dropped > 30%: decrease fee to attract more trading
    let new_fee_bps = if volume_delta > 1.5 {
        // Volume increased significantly - raise fee
        dynamic_fee.current_fee_bps.saturating_add(5)
    } else if volume_delta < 0.7 {
        // Volume decreased significantly - lower fee
        dynamic_fee.current_fee_bps.saturating_sub(5)
    } else {
        // No significant change - keep current fee
        dynamic_fee.current_fee_bps
    };
    
    // Clamp fee to within allowed range [0.05%, 0.30%] = [5bps, 30bps]
    Ok(new_fee_bps.clamp(dynamic_fee.min_fee_bps, dynamic_fee.max_fee_bps))
}

/// Get the current dynamic fee for an asset
pub fn get_current_dynamic_fee(env: &Env, asset: AssetId) -> u32 {
    let fee_key = FeesStorageKey::DynamicFee(asset);
    let dynamic_fee: DynamicFeeState = env.storage()
        .instance()
        .get(&fee_key)
        .unwrap_or(DynamicFeeState::new());
    dynamic_fee.current_fee_bps
}

/// Calculate and deduct dynamic fee from a trade amount
pub fn calculate_and_deduct_fee(amount: u128, fee_bps: u32) -> Result<(u128, u128), ContractError> {
    // Fee is calculated as (amount * fee_bps) / 10000 (since bps is 1/100th of a percent)
    let fee_amount = amount
        .checked_mul(fee_bps as u128)
        .ok_or(ContractError::Overflow)?
        .checked_div(10000)
        .ok_or(ContractError::DivisionByZero)?;
    
    let amount_after_fees = amount
        .checked_sub(fee_amount)
        .ok_or(ContractError::MathOverflow)?;
    
    Ok((amount_after_fees, fee_amount))
}

/// Admin function to update dynamic fee configuration
pub fn set_dynamic_fee_config(
    env: &Env,
    caller: &Address,
    asset: AssetId,
    min_fee_bps: u32,
    max_fee_bps: u32,
    period_seconds: u64,
) -> Result<(), ContractError> {
    caller.require_auth();
    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;
    if data.admin != *caller {
        return Err(ContractError::NotAdmin);
    }
    
    // Validate bounds
    if min_fee_bps < 5 || max_fee_bps > 30 || min_fee_bps >= max_fee_bps {
        return Err(ContractError::InvalidVarianceConfig);
    }
    if period_seconds < 300 { // Minimum 5 minutes to prevent excessive recalculations
        return Err(ContractError::InvalidVarianceConfig);
    }
    
    let fee_key = FeesStorageKey::DynamicFee(asset);
    let mut dynamic_fee: DynamicFeeState = env.storage()
        .instance()
        .get(&fee_key)
        .unwrap_or(DynamicFeeState::new());
    
    dynamic_fee.min_fee_bps = min_fee_bps;
    dynamic_fee.max_fee_bps = max_fee_bps;
    dynamic_fee.period_seconds = period_seconds;
    
    env.storage().instance().set(&fee_key, &dynamic_fee);
    
    Ok(())
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
    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;
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
                .ok_or(ContractError::MathOverflow)
        })?;

    let mut profiles = Vec::new(env);
    if total_weight == 0 || relayer_weights.len() == 0 {
        return Ok(profiles);
    }

    let pool_profile = (variable_pool as i128)
        .checked_mul(STANDARD_FIXED_POINT_SCALE)
        .ok_or(ContractError::MathOverflow)?;
    let interior_pool_profile = pool_profile
        .checked_mul(INTERIOR_FEE_PRECISION_SCALE)
        .ok_or(ContractError::MathOverflow)?;

    let last_index = relayer_weights.len() - 1;
    let mut assigned_profile = 0_i128;

    for index in 0..relayer_weights.len() {
        let profile = if index == last_index {
            pool_profile
                .checked_sub(assigned_profile)
                .ok_or(ContractError::MathOverflow)?
        } else {
            let weight = relayer_weights
                .get(index)
                .ok_or(ContractError::MathOverflow)? as i128;
            let interior_share = interior_pool_profile
                .checked_mul(weight)
                .ok_or(ContractError::MathOverflow)?
                .checked_div(total_weight)
                .ok_or(ContractError::DivisionByZero)?;
            interior_share
                .checked_div(INTERIOR_FEE_PRECISION_SCALE)
                .ok_or(ContractError::DivisionByZero)?
        };

        assigned_profile = assigned_profile
            .checked_add(profile)
            .ok_or(ContractError::MathOverflow)?;
        profiles.push_back(profile.try_into().map_err(|_| ContractError::MathOverflow)?);
    }

    Ok(profiles)
}

// ---------------------------------------------------------------------------
// Flash Loan Fee Distribution Handlers (#764)
// ---------------------------------------------------------------------------

/// Record flash loan fee revenue for a given asset.
pub fn record_flash_fee(env: &Env, asset: AssetId, amount: u64) -> Result<u64, ContractError> {
    if amount == 0 {
        return Ok(0);
    }
    let key = FeesStorageKey::FlashLoanPool(asset);
    let mut pool: FlashLoanFeePool = env
        .storage()
        .instance()
        .get(&key)
        .unwrap_or_else(|| FlashLoanFeePool::new(asset));

    pool.accumulated_fees = pool
        .accumulated_fees
        .checked_add(amount)
        .ok_or(ContractError::Overflow)?;

    env.storage().instance().set(&key, &pool);
    Ok(pool.accumulated_fees)
}

/// Retrieve the current flash loan fee pool for an asset.
pub fn get_flash_fee_pool(env: &Env, asset: AssetId) -> FlashLoanFeePool {
    let key = FeesStorageKey::FlashLoanPool(asset);
    env.storage()
        .instance()
        .get(&key)
        .unwrap_or_else(|| FlashLoanFeePool::new(asset))
}

/// Set the LP reward pool destination address for flash fee distributions.
pub fn set_lp_reward_pool(env: &Env, admin: &Address, lp_reward_pool: Address) -> Result<(), ContractError> {
    admin.require_auth();
    let data: crate::ContractData = env
        .storage()
        .instance()
        .get(&crate::DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;
    if data.admin != *admin {
        return Err(ContractError::NotAdmin);
    }
    env.storage().instance().set(&crate::LP_REWARD_POOL_KEY, &lp_reward_pool);
    Ok(())
}

/// Get the LP reward pool address (falls back to DAO treasury if not configured).
pub fn get_lp_reward_pool(env: &Env) -> Result<Address, ContractError> {
    if let Some(pool) = env.storage().instance().get::<_, Address>(&crate::LP_REWARD_POOL_KEY) {
        Ok(pool)
    } else {
        env.storage()
            .instance()
            .get::<_, Address>(&crate::TREASURY_KEY)
            .ok_or(ContractError::NotInitialized)
    }
}

/// Distribute accumulated flash loan service fees: 50% to LP reward pool and 50% to DAO treasury.
/// Emits `FlashLoanFeesDistributed` event with token breakdown.
pub fn distribute_flash_fees(
    env: &Env,
    caller: &Address,
    asset: AssetId,
) -> Result<(u64, u64), ContractError> {
    caller.require_auth();
    let key = FeesStorageKey::FlashLoanPool(asset);
    let mut pool: FlashLoanFeePool = env
        .storage()
        .instance()
        .get(&key)
        .unwrap_or_else(|| FlashLoanFeePool::new(asset));

    if pool.accumulated_fees == 0 {
        return Ok((0, 0));
    }

    let total = pool.accumulated_fees;
    let lp_share = total / 2;
    let treasury_share = total - lp_share;

    let treasury: Address = env
        .storage()
        .instance()
        .get(&crate::TREASURY_KEY)
        .ok_or(ContractError::NotInitialized)?;

    let lp_reward_pool: Address = env
        .storage()
        .instance()
        .get::<_, Address>(&crate::LP_REWARD_POOL_KEY)
        .unwrap_or_else(|| treasury.clone());

    pool.accumulated_fees = 0;
    pool.total_lp_distributed = pool
        .total_lp_distributed
        .checked_add(lp_share)
        .ok_or(ContractError::Overflow)?;
    pool.total_treasury_distributed = pool
        .total_treasury_distributed
        .checked_add(treasury_share)
        .ok_or(ContractError::Overflow)?;

    env.storage().instance().set(&key, &pool);

    // Emit FlashLoanFeesDistributed event
    crate::events::publish_flash_fees_distributed(
        env,
        crate::events::FlashLoanFeesDistributedEvent {
            asset,
            total_amount: total,
            lp_share,
            treasury_share,
            lp_reward_pool: lp_reward_pool.clone(),
            treasury: treasury.clone(),
        },
    );

    Ok((lp_share, treasury_share))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TimeLockedUpgradeContract, TimeLockedUpgradeContractClient};
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
    fn test_flash_loan_fee_accumulation_and_distribution() {
        let (env, client, admin, _) = setup();
        let asset: AssetId = 3897123275;
        let lp_pool = Address::generate(&env);

        // Set LP reward pool address
        client.set_lp_reward_pool(&admin, &lp_pool);

        // Record flash loan revenue
        let acc = client.record_flash_fee(&asset, &1000u64);
        assert_eq!(acc, 1000u64);

        let pool_status = client.get_flash_fee_pool(&asset);
        assert_eq!(pool_status.accumulated_fees, 1000u64);

        // Distribute fees (50% to LP reward pool, 50% to DAO treasury)
        let (lp_share, treasury_share) = client.distribute_flash_fees(&admin, &asset);
        assert_eq!(lp_share, 500u64);
        assert_eq!(treasury_share, 500u64);

        let pool_after = client.get_flash_fee_pool(&asset);
        assert_eq!(pool_after.accumulated_fees, 0u64);
        assert_eq!(pool_after.total_lp_distributed, 500u64);
        assert_eq!(pool_after.total_treasury_distributed, 500u64);
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