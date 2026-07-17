//! Dedicated instance storage for platform fee tracking (closes #540).
//!
//! Isolates fee variables from general application configuration to simplify
//! financial auditing and reduce state management complexity.

use soroban_sdk::Env;

use crate::auth::_require_authorized;
use crate::types::DataKey;

/// Write the query fee (in stroops). Requires admin authorization.
pub fn set_query_fee(env: &Env, caller: &soroban_sdk::Address, fee: i128) {
    _require_authorized(env, caller);
    env.storage().instance().set(&DataKey::QueryFee, &fee);
}

/// Read the current query fee (in stroops). Returns 0 if not set.
pub fn get_query_fee(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get::<DataKey, i128>(&DataKey::QueryFee)
        .unwrap_or(0)
}

/// Remove the query fee entry from instance storage.
pub fn remove_query_fee(env: &Env, caller: &soroban_sdk::Address) {
    _require_authorized(env, caller);
    env.storage().instance().remove(&DataKey::QueryFee);
}

#[cfg(test)]
mod fees_tests {
    use super::*;
    use soroban_sdk::{contract, contractimpl, Env, Vec};

    #[contract]
    struct TestContract;

    #[contractimpl]
    impl TestContract {}

    fn setup() -> (Env, soroban_sdk::Address, soroban_sdk::Address) {
        let env = Env::default();
        let cid = env.register(TestContract, ());
        let admin =
            <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
        env.as_contract(&cid, || {
            let mut admins = Vec::new(&env);
            admins.push_back(admin.clone());
            crate::auth::_set_admin(&env, &admins);
        });
        (env, cid, admin)
    }

    #[test]
    fn test_get_query_fee_default_zero() {
        let (env, cid, _) = setup();
        env.as_contract(&cid, || {
            assert_eq!(get_query_fee(&env), 0);
        });
    }

    #[test]
    fn test_set_and_get_query_fee() {
        let (env, cid, admin) = setup();
        env.as_contract(&cid, || {
            set_query_fee(&env, &admin, 500);
            assert_eq!(get_query_fee(&env), 500);
        });
    }

    #[test]
    #[should_panic]
    fn test_set_query_fee_requires_auth() {
        let (env, cid, _) = setup();
        let other =
            <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
        env.as_contract(&cid, || {
            set_query_fee(&env, &other, 100);
        });
    }

    #[test]
    fn test_remove_query_fee() {
        let (env, cid, admin) = setup();
        env.as_contract(&cid, || {
            set_query_fee(&env, &admin, 200);
            remove_query_fee(&env, &admin);
            assert_eq!(get_query_fee(&env), 0);
        });
    }
}
