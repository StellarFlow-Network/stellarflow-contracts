//! # Oracle Price Deviation Safety Threshold Guard (Issue #743)
//!
//! Protects AMM swaps from executing when the real-time spot price implied by
//! the pool reserves diverges significantly from the TWAP oracle value.
//!
//! ## Why this matters
//!
//! A swap executed against an oracle price that has drifted away from the true
//! on-chain reserve ratio is priced unfairly. This window is exactly what
//! flash-loan and oracle-manipulation attacks exploit: momentarily move the
//! pool or the feed, have the vulnerable swap execute, then monetise the
//! dislocation before it reverts. By gating every swap on a comparison of the
//! **spot** price (derived purely from `reserve_a`/`reserve_b`) against the
//! **TWAP** oracle reference, trades are reverted up-front when the two disagree
//! by more than a safety threshold (default 5%).
//!
//! ## Key mechanics
//!
//! 1. **Spot price derivation**: `compute_spot_price_from_reserves` recovers the
//!    real-time price from the pool's own reserves — the value an attacker would
//!    need to manipulate.
//! 2. **TWAP comparison**: `enforce_swap_oracle_guard` compares that spot price
//!    against the oracle TWAP value. Deviation is computed in basis points using
//!    integer arithmetic only (no floating point).
//! 3. **Revert on breach**: when `deviation_bps > max_deviation_bps`, execution
//!    aborts with [`ContractError::OracleDeviationTooHigh`].
//! 4. **Governance-adjustable threshold**: `set_oracle_deviation_config` lets the
//!    governance/admin role widen or tighten the band during volatile market
//!    conditions, or disable the guard entirely if a feed becomes unreliable.
//!
//! ## Fail-open policy
//!
//! When the oracle does not yet have a TWAP baseline for the asset (`None`) or the
//! reported baseline is non-positive, the guard passes the swap through so a
//! missing reference price never becomes a denial-of-service vector. Governance
//! can `disable()` the guard explicitly if a feed is known to be unreliable.

use soroban_sdk::{contracttype, symbol_short, Address, Env, Symbol};

use crate::amm::circuit_breaker::{
    calculate_price_deviation_bps, compute_spot_price_from_reserves,
};
use crate::{AssetId, ContractData, ContractError, DATA_KEY};

/// Default maximum allowed deviation between the AMM spot price and the TWAP
/// oracle price, in basis points: 500 bps = 5.00%.
pub const DEFAULT_MAX_ORACLE_DEVIATION_BPS: u32 = 500;

/// Absolute ceiling for `max_deviation_bps` to prevent a misconfiguration from
/// opening the full 100% range as an acceptable band: 5 000 bps = 50.00%.
pub const MAX_ORACLE_DEVIATION_BPS: u32 = 5_000;

/// Minimum allowed deviation threshold: 1 bps = 0.01%.
pub const MIN_ORACLE_DEVIATION_BPS: u32 = 1;

/// Instance-storage key for the governance-adjustable deviation guard config.
pub(crate) const ORACLE_DEVIATION_CONFIG_KEY: Symbol = symbol_short!("ODVCFG");

// ---------------------------------------------------------------------------
// Types & Data Structures
// ---------------------------------------------------------------------------

/// Governance-configurable parameters for the oracle deviation guard.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OracleDeviationConfig {
    /// Maximum allowed absolute deviation, in basis points, between the AMM
    /// spot price and the TWAP oracle value (e.g. 500 = 5%).
    pub max_deviation_bps: u32,
    /// Whether the guard is currently enforced. Disabling it makes swaps pass
    /// through without the spot-vs-oracle check (for volatile market periods).
    pub enabled: bool,
}

impl Default for OracleDeviationConfig {
    fn default() -> Self {
        Self {
            max_deviation_bps: DEFAULT_MAX_ORACLE_DEVIATION_BPS,
            enabled: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Verify that `cfg` satisfies the structural invariants of the guard.
///
/// - `max_deviation_bps` ∈ `[MIN_ORACLE_DEVIATION_BPS, MAX_ORACLE_DEVIATION_BPS]`
pub fn validate_oracle_deviation_config(cfg: &OracleDeviationConfig) -> Result<(), ContractError> {
    if cfg.max_deviation_bps < MIN_ORACLE_DEVIATION_BPS
        || cfg.max_deviation_bps > MAX_ORACLE_DEVIATION_BPS
    {
        return Err(ContractError::InvalidOracleDeviationConfig);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Storage Accessors
// ---------------------------------------------------------------------------

/// Retrieve the active oracle deviation config, falling back to the 5% default.
pub fn get_oracle_deviation_config(env: &Env) -> OracleDeviationConfig {
    env.storage()
        .instance()
        .get(&ORACLE_DEVIATION_CONFIG_KEY)
        .unwrap_or_default()
}

/// Adjust the oracle deviation guard parameters.
///
/// Only the governance admin may change them (e.g. to widen the band during a
/// volatile market or to disable the guard for an unreliable feed).
///
/// # Errors
/// - [`ContractError::NotInitialized`] — contract has not been initialized.
/// - [`ContractError::NotAdmin`] — `caller` is not the governance admin.
/// - [`ContractError::InvalidOracleDeviationConfig`] — config violates bounds.
pub fn set_oracle_deviation_config(
    env: &Env,
    caller: &Address,
    cfg: OracleDeviationConfig,
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

    validate_oracle_deviation_config(&cfg)?;

    env.storage()
        .instance()
        .set(&ORACLE_DEVIATION_CONFIG_KEY, &cfg);

    env.events().publish(
        (Symbol::new(env, "stellarflow"), Symbol::new(env, "odv_cfg")),
        (cfg.max_deviation_bps, cfg.enabled),
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Core Guard Logic
// ---------------------------------------------------------------------------

/// Core enforcement: abort when `spot_price` deviates from the TWAP oracle
/// value by more than the configured threshold.
///
/// Both prices must be expressed on the **same** fixed-point scale (the ratio
/// is scale-independent, so callers may use either the oracle's 10^9 scale or
/// the contract's 10^7 price scale as long as they are consistent).
///
/// # Fail-open conditions
/// - Guard `enabled == false` → passes.
/// - `twap_price == None` (no TWAP baseline yet) → passes.
/// - `twap_price <= 0` (unusable reference) → passes.
///
/// # Errors
/// - [`ContractError::DivisionByZero`] — `spot_price <= 0`.
/// - [`ContractError::Overflow`] — internal deviation math overflow.
/// - [`ContractError::OracleDeviationTooHigh`] — deviation exceeds threshold.
pub fn enforce_oracle_deviation_guard(
    env: &Env,
    pool_id: AssetId,
    spot_price: i128,
    twap_price: Option<i128>,
) -> Result<(), ContractError> {
    let cfg = get_oracle_deviation_config(env);

    if !cfg.enabled {
        return Ok(());
    }

    let Some(twap) = twap_price else {
        return Ok(());
    };
    if twap <= 0 {
        return Ok(());
    }
    if spot_price <= 0 {
        return Err(ContractError::DivisionByZero);
    }

    let dev_bps = calculate_price_deviation_bps(twap, spot_price)?;

    if dev_bps > cfg.max_deviation_bps {
        env.events().publish(
            (Symbol::new(env, "stellarflow"), Symbol::new(env, "odv_trip")),
            (
                pool_id,
                spot_price,
                twap,
                dev_bps,
                cfg.max_deviation_bps,
            ),
        );
        return Err(ContractError::OracleDeviationTooHigh);
    }

    Ok(())
}

/// Swap-facing guard: derive the real-time spot price from the pool reserves,
/// then enforce the deviation threshold against the TWAP oracle value.
///
/// Returns the derived spot price so callers can log/emit it on success.
/// The `twap_price` must be on the same fixed-point scale as the returned spot
/// price (the contract-standard `PRICE_SCALE = 10^7`).
///
/// # Errors
/// - [`ContractError::DivisionByZero`] — zero reserves or `spot_price <= 0`.
/// - [`ContractError::Overflow`] — internal deviation math overflow.
/// - [`ContractError::OracleDeviationTooHigh`] — deviation exceeds threshold.
pub fn enforce_swap_oracle_guard(
    env: &Env,
    pool_id: AssetId,
    reserve_in: u128,
    reserve_out: u128,
    twap_price: Option<i128>,
) -> Result<i128, ContractError> {
    let spot_price = compute_spot_price_from_reserves(reserve_in, reserve_out)?;
    enforce_oracle_deviation_guard(env, pool_id, spot_price, twap_price)?;
    Ok(spot_price)
}

// ---------------------------------------------------------------------------
// Unit Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger, LedgerInfo};
    use soroban_sdk::Env;

    /// Register a fresh contract, initialise it with governance `admin`, and
    /// return everything a storage-backed test needs.
    fn setup() -> (Env, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let admin = Address::generate(&env);
        env.as_contract(&contract_id, || {
            let data = ContractData {
                admin: admin.clone(),
                value: 0,
            };
            env.storage().instance().set(&crate::DATA_KEY, &data);
        });
        (env, contract_id, admin)
    }

    fn set_ledger_seq(env: &Env, seq: u32) {
        env.ledger().set(LedgerInfo {
            timestamp: env.ledger().timestamp(),
            protocol_version: env.ledger().protocol_version(),
            sequence_number: seq,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 0,
            min_persistent_entry_ttl: 0,
            max_entry_ttl: u32::MAX,
        });
    }

    // ── Defaults / config ────────────────────────────────────────────────

    #[test]
    fn default_config_is_5_percent_and_enabled() {
        let cfg = OracleDeviationConfig::default();
        assert_eq!(cfg.max_deviation_bps, DEFAULT_MAX_ORACLE_DEVIATION_BPS);
        assert_eq!(cfg.max_deviation_bps, 500);
        assert!(cfg.enabled);
        assert!(validate_oracle_deviation_config(&cfg).is_ok());
    }

    #[test]
    fn get_config_returns_default_before_any_set() {
        let (env, cid, _admin) = setup();
        let cfg = env.as_contract(&cid, || get_oracle_deviation_config(&env));
        assert_eq!(cfg, OracleDeviationConfig::default());
    }

    #[test]
    fn set_config_round_trips() {
        let (env, cid, admin) = setup();
        let custom = OracleDeviationConfig {
            max_deviation_bps: 1000,
            enabled: true,
        };
        env.as_contract(&cid, || {
            set_oracle_deviation_config(&env, &admin, custom.clone()).unwrap();
            assert_eq!(get_oracle_deviation_config(&env), custom);
        });
    }

    #[test]
    fn set_config_respects_contract_init_state() {
        // Uninitialized storage (no DATA_KEY) -> NotInitialized.
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let caller = Address::generate(&env);
        let res = env.as_contract(&contract_id, || {
            set_oracle_deviation_config(&env, &caller, OracleDeviationConfig::default())
        });
        assert_eq!(res, Err(ContractError::NotInitialized));
    }

    #[test]
    fn non_admin_cannot_set_config() {
        let (env, cid, _admin) = setup();
        let attacker = Address::generate(&env);
        let res = env.as_contract(&cid, || {
            set_oracle_deviation_config(&env, &attacker, OracleDeviationConfig::default())
        });
        assert_eq!(res, Err(ContractError::NotAdmin));
    }

    #[test]
    fn invalid_config_is_rejected() {
        let (env, cid, admin) = setup();

        // Zero deviation (below MIN_ORACLE_DEVIATION_BPS)
        let zero = OracleDeviationConfig {
            max_deviation_bps: 0,
            enabled: true,
        };
        assert_eq!(
            validate_oracle_deviation_config(&zero),
            Err(ContractError::InvalidOracleDeviationConfig)
        );
        env.as_contract(&cid, || {
            assert_eq!(
                set_oracle_deviation_config(&env, &admin, zero),
                Err(ContractError::InvalidOracleDeviationConfig)
            );
        });

        // Above ceiling (50%)
        let too_wide = OracleDeviationConfig {
            max_deviation_bps: MAX_ORACLE_DEVIATION_BPS + 1,
            enabled: true,
        };
        assert_eq!(
            validate_oracle_deviation_config(&too_wide),
            Err(ContractError::InvalidOracleDeviationConfig)
        );
        env.as_contract(&cid, || {
            assert_eq!(
                set_oracle_deviation_config(&env, &admin, too_wide),
                Err(ContractError::InvalidOracleDeviationConfig)
            );
        });
    }

    #[test]
    fn ceiling_and_minimum_bounds_are_accepted() {
        let (env, cid, admin) = setup();
        let min_cfg = OracleDeviationConfig {
            max_deviation_bps: MIN_ORACLE_DEVIATION_BPS,
            enabled: true,
        };
        assert!(validate_oracle_deviation_config(&min_cfg).is_ok());
        env.as_contract(&cid, || {
            set_oracle_deviation_config(&env, &admin, min_cfg).unwrap();
        });

        let max_cfg = OracleDeviationConfig {
            max_deviation_bps: MAX_ORACLE_DEVIATION_BPS,
            enabled: true,
        };
        assert!(validate_oracle_deviation_config(&max_cfg).is_ok());
        env.as_contract(&cid, || {
            set_oracle_deviation_config(&env, &admin, max_cfg).unwrap();
        });

        // Disabled guard with an out-of-range threshold is still rejected —
        // bounds are independent of the enabled flag.
        let disabled_invalid = OracleDeviationConfig {
            max_deviation_bps: 0,
            enabled: false,
        };
        assert_eq!(
            validate_oracle_deviation_config(&disabled_invalid),
            Err(ContractError::InvalidOracleDeviationConfig)
        );
    }

    // ── Guard enforcement ─────────────────────────────────────────────────

    #[test]
    fn guard_passes_within_threshold() {
        let (env, cid, _admin) = setup();
        let pool_id: AssetId = 2654435761;

        env.as_contract(&cid, || {
            // spot = 1.00, twap = 1.00 -> 0 bps deviation
            set_ledger_seq(&env, 10);
            assert!(enforce_oracle_deviation_guard(&env, pool_id, 10_000_000, Some(10_000_000)).is_ok());

            // spot 5% above twap -> exactly at boundary, allowed
            assert!(enforce_oracle_deviation_guard(&env, pool_id, 10_500_000, Some(10_000_000)).is_ok());

            // spot 5% below twap -> exactly at boundary, allowed
            assert!(enforce_oracle_deviation_guard(&env, pool_id, 9_500_000, Some(10_000_000)).is_ok());

            // small 4.99% drift -> allowed
            assert!(enforce_oracle_deviation_guard(&env, pool_id, 10_499_000, Some(10_000_000)).is_ok());
        });
    }

    #[test]
    fn guard_reverts_when_spot_above_oracle_threshold() {
        let (env, cid, _admin) = setup();
        let pool_id: AssetId = 2654435761;

        env.as_contract(&cid, || {
            // spot 5.01% above twap -> revert
            let res = enforce_oracle_deviation_guard(&env, pool_id, 10_501_000, Some(10_000_000));
            assert_eq!(res, Err(ContractError::OracleDeviationTooHigh));

            // spot 10% above twap -> revert
            let res = enforce_oracle_deviation_guard(&env, pool_id, 11_000_000, Some(10_000_000));
            assert_eq!(res, Err(ContractError::OracleDeviationTooHigh));
        });
    }

    #[test]
    fn guard_reverts_when_spot_below_oracle_threshold() {
        let (env, cid, _admin) = setup();
        let pool_id: AssetId = 2654435761;

        env.as_contract(&cid, || {
            // spot 5.01% below twap -> revert
            let res = enforce_oracle_deviation_guard(&env, pool_id, 9_499_000, Some(10_000_000));
            assert_eq!(res, Err(ContractError::OracleDeviationTooHigh));

            // spot 50% below twap -> revert
            let res = enforce_oracle_deviation_guard(&env, pool_id, 5_000_000, Some(10_000_000));
            assert_eq!(res, Err(ContractError::OracleDeviationTooHigh));
        });
    }

    #[test]
    fn guard_is_scale_independent() {
        let (env, cid, _admin) = setup();
        let pool_id: AssetId = 2654435761;

        env.as_contract(&cid, || {
            // Both prices on the oracle's 10^9 scale: 5% boundary still enforced.
            assert!(enforce_oracle_deviation_guard(&env, pool_id, 1_050_000_000, Some(1_000_000_000)).is_ok());
            let res = enforce_oracle_deviation_guard(&env, pool_id, 1_051_000_000, Some(1_000_000_000));
            assert_eq!(res, Err(ContractError::OracleDeviationTooHigh));
        });
    }

    #[test]
    fn guard_fails_open_without_twap_baseline() {
        let (env, cid, _admin) = setup();
        let pool_id: AssetId = 2654435761;

        env.as_contract(&cid, || {
            // No TWAP baseline yet -> pass (fail-open, never a DoS vector).
            assert!(enforce_oracle_deviation_guard(&env, pool_id, 1_000_000_000, None).is_ok());

            // Non-positive oracle reference -> pass.
            assert!(enforce_oracle_deviation_guard(&env, pool_id, 1_000_000_000, Some(0)).is_ok());
            assert!(enforce_oracle_deviation_guard(&env, pool_id, 1_000_000_000, Some(-1)).is_ok());
        });
    }

    #[test]
    fn guard_can_be_disabled_by_governance() {
        let (env, cid, admin) = setup();
        let pool_id: AssetId = 2654435761;

        env.as_contract(&cid, || {
            let disabled = OracleDeviationConfig {
                max_deviation_bps: 500,
                enabled: false,
            };
            set_oracle_deviation_config(&env, &admin, disabled).unwrap();

            // Even a 50% divergence passes while the guard is disabled.
            let res = enforce_oracle_deviation_guard(&env, pool_id, 15_000_000, Some(10_000_000));
            assert!(res.is_ok());
        });
    }

    #[test]
    fn guard_rejects_non_positive_spot_price() {
        let (env, cid, _admin) = setup();
        let pool_id: AssetId = 2654435761;

        env.as_contract(&cid, || {
            let res = enforce_oracle_deviation_guard(&env, pool_id, 0, Some(10_000_000));
            assert_eq!(res, Err(ContractError::DivisionByZero));

            let res = enforce_oracle_deviation_guard(&env, pool_id, -5, Some(10_000_000));
            assert_eq!(res, Err(ContractError::DivisionByZero));
        });
    }

    // ── Swap-facing helper (spot from reserves) ──────────────────────────

    #[test]
    fn swap_guard_derives_spot_and_enforces() {
        let (env, cid, _admin) = setup();
        let pool_id: AssetId = 2654435761;

        env.as_contract(&cid, || {
            // Equal reserves => spot 1.00; twap 1.00 -> passes, returns spot.
            let spot = enforce_swap_oracle_guard(&env, pool_id, 100_000, 100_000, Some(10_000_000)).unwrap();
            assert_eq!(spot, 10_000_000);

            // Reserves imply spot 1.10 vs twap 1.00 -> 10% > 5% -> revert.
            let res = enforce_swap_oracle_guard(&env, pool_id, 100_000, 110_000, Some(10_000_000));
            assert_eq!(res, Err(ContractError::OracleDeviationTooHigh));

            // Reserves imply spot 0.90 vs twap 1.00 -> 10% below -> revert.
            let res = enforce_swap_oracle_guard(&env, pool_id, 100_000, 90_000, Some(10_000_000));
            assert_eq!(res, Err(ContractError::OracleDeviationTooHigh));

            // Reserves imply spot ~1.04 vs twap 1.00 -> within 5% -> passes.
            let spot = enforce_swap_oracle_guard(&env, pool_id, 100_000, 104_000, Some(10_000_000)).unwrap();
            assert_eq!(spot, 10_400_000);
        });
    }

    #[test]
    fn swap_guard_rejects_zero_reserves() {
        let (env, cid, _admin) = setup();
        let pool_id: AssetId = 2654435761;

        env.as_contract(&cid, || {
            assert_eq!(
                enforce_swap_oracle_guard(&env, pool_id, 0, 100_000, Some(10_000_000)),
                Err(ContractError::DivisionByZero)
            );
            assert_eq!(
                enforce_swap_oracle_guard(&env, pool_id, 100_000, 0, Some(10_000_000)),
                Err(ContractError::DivisionByZero)
            );
        });
    }

    #[test]
    fn swap_guard_fails_open_without_twap() {
        let (env, cid, _admin) = setup();
        let pool_id: AssetId = 2654435761;

        let spot = env.as_contract(&cid, || {
            enforce_swap_oracle_guard(&env, pool_id, 100_000, 150_000, None).unwrap()
        });
        assert_eq!(spot, 15_000_000);
    }

    #[test]
    fn widened_threshold_allows_larger_drift() {
        let (env, cid, admin) = setup();
        let pool_id: AssetId = 2654435761;

        env.as_contract(&cid, || {
            // Governance widens the band to 10% during volatile conditions.
            let wide = OracleDeviationConfig {
                max_deviation_bps: 1000,
                enabled: true,
            };
            set_oracle_deviation_config(&env, &admin, wide).unwrap();

            // 10% drift now passes...
            assert!(enforce_swap_oracle_guard(&env, pool_id, 100_000, 110_000, Some(10_000_000)).is_ok());

            // ...but 11% still reverts.
            let res = enforce_swap_oracle_guard(&env, pool_id, 100_000, 111_000, Some(10_000_000));
            assert_eq!(res, Err(ContractError::OracleDeviationTooHigh));
        });
    }

    #[test]
    fn tightened_threshold_rejects_smaller_drift() {
        let (env, cid, admin) = setup();
        let pool_id: AssetId = 2654435761;

        env.as_contract(&cid, || {
            // Governance tightens the band to 2%.
            let tight = OracleDeviationConfig {
                max_deviation_bps: 200,
                enabled: true,
            };
            set_oracle_deviation_config(&env, &admin, tight).unwrap();

            assert!(enforce_swap_oracle_guard(&env, pool_id, 100_000, 102_000, Some(10_000_000)).is_ok());
            let res = enforce_swap_oracle_guard(&env, pool_id, 100_000, 103_000, Some(10_000_000));
            assert_eq!(res, Err(ContractError::OracleDeviationTooHigh));
        });
    }
}