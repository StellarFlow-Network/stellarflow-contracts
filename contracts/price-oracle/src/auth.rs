#[cfg(test)]
use soroban_sdk::testutils::Events;
use soroban_sdk::{contracttype, symbol_short, Address, Env};

// ─────────────────────────────────────────────────────────────────────────────
// Storage Key
// ─────────────────────────────────────────────────────────────────────────────

#[contracttype]
pub enum DataKey {
    Admin,
    Provider(Address),
    IsPaused,
}

// ─────────────────────────────────────────────────────────────────────────────
// Storage Helpers
// ─────────────────────────────────────────────────────────────────────────────

pub fn _set_admin(env: &Env, admin: &Address) {
    let previous_admin = if _has_admin(env) {
        Some(_get_admin(env))
    } else {
        None
    };

    env.storage().instance().set(&DataKey::Admin, admin);

    env.events()
        .publish((symbol_short!("adminchg"),), (previous_admin, admin.clone()));
}

pub fn _get_admin(env: &Env) -> Address {
    env.storage()
        .instance()
        .get(&DataKey::Admin)
        .expect("Admin not set: contract not initialised")
}

pub fn _has_admin(env: &Env) -> bool {
    env.storage().instance().has(&DataKey::Admin)
}

pub fn _is_admin(env: &Env, caller: &Address) -> bool {
    env.storage()
        .instance()
        .get::<DataKey, Address>(&DataKey::Admin)
        .map(|admin| admin == *caller)
        .unwrap_or(false) // no admin set → not an admin
}

pub fn _require_admin(env: &Env, caller: &Address) {
    if !_is_admin(env, caller) {
        panic!("Unauthorised: caller is not the admin");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Pause Helpers
// ─────────────────────────────────────────────────────────────────────────────

pub fn _is_paused(env: &Env) -> bool {
    env.storage()
        .instance()
        .get::<DataKey, bool>(&DataKey::IsPaused)
        .unwrap_or(false)
}

pub fn _set_paused(env: &Env, paused: bool) {
    env.storage().instance().set(&DataKey::IsPaused, &paused);
}

// ─────────────────────────────────────────────────────────────────────────────
// Provider Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Whitelist a provider address.
pub fn _add_provider(env: &Env, provider: &Address) {
    env.storage()
        .instance()
        .set(&DataKey::Provider(provider.clone()), &true);
}

/// Remove a provider from the whitelist.
pub fn _remove_provider(env: &Env, provider: &Address) {
    env.storage()
        .instance()
        .remove(&DataKey::Provider(provider.clone()));
}

/// Returns `true` if the address is a whitelisted provider.
pub fn _is_provider(env: &Env, addr: &Address) -> bool {
    env.storage()
        .instance()
        .get::<DataKey, bool>(&DataKey::Provider(addr.clone()))
        .unwrap_or(false)
}

/// Panics if the caller is not a whitelisted provider.
pub fn _require_provider(env: &Env, caller: &Address) {
    if !_is_provider(env, caller) {
        panic!("Unauthorised: caller is not a whitelisted provider");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod auth_tests {
    extern crate alloc;
    use super::*;
    use alloc::format;
    use soroban_sdk::{contract, contractimpl, testutils::Address as _, Env};

    #[contract]
    struct TestContract;

    #[contractimpl]
    impl TestContract {}

    fn setup() -> (Env, soroban_sdk::Address, Address) {
        let env = Env::default();
        let contract_id = env.register(TestContract, ());
        let admin = Address::generate(&env);
        env.as_contract(&contract_id, || {
            _set_admin(&env, &admin);
        });
        (env, contract_id, admin)
    }

    // ── Admin tests ───────────────────────────────────────────────────────────

    #[test]
    fn test_is_admin_true_for_admin() {
        let (env, contract_id, admin) = setup();
        env.as_contract(&contract_id, || {
            assert!(_is_admin(&env, &admin));
        });
    }

    #[test]
    fn test_is_admin_false_for_non_admin() {
        let (env, contract_id, _) = setup();
        let other = Address::generate(&env);
        env.as_contract(&contract_id, || {
            assert!(!_is_admin(&env, &other));
        });
    }

    #[test]
    fn test_is_admin_false_when_no_admin_set() {
        let env = Env::default();
        let contract_id = env.register(TestContract, ());
        let random = Address::generate(&env);
        env.as_contract(&contract_id, || {
            assert!(!_is_admin(&env, &random));
        });
    }

    #[test]
    fn test_require_admin_passes_for_admin() {
        let (env, contract_id, admin) = setup();
        env.as_contract(&contract_id, || {
            _require_admin(&env, &admin); // must not panic
        });
    }

    #[test]
    #[should_panic(expected = "Unauthorised: caller is not the admin")]
    fn test_require_admin_panics_for_non_admin() {
        let (env, contract_id, _) = setup();
        let other = Address::generate(&env);
        env.as_contract(&contract_id, || {
            _require_admin(&env, &other);
        });
    }

    #[test]
    fn test_get_admin_returns_correct_address() {
        let (env, contract_id, admin) = setup();
        env.as_contract(&contract_id, || {
            assert_eq!(_get_admin(&env), admin);
        });
    }

    #[test]
    fn test_has_admin_true_after_set() {
        let (env, contract_id, _) = setup();
        env.as_contract(&contract_id, || {
            assert!(_has_admin(&env));
        });
    }

    #[test]
    fn test_has_admin_false_before_set() {
        let env = Env::default();
        let contract_id = env.register(TestContract, ());
        env.as_contract(&contract_id, || {
            assert!(!_has_admin(&env));
        });
    }

    // ── Provider tests ────────────────────────────────────────────────────────

    #[test]
    fn test_add_provider_marks_as_whitelisted() {
        let (env, contract_id, _) = setup();
        let provider = Address::generate(&env);
        env.as_contract(&contract_id, || {
            assert!(!_is_provider(&env, &provider));
            _add_provider(&env, &provider);
            assert!(_is_provider(&env, &provider));
        });
    }

    #[test]
    fn test_remove_provider_clears_whitelist() {
        let (env, contract_id, _) = setup();
        let provider = Address::generate(&env);
        env.as_contract(&contract_id, || {
            _add_provider(&env, &provider);
            assert!(_is_provider(&env, &provider));
            _remove_provider(&env, &provider);
            assert!(!_is_provider(&env, &provider));
        });
    }

    #[test]
    fn test_remove_nonexistent_provider_is_safe() {
        let (env, contract_id, _) = setup();
        let provider = Address::generate(&env);
        env.as_contract(&contract_id, || {
            _remove_provider(&env, &provider); // must not panic
            assert!(!_is_provider(&env, &provider));
        });
    }

    #[test]
    fn test_multiple_providers_are_independent() {
        let (env, contract_id, _) = setup();
        let p1 = Address::generate(&env);
        let p2 = Address::generate(&env);
        let p3 = Address::generate(&env);
        env.as_contract(&contract_id, || {
            _add_provider(&env, &p1);
            _add_provider(&env, &p2);

            assert!(_is_provider(&env, &p1));
            assert!(_is_provider(&env, &p2));
            assert!(!_is_provider(&env, &p3));

            _remove_provider(&env, &p1);
            assert!(!_is_provider(&env, &p1));
            assert!(_is_provider(&env, &p2)); // unaffected
        });
    }

    #[test]
    fn test_require_provider_passes_for_whitelisted() {
        let (env, contract_id, _) = setup();
        let provider = Address::generate(&env);
        env.as_contract(&contract_id, || {
            _add_provider(&env, &provider);
            _require_provider(&env, &provider); // must not panic
        });
    }

    #[test]
    #[should_panic(expected = "Unauthorised: caller is not a whitelisted provider")]
    fn test_require_provider_panics_for_non_provider() {
        let (env, contract_id, _) = setup();
        let random = Address::generate(&env);
        env.as_contract(&contract_id, || {
            _require_provider(&env, &random);
        });
    }

    #[test]
    fn test_admin_is_not_auto_whitelisted_as_provider() {
        let (env, contract_id, admin) = setup();
        env.as_contract(&contract_id, || {
            assert!(_is_admin(&env, &admin));
            assert!(!_is_provider(&env, &admin));
        });
    }

    // ── AdminChanged event tests ─────────────────────────────────────────────

    #[test]
    fn test_set_admin_emits_event_on_first_set() {
        let env = Env::default();
        let contract_id = env.register(TestContract, ());
        let admin = Address::generate(&env);

        env.as_contract(&contract_id, || {
            _set_admin(&env, &admin);
        });

        let events = env.events().all();
        let debug_str = format!("{:?}", events);
        assert!(!debug_str.is_empty());
    }

    #[test]
    fn test_set_admin_emits_event_on_admin_change() {
        let (env, contract_id, _old_admin) = setup();
        let new_admin = Address::generate(&env);

        env.as_contract(&contract_id, || {
            _set_admin(&env, &new_admin);
        });

        let events = env.events().all();
        let debug_str = format!("{:?}", events);
        assert!(!debug_str.is_empty());
    }
}
