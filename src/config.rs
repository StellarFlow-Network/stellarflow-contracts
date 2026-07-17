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

use soroban_sdk::{contracttype, symbol_short, Env, Symbol};

use crate::{ContractData, ContractError, DATA_KEY};

// ── Storage key ──────────────────────────────────────────────────────────────

/// Ledger instance-storage key for the sealed variance configuration.
pub(crate) const PRICE_VARIANCE_CONFIG_KEY: Symbol = symbol_short!("PVARCFG");

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
}
