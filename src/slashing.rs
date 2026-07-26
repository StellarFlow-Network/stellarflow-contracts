use crate::ContractError;
use soroban_sdk::{contracttype, symbol_short, Address, Env, Map, Symbol, Vec};

// ── Issue #525: Multi-tier escrow penalties for repetitive ingestion drops ──
//
// Maintains a progressive penalty tracking matrix per (validator, asset feed)
// pair. Occasional connectivity blips incur a base bond deduction, while
// repeated outages inside a rolling 100-ledger window scale deductions
// exponentially to discourage persistent feed negligence.

/// Rolling ledger window for tracking repeated ingestion dropouts.
pub const ROLLING_FAULT_WINDOW_LEDGERS: u32 = 100;

/// Maximum exponential multiplier (2^10) to prevent unbounded bond seizure.
pub const MAX_PENALTY_MULTIPLIER: u64 = 1_024;

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct TrackingFaultHistory {
    /// Ledger sequences when ingestion dropouts were recorded.
    pub fault_ledgers: Vec<u32>,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum SlashingStorageKey {
    FaultHistory(Address, Symbol),
}

/// Result of applying a progressive escrow bond deduction.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct IngestionPenaltyResult {
    pub validator: Address,
    pub asset: Symbol,
    pub fault_count: u32,
    pub penalty_multiplier: u64,
    pub bond_deduction: u64,
    pub remaining_stake: u64,
}

/// Exponential multiplier: 2^(fault_count - 1), minimum 1, capped at 1024.
pub fn get_penalty_multiplier(fault_count: u32) -> u64 {
    if fault_count == 0 {
        return 1;
    }

    let shift = fault_count.saturating_sub(1).min(10);
    let multiplier = 1u64.checked_shl(shift).unwrap_or(MAX_PENALTY_MULTIPLIER);
    multiplier.min(MAX_PENALTY_MULTIPLIER)
}

/// Compute the structural bond deduction for a fault count and base bond amount.
pub fn calculate_bond_deduction(base_bond: u64, fault_count: u32) -> Result<u64, ContractError> {
    if base_bond == 0 {
        return Err(ContractError::InvalidStakeAmount);
    }

    let multiplier = get_penalty_multiplier(fault_count);
    base_bond
        .checked_mul(multiplier)
        .ok_or(ContractError::Overflow)
}

/// Count faults whose ledger sequence falls within the rolling window.
pub fn count_faults_in_window(history: &TrackingFaultHistory, current_ledger: u32) -> u32 {
    let window_start = current_ledger.saturating_sub(ROLLING_FAULT_WINDOW_LEDGERS);
    let mut count = 0u32;

    for i in 0..history.fault_ledgers.len() {
        let ledger = history.fault_ledgers.get(i).unwrap_or(0);
        if ledger >= window_start && ledger <= current_ledger {
            count = count.saturating_add(1);
        }
    }

    count
}

/// Drop fault entries that fell outside the rolling ledger window.
pub fn prune_fault_history(env: &Env, history: &mut TrackingFaultHistory, current_ledger: u32) {
    let window_start = current_ledger.saturating_sub(ROLLING_FAULT_WINDOW_LEDGERS);
    let mut retained = Vec::new(env);

    for i in 0..history.fault_ledgers.len() {
        let ledger = history.fault_ledgers.get(i).unwrap_or(0);
        if ledger >= window_start && ledger <= current_ledger {
            retained.push_back(ledger);
        }
    }

    history.fault_ledgers = retained;
}

fn load_fault_history(env: &Env, validator: &Address, asset: &Symbol) -> TrackingFaultHistory {
    env.storage()
        .persistent()
        .get(&SlashingStorageKey::FaultHistory(validator.clone(), asset.clone()))
        .unwrap_or(TrackingFaultHistory {
            fault_ledgers: Vec::new(env),
        })
}

fn store_fault_history(env: &Env, validator: &Address, asset: &Symbol, history: &TrackingFaultHistory) {
    env.storage().persistent().set(
        &SlashingStorageKey::FaultHistory(validator.clone(), asset.clone()),
        history,
    );
}

/// Record an ingestion dropout and return the active fault count in the window.
pub fn record_tracking_fault(
    env: &Env,
    validator: &Address,
    asset: &Symbol,
) -> Result<u32, ContractError> {
    let current_ledger = env.ledger().sequence();
    let mut history = load_fault_history(env, validator, asset);

    prune_fault_history(env, &mut history, current_ledger);
    history.fault_ledgers.push_back(current_ledger);
    store_fault_history(env, validator, asset, &history);

    Ok(count_faults_in_window(&history, current_ledger))
}

/// Read the active fault count without recording a new dropout.
pub fn get_fault_count_in_window(env: &Env, validator: &Address, asset: &Symbol) -> u32 {
    let current_ledger = env.ledger().sequence();
    let mut history = load_fault_history(env, validator, asset);
    prune_fault_history(env, &mut history, current_ledger);
    count_faults_in_window(&history, current_ledger)
}

/// Deduct the exponentially scaled bond from validator stake registries.
pub fn apply_escrow_penalty(
    env: &Env,
    validator: &Address,
    asset: &Symbol,
    base_bond: u64,
    fault_count: u32,
    stake_registry_key: &Symbol,
    total_staked_key: &Symbol,
    feed_stake_key: &crate::StakingStorageKey,
) -> Result<IngestionPenaltyResult, ContractError> {
    let bond_deduction = calculate_bond_deduction(base_bond, fault_count)?;
    let penalty_multiplier = get_penalty_multiplier(fault_count);

    let mut stakes: Map<Address, u64> = env
        .storage()
        .instance()
        .get(stake_registry_key)
        .unwrap_or_else(|| Map::new(env));

    let node_total = stakes.get(validator.clone()).unwrap_or(0);
    if node_total == 0 {
        return Err(ContractError::InsufficientBondForPenalty);
    }

    let actual_deduction = bond_deduction.min(node_total);
    let remaining_stake = node_total - actual_deduction;

    if remaining_stake == 0 {
        stakes.remove(validator.clone());
    } else {
        stakes.set(validator.clone(), remaining_stake);
    }

    let total: u64 = env.storage().instance().get(total_staked_key).unwrap_or(0);
    let new_total = total.saturating_sub(actual_deduction);

    env.storage().instance().set(stake_registry_key, &stakes);
    env.storage().instance().set(total_staked_key, &new_total);

    if let Some(mut feed_stake) = env
        .storage()
        .persistent()
        .get::<_, crate::storage::FeedStakeValue>(feed_stake_key)
    {
        feed_stake.amount = feed_stake.amount.saturating_sub(actual_deduction);
        if feed_stake.amount == 0 {
            env.storage().persistent().remove(feed_stake_key);
        } else {
            env.storage().persistent().set(feed_stake_key, &feed_stake);
        }
    }

    Ok(IngestionPenaltyResult {
        validator: validator.clone(),
        asset: asset.clone(),
        fault_count,
        penalty_multiplier,
        bond_deduction: actual_deduction,
        remaining_stake,
    })
}

/// Storage key for tracking slashed stakes per node.
pub(crate) const SLASHED_STAKES_KEY: Symbol = symbol_short!("SLASHED");

/// Minimum deviation threshold before any slashing applies (in basis points).
/// Deviations below this threshold are considered acceptable noise.
pub const MIN_DEVIATION_THRESHOLD_BPS: u32 = 50; // 0.5%

/// Maximum deviation that can be penalized (in basis points).
/// Deviations above this are capped at the maximum penalty tier.
pub const MAX_DEVIATION_BPS: u32 = 10_000; // 100%

/// Slashing penalty tiers based on deviation from consensus median.
/// Each tier represents a percentage of the validator's stake to be burned.
#[contracttype]
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum SlashingTier {
    /// No penalty - deviation within acceptable noise threshold.
    None = 0,
    /// Minor deviation - burn 1% of stake.
    Minor = 1,
    /// Moderate deviation - burn 5% of stake.
    Moderate = 5,
    /// Significant deviation - burn 15% of stake.
    Significant = 15,
    /// Severe deviation - burn 30% of stake.
    Severe = 30,
    /// Critical deviation - burn 50% of stake.
    Critical = 50,
    /// Extreme deviation - burn 100% of stake (total slash).
    Extreme = 100,
}

/// Calculate the proportional slashing penalty based on price deviation from consensus median.
///
/// Uses a multi-tiered burn scale that escalates penalties proportionally based on how far
/// a node's submitted price drifted from the true consensus median.
///
/// # Arguments
/// * `submitted_price` - The price submitted by the validator
/// * `consensus_median` - The consensus median price from all validators
/// * `stake_amount` - The validator's current staked amount
///
/// # Returns
/// * `Ok(burn_amount)` - The amount of stake to burn based on deviation tier
/// * `Err(ContractError)` - If calculation fails (e.g., division by zero)
pub fn calculate_slashing_penalty(
    submitted_price: u64,
    consensus_median: u64,
    stake_amount: u64,
) -> Result<u64, ContractError> {
    if consensus_median == 0 {
        return Err(ContractError::DivisionByZero);
    }

    let deviation_bps = calculate_deviation_bps(submitted_price, consensus_median);
    let tier = determine_slashing_tier(deviation_bps);
    let burn_percentage = tier as u64;

    let burn_amount = stake_amount
        .checked_mul(burn_percentage)
        .ok_or(ContractError::Overflow)?
        .checked_div(100)
        .ok_or(ContractError::DivisionByZero)?;

    Ok(burn_amount)
}

/// Calculate the deviation between submitted price and consensus median in basis points.
///
/// Returns the absolute deviation as a percentage in basis points (1 BPS = 0.01%).
fn calculate_deviation_bps(submitted_price: u64, consensus_median: u64) -> u32 {
    if consensus_median == 0 {
        return MAX_DEVIATION_BPS;
    }

    let diff = if submitted_price > consensus_median {
        submitted_price.saturating_sub(consensus_median)
    } else {
        consensus_median.saturating_sub(submitted_price)
    };

    // Calculate deviation as percentage in basis points
    // (diff / median) * 10_000
    let deviation = (diff as u128)
        .checked_mul(10_000)
        .unwrap_or(u128::MAX)
        .checked_div(consensus_median as u128)
        .unwrap_or(u128::MAX);

    deviation.min(MAX_DEVIATION_BPS as u128) as u32
}

/// Determine the slashing tier based on deviation in basis points.
///
/// Uses a sliding multi-tiered scale:
/// - 0-50 BPS (0-0.5%): No penalty
/// - 50-200 BPS (0.5-2%): Minor (1% burn)
/// - 200-500 BPS (2-5%): Moderate (5% burn)
/// - 500-1000 BPS (5-10%): Significant (15% burn)
/// - 1000-2500 BPS (10-25%): Severe (30% burn)
/// - 2500-5000 BPS (25-50%): Critical (50% burn)
/// - >5000 BPS (>50%): Extreme (100% burn)
fn determine_slashing_tier(deviation_bps: u32) -> SlashingTier {
    if deviation_bps < MIN_DEVIATION_THRESHOLD_BPS {
        SlashingTier::None
    } else if deviation_bps < 200 {
        SlashingTier::Minor
    } else if deviation_bps < 500 {
        SlashingTier::Moderate
    } else if deviation_bps < 1_000 {
        SlashingTier::Significant
    } else if deviation_bps < 2_500 {
        SlashingTier::Severe
    } else if deviation_bps < 5_000 {
        SlashingTier::Critical
    } else {
        SlashingTier::Extreme
    }
}

/// Apply a slashing penalty to a validator's stake.
///
/// Deducts the calculated burn amount from the validator's stake and records
/// the slashing event in persistent storage for audit purposes.
///
/// # Arguments
/// * `env` - The Soroban environment
/// * `node` - The address of the validator being slashed
/// * `burn_amount` - The amount of stake to burn
///
/// # Returns
/// * `Ok(())` - If slashing was successful
/// * `Err(ContractError)` - If the operation fails
pub fn apply_slashing_penalty(
    env: &Env,
    node: Address,
    burn_amount: u64,
) -> Result<(), ContractError> {
    // Record the slashing event for audit trail
    let mut slashed_stakes: Map<Address, u64> = env
        .storage()
        .instance()
        .get(&SLASHED_STAKES_KEY)
        .unwrap_or_else(|| Map::new(env));

    let total_slashed = slashed_stakes
        .get(node.clone())
        .unwrap_or(0)
        .checked_add(burn_amount)
        .ok_or(ContractError::Overflow)?;

    slashed_stakes.set(node, total_slashed);
    env.storage().instance().set(&SLASHED_STAKES_KEY, &slashed_stakes);

    Ok(())
}

/// Get the total amount slashed for a specific validator.
///
/// Returns the cumulative amount of stake that has been burned for the given node.
pub fn get_slashed_amount(env: &Env, node: Address) -> u64 {
    let slashed_stakes: Map<Address, u64> = env
        .storage()
        .instance()
        .get(&SLASHED_STAKES_KEY)
        .unwrap_or_else(|| Map::new(env));

    slashed_stakes.get(node).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_deviation_bps_no_deviation() {
        assert_eq!(calculate_deviation_bps(1000, 1000), 0);
    }

    #[test]
    fn test_calculate_deviation_bps_small_deviation() {
        // 1% deviation = 100 BPS
        assert_eq!(calculate_deviation_bps(1010, 1000), 100);
    }

    #[test]
    fn test_calculate_deviation_bps_large_deviation() {
        // 50% deviation = 5000 BPS
        assert_eq!(calculate_deviation_bps(1500, 1000), 5000);
    }

    #[test]
    fn test_calculate_deviation_bps_extreme_deviation() {
        // 100% deviation = 10000 BPS (capped)
        assert_eq!(calculate_deviation_bps(2000, 1000), 10_000);
    }

    #[test]
    fn test_determine_slashing_tier_none() {
        assert_eq!(determine_slashing_tier(30), SlashingTier::None);
    }

    #[test]
    fn test_determine_slashing_tier_minor() {
        assert_eq!(determine_slashing_tier(100), SlashingTier::Minor);
    }

    #[test]
    fn test_determine_slashing_tier_moderate() {
        assert_eq!(determine_slashing_tier(300), SlashingTier::Moderate);
    }

    #[test]
    fn test_determine_slashing_tier_significant() {
        assert_eq!(determine_slashing_tier(750), SlashingTier::Significant);
    }

    #[test]
    fn test_determine_slashing_tier_severe() {
        assert_eq!(determine_slashing_tier(1500), SlashingTier::Severe);
    }

    #[test]
    fn test_determine_slashing_tier_critical() {
        assert_eq!(determine_slashing_tier(3000), SlashingTier::Critical);
    }

    #[test]
    fn test_determine_slashing_tier_extreme() {
        assert_eq!(determine_slashing_tier(6000), SlashingTier::Extreme);
    }

    #[test]
    fn test_calculate_slashing_penalty_no_deviation() {
        let burn = calculate_slashing_penalty(1000, 1000, 1000).unwrap();
        assert_eq!(burn, 0);
    }

    #[test]
    fn test_calculate_slashing_penalty_minor() {
        // 1% deviation -> Minor tier -> 1% burn
        let burn = calculate_slashing_penalty(1010, 1000, 1000).unwrap();
        assert_eq!(burn, 10);
    }

    #[test]
    fn test_calculate_slashing_penalty_moderate() {
        // 3% deviation -> Moderate tier -> 5% burn
        let burn = calculate_slashing_penalty(1030, 1000, 1000).unwrap();
        assert_eq!(burn, 50);
    }

    #[test]
    fn test_calculate_slashing_penalty_extreme() {
        // 100% deviation -> Extreme tier -> 100% burn
        let burn = calculate_slashing_penalty(2000, 1000, 1000).unwrap();
        assert_eq!(burn, 1000);
    }

    #[test]
    fn test_calculate_slashing_penalty_zero_median() {
        let result = calculate_slashing_penalty(1000, 0, 1000);
        assert_eq!(result, Err(ContractError::DivisionByZero));
    }

    #[test]
    fn test_calculate_slashing_penalty_overflow() {
        let result = calculate_slashing_penalty(u64::MAX, 1, u64::MAX);
        assert_eq!(result, Err(ContractError::Overflow));
    }
}

#[cfg(test)]
mod escrow_penalty_tests {
    use super::*;
    use soroban_sdk::Env;

    fn sample_history(env: &Env, ledgers: &[u32]) -> TrackingFaultHistory {
        let mut fault_ledgers = Vec::new(env);
        for ledger in ledgers {
            fault_ledgers.push_back(*ledger);
        }
        TrackingFaultHistory { fault_ledgers }
    }

    #[test]
    fn first_outage_uses_base_multiplier() {
        assert_eq!(get_penalty_multiplier(1), 1);
        assert_eq!(calculate_bond_deduction(500, 1).unwrap(), 500);
    }

    #[test]
    fn repeated_outages_scale_exponentially() {
        assert_eq!(get_penalty_multiplier(2), 2);
        assert_eq!(get_penalty_multiplier(3), 4);
        assert_eq!(get_penalty_multiplier(4), 8);
        assert_eq!(calculate_bond_deduction(100, 4).unwrap(), 800);
    }

    #[test]
    fn multiplier_is_capped() {
        assert_eq!(get_penalty_multiplier(20), MAX_PENALTY_MULTIPLIER);
    }

    #[test]
    fn rolling_window_excludes_old_faults() {
        let env = Env::default();
        let history = sample_history(&env, &[50, 80, 150, 200]);
        assert_eq!(count_faults_in_window(&history, 200), 2);
        assert_eq!(count_faults_in_window(&history, 150), 3);
    }

    #[test]
    fn prune_drops_faults_outside_window() {
        let env = Env::default();
        let mut history = sample_history(&env, &[10, 50, 120, 200]);
        prune_fault_history(&env, &mut history, 200);
        assert_eq!(history.fault_ledgers.len(), 2);
        assert_eq!(history.fault_ledgers.get(0).unwrap(), 120);
    }

    #[test]
    fn apply_penalty_scales_with_repeat_outages() {
        use soroban_sdk::{contract, contractimpl, symbol_short, testutils::Address as _, Map};
        use crate::StakingStorageKey;

        #[contract]
        struct SlashHarness;

        #[contractimpl]
        impl SlashHarness {}

        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, SlashHarness);
        let validator = Address::generate(&env);
        let asset = symbol_short!("NGN");
        let stake_key = symbol_short!("STAKES");
        let total_key = symbol_short!("TOTAL");

        env.as_contract(&contract_id, || {
            let mut stakes = Map::new(&env);
            stakes.set(validator.clone(), 10_000u64);
            env.storage().instance().set(&stake_key, &stakes);
            env.storage().instance().set(&total_key, &10_000u64);

            record_tracking_fault(&env, &validator, &asset).unwrap();
            let first = apply_escrow_penalty(
                &env,
                &validator,
                &asset,
                100,
                1,
                &stake_key,
                &total_key,
                &StakingStorageKey::FeedStake(validator.clone(), crate::symbol_to_asset_id(&asset)),
            )
            .unwrap();
            assert_eq!(first.bond_deduction, 100);
            assert_eq!(first.penalty_multiplier, 1);

            record_tracking_fault(&env, &validator, &asset).unwrap();
            let second = apply_escrow_penalty(
                &env,
                &validator,
                &asset,
                100,
                2,
                &stake_key,
                &total_key,
                &StakingStorageKey::FeedStake(validator.clone(), crate::symbol_to_asset_id(&asset)),
            )
            .unwrap();
            assert_eq!(second.bond_deduction, 200);
            assert_eq!(second.penalty_multiplier, 2);

            record_tracking_fault(&env, &validator, &asset).unwrap();
            let third = apply_escrow_penalty(
                &env,
                &validator,
                &asset,
                100,
                3,
                &stake_key,
                &total_key,
                &StakingStorageKey::FeedStake(validator.clone(), crate::symbol_to_asset_id(&asset)),
            )
            .unwrap();
            assert_eq!(third.bond_deduction, 400);
            assert_eq!(third.penalty_multiplier, 4);
        });
    }

    #[test]
    fn faults_outside_window_do_not_increase_multiplier() {
        use soroban_sdk::{contract, contractimpl, symbol_short, testutils::Address as _};
        use soroban_sdk::testutils::Ledger;

        #[contract]
        struct WindowHarness;

        #[contractimpl]
        impl WindowHarness {}

        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, WindowHarness);
        let validator = Address::generate(&env);
        let asset = symbol_short!("KES");

        env.as_contract(&contract_id, || {
            let mut history = TrackingFaultHistory {
                fault_ledgers: Vec::new(&env),
            };
            history.fault_ledgers.push_back(10);
            env.storage().persistent().set(
                &SlashingStorageKey::FaultHistory(validator.clone(), asset.clone()),
                &history,
            );

            env.ledger().set(soroban_sdk::testutils::LedgerInfo {
                timestamp: env.ledger().timestamp(),
                protocol_version: env.ledger().protocol_version(),
                sequence_number: 200,
                network_id: Default::default(),
                base_reserve: 10,
                min_temp_entry_ttl: 0,
                min_persistent_entry_ttl: 0,
                max_entry_ttl: u32::MAX,
            });
            let count = record_tracking_fault(&env, &validator, &asset).unwrap();
            assert_eq!(count, 1);
            assert_eq!(get_penalty_multiplier(count), 1);
        });
    }
}
