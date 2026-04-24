use soroban_sdk::{contracttype, Address, Env, Vec};

// ─────────────────────────────────────────────────────────────────────────────
// Storage Key
// ─────────────────────────────────────────────────────────────────────────────

#[contracttype]
pub enum DataKey {
    Admin,
    Provider(Address),
    ProviderWeight(Address),
    IsPaused,
}

// ─────────────────────────────────────────────────────────────────────────────
// Storage Helpers
// ─────────────────────────────────────────────────────────────────────────────

pub fn _set_admin(env: &Env, admins: &Vec<Address>) {
    env.storage().instance().set(&DataKey::Admin, admins);
}

pub fn _get_admin(env: &Env) -> Vec<Address> {
    env.storage()
        .instance()
        .get(&DataKey::Admin)
        .expect("Admin not set: contract not initialised")
}

pub fn _has_admin(env: &Env) -> bool {
    env.storage().instance().has(&DataKey::Admin)
}

/// Check if a caller is in the authorized admin list.
pub fn _is_authorized(env: &Env, caller: &Address) -> bool {
    env.storage()
        .instance()
        .get::<DataKey, Vec<Address>>(&DataKey::Admin)
        .map(|admins| admins.iter().any(|admin| admin == *caller))
        .unwrap_or(false)
}

pub fn _require_authorized(env: &Env, caller: &Address) {
    if !_is_authorized(env, caller) {
        panic!("Unauthorised: caller is not in the authorized admin list");
    }
}

/// Add an address to the authorized admin list.
pub fn _add_authorized(env: &Env, new_admin: &Address) {
    let mut admins = _get_admin(env);
    // Avoid duplicates
    if !admins.iter().any(|admin| admin == *new_admin) {
        admins.push_back(new_admin.clone());
        _set_admin(env, &admins);
    }
}

/// Remove an address from the authorized admin list.
pub fn _remove_authorized(env: &Env, admin_to_remove: &Address) {
    let admins = _get_admin(env);
    let original_len = admins.len();

    // Build a new Vec without the removed admin (soroban Vec doesn't impl FromIterator)
    let mut filtered = Vec::new(env);
    for admin in admins.iter() {
        if admin != *admin_to_remove {
            filtered.push_back(admin);
        }
    }

    // Only update storage if something was actually removed
    if filtered.len() < original_len {
        _set_admin(env, &filtered);
    }
}

/// Permanently renounce ownership by deleting all admin keys from storage.
///
/// After this call, no address will be authorized as admin and all admin-only
/// functions will be permanently inaccessible. This makes the contract
/// immutable and controlled only by code logic.
pub fn _renounce_ownership(env: &Env) {
    env.storage().instance().remove(&DataKey::Admin);
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

pub fn _set_provider_weight(env: &Env, provider: &Address, weight: u32) {
    env.storage()
        .instance()
        .set(&DataKey::ProviderWeight(provider.clone()), &weight);
}

pub fn _get_provider_weight(env: &Env, provider: &Address) -> u32 {
    env.storage()
        .instance()
        .get::<DataKey, u32>(&DataKey::ProviderWeight(provider.clone()))
        .unwrap_or(0)
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod auth_tests {
    extern crate alloc;
    use super::*;
    use soroban_sdk::{contract, contractimpl};

    #[contract]
    struct TestContract;

    #[contractimpl]
    impl TestContract {}

    fn setup() -> (Env, soroban_sdk::Address, Address) {
        let env = Env::default();
        let contract_id = env.register(TestContract, ());
        let admin = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
        env.as_contract(&contract_id, || {
            let mut admins = Vec::new(&env);
            admins.push_back(admin.clone());
            _set_admin(&env, &admins);
        });
        (env, contract_id, admin)
    }

    // ── Admin tests ───────────────────────────────────────────────────────────

    #[test]
    fn test_is_authorized_true_for_admin() {
        let (env, contract_id, admin) = setup();
        env.as_contract(&contract_id, || {
            assert!(_is_authorized(&env, &admin));
        });
    }

    #[test]
    fn test_is_authorized_false_for_non_admin() {
        let (env, contract_id, _) = setup();
        let other = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
        env.as_contract(&contract_id, || {
            assert!(!_is_authorized(&env, &other));
        });
    }

    #[test]
    fn test_is_authorized_false_when_no_admin_set() {
        let env = Env::default();
        let contract_id = env.register(TestContract, ());
        let random = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
        env.as_contract(&contract_id, || {
            assert!(!_is_authorized(&env, &random));
        });
    }

    #[test]
    fn test_require_authorized_passes_for_admin() {
        let (env, contract_id, admin) = setup();
        env.as_contract(&contract_id, || {
            _require_authorized(&env, &admin); // must not panic
        });
    }

    #[test]
    #[should_panic(expected = "Unauthorised: caller is not in the authorized admin list")]
    fn test_require_authorized_panics_for_non_admin() {
        let (env, contract_id, _) = setup();
        let other = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
        env.as_contract(&contract_id, || {
            _require_authorized(&env, &other);
        });
    }

    #[test]
    fn test_get_admin_returns_correct_addresses() {
        let (env, contract_id, admin) = setup();
        env.as_contract(&contract_id, || {
            let admins = _get_admin(&env);
            assert_eq!(admins.len(), 1);
            assert_eq!(admins.get(0).unwrap(), admin);
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

    #[test]
    fn test_add_authorized_adds_new_admin() {
        let (env, contract_id, admin1) = setup();
        let admin2 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
        env.as_contract(&contract_id, || {
            assert!(_is_authorized(&env, &admin1));
            assert!(!_is_authorized(&env, &admin2));
            
            _add_authorized(&env, &admin2);
            
            assert!(_is_authorized(&env, &admin1));
            assert!(_is_authorized(&env, &admin2));
            
            let admins = _get_admin(&env);
            assert_eq!(admins.len(), 2);
        });
    }

    #[test]
    fn test_add_authorized_prevents_duplicates() {
        let (env, contract_id, admin) = setup();
        env.as_contract(&contract_id, || {
            let admins_before = _get_admin(&env);
            assert_eq!(admins_before.len(), 1);
            
            _add_authorized(&env, &admin);
            
            let admins_after = _get_admin(&env);
            assert_eq!(admins_after.len(), 1); // no duplicate added
        });
    }

    #[test]
    fn test_remove_authorized_removes_admin() {
        let (env, contract_id, admin1) = setup();
        let admin2 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
        env.as_contract(&contract_id, || {
            _add_authorized(&env, &admin2);
            assert_eq!(_get_admin(&env).len(), 2);
            
            _remove_authorized(&env, &admin1);
            
            assert!(!_is_authorized(&env, &admin1));
            assert!(_is_authorized(&env, &admin2));
            assert_eq!(_get_admin(&env).len(), 1);
        });
    }

    #[test]
    fn test_remove_authorized_is_safe_for_nonexistent() {
        let (env, contract_id, _) = setup();
        let nonexistent = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
        env.as_contract(&contract_id, || {
            _remove_authorized(&env, &nonexistent); // must not panic
            assert_eq!(_get_admin(&env).len(), 1);
        });
    }

    #[test]
    fn test_multiple_admins_are_independent() {
        let (env, contract_id, admin1) = setup();
        let admin2 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
        let admin3 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
        env.as_contract(&contract_id, || {
            _add_authorized(&env, &admin2);
            _add_authorized(&env, &admin3);

            assert!(_is_authorized(&env, &admin1));
            assert!(_is_authorized(&env, &admin2));
            assert!(_is_authorized(&env, &admin3));

            _remove_authorized(&env, &admin1);
            assert!(!_is_authorized(&env, &admin1));
            assert!(_is_authorized(&env, &admin2)); // unaffected
            assert!(_is_authorized(&env, &admin3)); // unaffected
        });
    }

    // ── Provider tests ────────────────────────────────────────────────────────

    #[test]
    fn test_add_provider_marks_as_whitelisted() {
        let (env, contract_id, _) = setup();
        let provider = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
        env.as_contract(&contract_id, || {
            assert!(!_is_provider(&env, &provider));
            _add_provider(&env, &provider);
            assert!(_is_provider(&env, &provider));
        });
    }

    #[test]
    fn test_remove_provider_clears_whitelist() {
        let (env, contract_id, _) = setup();
        let provider = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
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
        let provider = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
        env.as_contract(&contract_id, || {
            _remove_provider(&env, &provider); // must not panic
            assert!(!_is_provider(&env, &provider));
        });
    }

    #[test]
    fn test_multiple_providers_are_independent() {
        let (env, contract_id, _) = setup();
        let p1 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
        let p2 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
        let p3 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
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
        let provider = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
        env.as_contract(&contract_id, || {
            _add_provider(&env, &provider);
            _require_provider(&env, &provider); // must not panic
        });
    }

    #[test]
    #[should_panic(expected = "Unauthorised: caller is not a whitelisted provider")]
    fn test_require_provider_panics_for_non_provider() {
        let (env, contract_id, _) = setup();
        let random = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
        env.as_contract(&contract_id, || {
            _require_provider(&env, &random);
        });
    }

    #[test]
    fn test_admin_is_not_auto_whitelisted_as_provider() {
        let (env, contract_id, admin) = setup();
        env.as_contract(&contract_id, || {
            assert!(_is_authorized(&env, &admin));
            assert!(!_is_provider(&env, &admin));
        });
    }

    #[test]
    fn test_set_and_get_provider_weight() {
        let (env, contract_id, _) = setup();
        let provider = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
        env.as_contract(&contract_id, || {
            _add_provider(&env, &provider);
            assert_eq!(_get_provider_weight(&env, &provider), 0);
            
            _set_provider_weight(&env, &provider, 75);
            assert_eq!(_get_provider_weight(&env, &provider), 75);
            
            _set_provider_weight(&env, &provider, 100);
            assert_eq!(_get_provider_weight(&env, &provider), 100);
        });
    }

    #[test]
    fn test_weight_for_nonexistent_provider_is_zero() {
        let (env, contract_id, _) = setup();
        let random = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
        env.as_contract(&contract_id, || {
            assert_eq!(_get_provider_weight(&env, &random), 0);
        });
    }

    // ── Renounce ownership tests ──────────────────────────────────────────────

    #[test]
    fn test_renounce_ownership_removes_all_admins() {
        let (env, contract_id, admin1) = setup();
        let admin2 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
        env.as_contract(&contract_id, || {
            _add_authorized(&env, &admin2);
            assert_eq!(_get_admin(&env).len(), 2);
            assert!(_has_admin(&env));

            _renounce_ownership(&env);

            assert!(!_has_admin(&env));
        });
    }

    #[test]
    fn test_renounce_ownership_makes_is_authorized_false() {
        let (env, contract_id, admin) = setup();
        env.as_contract(&contract_id, || {
            assert!(_is_authorized(&env, &admin));

            _renounce_ownership(&env);

            assert!(!_is_authorized(&env, &admin));
        });
    }
}
