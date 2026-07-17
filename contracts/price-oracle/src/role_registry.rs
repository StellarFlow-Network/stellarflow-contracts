//! Context-aware multi-role access control matrix.
//!
//! Implements an explicit (Role x Action) permission matrix on top of
//! the existing admin/provider/council primitives in `auth.rs`.
//!
//! This module layers a named-role permission system on top of the existing
//! admin/provider/council primitives in `auth.rs`. Where `auth.rs` answers
//! "is this address in the admin Vec?", the role registry answers
//! "does this address hold a role that is permitted to perform action X?".
//!
//! ## Design
//!
//! - A `Role` enum names operational responsibilities (e.g. `PriceUpdater`,
//!   `BoundsAdjuster`, `OracleManager`). New roles are added by extending
//!   the enum; existing storage remains unchanged.
//! - `RoleKey::AddressRole(Role, Address)` records whether a specific
//!   address holds a specific role. One storage slot per (role, address)
//!   pair, keeping per-role grants independent from each other.
//! - `RoleKey::RolePermissions(Role)` records the `Vec<Symbol>` of action
//!   names that holders of a role are allowed to perform. Permissions are
//!   data, not code, so the matrix can be reconfigured at runtime by
//!   governance.
//! - `_role_can(role, action)` is the central permission check. It returns
//!   `true` iff the role's permitted-action list contains the queried
//!   action symbol.
//!
//! The existing admin Vec remains the root of trust: only an authorized
//! admin (per `auth::_require_authorized`) may grant/revoke roles or
//! reconfigure a role's permission set. This is a deliberately small,
//! additive layer; it does not replace any existing checks.
use soroban_sdk::{contracttype, panic_with_error, Address, Env, Symbol, Vec};

use crate::Error;

// ─────────────────────────────────────────────────────────────────────────────
// Role enum and storage keys
// ─────────────────────────────────────────────────────────────────────────────

/// Named operational roles. Extend this enum as the protocol grows new
/// administrative responsibilities. Existing role grants are unaffected
/// when new variants are added at the end.
#[contracttype]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// May submit verified price updates for whitelisted assets.
    PriceUpdater,
    /// May adjust per-asset price bounds (floors, deviation caps).
    BoundsAdjuster,
    /// May manage the set of tracked assets and oracle configuration.
    OracleManager,
    /// Verified coordinator node that may trigger the circuit-breaker.
    Coordinator,
}

/// Storage keys local to the role registry. Kept in this module so the
/// existing `auth::DataKey` and `types::DataKey` enums are not modified.
#[contracttype]
pub enum RoleKey {
    /// True if the address holds the role. One slot per (role, address).
    AddressRole(Role, Address),
    /// Action symbols permitted for holders of the role. One slot per role.
    RolePermissions(Role),
}

// ─────────────────────────────────────────────────────────────────────────────
// Role grant / revoke / query
// ─────────────────────────────────────────────────────────────────────────────

/// Grant `role` to `account`. No-op if the account already holds the role.
pub fn _grant_role(env: &Env, role: Role, account: &Address) {
    env.storage()
        .instance()
        .set(&RoleKey::AddressRole(role, account.clone()), &true);
}

/// Revoke `role` from `account`. No-op if the account does not hold the role.
pub fn _revoke_role(env: &Env, role: Role, account: &Address) {
    env.storage()
        .instance()
        .remove(&RoleKey::AddressRole(role, account.clone()));
}

/// Return `true` if `account` currently holds `role`.
pub fn _has_role(env: &Env, role: Role, account: &Address) -> bool {
    env.storage()
        .instance()
        .get::<RoleKey, bool>(&RoleKey::AddressRole(role, account.clone()))
        .unwrap_or(false)
}

/// Panic with `Error::NotAuthorized` if `account` does not hold `role`.
pub fn _require_role(env: &Env, role: Role, account: &Address) {
    if !_has_role(env, role, account) {
        panic_with_error!(env, Error::NotAuthorized);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Permission matrix
// ─────────────────────────────────────────────────────────────────────────────

/// Replace the full set of permitted actions for `role`. Pass an empty Vec
/// to clear the role's permissions without removing the role itself.
pub fn _set_role_permissions(env: &Env, role: Role, actions: &Vec<Symbol>) {
    env.storage()
        .instance()
        .set(&RoleKey::RolePermissions(role), actions);
}

/// Return the current list of permitted actions for `role`. Returns an
/// empty Vec if no permissions have been configured.
pub fn _get_role_permissions(env: &Env, role: Role) -> Vec<Symbol> {
    env.storage()
        .instance()
        .get::<RoleKey, Vec<Symbol>>(&RoleKey::RolePermissions(role))
        .unwrap_or_else(|| Vec::new(env))
}

/// Return `true` if holders of `role` are permitted to perform `action`.
pub fn _role_can(env: &Env, role: Role, action: &Symbol) -> bool {
    let actions = _get_role_permissions(env, role);
    actions.iter().any(|a| a == *action)
}

/// Convenience: panic with `Error::NotAuthorized` unless `account` holds
/// a role that is permitted to perform `action`. Checks all variants of
/// `Role` and returns on the first matching grant.
pub fn _require_can(env: &Env, account: &Address, action: &Symbol) {
    for role in [
        Role::PriceUpdater,
        Role::BoundsAdjuster,
        Role::OracleManager,
    ]
    .iter()
    {
        if _has_role(env, *role, account) && _role_can(env, *role, action) {
            return;
        }
    }
    panic_with_error!(env, Error::NotAuthorized);
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod role_registry_tests {
    use super::*;
    use soroban_sdk::{contract, contractimpl, symbol_short, testutils::Address as _};

    #[contract]
    struct TestContract;

    #[contractimpl]
    impl TestContract {}

    fn setup() -> (Env, Address, Address) {
        let env = Env::default();
        let contract_id = env.register(TestContract, ());
        let user = Address::generate(&env);
        (env, contract_id, user)
    }

    // ── grant / revoke / has ────────────────────────────────────────────────

    #[test]
    fn grant_role_marks_account_as_having_role() {
        let (env, contract_id, user) = setup();
        env.as_contract(&contract_id, || {
            assert!(!_has_role(&env, Role::PriceUpdater, &user));
            _grant_role(&env, Role::PriceUpdater, &user);
            assert!(_has_role(&env, Role::PriceUpdater, &user));
        });
    }

    #[test]
    fn revoke_role_clears_grant() {
        let (env, contract_id, user) = setup();
        env.as_contract(&contract_id, || {
            _grant_role(&env, Role::PriceUpdater, &user);
            assert!(_has_role(&env, Role::PriceUpdater, &user));
            _revoke_role(&env, Role::PriceUpdater, &user);
            assert!(!_has_role(&env, Role::PriceUpdater, &user));
        });
    }

    #[test]
    fn revoking_nonexistent_role_is_safe() {
        let (env, contract_id, user) = setup();
        env.as_contract(&contract_id, || {
            _revoke_role(&env, Role::BoundsAdjuster, &user); // must not panic
            assert!(!_has_role(&env, Role::BoundsAdjuster, &user));
        });
    }

    #[test]
    fn grants_for_different_roles_are_independent() {
        let (env, contract_id, user) = setup();
        env.as_contract(&contract_id, || {
            _grant_role(&env, Role::PriceUpdater, &user);
            assert!(_has_role(&env, Role::PriceUpdater, &user));
            assert!(!_has_role(&env, Role::BoundsAdjuster, &user));
            assert!(!_has_role(&env, Role::OracleManager, &user));
        });
    }

    #[test]
    fn grants_for_different_accounts_are_independent() {
        let (env, contract_id, user_a) = setup();
        let user_b = Address::generate(&env);
        env.as_contract(&contract_id, || {
            _grant_role(&env, Role::OracleManager, &user_a);
            assert!(_has_role(&env, Role::OracleManager, &user_a));
            assert!(!_has_role(&env, Role::OracleManager, &user_b));
        });
    }

    #[test]
    #[should_panic]
    fn require_role_panics_without_grant() {
        let (env, contract_id, user) = setup();
        env.as_contract(&contract_id, || {
            _require_role(&env, Role::PriceUpdater, &user);
        });
    }

    #[test]
    fn require_role_passes_with_grant() {
        let (env, contract_id, user) = setup();
        env.as_contract(&contract_id, || {
            _grant_role(&env, Role::PriceUpdater, &user);
            _require_role(&env, Role::PriceUpdater, &user); // must not panic
        });
    }

    // ── permission matrix ───────────────────────────────────────────────────

    #[test]
    fn role_with_no_permissions_returns_empty() {
        let (env, contract_id, _) = setup();
        env.as_contract(&contract_id, || {
            let perms = _get_role_permissions(&env, Role::PriceUpdater);
            assert_eq!(perms.len(), 0);
        });
    }

    #[test]
    fn set_role_permissions_overwrites_existing() {
        let (env, contract_id, _) = setup();
        env.as_contract(&contract_id, || {
            let mut first = Vec::new(&env);
            first.push_back(symbol_short!("update"));
            first.push_back(symbol_short!("submit"));
            _set_role_permissions(&env, Role::PriceUpdater, &first);
            assert_eq!(_get_role_permissions(&env, Role::PriceUpdater).len(), 2);

            let mut second = Vec::new(&env);
            second.push_back(symbol_short!("submit"));
            _set_role_permissions(&env, Role::PriceUpdater, &second);
            assert_eq!(_get_role_permissions(&env, Role::PriceUpdater).len(), 1);
        });
    }

    #[test]
    fn role_can_true_for_listed_action() {
        let (env, contract_id, _) = setup();
        env.as_contract(&contract_id, || {
            let mut perms = Vec::new(&env);
            perms.push_back(symbol_short!("update"));
            _set_role_permissions(&env, Role::PriceUpdater, &perms);

            assert!(_role_can(
                &env,
                Role::PriceUpdater,
                &symbol_short!("update")
            ));
        });
    }

    #[test]
    fn role_can_false_for_unlisted_action() {
        let (env, contract_id, _) = setup();
        env.as_contract(&contract_id, || {
            let mut perms = Vec::new(&env);
            perms.push_back(symbol_short!("update"));
            _set_role_permissions(&env, Role::PriceUpdater, &perms);

            assert!(!_role_can(
                &env,
                Role::PriceUpdater,
                &symbol_short!("revoke")
            ));
        });
    }

    #[test]
    fn role_can_false_when_no_permissions_set() {
        let (env, contract_id, _) = setup();
        env.as_contract(&contract_id, || {
            assert!(!_role_can(
                &env,
                Role::OracleManager,
                &symbol_short!("manage")
            ));
        });
    }

    // ── require_can ─────────────────────────────────────────────────────────

    #[test]
    fn require_can_passes_when_role_grants_action() {
        let (env, contract_id, user) = setup();
        env.as_contract(&contract_id, || {
            let mut perms = Vec::new(&env);
            perms.push_back(symbol_short!("update"));
            _set_role_permissions(&env, Role::PriceUpdater, &perms);
            _grant_role(&env, Role::PriceUpdater, &user);

            _require_can(&env, &user, &symbol_short!("update")); // must not panic
        });
    }

    #[test]
    #[should_panic]
    fn require_can_panics_without_grant() {
        let (env, contract_id, user) = setup();
        env.as_contract(&contract_id, || {
            let mut perms = Vec::new(&env);
            perms.push_back(symbol_short!("update"));
            _set_role_permissions(&env, Role::PriceUpdater, &perms);
            // user is NOT granted the role
            _require_can(&env, &user, &symbol_short!("update"));
        });
    }

    #[test]
    #[should_panic]
    fn require_can_panics_when_role_held_but_action_not_in_matrix() {
        let (env, contract_id, user) = setup();
        env.as_contract(&contract_id, || {
            let mut perms = Vec::new(&env);
            perms.push_back(symbol_short!("update"));
            _set_role_permissions(&env, Role::PriceUpdater, &perms);
            _grant_role(&env, Role::PriceUpdater, &user);

            _require_can(&env, &user, &symbol_short!("destroy"));
        });
    }
}
