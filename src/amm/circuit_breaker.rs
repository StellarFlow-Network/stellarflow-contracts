//! # On-Chain Spot Price Deviation Circuit Breaker (Issue #802)
//!
//! Provides emergency execution halts when single-block spot price changes exceed
//! safe thresholds (e.g., >15% price crash or spike), protecting liquidity pools
//! from flash loan exploits, oracle manipulation, and toxic flow.
//!
//! ## Key Mechanics
//! 1. **Tick Monitoring**: Tracks spot price difference relative to previous block ledger ticks.
//! 2. **Automatic Freeze**: Instantly freezes pool swaps when price deviation exceeds `max_deviation_bps`.
//! 3. **Automatic Cooldown**: Automatically restores trading after `cooldown_ledgers` (default: 100 ledgers).
//! 4. **Manual Override**: Allows authorized admins to manually freeze or unfreeze pools immediately.

use soroban_sdk::{
    contracttype, symbol_short, Address, Env, Symbol,
};

use crate::{AssetId, ContractData, ContractError, DATA_KEY};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Scale factor for basis points calculations (10,000 bps = 100%).
pub const BPS_SCALE: u32 = 10_000;

/// Default maximum spot price deviation allowed in a single block/tick: 1,500 bps = 15.00%.
pub const DEFAULT_MAX_SPOT_DEVIATION_BPS: u32 = 1_500;

/// Default automatic cooldown window before trading resumes: 100 ledgers (~500 seconds).
pub const DEFAULT_COOLDOWN_LEDGERS: u32 = 100;

/// Maximum allowable deviation ceiling to prevent misconfiguration: 5,000 bps = 50.00%.
pub const MAX_DEVIATION_BPS_CEILING: u32 = 5_000;

/// Minimum allowable deviation: 1 bps = 0.01%.
pub const MIN_DEVIATION_BPS: u32 = 1;

/// Standard fixed-point scale (10^7) for spot prices.
pub const PRICE_SCALE: i128 = 10_000_000;

// ---------------------------------------------------------------------------
// Types & Data Structures
// ---------------------------------------------------------------------------

/// Global operational configuration for the spot price circuit breaker.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpotCircuitBreakerConfig {
    /// Maximum allowed single-block spot price deviation in basis points (e.g. 1500 = 15%).
    pub max_deviation_bps: u32,
    /// Number of ledger sequence blocks required for automatic cooldown recovery.
    pub cooldown_ledgers: u32,
    /// Whether the circuit breaker monitoring and enforcement is active.
    pub enabled: bool,
}

impl Default for SpotCircuitBreakerConfig {
    fn default() -> Self {
        Self {
            max_deviation_bps: DEFAULT_MAX_SPOT_DEVIATION_BPS,
            cooldown_ledgers: DEFAULT_COOLDOWN_LEDGERS,
            enabled: true,
        }
    }
}

/// Recorded spot price tick for a liquidity pool.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpotPriceTick {
    /// Asset or pool identifier.
    pub pool_id: AssetId,
    /// Spot price scaled at `PRICE_SCALE` (10^7).
    pub last_price: i128,
    /// Ledger sequence number when this tick was recorded.
    pub ledger_sequence: u32,
    /// Ledger timestamp when this tick was recorded.
    pub timestamp: u64,
}

/// Snapshot of the circuit breaker state for an individual pool.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolCircuitBreakerState {
    /// Whether pool trading is currently frozen.
    pub is_frozen: bool,
    /// Ledger sequence number when the freeze occurred (0 if active).
    pub frozen_at_ledger: u32,
    /// Timestamp when the freeze occurred.
    pub frozen_at_timestamp: u64,
    /// Measured price deviation in basis points that triggered the freeze.
    pub breach_deviation_bps: u32,
    /// Baseline spot price prior to the breach.
    pub baseline_price: i128,
    /// Spot price that caused the breach.
    pub breach_price: i128,
    /// Address that manually triggered the freeze (None if tripped automatically).
    pub frozen_by: Option<Address>,
}

impl Default for PoolCircuitBreakerState {
    fn default() -> Self {
        Self {
            is_frozen: false,
            frozen_at_ledger: 0,
            frozen_at_timestamp: 0,
            breach_deviation_bps: 0,
            baseline_price: 0,
            breach_price: 0,
            frozen_by: None,
        }
    }
}

/// Storage keys for the circuit breaker subsystem.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CircuitBreakerStorageKey {
    /// Global circuit breaker configuration.
    Config,
    /// Last recorded spot price tick for a pool: `(LastSpotTick, pool_id)`.
    LastSpotTick(AssetId),
    /// Active circuit breaker state for a pool: `(PoolState, pool_id)`.
    PoolState(AssetId),
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Verify that every field of `cfg` satisfies structural invariants.
pub fn validate_circuit_breaker_config(cfg: &SpotCircuitBreakerConfig) -> Result<(), ContractError> {
    if cfg.max_deviation_bps < MIN_DEVIATION_BPS || cfg.max_deviation_bps > MAX_DEVIATION_BPS_CEILING {
        return Err(ContractError::InvalidCircuitBreakerConfig);
    }
    if cfg.cooldown_ledgers == 0 {
        return Err(ContractError::InvalidCircuitBreakerConfig);
    }
    Ok(())
}

/// Calculate the absolute relative price deviation in basis points between two prices.
///
/// `deviation_bps = (|new_price - baseline_price| * 10,000) / baseline_price`
pub fn calculate_price_deviation_bps(
    baseline_price: i128,
    new_price: i128,
) -> Result<u32, ContractError> {
    if baseline_price <= 0 || new_price <= 0 {
        return Err(ContractError::DivisionByZero);
    }

    let delta = if new_price >= baseline_price {
        new_price - baseline_price
    } else {
        baseline_price - new_price
    };

    let delta_u128 = delta as u128;
    let baseline_u128 = baseline_price as u128;

    let scaled = delta_u128
        .checked_mul(BPS_SCALE as u128)
        .ok_or(ContractError::Overflow)?;

    let dev_bps = scaled / baseline_u128;

    if dev_bps > u32::MAX as u128 {
        return Err(ContractError::Overflow);
    }

    Ok(dev_bps as u32)
}

/// Helper to compute spot price from pool reserves with fixed-point `PRICE_SCALE` (10^7).
///
/// `spot_price = (reserve_b * PRICE_SCALE) / reserve_a`
pub fn compute_spot_price_from_reserves(
    reserve_a: u128,
    reserve_b: u128,
) -> Result<i128, ContractError> {
    if reserve_a == 0 || reserve_b == 0 {
        return Err(ContractError::DivisionByZero);
    }

    let numerator = reserve_b
        .checked_mul(PRICE_SCALE as u128)
        .ok_or(ContractError::Overflow)?;

    let price = numerator / reserve_a;

    if price > i128::MAX as u128 {
        return Err(ContractError::Overflow);
    }

    Ok(price as i128)
}

// ---------------------------------------------------------------------------
// Storage Accessors
// ---------------------------------------------------------------------------

/// Retrieve the active global circuit breaker configuration.
pub fn get_circuit_breaker_config(env: &Env) -> SpotCircuitBreakerConfig {
    env.storage()
        .instance()
        .get(&CircuitBreakerStorageKey::Config)
        .unwrap_or_default()
}

/// Set the global circuit breaker configuration (Admin only).
pub fn set_circuit_breaker_config(
    env: &Env,
    caller: &Address,
    cfg: SpotCircuitBreakerConfig,
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

    validate_circuit_breaker_config(&cfg)?;

    env.storage()
        .instance()
        .set(&CircuitBreakerStorageKey::Config, &cfg);

    env.events().publish(
        (Symbol::new(env, "stellarflow"), Symbol::new(env, "cb_cfg")),
        (cfg.max_deviation_bps, cfg.cooldown_ledgers, cfg.enabled),
    );

    Ok(())
}

/// Retrieve the last recorded spot price tick for `pool_id`.
pub fn get_last_spot_price_tick(env: &Env, pool_id: AssetId) -> Option<SpotPriceTick> {
    env.storage()
        .persistent()
        .get(&CircuitBreakerStorageKey::LastSpotTick(pool_id))
}

/// Save the last recorded spot price tick for `pool_id`.
fn save_spot_price_tick(env: &Env, pool_id: AssetId, tick: &SpotPriceTick) {
    let key = CircuitBreakerStorageKey::LastSpotTick(pool_id);
    env.storage().persistent().set(&key, tick);
    env.storage().persistent().extend_ttl(
        &key,
        crate::storage::PERSISTENT_TTL_THRESHOLD,
        crate::storage::PERSISTENT_TTL_THRESHOLD,
    );
}

/// Retrieve the circuit breaker state for `pool_id`.
pub fn get_pool_circuit_breaker_state(env: &Env, pool_id: AssetId) -> PoolCircuitBreakerState {
    env.storage()
        .persistent()
        .get(&CircuitBreakerStorageKey::PoolState(pool_id))
        .unwrap_or_default()
}

/// Save the circuit breaker state for `pool_id`.
fn save_pool_circuit_breaker_state(
    env: &Env,
    pool_id: AssetId,
    state: &PoolCircuitBreakerState,
) {
    let key = CircuitBreakerStorageKey::PoolState(pool_id);
    env.storage().persistent().set(&key, state);
    env.storage().persistent().extend_ttl(
        &key,
        crate::storage::PERSISTENT_TTL_THRESHOLD,
        crate::storage::PERSISTENT_TTL_THRESHOLD,
    );
}

// ---------------------------------------------------------------------------
// Core Circuit Breaker Logic
// ---------------------------------------------------------------------------

/// Check whether pool trading is currently allowed for `pool_id`.
///
/// Automatically accounts for elapsed cooldown periods (100 ledgers).
pub fn is_pool_trading_allowed(env: &Env, pool_id: AssetId) -> bool {
    let state = get_pool_circuit_breaker_state(env, pool_id);
    if !state.is_frozen {
        return true;
    }

    let cfg = get_circuit_breaker_config(env);
    let cur_ledger = env.ledger().sequence();
    let cooldown_target = state.frozen_at_ledger.saturating_add(cfg.cooldown_ledgers);

    cur_ledger >= cooldown_target
}

/// Monitor and update the spot price tick for a pool, enforcing the deviation limit.
///
/// # Behavior
/// - If the pool is frozen and the 100-ledger cooldown has elapsed, the freeze is
///   automatically lifted, a reset event is emitted, and the new baseline is recorded.
/// - If the pool is frozen and still within cooldown, returns `ContractError::CircuitBreakerTripped`.
/// - If active and deviation relative to the previous tick exceeds `max_deviation_bps` (15%),
///   the pool is frozen, breach telemetry is recorded, and `ContractError::CircuitBreakerTripped` is returned.
/// - If deviation is within limits, the tick is updated and `Ok(())` is returned.
pub fn check_and_update_spot_price(
    env: &Env,
    pool_id: AssetId,
    current_spot_price: i128,
) -> Result<(), ContractError> {
    if current_spot_price <= 0 {
        return Err(ContractError::DivisionByZero);
    }

    let cur_ledger = env.ledger().sequence();
    let cur_time = env.ledger().timestamp();
    let cfg = get_circuit_breaker_config(env);
    let mut state = get_pool_circuit_breaker_state(env, pool_id);

    // 1. Handle currently frozen pool state
    if state.is_frozen {
        let cooldown_target = state.frozen_at_ledger.saturating_add(cfg.cooldown_ledgers);
        if cur_ledger >= cooldown_target {
            // Cooldown period elapsed -> Automatic recovery
            state.is_frozen = false;
            state.frozen_by = None;
            save_pool_circuit_breaker_state(env, pool_id, &state);

            env.events().publish(
                (Symbol::new(env, "stellarflow"), Symbol::new(env, "cb_reset")),
                (pool_id, cur_ledger, symbol_short!("cooldown")),
            );

            // Record current spot price as fresh baseline
            let new_tick = SpotPriceTick {
                pool_id,
                last_price: current_spot_price,
                ledger_sequence: cur_ledger,
                timestamp: cur_time,
            };
            save_spot_price_tick(env, pool_id, &new_tick);
            return Ok(());
        } else {
            // Still in cooldown period
            return Err(ContractError::CircuitBreakerTripped);
        }
    }

    // If disabled, just update tick without deviation tripping
    if !cfg.enabled {
        let tick = SpotPriceTick {
            pool_id,
            last_price: current_spot_price,
            ledger_sequence: cur_ledger,
            timestamp: cur_time,
        };
        save_spot_price_tick(env, pool_id, &tick);
        return Ok(());
    }

    // 2. Check deviation relative to previous block/tick
    if let Some(prev_tick) = get_last_spot_price_tick(env, pool_id) {
        let dev_bps = calculate_price_deviation_bps(prev_tick.last_price, current_spot_price)?;

        if dev_bps > cfg.max_deviation_bps {
            // Deviation limit breached -> Trip circuit breaker!
            state.is_frozen = true;
            state.frozen_at_ledger = cur_ledger;
            state.frozen_at_timestamp = cur_time;
            state.breach_deviation_bps = dev_bps;
            state.baseline_price = prev_tick.last_price;
            state.breach_price = current_spot_price;
            state.frozen_by = None;

            save_pool_circuit_breaker_state(env, pool_id, &state);

            let cooldown_until = cur_ledger.saturating_add(cfg.cooldown_ledgers);
            env.events().publish(
                (Symbol::new(env, "stellarflow"), Symbol::new(env, "cb_trip")),
                (
                    pool_id,
                    prev_tick.last_price,
                    current_spot_price,
                    dev_bps,
                    cfg.max_deviation_bps,
                    cur_ledger,
                    cooldown_until,
                ),
            );

            return Err(ContractError::CircuitBreakerTripped);
        }
    }

    // 3. Deviation is safe -> Update tick
    let tick = SpotPriceTick {
        pool_id,
        last_price: current_spot_price,
        ledger_sequence: cur_ledger,
        timestamp: cur_time,
    };
    save_spot_price_tick(env, pool_id, &tick);

    Ok(())
}

/// Manually freeze trading for `pool_id` (Admin or EmergencyAdmin).
pub fn manual_freeze_pool(
    env: &Env,
    caller: &Address,
    pool_id: AssetId,
) -> Result<(), ContractError> {
    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;

    let is_admin = data.admin == *caller;
    let is_emergency = crate::security::pausable::is_emergency_admin(env, caller);

    if !is_admin && !is_emergency {
        return Err(ContractError::NotAdmin);
    }
    caller.require_auth();

    let cur_ledger = env.ledger().sequence();
    let cur_time = env.ledger().timestamp();
    let last_price = get_last_spot_price_tick(env, pool_id)
        .map(|t| t.last_price)
        .unwrap_or(0);

    let state = PoolCircuitBreakerState {
        is_frozen: true,
        frozen_at_ledger: cur_ledger,
        frozen_at_timestamp: cur_time,
        breach_deviation_bps: 0,
        baseline_price: last_price,
        breach_price: last_price,
        frozen_by: Some(caller.clone()),
    };

    save_pool_circuit_breaker_state(env, pool_id, &state);

    env.events().publish(
        (Symbol::new(env, "stellarflow"), Symbol::new(env, "cb_manual_freeze")),
        (pool_id, caller.clone(), cur_ledger),
    );

    Ok(())
}

/// Manually unfreeze trading for `pool_id` (Admin or EmergencyAdmin).
pub fn manual_unfreeze_pool(
    env: &Env,
    caller: &Address,
    pool_id: AssetId,
) -> Result<(), ContractError> {
    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;

    let is_admin = data.admin == *caller;
    let is_emergency = crate::security::pausable::is_emergency_admin(env, caller);

    if !is_admin && !is_emergency {
        return Err(ContractError::NotAdmin);
    }
    caller.require_auth();

    let mut state = get_pool_circuit_breaker_state(env, pool_id);
    state.is_frozen = false;
    state.frozen_by = None;

    save_pool_circuit_breaker_state(env, pool_id, &state);

    let cur_ledger = env.ledger().sequence();
    env.events().publish(
        (Symbol::new(env, "stellarflow"), Symbol::new(env, "cb_reset")),
        (pool_id, cur_ledger, symbol_short!("manual")),
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Unit Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Events, Ledger, LedgerInfo};
    use soroban_sdk::Env;

    fn setup_env() -> (Env, Address, Address, AssetId) {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let emergency_admin = Address::generate(&env);
        let pool_id: AssetId = 2654435761; // KES pool

        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        env.as_contract(&contract_id, || {
            let data = ContractData {
                admin: admin.clone(),
                value: 0,
                max_fee_ceiling: 0,
            };
            env.storage().instance().set(&DATA_KEY, &data);
            crate::security::pausable::set_emergency_admin(&env, &admin, &emergency_admin).unwrap();
        });

        (env, admin, emergency_admin, pool_id)
    }

    fn set_ledger_seq(env: &Env, seq: u32) {
        let ts = env.ledger().timestamp();
        env.ledger().set(LedgerInfo {
            timestamp: ts,
            protocol_version: env.ledger().protocol_version(),
            sequence_number: seq,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 0,
            min_persistent_entry_ttl: 0,
            max_entry_ttl: u32::MAX,
        });
    }

    #[test]
    fn test_calculate_deviation_bps() {
        let base = 10_000_000i128; // 1.0
        // +10%
        assert_eq!(calculate_price_deviation_bps(base, 11_000_000i128), Ok(1000));
        // -15%
        assert_eq!(calculate_price_deviation_bps(base, 8_500_000i128), Ok(1500));
        // +20% spike
        assert_eq!(calculate_price_deviation_bps(base, 12_000_000i128), Ok(2000));
        // -20% crash
        assert_eq!(calculate_price_deviation_bps(base, 8_000_000i128), Ok(2000));
    }

    #[test]
    fn test_initial_spot_tick_records_without_tripping() {
        let (env, _admin, _emerg, pool_id) = setup_env();
        let cid = env.register_contract(None, crate::TimeLockedUpgradeContract);

        env.as_contract(&cid, || {
            set_ledger_seq(&env, 10);
            let res = check_and_update_spot_price(&env, pool_id, 10_000_000i128);
            assert!(res.is_ok());

            let tick = get_last_spot_price_tick(&env, pool_id).unwrap();
            assert_eq!(tick.last_price, 10_000_000i128);
            assert_eq!(tick.ledger_sequence, 10);
            assert!(is_pool_trading_allowed(&env, pool_id));
        });
    }

    #[test]
    fn test_normal_price_movement_within_limit() {
        let (env, _admin, _emerg, pool_id) = setup_env();
        let cid = env.register_contract(None, crate::TimeLockedUpgradeContract);

        env.as_contract(&cid, || {
            set_ledger_seq(&env, 10);
            check_and_update_spot_price(&env, pool_id, 10_000_000i128).unwrap();

            // +14% at ledger 11 (within 15% limit)
            set_ledger_seq(&env, 11);
            let res = check_and_update_spot_price(&env, pool_id, 11_400_000i128);
            assert!(res.is_ok());
            assert!(is_pool_trading_allowed(&env, pool_id));

            let tick = get_last_spot_price_tick(&env, pool_id).unwrap();
            assert_eq!(tick.last_price, 11_400_000i128);
            assert_eq!(tick.ledger_sequence, 11);
        });
    }

    #[test]
    fn test_price_crash_exceeding_15_percent_trips_breaker() {
        let (env, _admin, _emerg, pool_id) = setup_env();
        let cid = env.register_contract(None, crate::TimeLockedUpgradeContract);

        env.as_contract(&cid, || {
            set_ledger_seq(&env, 10);
            check_and_update_spot_price(&env, pool_id, 10_000_000i128).unwrap();

            // -20% crash (10.0 -> 8.0) at ledger 11 -> Trip!
            set_ledger_seq(&env, 11);
            let res = check_and_update_spot_price(&env, pool_id, 8_000_000i128);
            assert_eq!(res, Err(ContractError::CircuitBreakerTripped));

            let state = get_pool_circuit_breaker_state(&env, pool_id);
            assert!(state.is_frozen);
            assert_eq!(state.frozen_at_ledger, 11);
            assert_eq!(state.breach_deviation_bps, 2000);
            assert_eq!(state.baseline_price, 10_000_000i128);
            assert_eq!(state.breach_price, 8_000_000i128);
            assert!(!is_pool_trading_allowed(&env, pool_id));

            // Subsequent swap at ledger 12 is blocked
            set_ledger_seq(&env, 12);
            let res2 = check_and_update_spot_price(&env, pool_id, 8_000_000i128);
            assert_eq!(res2, Err(ContractError::CircuitBreakerTripped));
        });
    }

    #[test]
    fn test_price_spike_exceeding_15_percent_trips_breaker() {
        let (env, _admin, _emerg, pool_id) = setup_env();
        let cid = env.register_contract(None, crate::TimeLockedUpgradeContract);

        env.as_contract(&cid, || {
            set_ledger_seq(&env, 100);
            check_and_update_spot_price(&env, pool_id, 10_000_000i128).unwrap();

            // +25% spike (10.0 -> 12.5) at ledger 101 -> Trip!
            set_ledger_seq(&env, 101);
            let res = check_and_update_spot_price(&env, pool_id, 12_500_000i128);
            assert_eq!(res, Err(ContractError::CircuitBreakerTripped));

            let state = get_pool_circuit_breaker_state(&env, pool_id);
            assert!(state.is_frozen);
            assert_eq!(state.frozen_at_ledger, 101);
            assert_eq!(state.breach_deviation_bps, 2500);
        });
    }

    #[test]
    fn test_automatic_cooldown_resumes_swaps_after_100_ledgers() {
        let (env, _admin, _emerg, pool_id) = setup_env();
        let cid = env.register_contract(None, crate::TimeLockedUpgradeContract);

        env.as_contract(&cid, || {
            set_ledger_seq(&env, 100);
            check_and_update_spot_price(&env, pool_id, 10_000_000i128).unwrap();

            // Trip at ledger 101
            set_ledger_seq(&env, 101);
            let _ = check_and_update_spot_price(&env, pool_id, 8_000_000i128);
            assert!(!is_pool_trading_allowed(&env, pool_id));

            // Still blocked at ledger 200 (101 + 99 = 200, cooldown is 100)
            set_ledger_seq(&env, 200);
            assert!(!is_pool_trading_allowed(&env, pool_id));
            let blocked = check_and_update_spot_price(&env, pool_id, 8_000_000i128);
            assert_eq!(blocked, Err(ContractError::CircuitBreakerTripped));

            // Cooldown expires at ledger 201 (101 + 100) -> Auto resume!
            set_ledger_seq(&env, 201);
            assert!(is_pool_trading_allowed(&env, pool_id));

            let resumed = check_and_update_spot_price(&env, pool_id, 8_000_000i128);
            assert!(resumed.is_ok());

            let state = get_pool_circuit_breaker_state(&env, pool_id);
            assert!(!state.is_frozen);

            let tick = get_last_spot_price_tick(&env, pool_id).unwrap();
            assert_eq!(tick.last_price, 8_000_000i128);
            assert_eq!(tick.ledger_sequence, 201);
        });
    }

    #[test]
    fn test_manual_unpause_by_admin_resumes_swaps_before_cooldown() {
        let (env, admin, _emerg, pool_id) = setup_env();
        let cid = env.register_contract(None, crate::TimeLockedUpgradeContract);

        env.as_contract(&cid, || {
            set_ledger_seq(&env, 100);
            check_and_update_spot_price(&env, pool_id, 10_000_000i128).unwrap();

            // Trip at ledger 101
            set_ledger_seq(&env, 101);
            let _ = check_and_update_spot_price(&env, pool_id, 8_000_000i128);
            assert!(!is_pool_trading_allowed(&env, pool_id));

            // Admin manual unpause at ledger 105
            set_ledger_seq(&env, 105);
            let unfreeze_res = manual_unfreeze_pool(&env, &admin, pool_id);
            assert!(unfreeze_res.is_ok());
            assert!(is_pool_trading_allowed(&env, pool_id));

            // Swaps resume immediately
            let res = check_and_update_spot_price(&env, pool_id, 8_000_000i128);
            assert!(res.is_ok());
        });
    }

    #[test]
    fn test_manual_unpause_by_emergency_admin_succeeds() {
        let (env, _admin, emerg, pool_id) = setup_env();
        let cid = env.register_contract(None, crate::TimeLockedUpgradeContract);

        env.as_contract(&cid, || {
            set_ledger_seq(&env, 100);
            check_and_update_spot_price(&env, pool_id, 10_000_000i128).unwrap();

            // Trip at ledger 101
            set_ledger_seq(&env, 101);
            let _ = check_and_update_spot_price(&env, pool_id, 8_000_000i128);

            // Emergency Admin manual unpause
            let unfreeze_res = manual_unfreeze_pool(&env, &emerg, pool_id);
            assert!(unfreeze_res.is_ok());
            assert!(is_pool_trading_allowed(&env, pool_id));
        });
    }

    #[test]
    fn test_manual_unpause_by_unauthorized_fails() {
        let (env, _admin, _emerg, pool_id) = setup_env();
        let cid = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let attacker = Address::generate(&env);

        env.as_contract(&cid, || {
            set_ledger_seq(&env, 100);
            check_and_update_spot_price(&env, pool_id, 10_000_000i128).unwrap();

            // Trip at ledger 101
            set_ledger_seq(&env, 101);
            let _ = check_and_update_spot_price(&env, pool_id, 8_000_000i128);

            let unfreeze_res = manual_unfreeze_pool(&env, &attacker, pool_id);
            assert_eq!(unfreeze_res, Err(ContractError::NotAdmin));
        });
    }

    #[test]
    fn test_config_update_and_validation() {
        let (env, admin, _emerg, _pool_id) = setup_env();
        let cid = env.register_contract(None, crate::TimeLockedUpgradeContract);

        env.as_contract(&cid, || {
            // Default config
            let default_cfg = get_circuit_breaker_config(&env);
            assert_eq!(default_cfg.max_deviation_bps, DEFAULT_MAX_SPOT_DEVIATION_BPS);
            assert_eq!(default_cfg.cooldown_ledgers, DEFAULT_COOLDOWN_LEDGERS);
            assert!(default_cfg.enabled);

            // Valid update: 20% deviation, 50 ledgers cooldown
            let new_cfg = SpotCircuitBreakerConfig {
                max_deviation_bps: 2000,
                cooldown_ledgers: 50,
                enabled: true,
            };
            set_circuit_breaker_config(&env, &admin, new_cfg.clone()).unwrap();
            assert_eq!(get_circuit_breaker_config(&env), new_cfg);

            // Invalid update: 0% deviation
            let invalid_cfg1 = SpotCircuitBreakerConfig {
                max_deviation_bps: 0,
                cooldown_ledgers: 50,
                enabled: true,
            };
            assert_eq!(
                set_circuit_breaker_config(&env, &admin, invalid_cfg1),
                Err(ContractError::InvalidCircuitBreakerConfig)
            );

            // Invalid update: >50% deviation
            let invalid_cfg2 = SpotCircuitBreakerConfig {
                max_deviation_bps: 5001,
                cooldown_ledgers: 50,
                enabled: true,
            };
            assert_eq!(
                set_circuit_breaker_config(&env, &admin, invalid_cfg2),
                Err(ContractError::InvalidCircuitBreakerConfig)
            );

            // Invalid update: 0 cooldown ledgers
            let invalid_cfg3 = SpotCircuitBreakerConfig {
                max_deviation_bps: 1500,
                cooldown_ledgers: 0,
                enabled: true,
            };
            assert_eq!(
                set_circuit_breaker_config(&env, &admin, invalid_cfg3),
                Err(ContractError::InvalidCircuitBreakerConfig)
            );
        });
    }

    #[test]
    fn test_compute_spot_price_from_reserves() {
        // Equal reserves: 1000 / 1000 = 1.0 (10^7)
        let p1 = compute_spot_price_from_reserves(1000, 1000).unwrap();
        assert_eq!(p1, 10_000_000i128);

        // 2000 reserve_b / 1000 reserve_a = 2.0 (2 * 10^7)
        let p2 = compute_spot_price_from_reserves(1000, 2000).unwrap();
        assert_eq!(p2, 20_000_000i128);

        // Zero reserve errors
        assert_eq!(
            compute_spot_price_from_reserves(0, 1000),
            Err(ContractError::DivisionByZero)
        );
    }
}
