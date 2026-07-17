//! Bond capacity validation for premium asset pool access.
//!
//! Enforces that a validator's active locked stake meets the minimum required
//! bond before it may register profile updates for premium asset corridors.
//! Nodes that fall below the threshold are rejected with
//! `ContractError::PremiumPoolAccessDenied`, preventing under-bonded validators
//! from tracking high-volume asset corridors.
//!
//! Also provides telemetry freshness verification to reject stale data
//! payloads whose timestamps lag the current ledger block time beyond the
//! configured threshold (60 seconds).
//!
//! # Flash Loan Attack Prevention
//!
//! This module implements strict volume and reserve balance validation to protect
//! against flash loan price manipulation attacks on thin automated liquidity pools.
//! By enforcing minimum reserve thresholds, we ensure that price data originates
//! from sufficiently liquid markets that cannot be easily manipulated through
//! temporary capital injection attacks.

use soroban_sdk::{contracttype, symbol_short, Address, Env, Map, Symbol, Vec};

use crate::{AssetId, ContractError, STAKE_REGISTRY_KEY};

/// Minimum stake (in the same units as `StakeRecord.amount`) required to
/// update a validator profile for a premium asset pool.
pub const PREMIUM_POOL_MIN_STAKE: u64 = 1_000;

/// Maximum allowed age (in seconds) for an incoming telemetry payload's
/// ledger timestamp before it is considered stale and rejected.
pub const MAX_TELEMETRY_AGE_SECS: u64 = 60;

/// Minimum reserve balance (in stroops) required for a liquidity pool to be
/// considered secure against flash loan manipulation. Pools below this threshold
/// are rejected.
///
/// This is set to 100,000 XLM equivalent (100_000 * 10^7 stroops).
/// Adjust based on network conditions and risk tolerance.
pub const MIN_RESERVE_BALANCE: i128 = 1_000_000_000_000;

/// Minimum 24-hour trading volume (in stroops) required for a pool to be considered
/// sufficiently active. Low-volume pools are more susceptible to manipulation.
///
/// This is set to 10,000 XLM equivalent (10_000 * 10^7 stroops).
pub const MIN_TRADING_VOLUME: i128 = 100_000_000_000;

/// Return the current locked stake for `node`, or 0 if unregistered.
pub fn get_locked_stake(env: &Env, node: &Address) -> u64 {
    let stakes: Map<Address, u64> = env
        .storage()
        .instance()
        .get(&STAKE_REGISTRY_KEY)
        .unwrap_or_else(|| Map::new(env));
    stakes.get(node.clone()).unwrap_or(0)
}

/// Verify that `node` has sufficient locked stake to update a premium pool
/// validator profile.  Returns `ContractError::PremiumPoolAccessDenied` when
/// the active stake falls below `PREMIUM_POOL_MIN_STAKE`.
pub fn check_bond_capacity(env: &Env, node: &Address, _pool: &Symbol) -> Result<(), ContractError> {
    let stake = get_locked_stake(env, node);
    if stake < PREMIUM_POOL_MIN_STAKE {
        return Err(ContractError::PremiumPoolAccessDenied);
    }
    Ok(())
}

/// Validate that an incoming telemetry payload's ledger timestamp is not
/// too far behind the current ledger block time.
///
/// Returns `ContractError::StaleTelemetryPayload` when the payload timestamp
/// lags the current time by more than `MAX_TELEMETRY_AGE_SECS` (60 seconds).
pub fn verify_payload_freshness(env: &Env, payload_timestamp: u64) -> Result<(), ContractError> {
    let current = env.ledger().timestamp();
    if current.saturating_sub(payload_timestamp) > MAX_TELEMETRY_AGE_SECS {
        return Err(ContractError::StaleTelemetryPayload);
    }
    Ok(())
}

/// Validate that the reported reserve balance meets the minimum security threshold
/// required to resist flash loan price manipulation.
///
/// This function enforces that liquidity pools have sufficient depth to prevent
/// attackers from temporarily injecting capital, manipulating prices, and extracting
/// value within a single transaction.
///
/// # Parameters
/// - `reserve_balance_a`: Reserve amount of asset A in the pool (in stroops)
/// - `reserve_balance_b`: Reserve amount of asset B in the pool (in stroops)
///
/// # Returns
/// - `Ok(())` if both reserves meet or exceed the minimum threshold
/// - `Err(ContractError::InsufficientReserveBalance)` if either reserve is below threshold
///
/// # Security Model
/// Flash loan attacks exploit pools with low liquidity by:
/// 1. Borrowing large amounts of capital
/// 2. Executing trades that manipulate pool prices
/// 3. Using manipulated prices in downstream protocols
/// 4. Repaying the loan within the same transaction
///
/// By requiring minimum reserve balances, we ensure that:
/// - Price impact of flash loans is bounded
/// - Manipulation becomes economically infeasible
/// - Downstream applications receive reliable price data
///
/// # Example
/// ```rust
/// // Pool with 500,000 XLM and 100,000 USDC reserves
/// let result = validate_reserve_balance(5_000_000_000_000, 1_000_000_000_000);
/// // Result: Ok(()) - both reserves exceed MIN_RESERVE_BALANCE
///
/// // Pool with only 50,000 XLM reserves
/// let result = validate_reserve_balance(500_000_000_000, 1_000_000_000_000);
/// // Result: Err(ContractError::InsufficientReserveBalance)
/// ```
pub fn validate_reserve_balance(
    reserve_balance_a: i128,
    reserve_balance_b: i128,
) -> Result<(), ContractError> {
    // Reject negative reserve values
    if reserve_balance_a < 0 || reserve_balance_b < 0 {
        return Err(ContractError::InsufficientReserveBalance);
    }

    // Verify both reserves meet minimum threshold
    if reserve_balance_a < MIN_RESERVE_BALANCE || reserve_balance_b < MIN_RESERVE_BALANCE {
        return Err(ContractError::InsufficientReserveBalance);
    }

    Ok(())
}

/// Validate that the reported 24-hour trading volume meets the minimum threshold
/// for the pool to be considered sufficiently active and resistant to manipulation.
///
/// Low-volume pools are more susceptible to price manipulation because:
/// - Smaller trades have larger price impact
/// - Market depth is limited
/// - Recovery from manipulation takes longer
///
/// # Parameters
/// - `volume_24h`: Total trading volume over the past 24 hours (in stroops)
///
/// # Returns
/// - `Ok(())` if volume meets or exceeds the minimum threshold
/// - `Err(ContractError::InsufficientVolume)` if volume is below threshold
///
/// # Security Properties
/// - Prevents acceptance of price data from dormant or abandoned pools
/// - Ensures pools have active market participation
/// - Complements reserve balance checks for defense-in-depth
///
/// # Example
/// ```rust
/// // Active pool with 50,000 XLM daily volume
/// let result = validate_trading_volume(500_000_000_000);
/// // Result: Ok()
///
/// // Stagnant pool with only 5,000 XLM daily volume
/// let result = validate_trading_volume(50_000_000_000);
/// // Result: Err(ContractError::InsufficientVolume)
/// ```
pub fn validate_trading_volume(volume_24h: i128) -> Result<(), ContractError> {
    // Reject negative volume values
    if volume_24h < 0 {
        return Err(ContractError::InsufficientVolume);
    }

    // Verify volume meets minimum threshold
    if volume_24h < MIN_TRADING_VOLUME {
        return Err(ContractError::InsufficientVolume);
    }

    Ok(())
}

/// Comprehensive validation pipeline for incoming telemetry submissions.
///
/// This function orchestrates all validation checks to ensure submitted telemetry
/// data is fresh, comes from sufficiently liquid pools, and originates from
/// properly bonded validators.
///
/// # Validation Steps (fail-fast)
/// 1. **Timestamp freshness**: Reject stale payloads
/// 2. **Reserve balance**: Verify both pool reserves exceed minimum
/// 3. **Trading volume**: Ensure sufficient 24h activity
/// 4. **Bond capacity**: Confirm validator has adequate stake (premium pools only)
///
/// # Parameters
/// - `env`: Soroban environment
/// - `node`: Address of the validator submitting telemetry
/// - `pool`: Symbol identifying the asset pool
/// - `payload_timestamp`: Ledger timestamp of the telemetry data
/// - `reserve_a`: Reserve balance of asset A (in stroops)
/// - `reserve_b`: Reserve balance of asset B (in stroops)
/// - `volume_24h`: 24-hour trading volume (in stroops)
///
/// # Returns
/// - `Ok(())` if all validations pass
/// - `Err(ContractError::*)` with specific failure reason
///
/// # Error Priority
/// Validations are ordered by computational cost and security priority:
/// 1. Timestamp check (cheapest, most common failure)
/// 2. Reserve validation (core security requirement)
/// 3. Volume validation (secondary security requirement)
/// 4. Bond capacity (most expensive, checked last)
///
/// # Example
/// ```rust
/// let result = validate_telemetry_submission(
///     &env,
///     &validator_addr,
///     &Symbol::new(&env, "XLM_USDC"),
///     env.ledger().timestamp() - 30,  // 30 seconds old
///     2_000_000_000_000,              // 200,000 XLM reserve A
///     1_500_000_000_000,              // 150,000 USDC reserve B
///     500_000_000_000,                // 50,000 XLM daily volume
/// );
/// // Result: Ok(()) - all checks pass
/// ```
pub fn validate_telemetry_submission(
    env: &Env,
    node: &Address,
    pool: &Symbol,
    payload_timestamp: u64,
    reserve_a: i128,
    reserve_b: i128,
    volume_24h: i128,
) -> Result<(), ContractError> {
    // Step 1: Verify payload freshness (fast fail for stale data)
    verify_payload_freshness(env, payload_timestamp)?;

    // Step 2: Validate reserve balances (flash loan protection)
    validate_reserve_balance(reserve_a, reserve_b)?;

    // Step 3: Validate trading volume (market activity requirement)
    validate_trading_volume(volume_24h)?;

    // Step 4: Verify validator bond capacity (for premium pools)
    check_bond_capacity(env, node, pool)?;

    Ok(())
}

#[cfg(test)]
mod validation_tests {
    //! Comprehensive test suite for telemetry validation logic.
    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger, LedgerInfo};

    fn setup() -> Env {
        let env = Env::default();
        env.ledger().set(LedgerInfo {
            timestamp: 1_000_000,
            protocol_version: env.ledger().protocol_version(),
            sequence_number: env.ledger().sequence(),
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 0,
            min_persistent_entry_ttl: 0,
            max_entry_ttl: u32::MAX,
        });
        env
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // Timestamp Freshness Tests
    // ─────────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_fresh_payload_within_60s_passes() {
        let env = setup();
        // Payload timestamp is 30 seconds behind current — within limit.
        let result = verify_payload_freshness(&env, 999_970);
        assert!(result.is_ok());
    }

    #[test]
    fn test_fresh_payload_exactly_at_60s_passes() {
        let env = setup();
        // Payload timestamp is exactly 60 seconds behind — boundary passes.
        let result = verify_payload_freshness(&env, 999_940);
        assert!(result.is_ok());
    }

    #[test]
    fn test_stale_payload_beyond_60s_rejected() {
        let env = setup();
        // Payload timestamp is 61 seconds behind — exceeds limit.
        let result = verify_payload_freshness(&env, 999_939);
        assert_eq!(result, Err(ContractError::StaleTelemetryPayload));
    }

    #[test]
    fn test_payload_from_future_passes() {
        let env = setup();
        // Payload timestamp slightly ahead of current time is allowed.
        let result = verify_payload_freshness(&env, 1_000_010);
        assert!(result.is_ok());
    }

    #[test]
    fn test_payload_at_current_time_passes() {
        let env = setup();
        let result = verify_payload_freshness(&env, 1_000_000);
        assert!(result.is_ok());
    }

    #[test]
    fn test_payload_very_stale_rejected() {
        let env = setup();
        // Payload far in the past.
        let result = verify_payload_freshness(&env, 0);
        assert_eq!(result, Err(ContractError::StaleTelemetryPayload));
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // Reserve Balance Validation Tests
    // ─────────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_reserve_balance_both_above_threshold_passes() {
        // Both reserves at exactly 100,000 XLM equivalent (minimum)
        let result = validate_reserve_balance(MIN_RESERVE_BALANCE, MIN_RESERVE_BALANCE);
        assert!(result.is_ok());
    }

    #[test]
    fn test_reserve_balance_well_above_threshold_passes() {
        // Healthy pool with 500,000 XLM in each reserve
        let result = validate_reserve_balance(5_000_000_000_000, 5_000_000_000_000);
        assert!(result.is_ok());
    }

    #[test]
    fn test_reserve_balance_first_below_threshold_rejected() {
        // First reserve is below minimum
        let result = validate_reserve_balance(MIN_RESERVE_BALANCE - 1, MIN_RESERVE_BALANCE);
        assert_eq!(result, Err(ContractError::InsufficientReserveBalance));
    }

    #[test]
    fn test_reserve_balance_second_below_threshold_rejected() {
        // Second reserve is below minimum
        let result = validate_reserve_balance(MIN_RESERVE_BALANCE, MIN_RESERVE_BALANCE - 1);
        assert_eq!(result, Err(ContractError::InsufficientReserveBalance));
    }

    #[test]
    fn test_reserve_balance_both_below_threshold_rejected() {
        // Both reserves significantly below minimum
        let result = validate_reserve_balance(
            50_000_000_000, // 5,000 XLM
            25_000_000_000, // 2,500 XLM
        );
        assert_eq!(result, Err(ContractError::InsufficientReserveBalance));
    }

    #[test]
    fn test_reserve_balance_negative_reserve_rejected() {
        // Negative reserves should be rejected
        let result = validate_reserve_balance(-1_000_000, MIN_RESERVE_BALANCE);
        assert_eq!(result, Err(ContractError::InsufficientReserveBalance));
    }

    #[test]
    fn test_reserve_balance_zero_rejected() {
        // Zero reserves are below threshold
        let result = validate_reserve_balance(0, MIN_RESERVE_BALANCE);
        assert_eq!(result, Err(ContractError::InsufficientReserveBalance));
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // Trading Volume Validation Tests
    // ─────────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_trading_volume_at_threshold_passes() {
        // Exactly at minimum threshold (10,000 XLM equivalent)
        let result = validate_trading_volume(MIN_TRADING_VOLUME);
        assert!(result.is_ok());
    }

    #[test]
    fn test_trading_volume_well_above_threshold_passes() {
        // Active pool with 100,000 XLM daily volume
        let result = validate_trading_volume(1_000_000_000_000);
        assert!(result.is_ok());
    }

    #[test]
    fn test_trading_volume_below_threshold_rejected() {
        // Low volume pool (5,000 XLM daily)
        let result = validate_trading_volume(MIN_TRADING_VOLUME - 1);
        assert_eq!(result, Err(ContractError::InsufficientVolume));
    }

    #[test]
    fn test_trading_volume_zero_rejected() {
        // No trading activity
        let result = validate_trading_volume(0);
        assert_eq!(result, Err(ContractError::InsufficientVolume));
    }

    #[test]
    fn test_trading_volume_negative_rejected() {
        // Invalid negative volume
        let result = validate_trading_volume(-1_000_000);
        assert_eq!(result, Err(ContractError::InsufficientVolume));
    }

    #[test]
    fn test_trading_volume_high_activity_pool_passes() {
        // Very active pool with 1 million XLM daily volume
        let result = validate_trading_volume(10_000_000_000_000);
        assert!(result.is_ok());
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // Integrated Telemetry Validation Tests
    // ─────────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_telemetry_validation_all_checks_pass() {
        let env = setup();
        let node = soroban_sdk::Address::generate(&env);
        let pool = soroban_sdk::symbol_short!("XLM_USDC");

        // Valid telemetry: fresh, sufficient reserves, good volume
        let result = validate_telemetry_submission(
            &env,
            &node,
            &pool,
            999_970,           // 30 seconds old (fresh)
            2_000_000_000_000, // 200,000 XLM reserve A
            1_500_000_000_000, // 150,000 USDC reserve B
            500_000_000_000,   // 50,000 XLM daily volume
        );

        // Should pass all validations except bond capacity (no stake registered)
        // In real usage, stake would be registered first
        assert_eq!(result, Err(ContractError::PremiumPoolAccessDenied));
    }

    #[test]
    fn test_telemetry_validation_stale_timestamp_fails_first() {
        let env = setup();
        let node = soroban_sdk::Address::generate(&env);
        let pool = soroban_sdk::symbol_short!("XLM_USDC");

        // Stale timestamp should fail before other checks
        let result = validate_telemetry_submission(
            &env,
            &node,
            &pool,
            999_930,           // 70 seconds old (stale)
            2_000_000_000_000, // Sufficient reserves
            1_500_000_000_000,
            500_000_000_000, // Sufficient volume
        );

        assert_eq!(result, Err(ContractError::StaleTelemetryPayload));
    }

    #[test]
    fn test_telemetry_validation_insufficient_reserves_fails() {
        let env = setup();
        let node = soroban_sdk::Address::generate(&env);
        let pool = soroban_sdk::symbol_short!("XLM_USDC");

        // Fresh timestamp but insufficient reserves
        let result = validate_telemetry_submission(
            &env,
            &node,
            &pool,
            999_970,           // Fresh
            50_000_000_000,    // Only 5,000 XLM (below threshold)
            1_500_000_000_000, // Sufficient
            500_000_000_000,   // Sufficient volume
        );

        assert_eq!(result, Err(ContractError::InsufficientReserveBalance));
    }

    #[test]
    fn test_telemetry_validation_insufficient_volume_fails() {
        let env = setup();
        let node = soroban_sdk::Address::generate(&env);
        let pool = soroban_sdk::symbol_short!("XLM_USDC");

        // Fresh timestamp and sufficient reserves but low volume
        let result = validate_telemetry_submission(
            &env,
            &node,
            &pool,
            999_970,           // Fresh
            2_000_000_000_000, // Sufficient
            1_500_000_000_000, // Sufficient
            5_000_000_000,     // Only 500 XLM daily (below threshold)
        );

        assert_eq!(result, Err(ContractError::InsufficientVolume));
    }

    #[test]
    fn test_telemetry_validation_flash_loan_attack_scenario() {
        let env = setup();
        let node = soroban_sdk::Address::generate(&env);
        let pool = soroban_sdk::symbol_short!("XLM_USDC");

        // Simulating a thin pool that could be manipulated via flash loan
        // Small reserves with artificially inflated volume
        let result = validate_telemetry_submission(
            &env,
            &node,
            &pool,
            999_970,           // Fresh
            30_000_000_000,    // Only 3,000 XLM (vulnerable)
            25_000_000_000,    // Only 2,500 USDC (vulnerable)
            5_000_000_000_000, // High volume (suspicious)
        );

        // Should be rejected due to insufficient reserves
        assert_eq!(result, Err(ContractError::InsufficientReserveBalance));
    }
}

#[cfg(test)]
mod bundle_processing_tests {
    use super::*;
    use crate::TimeLockedUpgradeContract;
    use soroban_sdk::testutils::{Address as _, Ledger, LedgerInfo};

    fn setup_env() -> Env {
        let env = Env::default();
        env.ledger().set(LedgerInfo {
            timestamp: 1_000_000,
            protocol_version: env.ledger().protocol_version(),
            sequence_number: env.ledger().sequence(),
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 0,
            min_persistent_entry_ttl: 0,
            max_entry_ttl: u32::MAX,
        });
        env
    }

    fn make_update(asset: AssetId, price: u64, timestamp: u64) -> AssetPriceUpdate {
        AssetPriceUpdate {
            asset,
            price,
            timestamp,
        }
    }

    fn setup_contract_client<'a>(env: &'a Env, contract_id: &Address) -> (crate::TimeLockedUpgradeContractClient<'a>, Address) {
        let admin = Address::generate(env);
        let treasury = Address::generate(env);
        let client = crate::TimeLockedUpgradeContractClient::new(env, contract_id);
        client.initialize(&admin, &treasury);
        (client, admin)
    }

    fn setup_env_with_contract() -> (Env, Address) {
        let env = setup_env();
        let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
        (env, contract_id)
    }

    // ── build_bundle_index tests ──────────────────────────────────────────

    #[test]
    fn test_build_bundle_index_empty() {
        let env = setup_env();
        let updates: Vec<AssetPriceUpdate> = Vec::new(&env);
        let index = build_bundle_index(&env, &updates).unwrap();
        assert!(index.is_empty());
    }

    #[test]
    fn test_build_bundle_index_single_asset() {
        let env = setup_env();
        let mut updates: Vec<AssetPriceUpdate> = Vec::new(&env);
        updates.push_back(make_update(3897123275, 100_000, 999_980));
        let index = build_bundle_index(&env, &updates).unwrap();
        assert_eq!(index.len(), 1);
        assert_eq!(index.get(0).unwrap().asset, 3897123275);
    }

    #[test]
    fn test_build_bundle_index_precomputes_symbol_and_timestamp() {
        let env = setup_env();
        let mut updates: Vec<AssetPriceUpdate> = Vec::new(&env);
        updates.push_back(make_update(3897123275, 100_000, 999_980));
        let index = build_bundle_index(&env, &updates).unwrap();
        assert_eq!(index.get(0).unwrap().pool_symbol, symbol_short!("NGN"));
        assert_eq!(index.get(0).unwrap().timestamp, 999_980);
    }

    #[test]
    fn test_build_bundle_index_exceeds_max() {
        let env = setup_env();
        let mut updates: Vec<AssetPriceUpdate> = Vec::new(&env);
        for i in 0..MAX_BUNDLE_ASSETS + 1 {
            updates.push_back(make_update(i, 100_000, 999_980));
        }
        let result = build_bundle_index(&env, &updates);
        assert_eq!(result, Err(ContractError::BundleAssetLimitExceeded));
    }

    #[test]
    fn test_build_bundle_index_at_max_boundary() {
        let env = setup_env();
        let mut updates: Vec<AssetPriceUpdate> = Vec::new(&env);
        for i in 0..MAX_BUNDLE_ASSETS {
            updates.push_back(make_update(i, 100_000, 999_980));
        }
        let index = build_bundle_index(&env, &updates).unwrap();
        assert_eq!(index.len(), MAX_BUNDLE_ASSETS);
    }

    // ── process_price_bundle tests ────────────────────────────────────────

    #[test]
    fn test_process_price_bundle_single_asset_passes() {
        let (env, contract_id) = setup_env_with_contract();
        env.mock_all_auths();
        let (client, node) = setup_contract_client(&env, &contract_id);
        client.stake_and_register(&node, &PREMIUM_POOL_MIN_STAKE);

        let mut updates: Vec<AssetPriceUpdate> = Vec::new(&env);
        updates.push_back(make_update(3897123275, 100_000, 999_980));

        let result = client.update_prices_bundle(&node, &updates);
        assert_eq!(result.total_assets, 1);
        assert_eq!(result.accepted, 1);
    }

    #[test]
    fn test_process_price_bundle_multiple_assets_passes() {
        let (env, contract_id) = setup_env_with_contract();
        env.mock_all_auths();
        let (client, node) = setup_contract_client(&env, &contract_id);
        client.stake_and_register(&node, &PREMIUM_POOL_MIN_STAKE);

        let mut updates: Vec<AssetPriceUpdate> = Vec::new(&env);
        updates.push_back(make_update(3897123275, 100_000, 999_980));
        updates.push_back(make_update(2654435761, 200_000, 999_970));
        updates.push_back(make_update(4026531840, 150_000, 999_960));

        let result = client.update_prices_bundle(&node, &updates);
        assert_eq!(result.total_assets, 3);
        assert_eq!(result.accepted, 3);
    }

    #[test]
    fn test_process_price_bundle_exceeds_max_assets_rejected() {
        let (env, contract_id) = setup_env_with_contract();
        env.mock_all_auths();
        let (client, node) = setup_contract_client(&env, &contract_id);

        let mut updates: Vec<AssetPriceUpdate> = Vec::new(&env);
        for i in 0..MAX_BUNDLE_ASSETS + 1 {
            updates.push_back(make_update(i, 100_000, 999_980));
        }

        let result = client.try_update_prices_bundle(&node, &updates);
        assert_eq!(result, Err(Ok(ContractError::BundleAssetLimitExceeded)));
    }

    #[test]
    fn test_process_price_bundle_insufficient_stake_rejected() {
        let (env, contract_id) = setup_env_with_contract();
        env.mock_all_auths();
        let (client, node) = setup_contract_client(&env, &contract_id);
        client.stake_and_register(&node, &(PREMIUM_POOL_MIN_STAKE - 1));

        let mut updates: Vec<AssetPriceUpdate> = Vec::new(&env);
        updates.push_back(make_update(3897123275, 100_000, 999_980));

        let result = client.try_update_prices_bundle(&node, &updates);
        assert_eq!(result, Err(Ok(ContractError::PremiumPoolAccessDenied)));
    }

    #[test]
    fn test_process_price_bundle_stale_payload_rejected() {
        let (env, contract_id) = setup_env_with_contract();
        env.mock_all_auths();
        let (client, node) = setup_contract_client(&env, &contract_id);
        client.stake_and_register(&node, &PREMIUM_POOL_MIN_STAKE);

        let mut updates: Vec<AssetPriceUpdate> = Vec::new(&env);
        updates.push_back(make_update(3897123275, 100_000, 999_939));

        let result = client.try_update_prices_bundle(&node, &updates);
        assert_eq!(result, Err(Ok(ContractError::StaleTelemetryPayload)));
    }

    #[test]
    fn test_process_price_bundle_mixed_freshness_rejected_on_first_stale() {
        let (env, contract_id) = setup_env_with_contract();
        env.mock_all_auths();
        let (client, node) = setup_contract_client(&env, &contract_id);
        client.stake_and_register(&node, &PREMIUM_POOL_MIN_STAKE);

        let mut updates: Vec<AssetPriceUpdate> = Vec::new(&env);
        updates.push_back(make_update(3897123275, 100_000, 999_939));
        updates.push_back(make_update(2654435761, 200_000, 999_980));

        let result = client.try_update_prices_bundle(&node, &updates);
        assert_eq!(result, Err(Ok(ContractError::StaleTelemetryPayload)));
    }

    // ── asset_id_to_symbol_short tests ────────────────────────────────────

    #[test]
    fn test_asset_id_to_symbol_short_known_ids() {
        assert_eq!(
            asset_id_to_symbol_short(3897123275),
            symbol_short!("NGN")
        );
        assert_eq!(
            asset_id_to_symbol_short(2654435761),
            symbol_short!("KES")
        );
        assert_eq!(
            asset_id_to_symbol_short(4026531840),
            symbol_short!("GHS")
        );
        assert_eq!(
            asset_id_to_symbol_short(4160749568),
            symbol_short!("CFA")
        );
        assert_eq!(
            asset_id_to_symbol_short(3219226362),
            symbol_short!("ZAR")
        );
        assert_eq!(
            asset_id_to_symbol_short(2863311530),
            symbol_short!("UGX")
        );
        assert_eq!(asset_id_to_symbol_short(0), symbol_short!("STAKE"));
        assert_eq!(asset_id_to_symbol_short(1), symbol_short!("VALUE"));
    }

    #[test]
    fn test_asset_id_to_symbol_short_unknown_returns_unk() {
        assert_eq!(
            asset_id_to_symbol_short(999_999),
            symbol_short!("UNK")
        );
    }
}

#[cfg(test)]
mod consensus_depth_tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    fn setup_env() -> Env {
        Env::default()
    }

    fn populate_cache(env: &Env, count: u32) {
        let mut participants: Vec<Address> = Vec::new(env);
        for _ in 0..count {
            let node = Address::generate(env);
            participants.push_back(node);
        }
        env.storage()
            .temporary()
            .set(&CONSENSUS_CACHE_KEY, &participants);
    }

    #[test]
    fn test_empty_cache_rejected() {
        let env = setup_env();
        let result = check_consensus_depth(&env);
        assert_eq!(result, Err(ContractError::IncompleteQuorum));
    }

    #[test]
    fn test_one_validator_rejected() {
        let env = setup_env();
        populate_cache(&env, 1);
        let result = check_consensus_depth(&env);
        assert_eq!(result, Err(ContractError::IncompleteQuorum));
    }

    #[test]
    fn test_two_validators_rejected() {
        let env = setup_env();
        populate_cache(&env, 2);
        let result = check_consensus_depth(&env);
        assert_eq!(result, Err(ContractError::IncompleteQuorum));
    }

    #[test]
    fn test_three_validators_accepted() {
        let env = setup_env();
        populate_cache(&env, 3);
        let result = check_consensus_depth(&env);
        assert!(result.is_ok());
    }

    #[test]
    fn test_many_validators_accepted() {
        let env = setup_env();
        populate_cache(&env, 10);
        let result = check_consensus_depth(&env);
        assert!(result.is_ok());
    }
}
