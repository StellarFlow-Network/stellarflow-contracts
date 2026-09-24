//! Staging-phase tester allowlist (admin-pathway gating).
//!
//! # Purpose
//!
//! Exposing administrative functions on public test networks before a contract
//! has been fully audited creates unmitigated configuration security risks.
//! This module adds a **conditional access layer** that can be activated by the
//! contract admin before deploying to a staging environment and deactivated
//! once the audit is complete.
//!
//! When staging mode is **active**:
//! - Only addresses that have been explicitly added to the tester allowlist may
//!   invoke administrative write pathways (`propose_upgrade`, `execute_upgrade`,
//!   `cancel_upgrade`, `set_value`, `set_heartbeat_interval`,
//!   `upsert_node_profile`, `propose_admin_change`,
//!   `propose_ownership_transfer`).
//! - The contract admin is implicitly included; the allowlist supplements —
//!   rather than replaces — the existing `NotAdmin` guard.
//! - Callers not in the allowlist receive [`ContractError::StagingNotAuthorized`].
//!
//! When staging mode is **inactive** (the default), the check is a no-op and
//! all existing access control rules apply unchanged.
//!
//! # Storage
//!
//! A single [`StagingConfig`] value is written to Soroban **instance storage**
//! under [`STAGING_KEY`].  Instance storage was chosen because the flag must
//! survive ledger TTL extensions (persistent) but belongs to contract-wide
//! configuration that should be evicted alongside the contract instance
//! (not leaked into per-address or temporary namespaces).
//!
//! # Allowlist size
//!
//! The allowlist is capped at [`MAX_TESTERS`] entries to bound ledger entry
//! growth and prevent DoS via unbounded allowlist expansion.

use soroban_sdk::{contracttype, Address, Env, Vec};
use crate::{ContractData, ContractError, DATA_KEY, STAGING_KEY};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum number of addresses that may be simultaneously present in the
/// staging tester allowlist.  Keeps the ledger entry size predictable and
/// prevents the admin from inadvertently creating an unbounded allowlist that
/// would inflate transaction fees for every downstream read.
pub const MAX_TESTERS: u32 = 50;

// ── On-ledger data types ──────────────────────────────────────────────────────

/// Snapshot of the staging-phase access configuration stored in instance
/// storage under [`STAGING_KEY`].
///
/// Written atomically as a unit so all fields remain in sync across every
/// ledger write.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct StagingConfig {
    /// When `true`, the staging-phase tester allowlist is enforced on every
    /// administrative write pathway.
    pub active: bool,
    /// Ordered list of addresses that are authorised to invoke administrative
    /// pathways while staging mode is active.  The contract admin is always
    /// implicitly authorised regardless of this list.
    pub testers: Vec<Address>,
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Read the current [`StagingConfig`] from instance storage, or return the
/// safe default (staging disabled, empty allowlist) if it has never been
/// written.
fn load(env: &Env) -> StagingConfig {
    env.storage()
        .instance()
        .get(&STAGING_KEY)
        .unwrap_or_else(|| StagingConfig {
            active: false,
            testers: Vec::new(env),
        })
}

/// Write a [`StagingConfig`] snapshot back to instance storage.
fn save(env: &Env, cfg: &StagingConfig) {
    env.storage().instance().set(&STAGING_KEY, cfg);
}

/// Load the [`ContractData`] record or return [`ContractError::NotInitialized`].
fn load_data(env: &Env) -> Result<ContractData, ContractError> {
    env.storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Activate or deactivate staging mode.
///
/// Only the current contract admin may call this function.
///
/// When `enable` is `true`:
/// - Staging mode is switched on.  All administrative write pathways will
///   enforce the tester allowlist from this ledger forward.
///
/// When `enable` is `false`:
/// - Staging mode is switched off.  The tester allowlist is preserved so it
///   can be reused if staging mode is re-enabled, but it is no longer checked.
///
/// # Errors
///
/// - [`ContractError::NotInitialized`] — contract has not been initialised.
/// - [`ContractError::NotAdmin`] — caller is not the current contract admin.
pub fn set_staging_mode(
    env: &Env,
    admin: &Address,
    enable: bool,
) -> Result<(), ContractError> {
    let data = load_data(env)?;
    if data.admin != *admin {
        return Err(ContractError::NotAdmin);
    }
    admin.require_auth();

    let mut cfg = load(env);
    cfg.active = enable;
    save(env, &cfg);
    Ok(())
}

/// Add an address to the staging tester allowlist.
///
/// Only the current contract admin may call this function.
///
/// Adding an address that is already in the allowlist is a no-op (idempotent).
/// The allowlist size is capped at [`MAX_TESTERS`]; attempting to exceed this
/// limit returns [`ContractError::Overflow`].
///
/// # Errors
///
/// - [`ContractError::NotInitialized`] — contract has not been initialised.
/// - [`ContractError::NotAdmin`] — caller is not the current contract admin.
/// - [`ContractError::Overflow`] — allowlist is already at capacity.
pub fn add_tester(
    env: &Env,
    admin: &Address,
    tester: Address,
) -> Result<(), ContractError> {
    let data = load_data(env)?;
    if data.admin != *admin {
        return Err(ContractError::NotAdmin);
    }
    admin.require_auth();

    let mut cfg = load(env);

    // Idempotent: skip if the tester is already present.
    for i in 0..cfg.testers.len() {
        if cfg.testers.get(i).unwrap() == tester {
            return Ok(());
        }
    }

    if cfg.testers.len() >= MAX_TESTERS {
        return Err(ContractError::Overflow);
    }

    cfg.testers.push_back(tester);
    save(env, &cfg);
    Ok(())
}

/// Remove an address from the staging tester allowlist.
///
/// Only the current contract admin may call this function.
///
/// Removing an address that is not in the allowlist is a no-op (idempotent).
///
/// # Errors
///
/// - [`ContractError::NotInitialized`] — contract has not been initialised.
/// - [`ContractError::NotAdmin`] — caller is not the current contract admin.
pub fn remove_tester(
    env: &Env,
    admin: &Address,
    tester: &Address,
) -> Result<(), ContractError> {
    let data = load_data(env)?;
    if data.admin != *admin {
        return Err(ContractError::NotAdmin);
    }
    admin.require_auth();

    let mut cfg = load(env);
    let mut updated = Vec::new(env);
    for i in 0..cfg.testers.len() {
        let entry = cfg.testers.get(i).unwrap();
        if &entry != tester {
            updated.push_back(entry);
        }
    }
    cfg.testers = updated;
    save(env, &cfg);
    Ok(())
}

/// Return `true` if staging mode is currently active.
///
/// This is a pure read; no authentication is required.
pub fn is_staging_active(env: &Env) -> bool {
    load(env).active
}

/// Return the current [`StagingConfig`] snapshot.
///
/// This is a pure read; no authentication is required.
pub fn get_staging_config(env: &Env) -> StagingConfig {
    load(env)
}

/// **Core access gate** — must be called at the top of every administrative
/// write pathway that should be restricted during the staging phase.
///
/// # Behaviour
///
/// | Staging active | Caller is admin | Caller in allowlist | Outcome                    |
/// |:--------------:|:---------------:|:-------------------:|:---------------------------|
/// | false          | any             | any                 | `Ok(())` — no restriction  |
/// | true           | yes             | any                 | `Ok(())` — admin always OK |
/// | true           | no              | yes                 | `Ok(())` — tester allowed  |
/// | true           | no              | no                  | `Err(StagingNotAuthorized)`|
///
/// The function is intentionally a **pure check** with no side-effects; it
/// does not perform `require_auth` (that remains the caller's responsibility).
///
/// # Errors
///
/// - [`ContractError::StagingNotAuthorized`] — staging mode is active and
///   `caller` is neither the admin nor a registered tester.
pub fn check_staging_access(
    env: &Env,
    caller: &Address,
) -> Result<(), ContractError> {
    let cfg = load(env);

    // Fast path: staging mode is off — nothing to check.
    if !cfg.active {
        return Ok(());
    }

    // The contract admin is always permitted.
    let data = load_data(env)?;
    if data.admin == *caller {
        return Ok(());
    }

    // Linear scan of the tester allowlist.
    for i in 0..cfg.testers.len() {
        if cfg.testers.get(i).unwrap() == *caller {
            return Ok(());
        }
    }

    Err(ContractError::StagingNotAuthorized)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::Env;

    /// Initialise a minimal contract state (only DATA_KEY) so staging helpers
    /// can call `load_data` successfully.
    fn init_contract(env: &Env) -> (Address, Address) {
        let admin = Address::generate(env);
        let treasury = Address::generate(env);
        let data = ContractData {
            admin: admin.clone(),
            value: 0,
            max_fee_ceiling: 0,
        };
        env.storage().instance().set(&DATA_KEY, &data);
        (admin, treasury)
    }

    // ── set_staging_mode ─────────────────────────────────────────────────────

    #[test]
    fn staging_mode_off_by_default() {
        let env = Env::default();
        env.mock_all_auths();
        init_contract(&env);
        assert!(!is_staging_active(&env));
    }

    #[test]
    fn admin_can_enable_staging_mode() {
        let env = Env::default();
        env.mock_all_auths();
        let (admin, _) = init_contract(&env);

        set_staging_mode(&env, &admin, true).expect("admin should be able to enable staging");
        assert!(is_staging_active(&env));
    }

    #[test]
    fn admin_can_disable_staging_mode() {
        let env = Env::default();
        env.mock_all_auths();
        let (admin, _) = init_contract(&env);

        set_staging_mode(&env, &admin, true).unwrap();
        set_staging_mode(&env, &admin, false).expect("admin should be able to disable staging");
        assert!(!is_staging_active(&env));
    }

    #[test]
    fn non_admin_cannot_enable_staging_mode() {
        let env = Env::default();
        env.mock_all_auths();
        let (_admin, _) = init_contract(&env);
        let other = Address::generate(&env);

        let result = set_staging_mode(&env, &other, true);
        assert_eq!(result, Err(ContractError::NotAdmin));
    }

    // ── add_tester / remove_tester ───────────────────────────────────────────

    #[test]
    fn admin_can_add_and_remove_tester() {
        let env = Env::default();
        env.mock_all_auths();
        let (admin, _) = init_contract(&env);
        let tester = Address::generate(&env);

        add_tester(&env, &admin, tester.clone()).expect("add should succeed");
        let cfg = get_staging_config(&env);
        assert_eq!(cfg.testers.len(), 1);
        assert_eq!(cfg.testers.get(0).unwrap(), tester);

        remove_tester(&env, &admin, &tester).expect("remove should succeed");
        let cfg = get_staging_config(&env);
        assert_eq!(cfg.testers.len(), 0);
    }

    #[test]
    fn add_tester_is_idempotent() {
        let env = Env::default();
        env.mock_all_auths();
        let (admin, _) = init_contract(&env);
        let tester = Address::generate(&env);

        add_tester(&env, &admin, tester.clone()).unwrap();
        add_tester(&env, &admin, tester.clone()).unwrap(); // second call is no-op
        assert_eq!(get_staging_config(&env).testers.len(), 1);
    }

    #[test]
    fn remove_tester_is_idempotent() {
        let env = Env::default();
        env.mock_all_auths();
        let (admin, _) = init_contract(&env);
        let tester = Address::generate(&env);

        // Remove a tester that was never added — should not error.
        remove_tester(&env, &admin, &tester).expect("no-op remove should succeed");
        assert_eq!(get_staging_config(&env).testers.len(), 0);
    }

    #[test]
    fn non_admin_cannot_add_tester() {
        let env = Env::default();
        env.mock_all_auths();
        let (_admin, _) = init_contract(&env);
        let other = Address::generate(&env);
        let tester = Address::generate(&env);

        let result = add_tester(&env, &other, tester);
        assert_eq!(result, Err(ContractError::NotAdmin));
    }

    #[test]
    fn non_admin_cannot_remove_tester() {
        let env = Env::default();
        env.mock_all_auths();
        let (admin, _) = init_contract(&env);
        let other = Address::generate(&env);
        let tester = Address::generate(&env);

        add_tester(&env, &admin, tester.clone()).unwrap();
        let result = remove_tester(&env, &other, &tester);
        assert_eq!(result, Err(ContractError::NotAdmin));
    }

    // ── check_staging_access ─────────────────────────────────────────────────

    #[test]
    fn check_passes_when_staging_is_inactive() {
        let env = Env::default();
        env.mock_all_auths();
        let (_admin, _) = init_contract(&env);
        let random = Address::generate(&env);

        // Staging is off by default — any caller should pass.
        check_staging_access(&env, &random)
            .expect("check should pass when staging mode is off");
    }

    #[test]
    fn admin_always_passes_staging_check() {
        let env = Env::default();
        env.mock_all_auths();
        let (admin, _) = init_contract(&env);

        set_staging_mode(&env, &admin, true).unwrap();

        check_staging_access(&env, &admin)
            .expect("admin should always pass the staging check");
    }

    #[test]
    fn authorized_tester_passes_staging_check() {
        let env = Env::default();
        env.mock_all_auths();
        let (admin, _) = init_contract(&env);
        let tester = Address::generate(&env);

        set_staging_mode(&env, &admin, true).unwrap();
        add_tester(&env, &admin, tester.clone()).unwrap();

        check_staging_access(&env, &tester)
            .expect("authorized tester should pass the staging check");
    }

    #[test]
    fn unauthorized_caller_blocked_when_staging_is_active() {
        let env = Env::default();
        env.mock_all_auths();
        let (admin, _) = init_contract(&env);
        let unauthorized = Address::generate(&env);

        set_staging_mode(&env, &admin, true).unwrap();

        let result = check_staging_access(&env, &unauthorized);
        assert_eq!(result, Err(ContractError::StagingNotAuthorized));
    }

    #[test]
    fn removed_tester_is_blocked_after_removal() {
        let env = Env::default();
        env.mock_all_auths();
        let (admin, _) = init_contract(&env);
        let tester = Address::generate(&env);

        set_staging_mode(&env, &admin, true).unwrap();
        add_tester(&env, &admin, tester.clone()).unwrap();

        // Tester is in the allowlist — should pass.
        check_staging_access(&env, &tester).expect("tester should pass before removal");

        remove_tester(&env, &admin, &tester).unwrap();

        // Tester removed — should now be blocked.
        let result = check_staging_access(&env, &tester);
        assert_eq!(result, Err(ContractError::StagingNotAuthorized));
    }

    #[test]
    fn disabling_staging_mode_unblocks_all_callers() {
        let env = Env::default();
        env.mock_all_auths();
        let (admin, _) = init_contract(&env);
        let random = Address::generate(&env);

        set_staging_mode(&env, &admin, true).unwrap();
        // Random address is blocked while staging is active.
        assert_eq!(
            check_staging_access(&env, &random),
            Err(ContractError::StagingNotAuthorized)
        );

        set_staging_mode(&env, &admin, false).unwrap();
        // After staging is disabled, the random address passes.
        check_staging_access(&env, &random)
            .expect("random address should pass after staging mode is disabled");
    }
}
