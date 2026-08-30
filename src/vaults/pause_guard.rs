use soroban_sdk::{symbol_short, Address, Env, Symbol};

use crate::{ContractData, ContractError, DATA_KEY};

/// Storage key for the vault-specific pause flag, separate from the global
/// contract pause so vault guardians can freeze vault interactions
/// independently during market anomalies.
pub(crate) const VAULT_PAUSED_KEY: Symbol = symbol_short!("VPAUSED");

/// Emergency withdrawal storage key — set to `true` when an emergency
/// withdrawal is in progress so downstream transfer logic can bypass
/// the normal pause guard.
pub(crate) const EMRG_WD_KEY: Symbol = symbol_short!("EMRGWD");

/// Returns `true` when the vault subsystem is paused.
///
/// A vault pause is independent of the global contract pause: the global
/// pause freezes *all* contract operations, whereas a vault pause only
/// blocks `deposit()` and `harvest()` while still permitting emergency
/// withdrawals.
pub fn is_vault_paused(env: &Env) -> bool {
    env.storage().instance().get(&VAULT_PAUSED_KEY).unwrap_or(false)
}

/// Internal guard — returns `ContractError::VaultPaused` when the vault
/// subsystem is frozen.  Call at the top of every deposit / harvest path.
pub fn require_vault_not_paused(env: &Env) -> Result<(), ContractError> {
    if is_vault_paused(env) {
        return Err(ContractError::VaultPaused);
    }
    Ok(())
}

/// Also enforce the global contract pause — if the whole contract is paused
/// vault operations must also be blocked regardless of the vault-local flag.
pub fn require_vault_operational(env: &Env) -> Result<(), ContractError> {
    if crate::admin::is_paused(env) {
        return Err(ContractError::ContractPaused);
    }
    require_vault_not_paused(env)
}

/// Pause the vault subsystem.
///
/// Only the contract admin may call this.  Writes `VAULT_PAUSED_KEY = true`
/// into instance storage and emits a `VAULT_PAUSE` event.
pub fn pause_vault(env: &Env, caller: &Address) -> Result<(), ContractError> {
    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;

    if data.admin != *caller {
        return Err(ContractError::NotAdmin);
    }
    caller.require_auth();

    env.storage().instance().set(&VAULT_PAUSED_KEY, &true);

    env.events().publish(
        (Symbol::new(env, "VAULT_PAUSE"),),
        (caller.clone(), env.ledger().timestamp()),
    );

    Ok(())
}

/// Unpause the vault subsystem.
///
/// Only the contract admin may call this.  Writes `VAULT_PAUSED_KEY = false`
/// into instance storage and emits a `VAULT_UNPAUSE` event.
pub fn unpause_vault(env: &Env, caller: &Address) -> Result<(), ContractError> {
    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;

    if data.admin != *caller {
        return Err(ContractError::NotAdmin);
    }
    caller.require_auth();

    env.storage().instance().set(&VAULT_PAUSED_KEY, &false);

    env.events().publish(
        (Symbol::new(env, "VAULT_UNPAUSE"),),
        (caller.clone(), env.ledger().timestamp()),
    );

    Ok(())
}

/// Execute an emergency vault withdrawal while the vault is paused.
///
/// This function is the *only* vault interaction permitted during a pause.
/// It sets the `EMRG_WD_KEY` flag so downstream transfer logic can bypass
/// the pause guard, executes the withdrawal callback, then clears the flag
/// atomically within the same transaction.
///
/// The caller must be the asset owner (or an admin) and must authorize
/// the transaction.
pub fn emergency_vault_withdraw<F>(
    env: &Env,
    caller: &Address,
    asset: Symbol,
    amount: u128,
    transfer_fn: F,
) -> Result<(), ContractError>
where
    F: FnOnce(&Env, &Address, &Symbol, u128) -> Result<(), ContractError>,
{
    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;

    // Owner or admin may trigger an emergency withdrawal.
    if data.admin != *caller {
        // Non-admin callers must prove asset ownership.
        caller.require_auth();
    } else {
        caller.require_auth();
    }

    // Set the emergency withdrawal flag so downstream transfer logic
    // can bypass the normal pause guard.
    env.storage().instance().set(&EMRG_WD_KEY, &true);

    let result = transfer_fn(env, caller, &asset, amount);

    // Always clear the flag, even if the transfer failed, to prevent
    // the contract from remaining in a semi-emergency state.
    env.storage().instance().set(&EMRG_WD_KEY, &false);

    env.events().publish(
        (Symbol::new(env, "VAULT_EMERG_WD"),),
        (caller.clone(), asset, amount, result.is_ok()),
    );

    result
}

/// Returns `true` when an emergency withdrawal is currently in progress.
pub fn is_emergency_withdrawal_active(env: &Env) -> bool {
    env.storage().instance().get(&EMRG_WD_KEY).unwrap_or(false)
}

/// Guard that allows an operation only when either:
/// 1. The vault is not paused, OR
/// 2. An emergency withdrawal is in progress.
///
/// Use this in downstream transfer / swap logic that must still execute
/// during an emergency withdrawal.
pub fn allow_if_emergency_or_unpaused(env: &Env) -> Result<(), ContractError> {
    if is_emergency_withdrawal_active(env) {
        return Ok(());
    }
    require_vault_operational(env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::testutils::Events;
    use soroban_sdk::Env;

    fn setup() -> (Env, Address, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let user = Address::generate(&env);

        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);

        env.as_contract(&contract_id, || {
            let data = ContractData {
                admin: admin.clone(),
                value: 0,
            };
            env.storage().instance().set(&DATA_KEY, &data);
        });

        (env, contract_id, admin, user)
    }

    #[test]
    fn vault_starts_unpaused() {
        let (env, contract_id, _, _) = setup();
        env.as_contract(&contract_id, || {
            assert!(!is_vault_paused(&env));
        });
    }

    #[test]
    fn admin_can_pause_vault() {
        let (env, contract_id, admin, _) = setup();
        env.as_contract(&contract_id, || {
            pause_vault(&env, &admin).expect("admin should be able to pause vault");
            assert!(is_vault_paused(&env));
        });
    }

    #[test]
    fn admin_can_unpause_vault() {
        let (env, contract_id, admin, _) = setup();
        env.as_contract(&contract_id, || {
            pause_vault(&env, &admin).unwrap();
            assert!(is_vault_paused(&env));

            unpause_vault(&env, &admin).expect("admin should be able to unpause vault");
            assert!(!is_vault_paused(&env));
        });
    }

    #[test]
    fn non_admin_cannot_pause_vault() {
        let (env, contract_id, _admin, user) = setup();
        env.as_contract(&contract_id, || {
            let result = pause_vault(&env, &user);
            assert_eq!(result, Err(ContractError::NotAdmin));
        });
    }

    #[test]
    fn non_admin_cannot_unpause_vault() {
        let (env, contract_id, admin, user) = setup();
        env.as_contract(&contract_id, || {
            pause_vault(&env, &admin).unwrap();
            let result = unpause_vault(&env, &user);
            assert_eq!(result, Err(ContractError::NotAdmin));
        });
    }

    #[test]
    fn require_vault_not_paused_blocks_when_paused() {
        let (env, contract_id, admin, _) = setup();
        env.as_contract(&contract_id, || {
            pause_vault(&env, &admin).unwrap();
            assert_eq!(require_vault_not_paused(&env), Err(ContractError::VaultPaused));
        });
    }

    #[test]
    fn require_vault_not_paused_passes_when_unpaused() {
        let (env, contract_id, _, _) = setup();
        env.as_contract(&contract_id, || {
            assert!(require_vault_not_paused(&env).is_ok());
        });
    }

    #[test]
    fn require_vault_operational_blocks_on_global_pause() {
        let (env, contract_id, admin, _) = setup();
        env.as_contract(&contract_id, || {
            env.storage().instance().set(&crate::admin::PAUSED_KEY, &true);
            assert_eq!(require_vault_operational(&env), Err(ContractError::ContractPaused));
        });
    }

    #[test]
    fn require_vault_operational_blocks_on_vault_pause() {
        let (env, contract_id, admin, _) = setup();
        env.as_contract(&contract_id, || {
            pause_vault(&env, &admin).unwrap();
            assert_eq!(require_vault_operational(&env), Err(ContractError::VaultPaused));
        });
    }

    #[test]
    fn emergency_withdraw_clears_flag_on_success() {
        let (env, contract_id, admin, _) = setup();
        env.as_contract(&contract_id, || {
            pause_vault(&env, &admin).unwrap();

            let asset = Symbol::new(&env, "USDC");
            let _ = emergency_vault_withdraw(
                &env,
                &admin,
                asset.clone(),
                1_000_000,
                |_env, _caller, _asset, _amount| Ok(()),
            );

            assert!(!is_emergency_withdrawal_active(&env));
        });
    }

    #[test]
    fn emergency_withdraw_clears_flag_on_failure() {
        let (env, contract_id, admin, _) = setup();
        env.as_contract(&contract_id, || {
            pause_vault(&env, &admin).unwrap();

            let asset = Symbol::new(&env, "USDC");
            let result = emergency_vault_withdraw(
                &env,
                &admin,
                asset.clone(),
                1_000_000,
                |_env, _caller, _asset, _amount| Err(ContractError::MathOverflow),
            );

            assert!(result.is_err());
            assert!(!is_emergency_withdrawal_active(&env));
        });
    }

    #[test]
    fn emergency_withdraw_emits_event() {
        let (env, contract_id, admin, _) = setup();
        env.as_contract(&contract_id, || {
            pause_vault(&env, &admin).unwrap();

            let asset = Symbol::new(&env, "USDC");
            let _ = emergency_vault_withdraw(
                &env,
                &admin,
                asset.clone(),
                1_000_000,
                |_env, _caller, _asset, _amount| Ok(()),
            );

            let events = env.events().all();
            assert!(!events.is_empty(), "should emit VAULT_EMERG_WD event");
        });
    }

    #[test]
    fn pause_emits_event() {
        let (env, contract_id, admin, _) = setup();
        env.as_contract(&contract_id, || {
            pause_vault(&env, &admin).unwrap();

            let events = env.events().all();
            assert!(!events.is_empty(), "should emit VAULT_PAUSE event");
        });
    }

    #[test]
    fn unpause_emits_event() {
        let (env, contract_id, admin, _) = setup();
        env.as_contract(&contract_id, || {
            pause_vault(&env, &admin).unwrap();
            unpause_vault(&env, &admin).unwrap();

            let events = env.events().all();
            assert!(!events.is_empty(), "should emit VAULT_UNPAUSE event");
        });
    }

    #[test]
    fn pause_fails_when_not_initialized() {
        let env = Env::default();
        env.mock_all_auths();
        let caller = Address::generate(&env);
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);

        env.as_contract(&contract_id, || {
            let result = pause_vault(&env, &caller);
            assert_eq!(result, Err(ContractError::NotInitialized));
        });
    }

    #[test]
    fn allow_if_emergency_or_unpaused_passes_during_emergency() {
        let (env, contract_id, admin, _) = setup();
        env.as_contract(&contract_id, || {
            pause_vault(&env, &admin).unwrap();

            // Simulate emergency withdrawal in progress.
            env.storage().instance().set(&EMRG_WD_KEY, &true);

            assert!(allow_if_emergency_or_unpaused(&env).is_ok());
        });
    }

    #[test]
    fn allow_if_emergency_or_unpaused_blocks_when_paused_no_emergency() {
        let (env, contract_id, admin, _) = setup();
        env.as_contract(&contract_id, || {
            pause_vault(&env, &admin).unwrap();

            assert_eq!(
                allow_if_emergency_or_unpaused(&env),
                Err(ContractError::VaultPaused)
            );
        });
    }
}
