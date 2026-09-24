//! Sealed price-variance configuration (Issue #420).
//!
//! All operational variance settings are encapsulated inside a single
//! [`PriceVarianceConfig`] struct stored under one ledger key.  Updates must
//! always supply the **complete** struct so that every storage slot is
//! overwritten atomically, eliminating the memory-alignment mismatches that
//! arise when individual fields are mutated in isolation.
//!
//! # Storage contract
//!
//! - One key: [`PRICE_VARIANCE_CONFIG_KEY`].
//! - One writer: [`set_price_variance_config`] — full-struct replacement only.
//! - One reader: [`get_price_variance_config`] — returns the active config or
//!   the compile-time defaults.
//!
//! Callers must never write individual fields directly to storage; doing so
//! would leave neighbouring slots in an inconsistent state across ledger
//! registers.

use soroban_sdk::{contracttype, symbol_short, Address, Env, Symbol, Vec};

use crate::{ContractData, ContractError, DATA_KEY};

/// Multi-sig threshold governing admin-key rotations.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdminKeySet {
    /// Current administrator addresses authorized to rotate the key set.
    pub signers: Vec<Address>,
    /// Number of approvals required from `signers` to rotate keys.
    pub threshold: u32,
}

// ── Storage key ──────────────────────────────────────────────────────────────

/// Ledger instance-storage key for the sealed variance configuration.
pub(crate) const PRICE_VARIANCE_CONFIG_KEY: Symbol = symbol_short!("PVARCFG");

/// Ledger instance-storage key for the multi-sig admin key set.
pub(crate) const ADMIN_KEY_SET_KEY: Symbol = symbol_short!("ADMINKEYS");

/// Ledger instance-storage key for the liquidity pool fee tier controller.
pub(crate) const FEE_TIER_CONFIG_KEY: Symbol = symbol_short!("FEETIER");

// ── Default thresholds ───────────────────────────────────────────────────────

/// Default maximum spread (in basis points) permitted between two oracle
/// submissions before the pair is considered divergent.
///
/// 200 bps = 2 %.
pub const DEFAULT_MAX_SPREAD_BPS: u32 = 200;

/// Default maximum price deviation (in basis points) that a single submission
/// may exhibit relative to the current weighted-average before it is rejected
/// as an outlier.
///
/// 500 bps = 5 %.
pub const DEFAULT_MAX_DEVIATION_BPS: u32 = 500;

/// Default minimum number of independent oracle submissions required before a
/// consensus price is considered valid and publishable.
pub const DEFAULT_MIN_SUBMISSION_COUNT: u32 = 3;

/// Default maximum age of the oldest accepted submission (in seconds).
/// Submissions older than this threshold are treated as stale.
///
/// 300 s = 5 minutes.
pub const DEFAULT_MAX_SUBMISSION_AGE_SECS: u64 = 300;

/// Upper bound (in basis points) that [`max_spread_bps`] and
/// [`max_deviation_bps`] must not exceed.  Prevents misconfiguration from
/// opening the full 100 % range as an acceptable band.
///
/// 5 000 bps = 50 %.
pub const VARIANCE_BPS_CEILING: u32 = 5_000;

// ── Dynamic liquidity pool fee tiers ─────────────────────────────────────────

/// Minimum allowed pool fee tier in basis points (0.05 %).
pub const MIN_FEE_TIER_BPS: u32 = 5;

/// Maximum allowed pool fee tier in basis points (1.00 %).
pub const MAX_FEE_TIER_BPS: u32 = 100;

/// Default pool fee tier in basis points (0.30 %).
pub const DEFAULT_FEE_TIER_BPS: u32 = 30;

/// LP token holder share of collected fees, in basis points (80 %).
pub const LP_FEE_SHARE_BPS: u32 = 8_000;

/// Protocol treasury vault share of collected fees, in basis points (20 %).
pub const PROTOCOL_FEE_SHARE_BPS: u32 = 2_000;

// ── Sealed configuration struct ──────────────────────────────────────────────

/// Immutable snapshot of all price-variance operational settings.
///
/// This struct is the **single source of truth** for variance parameters.
/// It is written and read as one atomic unit so that all slots in ledger
/// instance storage remain perfectly aligned after every update.
///
/// # Invariants (enforced by [`validate_price_variance_config`])
///
/// - `max_spread_bps` ∈ `[1, VARIANCE_BPS_CEILING]`
/// - `max_deviation_bps` ∈ `[1, VARIANCE_BPS_CEILING]`
/// - `max_spread_bps` ≤ `max_deviation_bps` (spread is always the tighter bound)
/// - `min_submission_count` ≥ 1
/// - `max_submission_age_secs` ≥ 1
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PriceVarianceConfig {
    /// Maximum tolerated spread between two oracle rates, in basis points.
    /// Pairs whose spread exceeds this threshold are flagged as divergent.
    pub max_spread_bps: u32,

    /// Maximum tolerated deviation of a single submission from the running
    /// weighted average, in basis points.  Outliers beyond this bound are
    /// rejected before they influence the consensus price.
    pub max_deviation_bps: u32,

    /// Minimum number of valid, non-stale oracle submissions required to
    /// form a publishable consensus price.
    pub min_submission_count: u32,

    /// Maximum age in seconds of the oldest submission that may still
    /// participate in the consensus round.
    pub max_submission_age_secs: u64,
}

impl Default for PriceVarianceConfig {
    fn default() -> Self {
        Self {
            max_spread_bps: DEFAULT_MAX_SPREAD_BPS,
            max_deviation_bps: DEFAULT_MAX_DEVIATION_BPS,
            min_submission_count: DEFAULT_MIN_SUBMISSION_COUNT,
            max_submission_age_secs: DEFAULT_MAX_SUBMISSION_AGE_SECS,
        }
    }
}

/// Compact byte-packed representation of [`PriceVarianceConfig`] to minimize
/// Soroban instance storage foot-print and rent costs (Issue #747).
///
/// Refactors 4 independent config fields (u32, u32, u32, u64 = 20 raw bytes)
/// into a single 64-bit packed payload (8 bytes):
/// - bits [0..16]   : max_spread_bps (u16)
/// - bits [16..32]  : max_deviation_bps (u16)
/// - bits [32..40]  : min_submission_count (u8)
/// - bits [40..64]  : max_submission_age_secs (24 bits)
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackedPriceVarianceConfig {
    pub packed: u64,
}

impl PriceVarianceConfig {
    pub fn pack(&self) -> PackedPriceVarianceConfig {
        let spread = (self.max_spread_bps.min(0xFFFF) as u64) & 0xFFFF;
        let dev = ((self.max_deviation_bps.min(0xFFFF) as u64) & 0xFFFF) << 16;
        let count = ((self.min_submission_count.min(0xFF) as u64) & 0xFF) << 32;
        let age = ((self.max_submission_age_secs.min(0xFF_FFFF) as u64) & 0xFF_FFFF) << 40;
        PackedPriceVarianceConfig {
            packed: spread | dev | count | age,
        }
    }
}

impl PackedPriceVarianceConfig {
    pub fn unpack(&self) -> PriceVarianceConfig {
        let max_spread_bps = (self.packed & 0xFFFF) as u32;
        let max_deviation_bps = ((self.packed >> 16) & 0xFFFF) as u32;
        let min_submission_count = ((self.packed >> 32) & 0xFF) as u32;
        let max_submission_age_secs = ((self.packed >> 40) & 0xFF_FFFF) as u64;
        PriceVarianceConfig {
            max_spread_bps,
            max_deviation_bps,
            min_submission_count,
            max_submission_age_secs,
        }
    }
}

// ── Validation ───────────────────────────────────────────────────────────────

/// Verify that every field of `cfg` satisfies the struct invariants.
///
/// Returns [`ContractError::InvalidVarianceConfig`] on the first violated
/// constraint so callers receive a clear, unambiguous rejection signal.
pub fn validate_price_variance_config(cfg: &PriceVarianceConfig) -> Result<(), ContractError> {
    // Individual field lower-bound checks.
    if cfg.max_spread_bps == 0
        || cfg.max_deviation_bps == 0
        || cfg.min_submission_count == 0
        || cfg.max_submission_age_secs == 0
    {
        return Err(ContractError::InvalidVarianceConfig);
    }

    // Upper-bound ceiling to prevent a 100 %-wide acceptance window.
    if cfg.max_spread_bps > VARIANCE_BPS_CEILING || cfg.max_deviation_bps > VARIANCE_BPS_CEILING {
        return Err(ContractError::InvalidVarianceConfig);
    }

    // Spread must be no wider than the single-submission deviation cap.
    if cfg.max_spread_bps > cfg.max_deviation_bps {
        return Err(ContractError::InvalidVarianceConfig);
    }

    Ok(())
}
/// Verify that a proposed admin key set satisfies multi-sig sanity rules.
pub fn validate_admin_key_set(keys: &AdminKeySet) -> Result<(), ContractError> {
    if keys.signers.len() == 0 || keys.threshold == 0 || keys.threshold > keys.signers.len() {
        return Err(ContractError::InvalidVarianceConfig);
    }
    if has_duplicate_addresses(&keys.signers) {
        return Err(ContractError::InvalidVarianceConfig);
    }
    Ok(())
}

/// Read the current governing admin key set.
pub fn get_admin_key_set(env: &Env) -> Result<AdminKeySet, ContractError> {
    let stored: Option<AdminKeySet> = env.storage().instance().get(&ADMIN_KEY_SET_KEY);
    if let Some(keys) = stored {
        return Ok(keys);
    }

    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;
    let mut signers = Vec::new(env);
    signers.push_back(data.admin.clone());
    Ok(AdminKeySet { signers, threshold: 1 })
}

/// Rotate the multi-sig admin keys after receiving current-threshold approval.
pub fn rotate_admin_keys(
    env: &Env,
    approving_signers: Vec<Address>,
    new_signers: Vec<Address>,
    new_threshold: u32,
) -> Result<(), ContractError> {
    let current = get_admin_key_set(env)?;
    validate_admin_key_set(&current)?;
    validate_admin_key_set(&AdminKeySet {
        signers: new_signers.clone(),
        threshold: new_threshold,
    })?;

    if has_duplicate_addresses(&approving_signers) || approving_signers.len() < current.threshold {
        return Err(ContractError::NotAdmin);
    }

    for signer in approving_signers.iter() {
        signer.require_auth();
        if !current.signers.iter().any(|member| member == signer) {
            return Err(ContractError::NotAdmin);
        }
    }

    let new_key_set = AdminKeySet {
        signers: new_signers.clone(),
        threshold: new_threshold,
    };
    env.storage().instance().set(&ADMIN_KEY_SET_KEY, &new_key_set);

    let mut data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .

// ── Dynamic liquidity pool fee tier controller ──────────────────────────────

/// Active fee tier configuration for the liquidity pool.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeeTierConfig {
    /// Pool fee tier in basis points (5, 30 or 100).
    pub fee_tier_bps: u32,
}

impl Default for FeeTierConfig {
    fn default() -> Self {
        Self {
            fee_tier_bps: DEFAULT_FEE_TIER_BPS,
        }
    }
}

/// Validate fee tier is one of the allowed governance tiers.
pub fn validate_fee_tier_config(cfg: &FeeTierConfig) -> Result<(), ContractError> {
    if cfg.fee_tier_bps != MIN_FEE_TIER_BPS
        && cfg.fee_tier_bps != DEFAULT_FEE_TIER_BPS
        && cfg.fee_tier_bps != MAX_FEE_TIER_BPS
    {
        return Err(ContractError::InvalidVarianceConfig);
    }
    Ok(())
}

/// Set the active fee tier as the contract admin.
pub fn set_fee_tier_config(
    env: &Env,
    caller: &Address,
    cfg: FeeTierConfig,
) -> Result<(), ContractError> {
    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;
    if data.admin != *caller {
        return Err(ContractError::NotAdmin);
    }
    caller.require_auth();
    validate_fee_tier_config(&cfg)?;
    env.storage().instance().set(&FEE_TIER_CONFIG_KEY, &cfg);
    Ok(())
}

/// Read the active fee tier configuration.
pub fn get_fee_tier_config(env: &Env) -> FeeTierConfig {
    env.storage()
        .instance()
        .get(&FEE_TIER_CONFIG_KEY)
        .unwrap_or_default()
}

/// Apply a governance vote to adjust the fee tier within safety bounds.
pub fn vote_fee_tier_change(
    env: &Env,
    approving_signers: Vec<Address>,
    proposed_fee_tier_bps: u32,
) -> Result<(), ContractError> {
    let keys = get_admin_key_set(env)?;
    validate_admin_key_set(&keys)?;
    if has_duplicate_addresses(&approving_signers) || approving_signers.len() < keys.threshold {
        return Err(ContractError::NotAdmin);
    }
    for signer in approving_signers.iter() {
        signer.require_auth();
        if !keys.signers.iter().any(|member| member == signer) {
            return Err(ContractError::NotAdmin);
        }
    }
    let cfg = FeeTierConfig {
        fee_tier_bps: proposed_fee_tier_bps,
    };
    validate_fee_tier_config(&cfg)?;
    env.storage().instance().set(&FEE_TIER_CONFIG_KEY, &cfg);
    Ok(())
}
/// Verify that a proposed admin key set satisfies multi-sig sanity rules.
pub fn validate_admin_key_set(keys: &AdminKeySet) -> Result<(), ContractError> {
    if keys.signers.len() == 0 || keys.threshold == 0 || keys.threshold > keys.signers.len() {
        return Err(ContractError::InvalidVarianceConfig);
    }
    if has_duplicate_addresses(&keys.signers) {
        return Err(ContractError::InvalidVarianceConfig);
    }
    Ok(())
}

/// Read the current governing admin key set.
pub fn get_admin_key_set(env: &Env) -> Result<AdminKeySet, ContractError> {
    let stored: Option<AdminKeySet> = env.storage().instance().get(&ADMIN_KEY_SET_KEY);
    if let Some(keys) = stored {
        return Ok(keys);
    }

    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;
    let mut signers = Vec::new(env);
    signers.push_back(data.admin.clone());
    Ok(AdminKeySet { signers, threshold: 1 })
}

/// Rotate the multi-sig admin keys after receiving current-threshold approval.
pub fn rotate_admin_keys(
    env: &Env,
    approving_signers: Vec<Address>,
    new_signers: Vec<Address>,
    new_threshold: u32,
) -> Result<(), ContractError> {
    let current = get_admin_key_set(env)?;
    validate_admin_key_set(&current)?;
    validate_admin_key_set(&AdminKeySet {
        signers: new_signers.clone(),
        threshold: new_threshold,
    })?;

    if has_duplicate_addresses(&approving_signers) || approving_signers.len() < current.threshold {
        return Err(ContractError::NotAdmin);
    }

    for signer in approving_signers.iter() {
        signer.require_auth();
        if !current.signers.iter().any(|member| member == signer) {
            return Err(ContractError::NotAdmin);
        }
    }

    let new_key_set = AdminKeySet {
        signers: new_signers.clone(),
        threshold: new_threshold,
    };
    env.storage().instance().set(&ADMIN_KEY_SET_KEY, &new_key_set);

    let mut data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .

// ── Storage accessors ─────────────────────────────────────────────────────────

/// Write the complete variance configuration to instance storage, replacing
/// every field atomically.
///
/// # Errors
///
/// - [`ContractError::NotInitialized`] — contract has not been initialised.
/// - [`ContractError::NotAdmin`] — `caller` is not the current admin.
/// - [`ContractError::InvalidVarianceConfig`] — one or more fields violate the
///   struct invariants (see [`validate_price_variance_config`]).
///
/// # Atomicity guarantee
///
/// The entire [`PriceVarianceConfig`] is serialised as one value and stored
/// under a single key.  There is no code path that touches individual fields
/// separately, so partial-update mismatches across ledger registers cannot
/// occur.
pub fn set_price_variance_config(
    env: &Env,
    caller: &soroban_sdk::Address,
    cfg: PriceVarianceConfig,
) -> Result<(), ContractError> {
    // Auth — only the admin may mutate the variance configuration.
    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;

    if data.admin != *caller {
        return Err(ContractError::NotAdmin);
    }
    caller.require_auth();

    // Validate the complete struct before touching storage.
    validate_price_variance_config(&cfg)?;

    // Full-struct overwrite: the entire config is replaced in one operation.
    env.storage()
        .instance()
        .set(&PRICE_VARIANCE_CONFIG_KEY, &cfg);

    Ok(())
}

/// Read the active variance configuration from instance storage.
///
/// Falls back to [`PriceVarianceConfig::default`] when the config has never
/// been written, so callers never have to handle a missing-key error.
pub fn get_price_variance_config(env: &Env) -> PriceVarianceConfig {
    env.storage()
        .instance()
        .get(&PRICE_VARIANCE_CONFIG_KEY)
        .unwrap_or_default()
}

// ── Adaptive fee configuration (Issue #766) ──────────────────────────────────

/// Ledger storage key for the per-pool sealed adaptive fee configuration.
#[contracttype]
pub enum AdaptiveConfigKey {
    /// Config record keyed by the pool's numeric [`crate::AssetId`].
    Pool(crate::AssetId),
}

/// Default base swap fee in basis points (0.30 %) — the fee charged when
/// short-term volatility is at or below [`DEFAULT_LOW_VOLATILITY_BPS`].
pub const DEFAULT_ADAPTIVE_BASE_FEE_BPS: u32 = 30;

/// Default maximum adaptive fee cap in basis points (1.50 %). Fee is scaled up
/// to this value as short-term volatility reaches the high threshold.
pub const DEFAULT_ADAPTIVE_MAX_FEE_BPS: u32 = 150;

/// Default number of short-term price observations retained in the per-pool
/// historical ring buffer used to compute volatility.
pub const DEFAULT_ADAPTIVE_RING_BUFFER_LEN: u32 = 20;

/// Default minimum time (seconds) between recorded price observations for a
/// pool. Prevents duplicate observations from inflating a single snapshot.
pub const DEFAULT_ADAPTIVE_SAMPLE_INTERVAL_SECS: u64 = 300;

/// Volatility (in basis points) at or below which the adaptive fee rests at
/// its base (no uplift). 200 bps = 2 %.
pub const DEFAULT_ADAPTIVE_LOW_VOLATILITY_BPS: u32 = 200;

/// Volatility (in basis points) at which the adaptive fee reaches its maximum
/// cap. 500 bps = 5 %.
pub const DEFAULT_ADAPTIVE_HIGH_VOLATILITY_BPS: u32 = 500;

/// Half-life (seconds) of the exponential decay applied to the volatility
/// pressure (and therefore the fee) back toward baseline when the pool stops
/// producing fresh, high-variance observations. 3600 s = 1 hour.
pub const DEFAULT_ADAPTIVE_DECAY_HALF_LIFE_SECS: u64 = 3600;

/// Upper bound for `max_fee_bps` (100 %) to keep the adaptive fee an actual
/// fee and not a confiscation.
pub const ADAPTIVE_FEE_BPS_CEILING: u32 = 10_000;

/// Sealed adaptive fee scaling configuration for a pool (Issue #766).
///
/// Mirrors the [`PriceVarianceConfig`] storage contract: written and read as
/// one atomic [`contracttype`] value under [`AdaptiveConfigKey::Pool`] so every
/// ledger register stays aligned. This is the **single source of truth** for
/// the volatility-to-fee curve applied to a pool's swaps.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdaptiveFeeConfig {
    /// Base swap fee in basis points charged at low volatility.
    pub base_fee_bps: u32,
    /// Maximum cap the adaptive fee can scale up to during volatility spikes.
    pub max_fee_bps: u32,
    /// Number of short-term TWAP price observations in the historical buffer.
    pub ring_buffer_len: u32,
    /// Minimum seconds between recorded price observations.
    pub sample_interval_secs: u64,
    /// Volatility threshold (bps) below which no fee uplift is applied.
    pub low_volatility_bps: u32,
    /// Volatility threshold (bps) at which the fee reaches its maximum cap.
    pub high_volatility_bps: u32,
    /// Half-life (seconds) of the exponential fee decay toward baseline.
    pub decay_half_life_secs: u64,
}

impl Default for AdaptiveFeeConfig {
    fn default() -> Self {
        Self {
            base_fee_bps: DEFAULT_ADAPTIVE_BASE_FEE_BPS,
            max_fee_bps: DEFAULT_ADAPTIVE_MAX_FEE_BPS,
            ring_buffer_len: DEFAULT_ADAPTIVE_RING_BUFFER_LEN,
            sample_interval_secs: DEFAULT_ADAPTIVE_SAMPLE_INTERVAL_SECS,
            low_volatility_bps: DEFAULT_ADAPTIVE_LOW_VOLATILITY_BPS,
            high_volatility_bps: DEFAULT_ADAPTIVE_HIGH_VOLATILITY_BPS,
            decay_half_life_secs: DEFAULT_ADAPTIVE_DECAY_HALF_LIFE_SECS,
        }
    }
}

/// Verify that every field of `cfg` satisfies the adaptive fee invariants.
///
/// Returns [`ContractError::InvalidVarianceConfig`] on the first violated
/// constraint, mirroring the existing variance-config validation.
pub fn validate_adaptive_fee_config(cfg: &AdaptiveFeeConfig) -> Result<(), ContractError> {
    if cfg.base_fee_bps == 0
        || cfg.max_fee_bps == 0
        || cfg.ring_buffer_len < 2
        || cfg.sample_interval_secs == 0
        || cfg.low_volatility_bps == 0
        || cfg.high_volatility_bps == 0
        || cfg.decay_half_life_secs == 0
    {
        return Err(ContractError::InvalidVarianceConfig);
    }
    if cfg.max_fee_bps > ADAPTIVE_FEE_BPS_CEILING {
        return Err(ContractError::InvalidVarianceConfig);
    }
    if cfg.base_fee_bps > cfg.max_fee_bps {
        return Err(ContractError::InvalidVarianceConfig);
    }
    if cfg.low_volatility_bps > cfg.high_volatility_bps {
        return Err(ContractError::InvalidVarianceConfig);
    }
    Ok(())
}

/// Write the complete adaptive fee configuration for a pool to instance
/// storage, replacing every field atomically. Admin-only.
pub fn set_adaptive_fee_config(
    env: &Env,
    caller: &soroban_sdk::Address,
    pool: crate::AssetId,
    cfg: AdaptiveFeeConfig,
) -> Result<(), ContractError> {
    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;

    if data.admin != *caller {
        return Err(ContractError::NotAdmin);
    }
    caller.require_auth();

    validate_adaptive_fee_config(&cfg)?;

    let key = AdaptiveConfigKey::Pool(pool);
    env.storage().instance().set(&key, &cfg);
    Ok(())
}

/// Read the active adaptive fee configuration for a pool.
///
/// Returns `None` when the pool has never opted into adaptive fee scaling,
/// signalling callers to fall back to the legacy volume-based dynamic fee.
pub fn get_adaptive_fee_config(env: &Env, pool: crate::AssetId) -> Option<AdaptiveFeeConfig> {
    let key = AdaptiveConfigKey::Pool(pool);
    env.storage().instance().get(&key)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── validate_price_variance_config ────────────────────────────────────────

    #[test]
    fn default_config_is_valid() {
        assert!(validate_price_variance_config(&PriceVarianceConfig::default()).is_ok());
    }

    #[test]
    fn zero_spread_bps_is_rejected() {
        let cfg = PriceVarianceConfig {
            max_spread_bps: 0,
            ..PriceVarianceConfig::default()
        };
        assert_eq!(
            validate_price_variance_config(&cfg),
            Err(ContractError::InvalidVarianceConfig)
        );
    }

    #[test]
    fn zero_deviation_bps_is_rejected() {
        let cfg = PriceVarianceConfig {
            max_deviation_bps: 0,
            ..PriceVarianceConfig::default()
        };
        assert_eq!(
            validate_price_variance_config(&cfg),
            Err(ContractError::InvalidVarianceConfig)
        );
    }

    #[test]
    fn zero_min_submission_count_is_rejected() {
        let cfg = PriceVarianceConfig {
            min_submission_count: 0,
            ..PriceVarianceConfig::default()
        };
        assert_eq!(
            validate_price_variance_config(&cfg),
            Err(ContractError::InvalidVarianceConfig)
        );
    }

    #[test]
    fn zero_max_submission_age_is_rejected() {
        let cfg = PriceVarianceConfig {
            max_submission_age_secs: 0,
            ..PriceVarianceConfig::default()
        };
        assert_eq!(
            validate_price_variance_config(&cfg),
            Err(ContractError::InvalidVarianceConfig)
        );
    }

    #[test]
    fn spread_above_ceiling_is_rejected() {
        let cfg = PriceVarianceConfig {
            max_spread_bps: VARIANCE_BPS_CEILING + 1,
            max_deviation_bps: VARIANCE_BPS_CEILING + 1,
            ..PriceVarianceConfig::default()
        };
        assert_eq!(
            validate_price_variance_config(&cfg),
            Err(ContractError::InvalidVarianceConfig)
        );
    }

    #[test]
    fn deviation_above_ceiling_is_rejected() {
        let cfg = PriceVarianceConfig {
            max_spread_bps: 100,
            max_deviation_bps: VARIANCE_BPS_CEILING + 1,
            ..PriceVarianceConfig::default()
        };
        assert_eq!(
            validate_price_variance_config(&cfg),
            Err(ContractError::InvalidVarianceConfig)
        );
    }

    #[test]
    fn spread_wider_than_deviation_is_rejected() {
        // spread (600) > deviation (400) violates the ordering invariant.
        let cfg = PriceVarianceConfig {
            max_spread_bps: 600,
            max_deviation_bps: 400,
            ..PriceVarianceConfig::default()
        };
        assert_eq!(
            validate_price_variance_config(&cfg),
            Err(ContractError::InvalidVarianceConfig)
        );
    }

    #[test]
    fn spread_equal_to_deviation_is_valid() {
        let cfg = PriceVarianceConfig {
            max_spread_bps: 300,
            max_deviation_bps: 300,
            ..PriceVarianceConfig::default()
        };
        assert!(validate_price_variance_config(&cfg).is_ok());
    }

    #[test]
    fn at_ceiling_boundary_is_valid() {
        let cfg = PriceVarianceConfig {
            max_spread_bps: VARIANCE_BPS_CEILING,
            max_deviation_bps: VARIANCE_BPS_CEILING,
            ..PriceVarianceConfig::default()
        };
        assert!(validate_price_variance_config(&cfg).is_ok());
    }

    // ── get/set round-trip (Soroban mock environment) ─────────────────────────

    #[test]
    fn get_returns_default_before_any_set() {
        use crate::TimeLockedUpgradeContract;
        let env = soroban_sdk::Env::default();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let client = crate::TimeLockedUpgradeContractClient::new(&env, &contract_id);
        let cfg = client.get_price_variance_config();
        assert_eq!(cfg, PriceVarianceConfig::default());
    }

    #[test]
    fn set_and_get_round_trips_full_struct() {
        use crate::TimeLockedUpgradeContract;
        use soroban_sdk::testutils::Address as _;
        use soroban_sdk::Address;

        let env = soroban_sdk::Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let client = crate::TimeLockedUpgradeContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        client.initialize(&admin, &treasury);

        let custom = PriceVarianceConfig {
            max_spread_bps: 150,
            max_deviation_bps: 400,
            min_submission_count: 5,
            max_submission_age_secs: 120,
        };

        client.set_price_variance_config(&admin, &custom);

        let retrieved = client.get_price_variance_config();
        assert_eq!(retrieved, custom);
    }

    #[test]
    fn set_rejects_non_admin_caller() {
        use crate::TimeLockedUpgradeContract;
        use soroban_sdk::testutils::Address as _;
        use soroban_sdk::Address;

        let env = soroban_sdk::Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let client = crate::TimeLockedUpgradeContractClient::new(&env, &contract_id);

        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let admin = Address::generate(&env);
        let intruder = Address::generate(&env);
        let treasury = Address::generate(&env);
        client.initialize(&admin, &treasury);

        let result = client.try_set_price_variance_config(&intruder, &PriceVarianceConfig::default());
        assert_eq!(result, Err(Ok(ContractError::NotAdmin)));
    }

    #[test]
    fn set_rejects_invalid_config() {
        use crate::TimeLockedUpgradeContract;
        use soroban_sdk::testutils::Address as _;
        use soroban_sdk::Address;

        let env = soroban_sdk::Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let client = crate::TimeLockedUpgradeContractClient::new(&env, &contract_id);

        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        client.initialize(&admin, &treasury);

        let bad = PriceVarianceConfig {
            max_spread_bps: 0, // violates lower-bound invariant
            ..PriceVarianceConfig::default()
        };
        let result = client.try_set_price_variance_config(&admin, &bad);
        assert_eq!(
            result,
            Err(Ok(ContractError::InvalidVarianceConfig))
        );
    }

    #[test]
    fn test_storage_allocation_minimizer_rent_reduction_assertion() {
        let original = PriceVarianceConfig::default();
        let packed = original.pack();
        let unpacked = packed.unpack();

        assert_eq!(unpacked, original);

        let original_size = std::mem::size_of::<PriceVarianceConfig>();
        let packed_size = std::mem::size_of::<PackedPriceVarianceConfig>();

        assert_eq!(original_size, 20);
        assert_eq!(packed_size, 8);

        let reduction_pct = ((original_size - packed_size) as f64 / original_size as f64) * 100.0;
        assert!(reduction_pct >= 25.0, "Storage reduction percentage {}% is less than 25%", reduction_pct);
    }
}
