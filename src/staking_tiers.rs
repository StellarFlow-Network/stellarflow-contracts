//! Dynamic staking tier assignment (Issue #300).
//!
//! Links validator staking requirements to each currency feed's volume and
//! volatility profile so smaller regional nodes can join low-activity corridors
//! without meeting the collateral bar required for high-volume, high-volatility feeds.

use soroban_sdk::contracttype;

use crate::ContractError;

/// Volume percentile below which a feed is treated as a regional corridor.
pub const VOLUME_LOW_THRESHOLD: u32 = 33;
/// Volume percentile at or above which a feed is considered high-volume.
pub const VOLUME_HIGH_THRESHOLD: u32 = 67;
/// Volatility (basis points) below which a feed is considered stable.
pub const VOLATILITY_LOW_BPS: u32 = 300;
/// Volatility (basis points) at or above which a feed is considered volatile.
pub const VOLATILITY_HIGH_BPS: u32 = 800;

/// Default minimum stake for regional (low volume / low volatility) feeds.
pub const DEFAULT_REGIONAL_MIN_STAKE: u64 = 100;
/// Default minimum stake for standard feeds.
pub const DEFAULT_STANDARD_MIN_STAKE: u64 = 1_000;
/// Default minimum stake for premier (high volume / high volatility) feeds.
pub const DEFAULT_PREMIER_MIN_STAKE: u64 = 10_000;

/// Collateral tier derived from a feed's risk profile.
#[contracttype]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StakingTier {
    /// Low-volume, low-volatility regional corridors.
    Regional = 1,
    /// Typical currency feeds.
    Standard = 2,
    /// High-volume, high-volatility major feeds.
    Premier = 3,
}

/// Per-asset feed characteristics that drive tier assignment.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetFeedMetrics {
    /// Normalised trading-volume score in `[0, 100]`.
    pub volume_score: u32,
    /// Typical price volatility expressed in basis points.
    pub volatility_bps: u32,
}

/// Admin-configurable minimum stake amounts for each tier.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StakingTierConfig {
    pub regional_min_stake: u64,
    pub standard_min_stake: u64,
    pub premier_min_stake: u64,
}

impl Default for StakingTierConfig {
    fn default() -> Self {
        Self {
            regional_min_stake: DEFAULT_REGIONAL_MIN_STAKE,
            standard_min_stake: DEFAULT_STANDARD_MIN_STAKE,
            premier_min_stake: DEFAULT_PREMIER_MIN_STAKE,
        }
    }
}

/// Map cumulative corridor fee volume to a `[0, 100]` volume score.
pub fn derive_volume_score_from_corridor(collected: u64) -> u32 {
    if collected < 1_000_000 {
        10
    } else if collected < 10_000_000 {
        30
    } else if collected < 100_000_000 {
        50
    } else if collected < 1_000_000_000 {
        75
    } else {
        95
    }
}

/// Assign a staking tier from volume and volatility characteristics.
pub fn assign_tier(metrics: &AssetFeedMetrics) -> StakingTier {
    let low_volume = metrics.volume_score < VOLUME_LOW_THRESHOLD;
    let low_volatility = metrics.volatility_bps < VOLATILITY_LOW_BPS;
    let high_volume = metrics.volume_score >= VOLUME_HIGH_THRESHOLD;
    let high_volatility = metrics.volatility_bps >= VOLATILITY_HIGH_BPS;

    if low_volume && low_volatility {
        StakingTier::Regional
    } else if high_volume && high_volatility {
        StakingTier::Premier
    } else {
        StakingTier::Standard
    }
}

/// Resolve the minimum stake required for a tier.
pub fn required_stake_for_tier(tier: StakingTier, config: &StakingTierConfig) -> u64 {
    match tier {
        StakingTier::Regional => config.regional_min_stake,
        StakingTier::Standard => config.standard_min_stake,
        StakingTier::Premier => config.premier_min_stake,
    }
}

/// Validate an admin-supplied tier configuration.
pub fn validate_tier_config(config: &StakingTierConfig) -> Result<(), ContractError> {
    if config.regional_min_stake == 0
        || config.standard_min_stake == 0
        || config.premier_min_stake == 0
    {
        return Err(ContractError::InvalidTierConfig);
    }

    if config.regional_min_stake > config.standard_min_stake
        || config.standard_min_stake > config.premier_min_stake
    {
        return Err(ContractError::InvalidTierConfig);
    }

    Ok(())
}

/// Merge an admin-provided volume score with on-chain corridor activity.
pub fn effective_volume_score(admin_score: u32, corridor_collected: u64) -> u32 {
    let derived = derive_volume_score_from_corridor(corridor_collected);
    admin_score.max(derived).min(100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regional_tier_for_low_volume_low_volatility() {
        let metrics = AssetFeedMetrics {
            volume_score: 20,
            volatility_bps: 100,
        };
        assert_eq!(assign_tier(&metrics), StakingTier::Regional);
    }

    #[test]
    fn premier_tier_for_high_volume_high_volatility() {
        let metrics = AssetFeedMetrics {
            volume_score: 80,
            volatility_bps: 1_000,
        };
        assert_eq!(assign_tier(&metrics), StakingTier::Premier);
    }

    #[test]
    fn standard_tier_for_mixed_profile() {
        let metrics = AssetFeedMetrics {
            volume_score: 80,
            volatility_bps: 200,
        };
        assert_eq!(assign_tier(&metrics), StakingTier::Standard);
    }

    #[test]
    fn corridor_volume_score_increases_with_activity() {
        assert_eq!(derive_volume_score_from_corridor(500_000), 10);
        assert_eq!(derive_volume_score_from_corridor(50_000_000), 50);
        assert_eq!(derive_volume_score_from_corridor(2_000_000_000), 95);
    }

    #[test]
    fn tier_config_must_be_monotonic() {
        let invalid = StakingTierConfig {
            regional_min_stake: 5_000,
            standard_min_stake: 1_000,
            premier_min_stake: 10_000,
        };
        assert_eq!(
            validate_tier_config(&invalid),
            Err(ContractError::InvalidTierConfig)
        );
    }

    #[test]
    fn effective_volume_score_uses_higher_of_admin_and_corridor() {
        assert_eq!(effective_volume_score(20, 2_000_000_000), 95);
        assert_eq!(effective_volume_score(90, 500_000), 90);
    }
}
