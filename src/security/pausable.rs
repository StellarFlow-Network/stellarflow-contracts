use soroban_sdk::{symbol_short, Address, Env, Symbol, Vec};
use crate::{ContractData, ContractError, DATA_KEY};
use crate::auth;

pub(crate) const EMERGENCY_ADMIN_KEY: Symbol = symbol_short!("EMRGADM");

pub fn set_emergency_admin(
    env: &Env,
    caller: &Address,
    emergency_admin: &Address,
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

    env.storage()
        .instance()
        .set(&EMERGENCY_ADMIN_KEY, emergency_admin);

    env.events().publish(
        (Symbol::new(env, "EMRGADM_SET"),),
        (caller.clone(), emergency_admin.clone()),
    );

    Ok(())
}

pub fn get_emergency_admin(env: &Env) -> Option<Address> {
    env.storage().instance().get(&EMERGENCY_ADMIN_KEY)
}

pub fn is_emergency_admin(env: &Env, addr: &Address) -> bool {
    get_emergency_admin(env)
        .map(|admin| admin == *addr)
        .unwrap_or(false)
}

fn _require_emergency_admin(env: &Env, caller: &Address) -> Result<(), ContractError> {
    if !is_emergency_admin(env, caller) {
        return Err(ContractError::NotEmergencyAdmin);
    }
    Ok(())
}

pub fn emergency_pause(
    env: &Env,
    caller: &Address,
) -> Result<(), ContractError> {
    let _data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;

    _require_emergency_admin(env, caller)?;
    caller.require_auth();

    env.storage()
        .instance()
        .set(&crate::admin::PAUSED_KEY, &true);

    env.events().publish(
        (Symbol::new(env, "EMRG_PAUSE"),),
        (caller.clone(), env.ledger().timestamp()),
    );

    Ok(())
}

pub fn emergency_unpause(
    env: &Env,
    caller: &Address,
    signers: &Vec<Address>,
) -> Result<(), ContractError> {
    let _data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;

    caller.require_auth();

    auth::require_multisig(env, signers)?;

    env.storage()
        .instance()
        .set(&crate::admin::PAUSED_KEY, &false);

    env.events().publish(
        (Symbol::new(env, "EMRG_UNPAUSE"),),
        (caller.clone(), env.ledger().timestamp()),
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Events};
    use soroban_sdk::{Env, IntoVal};

    fn setup() -> (Env, Address, Address, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let emergency_admin = Address::generate(&env);
        let signer = Address::generate(&env);

        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);

        env.as_contract(&contract_id, || {
            let data = ContractData {
                admin: admin.clone(),
                value: 0,
                max_fee_ceiling: 0,
            };
            env.storage().instance().set(&DATA_KEY, &data);
        });

        (env, contract_id, admin, emergency_admin, signer)
    }

    #[test]
    fn test_set_emergency_admin_by_admin_succeeds() {
        let (env, contract_id, admin, emergency_admin, _) = setup();
        env.as_contract(&contract_id, || {
            set_emergency_admin(&env, &admin, &emergency_admin)
                .expect("admin should be able to set emergency admin");

            let stored = get_emergency_admin(&env).expect("should be set");
            assert_eq!(stored, emergency_admin);
            assert!(is_emergency_admin(&env, &emergency_admin));
            assert!(!is_emergency_admin(&env, &admin));
        });
    }

    #[test]
    fn test_set_emergency_admin_fails_for_non_admin() {
        let (env, contract_id, _admin, emergency_admin, _) = setup();
        let non_admin = Address::generate(&env);
        env.as_contract(&contract_id, || {
            let result = set_emergency_admin(&env, &non_admin, &emergency_admin);
            assert_eq!(result, Err(ContractError::NotAdmin));
        });
    }

    #[test]
    fn test_emergency_pause_by_emergency_admin_succeeds() {
        let (env, contract_id, admin, emergency_admin, _) = setup();
        env.as_contract(&contract_id, || {
            set_emergency_admin(&env, &admin, &emergency_admin).unwrap();

            emergency_pause(&env, &emergency_admin)
                .expect("emergency admin should be able to pause");

            let paused: bool = env
                .storage()
                .instance()
                .get(&crate::admin::PAUSED_KEY)
                .unwrap_or(false);
            assert!(paused);
        });
    }

    #[test]
    fn test_emergency_pause_fails_for_non_emergency_admin() {
        let (env, contract_id, admin, _emergency_admin, _) = setup();
        env.as_contract(&contract_id, || {
            let result = emergency_pause(&env, &admin);
            assert_eq!(result, Err(ContractError::NotEmergencyAdmin));
        });
    }

    #[test]
    fn test_emergency_unpause_with_multisig_succeeds() {
        let (env, contract_id, admin, emergency_admin, signer) = setup();
        env.as_contract(&contract_id, || {
            set_emergency_admin(&env, &admin, &emergency_admin).unwrap();
            emergency_pause(&env, &emergency_admin).unwrap();

            let mut signers = Vec::new(&env);
            signers.push_back(admin.clone());
            signers.push_back(signer.clone());

            emergency_unpause(&env, &admin, &signers)
                .expect("multi-sig should be able to unpause");

            let paused: bool = env
                .storage()
                .instance()
                .get(&crate::admin::PAUSED_KEY)
                .unwrap_or(false);
            assert!(!paused);
        });
    }

    #[test]
    fn test_emergency_pause_emits_event() {
        let (env, contract_id, admin, emergency_admin, _) = setup();
        env.as_contract(&contract_id, || {
            set_emergency_admin(&env, &admin, &emergency_admin).unwrap();
            emergency_pause(&env, &emergency_admin).unwrap();

            let events = env.events().all();
            let found = events.iter().any(|e| {
                e.0 == contract_id
                    && e.1 == soroban_sdk::vec![&env, Symbol::new(&env, "EMRG_PAUSE").into_val(&env)]
            });
            assert!(found, "should emit EMRG_PAUSE event");
        });
    }

    #[test]
    fn test_emergency_unpause_emits_event() {
        let (env, contract_id, admin, emergency_admin, signer) = setup();
        env.as_contract(&contract_id, || {
            set_emergency_admin(&env, &admin, &emergency_admin).unwrap();
            emergency_pause(&env, &emergency_admin).unwrap();

            let mut signers = Vec::new(&env);
            signers.push_back(admin.clone());
            signers.push_back(signer.clone());
            emergency_unpause(&env, &admin, &signers).unwrap();

            let events = env.events().all();
            let found = events.iter().any(|e| {
                e.0 == contract_id
                    && e.1 == soroban_sdk::vec![&env, Symbol::new(&env, "EMRG_UNPAUSE").into_val(&env)]
            });
            assert!(found, "should emit EMRG_UNPAUSE event");
        });
    }

    #[test]
    fn test_emergency_pause_fails_when_not_initialized() {
        let env = Env::default();
        env.mock_all_auths();
        let caller = Address::generate(&env);
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);

        env.as_contract(&contract_id, || {
            let result = emergency_pause(&env, &caller);
            assert_eq!(result, Err(ContractError::NotInitialized));
        });
    }
}
