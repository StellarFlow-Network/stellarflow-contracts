//! Adaptive fee scaling based on pool volatility multipliers (Issue #766).
//!
//! During high short-term market volatility, swap fees are dynamically scaled
//! up from a base rate (default 0.30 %) to a maximum cap (default 1.50 %) to
//! compensate liquidity providers for the elevated impermanent-loss risk. As
//! volatility normalizes the fee automatically decays back to baseline.
//!
//! # Design
//!
//! 1. **Historical ring buffer** — each pool keeps a bounded, persistent ring
//!    buffer of recent short-term TWAP prices ([`PriceObservationBuffer`]).
//!    Observations are optionally pulled on-chain from the price oracle's
//!    `get_twap(Symbol)` feed, mirroring the pattern used by vault
//!    liquidation (`vaults::liquidation`).
//! 2. **Volatility** — the sample variance of the ring buffer is reduced to an
//!    integer square root (standard deviation) and normalized to basis points
//!    relative to the mean price.
//! 3. **Fee scaling** — base fee is linearly interpolated toward the maximum
//!    cap between [`AdaptiveFeeConfig::low_volatility_bps`] and
//!    [`AdaptiveFeeConfig::high_volatility_bps`].
//! 4. **Decay** — the applied volatility (and therefore fee) exponentially
//!    decays toward baseline over [`AdaptiveFeeConfig::decay_half_life_secs`]
//!    when the pool runs out of fresh, high-variance observations, so the fee
//!    automatically relaxes as markets calm.
//!
//! A pool opts into adaptive scaling by having an [`AdaptiveFeeConfig`]
//! written for it (admin). Pools without one keep the legacy volume-based
//! dynamic fee.
//!
//! # Storage
//!
//! All keys live under the [`AdaptiveStorageKey`] namespace, keyed by the
//! numeric [`crate::AssetId`] of the pool — a single dimension that avoids
//! symbol lookups (and their panics) inside the hot swap path.

use crate::config::{get_adaptive_fee_config, AdaptiveFeeConfig};
use crate::{events, AssetId, ContractError};
use soroban_sdk::{contracttype, symbol_short, Address, Env, IntoVal, Symbol, Vec};

/// Identity constant for the adaptive-fee event topic.
const EV_TOPIC_ADAPTIVE: Symbol = symbol_short!("afee");

/// Reference to the shared persistent-TTL threshold used to keep long-lived
/// ring-buffer entries alive across ledgers.
const RENT_THRESHOLD: u32 = crate::storage::PERSISTENT_TTL_THRESHOLD;

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

/// Storage namespace for all adaptive fee state.
#[contracttype]
pub enum AdaptiveStorageKey {
    /// Historical short-term price ring buffer for a pool.
    Ring(AssetId),
    /// Decaying adaptive fee/volatility state for a pool.
    State(AssetId),
}

/// Bounded historical ring buffer of short-term pool prices.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct PriceObservationBuffer {
    /// The price series (newest last), length ≤ configured `ring_buffer_len`.
    pub prices: Vec<i128>,
    /// The asset symbol whose oracle feed was recorded in `prices`.
    pub asset_symbol: Symbol,
    /// Ledger timestamp of the most recent observation.
    pub last_observed: u64,
}

/// Decaying adaptive fee/volatility snapshot for a pool.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct AdaptiveFeeState {
    /// Effective (post-decay) short-term volatility in basis points.
    pub volatility_bps: u64,
    /// The scaled swap fee in basis points currently in effect.
    pub fee_bps: u32,
    /// Ledger timestamp this state was last refreshed.
    pub last_updated: u64,
}

impl Default for AdaptiveFeeState {
    fn default() -> Self {
        Self {
            volatility_bps: 0,
            fee_bps: 0,
            last_updated: 0,
        }
    }
}

/// Point-in-time snapshot returned to callers/keepers for observability.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct AdaptiveFeeSnapshot {
    pub pool: AssetId,
    pub asset_symbol: Symbol,
    pub volatility_bps: u64,
    pub fee_bps: u32,
    pub base_fee_bps: u32,
    pub max_fee_bps: u32,
    pub observations: u32,
}

// ---------------------------------------------------------------------------
// Ring-buffer management
// ---------------------------------------------------------------------------

// Default keeps a pool unconfigured until an admin opts in via config.
const EMPTY_RING_LEN: u32 = 0;

/// Load the price ring buffer for a pool, or create an empty default.
fn load_ring(env: &Env, pool: AssetId) -> PriceObservationBuffer {
    env.storage()
        .persistent()
        .get(&AdaptiveStorageKey::Ring(pool))
        .unwrap_or(PriceObservationBuffer {
            prices: Vec::new(env),
            asset_symbol: Symbol::new(env, ""),
            last_observed: 0,
        })
}

/// Persist the price ring buffer for a pool with a bumped TTL.
fn save_ring(env: &Env, pool: AssetId, buf: &PriceObservationBuffer) {
    let key = AdaptiveStorageKey::Ring(pool);
    env.storage().persistent().set(&key, buf);
    env.storage()
        .persistent()
        .extend_ttl(&key, RENT_THRESHOLD, RENT_THRESHOLD);
}

/// Load the adaptive state snapshot for a pool.
fn load_state(env: &Env, pool: AssetId) -> AdaptiveFeeState {
    env.storage()
        .instance()
        .get(&AdaptiveStorageKey::State(pool))
        .unwrap_or_else(|| AdaptiveFeeState {
            volatility_bps: 0,
            fee_bps: 0,
            last_updated: env.ledger().timestamp(),
        })
}

/// Persist the adaptive state snapshot for a pool.
fn save_state(env: &Env, pool: AssetId, state: &AdaptiveFeeState) {
    env.storage()
        .instance()
        .set(&AdaptiveStorageKey::State(pool), state);
}

/// Trim `buf.prices` down to at most `max_len` entries, dropping oldest first.
///
/// Rebuilds the backing `Vec` using only `get`/`push_back` (no dependence on
/// element-removal APIs), so capacity enforcement is portable across Soroban
/// SDK versions.
fn trim_ring(env: &Env, buf: &mut Vec<i128>, max_len: u32) {
    while buf.len() > max_len {
        let mut trimmed = Vec::new(env);
        for i in 1..buf.len() {
            if let Some(v) = buf.get(i) {
                trimmed.push_back(v);
            }
        }
        *buf = trimmed;
    }
}

/// Record a short-term price observation for a pool.
///
/// Respects the configured [`AdaptiveFeeConfig::sample_interval_secs`] so a
/// burst of identical observations in a single ledger does not fill the ring
/// with duplicates. Returns the ring length after recording (0 when skipped).
pub fn record_price_observation(
    env: &Env,
    pool: AssetId,
    asset_symbol: Symbol,
    price: i128,
) -> Result<u64, ContractError> {
    let cfg = get_adaptive_fee_config(env, pool).ok_or(ContractError::NotRegistered)?;
    if price <= 0 {
        return Err(ContractError::AmountTooLow);
    }

    let now = env.ledger().timestamp();
    let mut buf = load_ring(env, pool);

    let populated = buf.prices.len() > EMPTY_RING_LEN;
    let elapsed = if now >= buf.last_observed {
        now - buf.last_observed
    } else {
        0
    };
    if populated && elapsed < cfg.sample_interval_secs {
        // Too soon since the previous observation; skip so the window stays
        // representative rather than dominated by a single block.
        return Ok(buf.prices.len() as u64);
    }

    if !populated {
        buf.asset_symbol = asset_symbol.clone();
    }
    buf.prices.push_back(price);
    trim_ring(env, &mut buf.prices, cfg.ring_buffer_len);
    buf.last_observed = now;
    save_ring(env, pool, &buf);

    Ok(buf.prices.len() as u64)
}

/// Pull the latest TWAP price from an oracle contract's `get_twap(Symbol)`
/// feed and record it into the pool's ring buffer.
///
/// # Errors
///
/// - [`ContractError::NotInitialized`] — the oracle returned no/stale price.
/// - [`ContractError::NotRegistered`] — the pool has no adaptive fee config.
pub fn observe_from_oracle(
    env: &Env,
    pool: AssetId,
    asset_symbol: Symbol,
    oracle: &Address,
) -> Result<u64, ContractError> {
    get_adaptive_fee_config(env, pool).ok_or(ContractError::NotRegistered)?;

    // Same invocation shape as `vaults::liquidation::read_twap`.
    let result: Result<Option<i128>, soroban_sdk::Error> = env.invoke_contract(
        oracle,
        &symbol_short!("get_twap"),
        soroban_sdk::vec![env, asset_symbol.clone().into_val(env)],
    );
    let price = match result {
        Ok(Some(p)) => p,
        _ => return Err(ContractError::NotInitialized),
    };

    record_price_observation(env, pool, asset_symbol, price)
}

// ---------------------------------------------------------------------------
// Volatility & fee computation
// ---------------------------------------------------------------------------

/// Integer square root via the digit-by-digit (binary) method; exact for
/// `u128`. Returns 0 for input 0. `num` is consumed as the residual during
/// the bit recovery, matching the standard integer-sqrt algorithm.
fn isqrt(mut num: u128) -> u128 {
    let mut res: u128 = 0;
    let mut bit: u128 = 1 << 126;
    while bit > num {
        bit >>= 2;
    }
    while bit > 0 {
        if num >= res + bit {
            num -= res + bit;
            res = (res >> 1) + bit;
        } else {
            res >>= 1;
        }
        bit >>= 2;
    }
    res
}

/// Compute short-term volatility (bps) for a pool from its ring buffer.
///
/// Uses population variance `sum((x - mean)²)/n`, reduced to the standard
/// deviation (integer sqrt) and expressed as basis points of the mean price.
/// Returns `(volatility_bps, has_data)`. `has_data` is false whenever fewer
/// than two observations exist or the mean is non-positive, in which case the
/// caller falls back to baseline (no uplift).
fn compute_volatility_bps(
    env: &Env,
    pool: AssetId,
    cfg: &AdaptiveFeeConfig,
) -> Result<(u64, bool), ContractError> {
    let buf = load_ring(env, pool);
    let n = buf.prices.len();
    if n < 2 {
        return Ok((0, false));
    }

    // Mean of the window.
    let mut sum: i128 = 0;
    for i in 0..n {
        let p = buf.prices.get(i).ok_or(ContractError::NotRegistered)?;
        sum = sum.checked_add(p).ok_or(ContractError::Overflow)?;
    }
    let mean = sum / (n as i128);
    if mean <= 0 {
        return Ok((0, false));
    }

    // Sum of squared deviations (mean excluded since n ≥ 2).
    let mut sum_sq: i128 = 0;
    for i in 0..n {
        let p = buf.prices.get(i).ok_or(ContractError::NotRegistered)?;
        let dev = p.checked_sub(mean).ok_or(ContractError::Overflow)?.abs();
        let sq = dev.checked_mul(dev).ok_or(ContractError::Overflow)?;
        sum_sq = sum_sq.checked_add(sq).ok_or(ContractError::Overflow)?;
    }
    let variance = sum_sq / (n as i128);
    let std_dev = isqrt(variance.unsigned_abs());

    let denom = mean.unsigned_abs();
    let vol_bps = if denom == 0 {
        0
    } else {
        std_dev
            .checked_mul(10_000)
            .ok_or(ContractError::Overflow)?
            .checked_div(denom)
            .ok_or(ContractError::DivisionByZero)?
    };

    // If the whole window has gone stale, treat the instantaneous volatility
    // as zero so only the time-decay term drives the fee back to baseline.
    let now = env.ledger().timestamp();
    let window_secs = (cfg.sample_interval_secs as u128)
        .checked_mul(cfg.ring_buffer_len as u128)
        .ok_or(ContractError::Overflow)?;
    let stale = now
        .checked_sub(buf.last_observed)
        .map(|age| (age as u128) > window_secs)
        .unwrap_or(true);

    Ok((if stale { 0u64 } else { vol_bps as u64 }, true))
}

/// Linearly map a volatility value (bps) to a fee (bps) within a pool's
/// configured `[base, max]` band.
fn fee_for_volatility(vol_bps: u64, cfg: &AdaptiveFeeConfig) -> u32 {
    let low = cfg.low_volatility_bps as u128;
    let high = cfg.high_volatility_bps as u128;
    let v = vol_bps as u128;
    let base = cfg.base_fee_bps as u128;
    let max = cfg.max_fee_bps as u128;

    if v <= low {
        return cfg.base_fee_bps;
    }
    if v >= high {
        return cfg.max_fee_bps;
    }
    // base + (max - base) * (v - low) / (high - low)
    let span = max.checked_sub(base).unwrap_or(0);
    let ratio = (v - low) * span / (high - low);
    ((base + ratio) as u32).clamp(cfg.base_fee_bps, cfg.max_fee_bps)
}

/// Exponential-style decay toward baseline using a half-life model that is
/// integer-safe: `prev * half_life / (half_life + elapsed)`.
fn decayed_volatility(prev_vol: u64, half_life_secs: u64, elapsed_secs: u64) -> u64 {
    if elapsed_secs == 0 {
        return prev_vol;
    }
    let numerator = (prev_vol as u128) * (half_life_secs as u128);
    let denom = (half_life_secs as u128) + (elapsed_secs as u128);
    if denom == 0 {
        return prev_vol;
    }
    (numerator / denom) as u64
}

/// Resolve the current adaptive fee (and effective volatility) for a pool.
///
/// Combines the fresh instantaneous volatility from the ring buffer with an
/// exponentially decaying tail from the previous state, maps the effective
/// volatility to a fee in `[base, max]`, and persists the new state. This is
/// the stateful function invoked from the swap path and the public queries.
pub fn resolve_adaptive_fee(
    env: &Env,
    pool: AssetId,
) -> Result<(u32, u64), ContractError> {
    let cfg = get_adaptive_fee_config(env, pool).ok_or(ContractError::NotRegistered)?;
    let now = env.ledger().timestamp();

    let (inst_vol, _has_data) = compute_volatility_bps(env, pool, &cfg)?;

    let prev = load_state(env, pool);
    let elapsed = if now >= prev.last_updated {
        now - prev.last_updated
    } else {
        0
    };
    let decayed = decayed_volatility(prev.volatility_bps, cfg.decay_half_life_secs, elapsed);
    let effective_vol = inst_vol.max(decayed);
    let fee_bps = fee_for_volatility(effective_vol, &cfg);

    let next = AdaptiveFeeState {
        volatility_bps: effective_vol,
        fee_bps,
        last_updated: now,
    };

    // Emit an indexer event only when the fee actually moved, to avoid
    // spamming the ledger with identical snapshots on every swap.
    if fee_bps != prev.fee_bps {
        let ring = load_ring(env, pool);
        emit_adaptive_fee_event(env, pool, &ring.asset_symbol, fee_bps, effective_vol);
    }

    save_state(env, pool, &next);

    Ok((fee_bps, effective_vol))
}

/// Read the current adaptive fee snapshot for observation without mutating
/// persisted state (a pure read used by keepers/off-chain querying).
pub fn get_adaptive_fee_snapshot(env: &Env, pool: AssetId) -> Result<AdaptiveFeeSnapshot, ContractError> {
    let cfg = get_adaptive_fee_config(env, pool).ok_or(ContractError::NotRegistered)?;
    let (fee_bps, volatile_bps) = resolve_adaptive_fee(env, pool)?;
    let buf = load_ring(env, pool);
    Ok(AdaptiveFeeSnapshot {
        pool,
        asset_symbol: buf.asset_symbol,
        volatility_bps: volatile_bps,
        fee_bps,
        base_fee_bps: cfg.base_fee_bps,
        max_fee_bps: cfg.max_fee_bps,
        observations: buf.prices.len() as u32,
    })
}

/// Current short-term volatility (bps) for a pool, purely as a query.
pub fn get_pool_volatility_bps(env: &Env, pool: AssetId) -> Result<u64, ContractError> {
    get_adaptive_fee_config(env, pool).ok_or(ContractError::NotRegistered)?;
    let (_, vol) = resolve_adaptive_fee(env, pool)?;
    Ok(vol)
}

/// Emit an `AdaptiveFeeChanged` event for indexers when the fee is refreshed.
pub fn emit_adaptive_fee_event(
    env: &Env,
    pool: AssetId,
    symbol: &Symbol,
    fee_bps: u32,
    volatility_bps: u64,
) {
    let _ = events::emit_simple2(
        env,
        events::EV_ADAPTIVE_FEE,
        EV_TOPIC_ADAPTIVE,
        (pool, symbol.clone(), fee_bps, volatility_bps),
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::set_adaptive_fee_config;
    use soroban_sdk::testutils::{Address as _, Ledger, LedgerInfo};
    use soroban_sdk::Env;

    fn setup() -> (Env, crate::TimeLockedUpgradeContractClient<'static>, Address, AssetId) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let client = crate::TimeLockedUpgradeContractClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        client.initialize(&admin, &treasury);
        // A pool with the default adaptive config applied.
        let pool: AssetId = 3897123275;
        client.set_adaptive_fee_config(&admin, &pool, &AdaptiveFeeConfig::default());
        (env, client, admin, pool)
    }

    fn set_time(env: &Env, secs: u64) {
        env.ledger().set(LedgerInfo {
            timestamp: secs,
            protocol_version: 20,
            sequence_number: 1,
            network_id: Default::default(),
            base_reserve: 0,
            min_temp_entry_ttl: 0,
            min_live_entry_ttl: 0,
            max_entry_ttl: u32::MAX,
            ledger_entries: Default::default(),
        });
    }

    #[test]
    fn record_observations_and_trim_ring() {
        let (env, _client, _admin, pool) = setup();
        let sym: Symbol = symbol_short!("NGN");
        set_time(&env, 1_000_000);
        let cfg = get_adaptive_fee_config(&env, pool).unwrap();

        let mut last = 0;
        for i in 0..(cfg.ring_buffer_len + 5) {
            // Advance the clock past the sample interval each push.
            set_time(&env, 1_000_000 + 1_000_000 + (i as u64) * cfg.sample_interval_secs);
            last = record_price_observation(&env, pool, sym.clone(), 100 + (i as i128)).unwrap();
        }
        // Buffer is capped at the configured length.
        assert!(last <= cfg.ring_buffer_len as u64);
        assert!(last > 0);
        let vol = get_pool_volatility_bps(&env, pool).unwrap();
        assert!(vol > 0, "with many observations variance should be non-zero");
    }

    #[test]
    fn fee_stays_at_base_when_volatility_below_threshold() {
        let (env, client, _admin, pool) = setup();
        // No observations -> no uplift, fee = base.
        let snap = client.get_adaptive_fee(&pool);
        assert_eq!(snap.volatility_bps, 0);
        assert_eq!(snap.fee_bps, 30);
    }

    #[test]
    fn fee_reaches_max_cap_at_high_volatility() {
        let (env, client, _admin, pool) = setup();
        let sym: Symbol = symbol_short!("NGN");
        // High dispersion within the window -> climbing fee capped at max.
        let cfg = get_adaptive_fee_config(&env, pool).unwrap();
        let start = 1_000_000u64;
        for i in 0..cfg.ring_buffer_len {
            let price = 1000 + (i as i128) * 5000; // extreme oscillations
            set_time(&env, start + (i as u64) * cfg.sample_interval_secs);
            let _ = record_price_observation(&env, pool, sym.clone(), price).unwrap();
        }
        let snap = client.get_adaptive_fee(&pool);
        assert_eq!(snap.fee_bps, 150, "high volatility should hit the max cap of 150bps");
    }

    #[test]
    fn fee_decays_back_to_base_when_observations_stop() {
        let (env, client, _admin, pool) = setup();
        let sym: Symbol = symbol_short!("NGN");
        let cfg = get_adaptive_fee_config(&env, pool).unwrap();
        let start = 2_000_000u64;
        // First drive the fee to max via a volatile burst.
        for i in 0..cfg.ring_buffer_len {
            let price = 1000 + (i as i128) * 5000;
            set_time(&env, start + (i as u64) * cfg.sample_interval_secs);
            let _ = record_price_observation(&env, pool, sym.clone(), price).unwrap();
        }
        let snap_max = client.get_adaptive_fee(&pool);
        assert_eq!(snap_max.fee_bps, 150);

        // Then let several decay half-lives pass with no new observations.
        set_time(&env, start + cfg.sample_interval_secs * 100 + cfg.decay_half_life_secs * 8);
        let snap_later = client.get_adaptive_fee(&pool);
        assert!(
            snap_later.fee_bps < snap_max.fee_bps,
            "fee must decay back toward base: {} < {}",
            snap_later.fee_bps,
            snap_max.fee_bps
        );
        // The decaying tail relaxes toward baseline; it must be strictly below max.
        assert!(snap_later.fee_bps < 150);
    }

    #[test]
    fn unconfigured_pool_is_rejected() {
        let (env, _client, _admin, _pool) = setup();
        let unconfigured: AssetId = 999_999_999;
        assert_eq!(
            resolve_adaptive_fee(&env, unconfigured),
            Err(ContractError::NotRegistered)
        );
    }
}
