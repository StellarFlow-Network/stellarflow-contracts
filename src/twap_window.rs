//! TWAP Oracle Dynamic Sample Window Inspector (Issue #1020).
//!
//! The oracle's time-weighted average price (TWAP) is only as trustworthy as
//! the number of observations behind it and the freshness of the window those
//! observations are drawn from. In calm markets a short window keeps the TWAP
//! responsive; in turbulent markets that same window can contain so few
//! samples that the average is trivially manipulated. This module reconciles
//! the two by *dynamically scaling* the observation window with realized
//! volatility and by refusing to price when the sample set is too thin.
//!
//! # Behaviour
//!
//! 1. **Dynamic window** — the observation window expands from
//!    [`NORMAL_WINDOW_SECS`] (15 minutes) to [`HIGH_VOLATILITY_WINDOW_SECS`]
//!    (60 minutes) once realized volatility meets or exceeds the configured
//!    threshold (default [`DEFAULT_VOLATILITY_THRESHOLD_BPS`]). Expanding the
//!    window on turbulence admits older observations and stabilises the TWAP
//!    precisely when it is most needed.
//! 2. **Minimum sample count** — pricing requires at least
//!    [`N_MIN_OBSERVATIONS`] (`Nmin = 10`) observations inside the active
//!    window.
//! 3. **Fail closed** — pricing calls revert with
//!    [`crate::ContractError::InsufficientObservations`] whenever the minimum
//!    sample count is not satisfied, rather than returning a manipulable price.
//!
//! Volatility is measured as the mean absolute tick-to-tick return of the
//! observations inside the widest (high-volatility) lookback, expressed in
//! basis points and computed with saturating integer arithmetic so the
//! inspector can never panic on-chain.

use soroban_sdk::{contracttype, Env, Symbol, Vec};

use crate::ContractError;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Observation window used in normal (low-volatility) markets: 15 minutes.
pub const NORMAL_WINDOW_SECS: u64 = 15 * 60;

/// Observation window used during high price volatility: 60 minutes.
pub const HIGH_VOLATILITY_WINDOW_SECS: u64 = 60 * 60;

/// Minimum number of observation points (`Nmin`) required before the oracle is
/// permitted to serve a TWAP price.
pub const N_MIN_OBSERVATIONS: u32 = 10;

/// Default realized-volatility threshold (in basis points) at or above which
/// the window expands to [`HIGH_VOLATILITY_WINDOW_SECS`]. 100 bps = 1.00 %.
pub const DEFAULT_VOLATILITY_THRESHOLD_BPS: u64 = 100;

/// Hard cap on retained observations per asset. Bounds storage footprint and
/// the cost of volatility inspection.
pub const MAX_OBSERVATIONS: u32 = 64;

/// Basis-point denominator used by the volatility metric.
const BPS_DENOM: u128 = 10_000;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A single `(timestamp, price)` observation retained for TWAP inspection.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TwapObservation {
    /// Ledger timestamp at which the price was observed.
    pub timestamp: u64,
    /// Observed price, on the oracle's normalised fixed-point scale.
    pub price: i128,
}

/// Tunable parameters controlling the dynamic sample window.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TwapWindowConfig {
    /// Window length used when volatility is below the threshold.
    pub normal_window_secs: u64,
    /// Window length used when volatility meets or exceeds the threshold.
    pub high_volatility_window_secs: u64,
    /// Minimum number of observations required to price (`Nmin`).
    pub min_observations: u32,
    /// Realized-volatility threshold in basis points.
    pub volatility_threshold_bps: u64,
    /// Maximum observations retained per asset.
    pub max_observations: u32,
}

impl TwapWindowConfig {
    /// The protocol-default configuration.
    pub fn default_config() -> Self {
        Self {
            normal_window_secs: NORMAL_WINDOW_SECS,
            high_volatility_window_secs: HIGH_VOLATILITY_WINDOW_SECS,
            min_observations: N_MIN_OBSERVATIONS,
            volatility_threshold_bps: DEFAULT_VOLATILITY_THRESHOLD_BPS,
            max_observations: MAX_OBSERVATIONS,
        }
    }
}

/// Result of inspecting an asset's current TWAP window.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TwapWindowInspection {
    /// Asset whose window was inspected.
    pub asset: Symbol,
    /// Active observation window, in seconds.
    pub window_secs: u64,
    /// Number of observations inside the active window.
    pub sample_count: u32,
    /// Realized volatility over the wide lookback, in basis points.
    pub volatility_bps: u64,
    /// Whether volatility crossed the configured high-volatility threshold.
    pub high_volatility: bool,
    /// Minimum observations required (`Nmin`).
    pub min_required: u32,
    /// Whether `sample_count >= min_required`.
    pub sufficient: bool,
    /// Windowed TWAP price (0 when the window holds no observations).
    pub twap: i128,
}

/// Storage namespace for the inspector.
///
/// Variant names are globally unique across the contract's storage keys because
/// Soroban namespaces storage-key enums by variant name only.
#[contracttype]
pub enum TwapWindowKey {
    /// Retained observation log for an asset.
    TwapWindowObservationLog(Symbol),
    /// Per-asset configuration override.
    TwapWindowConfigEntry(Symbol),
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Validate a candidate configuration, returning
/// [`ContractError::InvalidArgument`] on the first violated invariant.
pub fn validate_config(cfg: &TwapWindowConfig) -> Result<(), ContractError> {
    if cfg.normal_window_secs == 0
        || cfg.high_volatility_window_secs < cfg.normal_window_secs
        || cfg.min_observations == 0
        || cfg.min_observations > cfg.max_observations
        || cfg.volatility_threshold_bps == 0
        || cfg.max_observations == 0
    {
        return Err(ContractError::InvalidArgument);
    }
    Ok(())
}

/// Read the configuration for an asset, falling back to the protocol default.
pub fn get_config(env: &Env, asset: &Symbol) -> TwapWindowConfig {
    env.storage()
        .persistent()
        .get(&TwapWindowKey::TwapWindowConfigEntry(asset.clone()))
        .unwrap_or_else(TwapWindowConfig::default_config)
}

/// Persist an asset configuration after validating it.
pub fn set_config(env: &Env, asset: &Symbol, cfg: &TwapWindowConfig) -> Result<(), ContractError> {
    validate_config(cfg)?;
    env.storage()
        .persistent()
        .set(&TwapWindowKey::TwapWindowConfigEntry(asset.clone()), cfg);
    Ok(())
}

/// Remove an asset override, reverting it to the protocol default.
pub fn clear_config(env: &Env, asset: &Symbol) {
    env.storage()
        .persistent()
        .remove(&TwapWindowKey::TwapWindowConfigEntry(asset.clone()));
}

// ---------------------------------------------------------------------------
// Observation log
// ---------------------------------------------------------------------------

/// Load the retained observation log for an asset (oldest first).
pub fn observations(env: &Env, asset: &Symbol) -> Vec<TwapObservation> {
    env.storage()
        .persistent()
        .get(&TwapWindowKey::TwapWindowObservationLog(asset.clone()))
        .unwrap_or_else(|| Vec::new(env))
}

/// Append an observation, trimming the log to the configured retention bounds.
///
/// Entries older than [`HIGH_VOLATILITY_WINDOW_SECS`] relative to the newest
/// sample are dropped, and at most `max_observations` entries are retained.
pub fn record_observation(env: &Env, asset: &Symbol, price: i128, timestamp: u64) {
    let cfg = get_config(env, asset);
    let existing = observations(env, asset);

    let mut kept: Vec<TwapObservation> = Vec::new(env);
    for i in 0..existing.len() {
        let obs = existing.get(i).unwrap();
        if obs
            .timestamp
            .saturating_add(cfg.high_volatility_window_secs)
            >= timestamp
        {
            kept.push_back(obs);
        }
    }
    kept.push_back(TwapObservation { timestamp, price });

    // Enforce the hard retention cap, dropping the oldest entries.
    let len = kept.len();
    if len > cfg.max_observations {
        let drop = len - cfg.max_observations;
        let mut trimmed: Vec<TwapObservation> = Vec::new(env);
        for i in drop..len {
            trimmed.push_back(kept.get(i).unwrap());
        }
        kept = trimmed;
    }

    env.storage().persistent().set(
        &TwapWindowKey::TwapWindowObservationLog(asset.clone()),
        &kept,
    );
}

// ---------------------------------------------------------------------------
// Inspection logic
// ---------------------------------------------------------------------------

/// Whether `timestamp` falls inside a window ending at `now`.
fn within_window(timestamp: u64, now: u64, window_secs: u64) -> bool {
    timestamp.saturating_add(window_secs) >= now
}

/// Count the observations inside the window `[now - window_secs, now]`.
pub fn count_in_window(samples: &Vec<TwapObservation>, now: u64, window_secs: u64) -> u32 {
    let mut count = 0u32;
    for i in 0..samples.len() {
        let obs = samples.get(i).unwrap();
        if within_window(obs.timestamp, now, window_secs) {
            count += 1;
        }
    }
    count
}

/// Simple average price of the observations inside the active window.
///
/// Returns `0` when the window contains no observations.
pub fn windowed_twap(samples: &Vec<TwapObservation>, now: u64, window_secs: u64) -> i128 {
    let mut sum: i128 = 0;
    let mut count: i128 = 0;
    for i in 0..samples.len() {
        let obs = samples.get(i).unwrap();
        if within_window(obs.timestamp, now, window_secs) {
            sum = sum.saturating_add(obs.price);
            count += 1;
        }
    }
    if count == 0 {
        0
    } else {
        sum / count
    }
}

/// Realized volatility, in basis points, over `lookback_secs`.
///
/// Defined as the mean absolute tick-to-tick return of consecutive in-window
/// observations:
///
/// `vol = mean( |p_i - p_{i-1}| * 10_000 / p_{i-1} )`
///
/// Returns `0` when fewer than two usable observations are present. The
/// computation uses saturating integer arithmetic and never panics.
pub fn realized_volatility_bps(
    samples: &Vec<TwapObservation>,
    now: u64,
    lookback_secs: u64,
) -> u64 {
    let mut prev: Option<i128> = None;
    let mut total: u128 = 0;
    let mut pairs: u128 = 0;

    for i in 0..samples.len() {
        let obs = samples.get(i).unwrap();
        if !within_window(obs.timestamp, now, lookback_secs) {
            continue;
        }
        if let Some(previous) = prev {
            if previous > 0 {
                let delta = obs
                    .price
                    .checked_sub(previous)
                    .map(|d| d.unsigned_abs())
                    .unwrap_or(u128::MAX);
                let bps = delta
                    .checked_mul(BPS_DENOM)
                    .map(|n| n / (previous as u128))
                    .unwrap_or(u128::MAX);
                total = total.saturating_add(bps);
                pairs += 1;
            }
        }
        prev = Some(obs.price);
    }

    if pairs == 0 {
        0
    } else {
        (total / pairs) as u64
    }
}

/// Inspect the asset's dynamic window without failing on a thin sample set.
pub fn inspect(env: &Env, asset: Symbol, now: u64) -> TwapWindowInspection {
    let cfg = get_config(env, &asset);
    let samples = observations(env, &asset);

    let volatility_bps = realized_volatility_bps(&samples, now, cfg.high_volatility_window_secs);
    let high_volatility = volatility_bps >= cfg.volatility_threshold_bps;
    let window_secs = if high_volatility {
        cfg.high_volatility_window_secs
    } else {
        cfg.normal_window_secs
    };

    let sample_count = count_in_window(&samples, now, window_secs);
    let sufficient = sample_count >= cfg.min_observations;
    let twap = windowed_twap(&samples, now, window_secs);

    TwapWindowInspection {
        asset,
        window_secs,
        sample_count,
        volatility_bps,
        high_volatility,
        min_required: cfg.min_observations,
        sufficient,
        twap,
    }
}

/// Inspect the window and fail closed when the sample set is too thin.
///
/// This is the gate pricing calls must pass through: it returns
/// [`ContractError::InsufficientObservations`] when fewer than `Nmin`
/// observations are present inside the active window.
pub fn enforce(env: &Env, asset: Symbol, now: u64) -> Result<TwapWindowInspection, ContractError> {
    let inspection = inspect(env, asset, now);
    if !inspection.sufficient {
        return Err(ContractError::InsufficientObservations);
    }
    Ok(inspection)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_series(env: &Env, prices: &[i128], start: u64, step: u64) -> Vec<TwapObservation> {
        let mut v = Vec::new(env);
        for (i, price) in prices.iter().enumerate() {
            v.push_back(TwapObservation {
                timestamp: start + (i as u64) * step,
                price: *price,
            });
        }
        v
    }

    #[test]
    fn default_config_uses_required_bounds() {
        let cfg = TwapWindowConfig::default_config();
        assert_eq!(cfg.normal_window_secs, 15 * 60);
        assert_eq!(cfg.high_volatility_window_secs, 60 * 60);
        assert_eq!(cfg.min_observations, 10);
        assert!(validate_config(&cfg).is_ok());
    }

    #[test]
    fn validate_config_rejects_inverted_windows() {
        let mut cfg = TwapWindowConfig::default_config();
        cfg.high_volatility_window_secs = cfg.normal_window_secs - 1;
        assert!(validate_config(&cfg).is_err());
    }

    #[test]
    fn validate_config_rejects_zero_min_observations() {
        let mut cfg = TwapWindowConfig::default_config();
        cfg.min_observations = 0;
        assert!(validate_config(&cfg).is_err());
    }

    #[test]
    fn count_in_window_filters_old_samples() {
        let env = Env::default();
        let now = 10_000u64;
        // Three recent samples inside 15 min, one old outside.
        let samples = sample_series(&env, &[1, 2, 3], now - 300, 100);
        let mut extended = samples.clone();
        extended.push_back(TwapObservation {
            timestamp: now - 10_000,
            price: 4,
        });
        assert_eq!(count_in_window(&extended, now, NORMAL_WINDOW_SECS), 3);
        assert_eq!(
            count_in_window(&extended, now, HIGH_VOLATILITY_WINDOW_SECS),
            4
        );
    }

    #[test]
    fn windowed_twap_averages_in_window_only() {
        let env = Env::default();
        let now = 10_000u64;
        let mut samples = sample_series(&env, &[100, 200], now - 100, 50);
        samples.push_back(TwapObservation {
            timestamp: now - 10_000,
            price: 1_000_000,
        });
        assert_eq!(windowed_twap(&samples, now, NORMAL_WINDOW_SECS), 150);
    }

    #[test]
    fn volatility_is_zero_for_flat_prices() {
        let env = Env::default();
        let now = 10_000u64;
        let samples = sample_series(&env, &[1_000, 1_000, 1_000], now - 200, 100);
        assert_eq!(
            realized_volatility_bps(&samples, now, HIGH_VOLATILITY_WINDOW_SECS),
            0
        );
    }

    #[test]
    fn volatility_matches_mean_abs_return() {
        let env = Env::default();
        let now = 10_000u64;
        // 100 -> 110 (+10%) and 110 -> 121 (+10%) => 1000 bps mean.
        let samples = sample_series(&env, &[100, 110, 121], now - 200, 100);
        assert_eq!(
            realized_volatility_bps(&samples, now, HIGH_VOLATILITY_WINDOW_SECS),
            1_000
        );
    }

    #[test]
    fn record_observation_caps_retention() {
        let env = Env::default();
        let cid = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let asset = soroban_sdk::symbol_short!("XLM");
        env.as_contract(&cid, || {
            let mut cfg = TwapWindowConfig::default_config();
            cfg.max_observations = 3;
            set_config(&env, &asset, &cfg).unwrap();
            for i in 0..6u64 {
                record_observation(&env, &asset, 100 + i as i128, 1_000 + i);
            }
            let stored = observations(&env, &asset);
            assert_eq!(stored.len(), 3);
            // Newest observations retained.
            assert_eq!(stored.get(2).unwrap().price, 105);
        });
    }

    #[test]
    fn inspect_expands_window_when_volatile() {
        let env = Env::default();
        let cid = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let asset = soroban_sdk::symbol_short!("XLM");
        env.as_contract(&cid, || {
            let now = 100_000u64;
            // 12 samples spanning ~55 minutes with large tick moves (10%).
            let mut price = 1_000i128;
            for i in 0..12u64 {
                record_observation(&env, &asset, price, now - 3_300 + i * 300);
                price = price + price / 10; // +10% each tick
            }
            let inspection = inspect(&env, asset.clone(), now);
            assert!(inspection.high_volatility);
            assert_eq!(inspection.window_secs, HIGH_VOLATILITY_WINDOW_SECS);
            // 60-minute window admits every sample.
            assert_eq!(inspection.sample_count, 12);
            assert!(inspection.sufficient);
        });
    }

    #[test]
    fn enforce_reverts_when_below_nmin() {
        let env = Env::default();
        let cid = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let asset = soroban_sdk::symbol_short!("XLM");
        env.as_contract(&cid, || {
            let now = 100_000u64;
            for i in 0..9u64 {
                record_observation(&env, &asset, 1_000, now - 100 + i);
            }
            let inspection = inspect(&env, asset.clone(), now);
            assert!(!inspection.sufficient);
            assert_eq!(inspection.sample_count, 9);
            assert_eq!(
                enforce(&env, asset, now),
                Err(ContractError::InsufficientObservations)
            );
        });
    }

    #[test]
    fn enforce_passes_at_exactly_nmin() {
        let env = Env::default();
        let cid = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let asset = soroban_sdk::symbol_short!("XLM");
        env.as_contract(&cid, || {
            let now = 100_000u64;
            for i in 0..N_MIN_OBSERVATIONS as u64 {
                record_observation(&env, &asset, 1_000, now - 200 + i);
            }
            let inspection = enforce(&env, asset, now).unwrap();
            assert_eq!(inspection.sample_count, N_MIN_OBSERVATIONS);
            assert_eq!(inspection.twap, 1_000);
        });
    }

    #[test]
    fn normal_window_is_used_when_volatility_is_low() {
        let env = Env::default();
        let cid = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let asset = soroban_sdk::symbol_short!("XLM");
        env.as_contract(&cid, || {
            let now = 100_000u64;
            for i in 0..N_MIN_OBSERVATIONS as u64 {
                record_observation(&env, &asset, 1_000, now - 200 + i);
            }
            let inspection = inspect(&env, asset, now);
            assert!(!inspection.high_volatility);
            assert_eq!(inspection.window_secs, NORMAL_WINDOW_SECS);
        });
    }
}
