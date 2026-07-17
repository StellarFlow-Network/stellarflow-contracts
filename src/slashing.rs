use crate::ContractError;
use soroban_sdk::{contracttype, symbol_short, Address, Env, Map, Symbol};

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
