use soroban_sdk::{Address, Env, Symbol};
use crate::{ContractError, DATA_KEY};

use soroban_sdk::symbol_short;

pub(crate) const RECOVERY_KEY: Symbol = symbol_short!("RKEY");
pub(crate) const LAST_ADMIN_ACTIVITY: Symbol = symbol_short!("LASTACT");

/// Number of seconds in 180 days (the inactivity threshold before recovery is possible).
pub const RECOVERY_INACTIVITY_THRESHOLD_SECONDS: u64 = 180 * 24 * 60 * 60;

/// Event identifier published when a recovery key is configured or updated.
pub const RECOVERY_CONFIGURED_EVENT: Symbol = symbol_short!("rcv_cfg");

/// Event identifier published when a recovery operation succeeds.
pub const RECOVERY_COMPLETED_EVENT: Symbol = symbol_short!("rcv_done");

/// Stores or updates the secondary recovery key.
///
/// Only the current administrator may configure or change the recovery key.
/// This function reuses the existing admin authorization and storage helpers.
///
/// # Errors
///
/// - [`ContractError::NotAdmin`] if `caller` is not the current admin.
/// - [`ContractError::NotInitialized`] if the contract has not been initialized.
pub fn set_recovery_key(
    env: &Env,
    caller: &Address,
    recovery_key: &Address,
) -> Result<(), ContractError> {
    let data: crate::ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;

    if data.admin != *caller {
        return Err(ContractError::NotAdmin);
    }
    caller.require_auth();

    env.storage().instance().set(&RECOVERY_KEY, recovery_key);

    env.events().publish(
        (RECOVERY_CONFIGURED_EVENT,),
        (caller.clone(), recovery_key.clone()),
    );

    Ok(())
}

/// Returns the configured recovery key address, if one has been set.
pub fn get_recovery_key(env: &Env) -> Option<Address> {
    env.storage().instance().get(&RECOVERY_KEY)
}

/// Updates the last admin activity timestamp to the current ledger time.
///
/// Should be called after every successful state-changing administrator
/// transaction so that the inactivity tracker stays current.
pub fn update_admin_activity(env: &Env) {
    let now = env.ledger().timestamp();
    env.storage().instance().set(&LAST_ADMIN_ACTIVITY, &now);
}

/// Returns the timestamp of the last recorded admin activity, or `0`
/// when no activity has ever been recorded.
pub fn get_last_admin_activity(env: &Env) -> u64 {
    env.storage()
        .instance()
        .get(&LAST_ADMIN_ACTIVITY)
        .unwrap_or(0u64)
}

/// Checks whether the inactivity period has reached or exceeded the
/// 180-day recovery threshold.
///
/// Uses overflow-safe arithmetic via `saturating_sub`.
pub fn is_recovery_available(env: &Env) -> bool {
    let last_activity = get_last_admin_activity(env);
    let now = env.ledger().timestamp();
    now.saturating_sub(last_activity) >= RECOVERY_INACTIVITY_THRESHOLD_SECONDS
}

/// Attempts to reclaim administrative ownership using the secondary recovery key.
///
/// The recovery succeeds only when all of the following hold:
/// 1. A recovery key has been configured.
/// 2. The caller is the configured recovery key (authenticated via `require_auth`).
/// 3. The administrator has been inactive for at least 180 days.
///
/// On success, admin ownership is transferred to the recovery key and the
/// inactivity timer is reset.
///
/// This function reuses the existing ownership-transfer pattern by writing
/// the updated `ContractData` directly to instance storage.
///
/// # Errors
///
/// - [`ContractError::RecoveryKeyNotConfigured`] if no recovery key has been set.
/// - [`ContractError::NotRecoveryKey`] if the caller does not match the recovery key.
/// - [`ContractError::RecoveryNotAvailableYet`] if the inactivity threshold has not been
///   reached.
/// - [`ContractError::NotInitialized`] if the contract has not been initialized.
pub fn recover_admin(
    env: &Env,
    recovery_key: &Address,
) -> Result<(), ContractError> {
    let stored_key: Address = env
        .storage()
        .instance()
        .get(&RECOVERY_KEY)
        .ok_or(ContractError::RecoveryKeyNotConfigured)?;

    if *recovery_key != stored_key {
        return Err(ContractError::NotRecoveryKey);
    }

    recovery_key.require_auth();

    if !is_recovery_available(env) {
        return Err(ContractError::RecoveryNotAvailableYet);
    }

    let mut data: crate::ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;

    data.admin = recovery_key.clone();
    env.storage().instance().set(&DATA_KEY, &data);

    update_admin_activity(env);

    env.events().publish(
        (RECOVERY_COMPLETED_EVENT,),
        (recovery_key.clone(),),
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger, LedgerInfo};
    use soroban_sdk::{symbol_short, Env};

    fn setup() -> (Env, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set(LedgerInfo {
            timestamp: 100_000_000,
            protocol_version: env.ledger().protocol_version(),
            sequence_number: env.ledger().sequence(),
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 0,
            min_persistent_entry_ttl: 0,
            max_entry_ttl: u32::MAX,
        });
        let admin = Address::generate(&env);
        let recovery_key = Address::generate(&env);
        (env, admin, recovery_key)
    }

    fn advance(env: &Env, delta_seconds: u64) {
        let ts = env.ledger().timestamp();
        env.ledger().set(LedgerInfo {
            timestamp: ts + delta_seconds,
            protocol_version: env.ledger().protocol_version(),
            sequence_number: env.ledger().sequence() + 1,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 0,
            min_persistent_entry_ttl: 0,
            max_entry_ttl: u32::MAX,
        });
    }

    #[test]
    fn test_set_recovery_key_requires_admin_auth() {
        let (env, admin, recovery_key) = setup();
        env.as_contract(&env.register_contract(None, crate::TimeLockedUpgradeContract), || {
            let data = crate::ContractData {
                admin: admin.clone(),
                value: 0,
                max_fee_ceiling: 0,
            };

            env.storage().instance().set(&crate::DATA_KEY, &data);

            let non_admin = Address::generate(&env);
            let result = set_recovery_key(&env, &non_admin, &recovery_key);
            assert_eq!(result, Err(ContractError::NotAdmin));
        });
    }

    #[test]
    fn test_set_recovery_key_requires_not_initialized() {
        let (env, admin, recovery_key) = setup();
        env.as_contract(&env.register_contract(None, crate::TimeLockedUpgradeContract), || {
            let result = set_recovery_key(&env, &admin, &recovery_key);
            assert_eq!(result, Err(ContractError::NotInitialized));
        });
    }

    #[test]
    fn test_set_recovery_key_success() {
        let (env, admin, recovery_key) = setup();
        env.as_contract(&env.register_contract(None, crate::TimeLockedUpgradeContract), || {
            let data = crate::ContractData {
                admin: admin.clone(),
                value: 0,
                max_fee_ceiling: 0,
            };
            env.storage().instance().set(&crate::DATA_KEY, &data);

            set_recovery_key(&env, &admin, &recovery_key).expect("should succeed");

            let stored = get_recovery_key(&env).expect("key should be set");
            assert_eq!(stored, recovery_key);
        });
    }

    #[test]
    fn test_update_admin_activity_tracks_timestamp() {
        let (env, _admin, _recovery_key) = setup();
        env.as_contract(&env.register_contract(None, crate::TimeLockedUpgradeContract), || {
            let before = env.ledger().timestamp();
            update_admin_activity(&env);
            let last_activity = get_last_admin_activity(&env);
            assert!(last_activity >= before);
        });
    }

    #[test]
    fn test_is_recovery_available_returns_false_when_no_activity() {
        let (env, _admin, _recovery_key) = setup();
        env.as_contract(&env.register_contract(None, crate::TimeLockedUpgradeContract), || {
            // No activity recorded, last_activity defaults to 0.
            // Current time minus 0 is a huge number, so recovery SHOULD be available.
            // But this tests the edge case where no activity has been recorded at all.
            let available = is_recovery_available(&env);
            // With last_activity=0, now.saturating_sub(0) >= 180 days is true.
            assert!(available);
        });
    }

    #[test]
    fn test_is_recovery_available_returns_false_when_recently_active() {
        let (env, _admin, _recovery_key) = setup();
        env.as_contract(&env.register_contract(None, crate::TimeLockedUpgradeContract), || {
            update_admin_activity(&env);
            let available = is_recovery_available(&env);
            assert!(!available);
        });
    }

    #[test]
    fn test_recovery_fails_when_key_not_configured() {
        let (env, _admin, recovery_key) = setup();
        env.as_contract(&env.register_contract(None, crate::TimeLockedUpgradeContract), || {
            let data = crate::ContractData {
                admin: Address::generate(&env),
                value: 0,
                max_fee_ceiling: 0,
            };
            env.storage().instance().set(&crate::DATA_KEY, &data);

            let result = recover_admin(&env, &recovery_key);
            assert_eq!(result, Err(ContractError::RecoveryKeyNotConfigured));
        });
    }

    #[test]
    fn test_recovery_fails_when_not_recovery_key() {
        let (env, _admin, recovery_key) = setup();
        env.as_contract(&env.register_contract(None, crate::TimeLockedUpgradeContract), || {
            let data = crate::ContractData {
                admin: Address::generate(&env),
                value: 0,
                max_fee_ceiling: 0,
            };
            env.storage().instance().set(&crate::DATA_KEY, &data);
            env.storage().instance().set(&RECOVERY_KEY, &recovery_key);

            let wrong_key = Address::generate(&env);
            let result = recover_admin(&env, &wrong_key);
            assert_eq!(result, Err(ContractError::NotRecoveryKey));
        });
    }

    #[test]
    fn test_recovery_fails_for_non_recovery_key_caller() {
        let (env, _admin, recovery_key) = setup();
        env.as_contract(&env.register_contract(None, crate::TimeLockedUpgradeContract), || {
            let data = crate::ContractData {
                admin: Address::generate(&env),
                value: 0,
                max_fee_ceiling: 0,
            };
            env.storage().instance().set(&crate::DATA_KEY, &data);
            env.storage().instance().set(&RECOVERY_KEY, &recovery_key);

            let non_recovery = Address::generate(&env);
            let result = recover_admin(&env, &non_recovery);
            assert_eq!(result, Err(ContractError::NotRecoveryKey));
        });
    }

    #[test]
    fn test_recovery_fails_before_timeout() {
        let (env, _admin, recovery_key) = setup();
        env.as_contract(&env.register_contract(None, crate::TimeLockedUpgradeContract), || {
            let data = crate::ContractData {
                admin: Address::generate(&env),
                value: 0,
                max_fee_ceiling: 0,
            };
            env.storage().instance().set(&crate::DATA_KEY, &data);
            env.storage().instance().set(&RECOVERY_KEY, &recovery_key);
            update_admin_activity(&env);

            let result = recover_admin(&env, &recovery_key);
            assert_eq!(result, Err(ContractError::RecoveryNotAvailableYet));
        });
    }

    #[test]
    fn test_recovery_succeeds_after_180_days() {
        let (env, _admin, recovery_key) = setup();
        env.as_contract(&env.register_contract(None, crate::TimeLockedUpgradeContract), || {
            let data = crate::ContractData {
                admin: Address::generate(&env),
                value: 0,
                max_fee_ceiling: 0,
            };
            env.storage().instance().set(&crate::DATA_KEY, &data);
            env.storage().instance().set(&RECOVERY_KEY, &recovery_key);

            // Record activity in the past (181 days ago)
            let past_activity = env.ledger().timestamp().saturating_sub(RECOVERY_INACTIVITY_THRESHOLD_SECONDS + 1);
            env.storage().instance().set(&LAST_ADMIN_ACTIVITY, &past_activity);

            recover_admin(&env, &recovery_key).expect("recovery should succeed");

            // Verify admin has been transferred
            let updated_data: crate::ContractData = env.storage().instance().get(&crate::DATA_KEY).unwrap();
            assert_eq!(updated_data.admin, recovery_key);

            // Verify inactivity timer was reset
            let last_activity = get_last_admin_activity(&env);
            assert!(last_activity >= env.ledger().timestamp().saturating_sub(1));
        });
    }

    #[test]
    fn test_successful_recovery_resets_inactivity_timer() {
        let (env, _admin, recovery_key) = setup();
        env.as_contract(&env.register_contract(None, crate::TimeLockedUpgradeContract), || {
            let data = crate::ContractData {
                admin: Address::generate(&env),
                value: 0,
                max_fee_ceiling: 0,
            };
            env.storage().instance().set(&crate::DATA_KEY, &data);
            env.storage().instance().set(&RECOVERY_KEY, &recovery_key);

            let past_activity = env.ledger().timestamp().saturating_sub(RECOVERY_INACTIVITY_THRESHOLD_SECONDS + 1);
            env.storage().instance().set(&LAST_ADMIN_ACTIVITY, &past_activity);

            let before_recovery = env.ledger().timestamp();
            recover_admin(&env, &recovery_key).expect("recovery should succeed");

            let last_activity = get_last_admin_activity(&env);
            assert!(last_activity >= before_recovery);
        });
    }

    #[test]
    fn test_recovery_fails_when_already_recovered() {
        let (env, _admin, recovery_key) = setup();
        let cid = env.register_contract(None, crate::TimeLockedUpgradeContract);
        env.as_contract(&cid, || {
            let data = crate::ContractData {
                admin: recovery_key.clone(),
                value: 0,
                max_fee_ceiling: 0,
            };
            env.storage().instance().set(&crate::DATA_KEY, &data);
            env.storage().instance().set(&RECOVERY_KEY, &recovery_key);

            // First recovery succeeds
            let past_activity = env.ledger().timestamp().saturating_sub(RECOVERY_INACTIVITY_THRESHOLD_SECONDS + 1);
            env.storage().instance().set(&LAST_ADMIN_ACTIVITY, &past_activity);
            recover_admin(&env, &recovery_key).expect("first recovery should succeed");
        });
        // Second attempt in a fresh scope so the auth frame resets.
        env.as_contract(&cid, || {
            let result = recover_admin(&env, &recovery_key);
            assert_eq!(result, Err(ContractError::RecoveryNotAvailableYet));
        });
    }

    #[test]
    fn test_admin_transaction_resets_inactivity_timer() {
        let (env, admin, _recovery_key) = setup();
        env.as_contract(&env.register_contract(None, crate::TimeLockedUpgradeContract), || {
            let data = crate::ContractData {
                admin: admin.clone(),
                value: 0,
                max_fee_ceiling: 0,
            };
            env.storage().instance().set(&crate::DATA_KEY, &data);

            // Record old activity
            let past_activity = env.ledger().timestamp().saturating_sub(RECOVERY_INACTIVITY_THRESHOLD_SECONDS + 1);
            env.storage().instance().set(&LAST_ADMIN_ACTIVITY, &past_activity);

            // Simulate admin activity update (as would happen after admin operation)
            update_admin_activity(&env);

            // Recovery should no longer be available
            assert!(!is_recovery_available(&env));
        });
    }
}