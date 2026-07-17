//! # Circuit-Breaker Protocol (`admin.rs`)
//!
//! During severe market crashes or systemic liquidity infrastructure drops the
//! oracle must be able to instantly stop serving price data for high-volatility
//! asset pairs so that downstream protocols are not poisoned by toxic reads.
//!
//! ## Design
//!
//! * **Coordinator role** — a distinct role granted by an admin to trusted
//!   off-chain nodes (relayers, sentinel bots, governance multi-sigs).  Only
//!   addresses that hold this role may trip or reset the circuit-breaker.
//! * **Global flag** — a single boolean stored in persistent instance storage
//!   (`DataKey::CircuitBreakerActive`).  When `true`, every price query for a
//!   high-volatility asset returns `ContractError::CircuitBreakerActive`.
//! * **Per-asset flag** — a per-symbol boolean
//!   (`DataKey::CircuitBreakerPairedAsset(symbol)`) that lets coordinators
//!   quarantine individual pairs without halting the entire oracle.
//! * **Audit trail** — the timestamp and the triggering coordinator address are
//!   stored so that governance can replay the event history.
//!
//! ## Caller Flow
//!
//! ```
//! admin  → register_circuit_breaker_coordinator(admin, coordinator_addr)
//!
//! coordinator → trip_circuit_breaker(coordinator)          // global halt
//!            → trip_circuit_breaker_for_asset(coordinator, asset)  // per-pair
//!            → reset_circuit_breaker(coordinator)          // lift global halt
//!            → reset_circuit_breaker_for_asset(coordinator, asset) // lift per-pair
//!
//! anyone → is_circuit_breaker_active()                     // read-only query
//!        → is_asset_circuit_breaker_active(asset)
//!        → get_circuit_breaker_info()
//! ```

use soroban_sdk::{panic_with_error, Address, Env, Symbol, Vec};

use crate::auth::DataKey;
use crate::role_registry::{Role, _grant_role, _has_role, _revoke_role};
use crate::ContractError;

// ─────────────────────────────────────────────────────────────────────────────
// Public information struct returned by `get_circuit_breaker_info`
// ─────────────────────────────────────────────────────────────────────────────

/// Snapshot of the circuit-breaker state returned by
/// [`get_circuit_breaker_info`].
#[soroban_sdk::contracttype]
#[derive(Clone)]
pub struct CircuitBreakerInfo {
    /// Whether the global circuit-breaker flag is currently active.
    pub is_active: bool,
    /// Ledger timestamp at which the circuit-breaker was last tripped, or 0.
    pub tripped_at: u64,
    /// Address of the coordinator that last tripped the breaker, or the zero
    /// address if the breaker has never been tripped.
    pub tripped_by: Option<Address>,
    /// Number of currently registered coordinator nodes.
    pub coordinator_count: u32,
}

// ─────────────────────────────────────────────────────────────────────────────
// Internal helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Return the list of registered coordinator addresses.
pub fn _get_coordinators(env: &Env) -> Vec<Address> {
    env.storage()
        .instance()
        .get::<DataKey, Vec<Address>>(&DataKey::CircuitBreakerCoordinators)
        .unwrap_or_else(|| Vec::new(env))
}

/// Overwrite the coordinator list.
fn _set_coordinators(env: &Env, coordinators: &Vec<Address>) {
    env.storage()
        .instance()
        .set(&DataKey::CircuitBreakerCoordinators, coordinators);
}

/// Return `true` when the global circuit-breaker flag is set.
pub fn _is_circuit_breaker_active(env: &Env) -> bool {
    env.storage()
        .instance()
        .get::<DataKey, bool>(&DataKey::CircuitBreakerActive)
        .unwrap_or(false)
}

/// Activate or deactivate the global circuit-breaker flag.
fn _set_circuit_breaker_active(env: &Env, active: bool) {
    env.storage()
        .instance()
        .set(&DataKey::CircuitBreakerActive, &active);
}

/// Return `true` when the per-asset circuit-breaker flag is set.
pub fn _is_asset_circuit_breaker_active(env: &Env, asset: &Symbol) -> bool {
    env.storage()
        .instance()
        .get::<DataKey, bool>(&DataKey::CircuitBreakerPairedAsset(asset.clone()))
        .unwrap_or(false)
}

/// Enforce that either the global or the per-asset circuit-breaker is inactive.
///
/// Called at the top of every price-read function.  If either flag is set the
/// function panics with `ContractError::CircuitBreakerActive`.
pub fn _require_circuit_breaker_inactive(env: &Env, asset: &Symbol) {
    if _is_circuit_breaker_active(env) || _is_asset_circuit_breaker_active(env, asset) {
        panic_with_error!(env, ContractError::CircuitBreakerActive);
    }
}

/// Verify that `caller` holds the `Coordinator` role; panic otherwise.
fn _require_coordinator(env: &Env, caller: &Address) {
    if !_has_role(env, Role::Coordinator, caller) {
        panic_with_error!(env, ContractError::NotCoordinator);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Admin — coordinator management
// ─────────────────────────────────────────────────────────────────────────────

/// Register a new coordinator node.
///
/// Only an authorized admin may call this.  Once registered, the `coordinator`
/// address may trip and reset the circuit-breaker.
///
/// # Arguments
/// * `admin`       — A currently authorized admin address.
/// * `coordinator` — Address to grant the `Coordinator` role.
///
/// # Errors
/// * `ContractError::Unauthorized` — `admin` is not an authorized admin.
pub fn register_circuit_breaker_coordinator(
    env: &Env,
    admin: &Address,
    coordinator: &Address,
) -> Result<(), ContractError> {
    admin.require_auth();
    crate::auth::_require_authorized(env, admin);

    // Idempotent: do nothing if already registered.
    if _has_role(env, Role::Coordinator, coordinator) {
        return Ok(());
    }

    _grant_role(env, Role::Coordinator, coordinator);

    // Keep the explicit list for enumeration (health dashboards, governance).
    let mut coordinators = _get_coordinators(env);
    coordinators.push_back(coordinator.clone());
    _set_coordinators(env, &coordinators);

    env.events().publish(
        (Symbol::new(env, "cb_coordinator_added"),),
        (admin.clone(), coordinator.clone()),
    );

    Ok(())
}

/// Revoke coordinator privileges from an address.
///
/// Only an authorized admin may call this.
///
/// # Arguments
/// * `admin`       — A currently authorized admin address.
/// * `coordinator` — Address whose `Coordinator` role is to be removed.
///
/// # Errors
/// * `ContractError::Unauthorized` — `admin` is not an authorized admin.
pub fn remove_circuit_breaker_coordinator(
    env: &Env,
    admin: &Address,
    coordinator: &Address,
) -> Result<(), ContractError> {
    admin.require_auth();
    crate::auth::_require_authorized(env, admin);

    _revoke_role(env, Role::Coordinator, coordinator);

    // Remove from the enumeration list.
    let old = _get_coordinators(env);
    let mut updated: Vec<Address> = Vec::new(env);
    for c in old.iter() {
        if c != *coordinator {
            updated.push_back(c);
        }
    }
    _set_coordinators(env, &updated);

    env.events().publish(
        (Symbol::new(env, "cb_coordinator_removed"),),
        (admin.clone(), coordinator.clone()),
    );

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Coordinator — trip / reset (global)
// ─────────────────────────────────────────────────────────────────────────────

/// Trip the global circuit-breaker, instantly blocking all price reads.
///
/// Only a verified coordinator node may call this.  The coordinator's address
/// and the current ledger timestamp are stored for audit purposes.
///
/// # Arguments
/// * `coordinator` — Address holding the `Coordinator` role.
///
/// # Errors
/// * `ContractError::NotCoordinator`          — caller lacks the role.
/// * `ContractError::CircuitBreakerAlreadyActive` — already tripped.
pub fn trip_circuit_breaker(
    env: &Env,
    coordinator: &Address,
) -> Result<(), ContractError> {
    coordinator.require_auth();
    _require_coordinator(env, coordinator);

    if _is_circuit_breaker_active(env) {
        return Err(ContractError::CircuitBreakerAlreadyActive);
    }

    _set_circuit_breaker_active(env, true);

    let now = env.ledger().timestamp();
    env.storage()
        .instance()
        .set(&DataKey::CircuitBreakerTrippedAt, &now);
    env.storage()
        .instance()
        .set(&DataKey::CircuitBreakerTrippedBy, coordinator);

    env.events().publish(
        (Symbol::new(env, "circuit_breaker_tripped"),),
        (coordinator.clone(), now),
    );

    Ok(())
}

/// Reset (lift) the global circuit-breaker, re-enabling price reads.
///
/// Only a verified coordinator node may call this.
///
/// # Arguments
/// * `coordinator` — Address holding the `Coordinator` role.
///
/// # Errors
/// * `ContractError::NotCoordinator`      — caller lacks the role.
/// * `ContractError::CircuitBreakerNotActive` — nothing to reset.
pub fn reset_circuit_breaker(
    env: &Env,
    coordinator: &Address,
) -> Result<(), ContractError> {
    coordinator.require_auth();
    _require_coordinator(env, coordinator);

    if !_is_circuit_breaker_active(env) {
        return Err(ContractError::CircuitBreakerNotActive);
    }

    _set_circuit_breaker_active(env, false);

    env.events().publish(
        (Symbol::new(env, "circuit_breaker_reset"),),
        (coordinator.clone(), env.ledger().timestamp()),
    );

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Coordinator — trip / reset (per-asset)
// ─────────────────────────────────────────────────────────────────────────────

/// Trip the circuit-breaker for a specific high-volatility asset pair.
///
/// Only a verified coordinator node may call this.  The per-asset flag is
/// independent of the global flag — it quarantines a single pairing without
/// halting the entire oracle.
///
/// # Arguments
/// * `coordinator` — Address holding the `Coordinator` role.
/// * `asset`       — The asset symbol to quarantine (e.g., `Symbol::new(env, "NGNGHS")`).
///
/// # Errors
/// * `ContractError::NotCoordinator` — caller lacks the role.
pub fn trip_circuit_breaker_for_asset(
    env: &Env,
    coordinator: &Address,
    asset: &Symbol,
) -> Result<(), ContractError> {
    coordinator.require_auth();
    _require_coordinator(env, coordinator);

    env.storage()
        .instance()
        .set(&DataKey::CircuitBreakerPairedAsset(asset.clone()), &true);

    env.events().publish(
        (Symbol::new(env, "cb_asset_tripped"),),
        (coordinator.clone(), asset.clone(), env.ledger().timestamp()),
    );

    Ok(())
}

/// Reset the circuit-breaker for a specific asset pair.
///
/// Only a verified coordinator node may call this.
///
/// # Arguments
/// * `coordinator` — Address holding the `Coordinator` role.
/// * `asset`       — The asset symbol to un-quarantine.
///
/// # Errors
/// * `ContractError::NotCoordinator` — caller lacks the role.
pub fn reset_circuit_breaker_for_asset(
    env: &Env,
    coordinator: &Address,
    asset: &Symbol,
) -> Result<(), ContractError> {
    coordinator.require_auth();
    _require_coordinator(env, coordinator);

    env.storage()
        .instance()
        .remove(&DataKey::CircuitBreakerPairedAsset(asset.clone()));

    env.events().publish(
        (Symbol::new(env, "cb_asset_reset"),),
        (coordinator.clone(), asset.clone(), env.ledger().timestamp()),
    );

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Read-only queries
// ─────────────────────────────────────────────────────────────────────────────

/// Return `true` when the global circuit-breaker flag is active.
pub fn is_circuit_breaker_active(env: &Env) -> bool {
    _is_circuit_breaker_active(env)
}

/// Return `true` when the per-asset circuit-breaker flag is active for `asset`.
pub fn is_asset_circuit_breaker_active(env: &Env, asset: &Symbol) -> bool {
    _is_asset_circuit_breaker_active(env, asset)
}

/// Return a snapshot of the circuit-breaker state for monitoring dashboards.
pub fn get_circuit_breaker_info(env: &Env) -> CircuitBreakerInfo {
    let is_active = _is_circuit_breaker_active(env);
    let tripped_at: u64 = env
        .storage()
        .instance()
        .get(&DataKey::CircuitBreakerTrippedAt)
        .unwrap_or(0);
    let tripped_by: Option<Address> = env
        .storage()
        .instance()
        .get(&DataKey::CircuitBreakerTrippedBy);
    let coordinator_count = _get_coordinators(env).len();

    CircuitBreakerInfo {
        is_active,
        tripped_at,
        tripped_by,
        coordinator_count,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Env};

    // Minimal stub contract so we can invoke `as_contract`.
    soroban_sdk::contract!(stub_contract);

    fn setup() -> (Env, Address, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(stub_contract, ());
        let admin = Address::generate(&env);
        let coordinator = Address::generate(&env);

        env.as_contract(&contract_id, || {
            // Bootstrap admin list so `_require_authorized` passes.
            let mut admins = Vec::new(&env);
            admins.push_back(admin.clone());
            crate::auth::_set_admin(&env, &admins);
        });

        (env, contract_id, admin, coordinator)
    }

    // ── Coordinator registration ──────────────────────────────────────────

    #[test]
    fn register_coordinator_grants_role() {
        let (env, contract_id, admin, coordinator) = setup();
        env.as_contract(&contract_id, || {
            assert!(!_has_role(&env, Role::Coordinator, &coordinator));
            register_circuit_breaker_coordinator(&env, &admin, &coordinator)
                .expect("should succeed");
            assert!(_has_role(&env, Role::Coordinator, &coordinator));
            assert_eq!(_get_coordinators(&env).len(), 1);
        });
    }

    #[test]
    fn register_coordinator_is_idempotent() {
        let (env, contract_id, admin, coordinator) = setup();
        env.as_contract(&contract_id, || {
            register_circuit_breaker_coordinator(&env, &admin, &coordinator).unwrap();
            register_circuit_breaker_coordinator(&env, &admin, &coordinator).unwrap();
            // Still only one entry in the list.
            assert_eq!(_get_coordinators(&env).len(), 1);
        });
    }

    #[test]
    fn remove_coordinator_revokes_role() {
        let (env, contract_id, admin, coordinator) = setup();
        env.as_contract(&contract_id, || {
            register_circuit_breaker_coordinator(&env, &admin, &coordinator).unwrap();
            remove_circuit_breaker_coordinator(&env, &admin, &coordinator)
                .expect("should succeed");
            assert!(!_has_role(&env, Role::Coordinator, &coordinator));
            assert_eq!(_get_coordinators(&env).len(), 0);
        });
    }

    // ── Global trip / reset ───────────────────────────────────────────────

    #[test]
    fn trip_circuit_breaker_sets_active_flag() {
        let (env, contract_id, admin, coordinator) = setup();
        env.as_contract(&contract_id, || {
            register_circuit_breaker_coordinator(&env, &admin, &coordinator).unwrap();
            assert!(!is_circuit_breaker_active(&env));
            trip_circuit_breaker(&env, &coordinator).expect("should succeed");
            assert!(is_circuit_breaker_active(&env));
        });
    }

    #[test]
    fn trip_circuit_breaker_records_audit_trail() {
        let (env, contract_id, admin, coordinator) = setup();
        env.as_contract(&contract_id, || {
            register_circuit_breaker_coordinator(&env, &admin, &coordinator).unwrap();
            trip_circuit_breaker(&env, &coordinator).unwrap();
            let info = get_circuit_breaker_info(&env);
            assert!(info.is_active);
            assert!(info.tripped_by.is_some());
            assert_eq!(info.tripped_by.unwrap(), coordinator);
        });
    }

    #[test]
    fn trip_circuit_breaker_twice_returns_error() {
        let (env, contract_id, admin, coordinator) = setup();
        env.as_contract(&contract_id, || {
            register_circuit_breaker_coordinator(&env, &admin, &coordinator).unwrap();
            trip_circuit_breaker(&env, &coordinator).unwrap();
            let result = trip_circuit_breaker(&env, &coordinator);
            assert_eq!(result, Err(ContractError::CircuitBreakerAlreadyActive));
        });
    }

    #[test]
    fn unregistered_coordinator_cannot_trip() {
        let (env, contract_id, _admin, coordinator) = setup();
        env.as_contract(&contract_id, || {
            // coordinator has no role
            let result = trip_circuit_breaker(&env, &coordinator);
            assert_eq!(result, Err(ContractError::NotCoordinator));
        });
    }

    #[test]
    fn reset_circuit_breaker_clears_flag() {
        let (env, contract_id, admin, coordinator) = setup();
        env.as_contract(&contract_id, || {
            register_circuit_breaker_coordinator(&env, &admin, &coordinator).unwrap();
            trip_circuit_breaker(&env, &coordinator).unwrap();
            reset_circuit_breaker(&env, &coordinator).expect("should succeed");
            assert!(!is_circuit_breaker_active(&env));
        });
    }

    #[test]
    fn reset_circuit_breaker_when_not_active_returns_error() {
        let (env, contract_id, admin, coordinator) = setup();
        env.as_contract(&contract_id, || {
            register_circuit_breaker_coordinator(&env, &admin, &coordinator).unwrap();
            let result = reset_circuit_breaker(&env, &coordinator);
            assert_eq!(result, Err(ContractError::CircuitBreakerNotActive));
        });
    }

    // ── Per-asset trip / reset ────────────────────────────────────────────

    #[test]
    fn trip_asset_circuit_breaker_quarantines_pair() {
        let (env, contract_id, admin, coordinator) = setup();
        env.as_contract(&contract_id, || {
            register_circuit_breaker_coordinator(&env, &admin, &coordinator).unwrap();
            let asset = Symbol::new(&env, "NGNGHS");
            assert!(!is_asset_circuit_breaker_active(&env, &asset));
            trip_circuit_breaker_for_asset(&env, &coordinator, &asset).unwrap();
            assert!(is_asset_circuit_breaker_active(&env, &asset));
        });
    }

    #[test]
    fn reset_asset_circuit_breaker_lifts_quarantine() {
        let (env, contract_id, admin, coordinator) = setup();
        env.as_contract(&contract_id, || {
            register_circuit_breaker_coordinator(&env, &admin, &coordinator).unwrap();
            let asset = Symbol::new(&env, "NGNGHS");
            trip_circuit_breaker_for_asset(&env, &coordinator, &asset).unwrap();
            reset_circuit_breaker_for_asset(&env, &coordinator, &asset).unwrap();
            assert!(!is_asset_circuit_breaker_active(&env, &asset));
        });
    }

    #[test]
    fn asset_and_global_flags_are_independent() {
        let (env, contract_id, admin, coordinator) = setup();
        env.as_contract(&contract_id, || {
            register_circuit_breaker_coordinator(&env, &admin, &coordinator).unwrap();
            let asset_a = Symbol::new(&env, "NGNGHS");
            let asset_b = Symbol::new(&env, "KESZAR");
            trip_circuit_breaker_for_asset(&env, &coordinator, &asset_a).unwrap();
            // asset_b and the global flag are unaffected.
            assert!(!is_asset_circuit_breaker_active(&env, &asset_b));
            assert!(!is_circuit_breaker_active(&env));
        });
    }

    // ── require_circuit_breaker_inactive ─────────────────────────────────

    #[test]
    #[should_panic]
    fn require_inactive_panics_when_global_active() {
        let (env, contract_id, admin, coordinator) = setup();
        env.as_contract(&contract_id, || {
            register_circuit_breaker_coordinator(&env, &admin, &coordinator).unwrap();
            trip_circuit_breaker(&env, &coordinator).unwrap();
            let asset = Symbol::new(&env, "NGNGHS");
            _require_circuit_breaker_inactive(&env, &asset);
        });
    }

    #[test]
    #[should_panic]
    fn require_inactive_panics_when_asset_active() {
        let (env, contract_id, admin, coordinator) = setup();
        env.as_contract(&contract_id, || {
            register_circuit_breaker_coordinator(&env, &admin, &coordinator).unwrap();
            let asset = Symbol::new(&env, "NGNGHS");
            trip_circuit_breaker_for_asset(&env, &coordinator, &asset).unwrap();
            _require_circuit_breaker_inactive(&env, &asset);
        });
    }

    #[test]
    fn require_inactive_passes_when_both_clear() {
        let (env, contract_id, _admin, _coordinator) = setup();
        env.as_contract(&contract_id, || {
            let asset = Symbol::new(&env, "NGNGHS");
            // Must not panic.
            _require_circuit_breaker_inactive(&env, &asset);
        });
    }
}
