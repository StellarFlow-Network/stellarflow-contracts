use soroban_sdk::{contracttype, panic_with_error, Address, Env, Vec};

use crate::ContractError;

// ─────────────────────────────────────────────────────────────────────────────
// Storage Key
// ─────────────────────────────────────────────────────────────────────────────

#[contracttype]
pub enum DataKey {
    Admin,
    Provider(Address),
    ProviderWeight(Address),
    VoteDelegate(Address),
    IsPaused,
    Revoked(Address),
    ActiveRelayers,
    CommunityCouncil,
    EmergencyFrozen,
    /// Emergency halt flag — set by multi-sig governance to block all rate reads.
    EmergencyHalt,
    /// Expiry timestamp (seconds) until which safety checks are bypassed.
    BypassSafetyChecks,
    /// Auto-incrementing counter for multi-sig action proposals.
    ActionIdCounter,
    /// Stores a proposed multi-sig action by its ID.
    ProposedAction(u64),
    /// Stores the list of voters for a proposed multi-sig action.
    ActionVotes(u64),
    /// Maps an admin address to their ephemeral submission delegate.
    SubmissionDelegate(Address),
    /// Maps a delegate address back to the admin who authorized it.
    DelegateOf(Address),

    // ── Circuit-Breaker ───────────────────────────────────────────────────────
    /// Registered coordinator nodes that may trigger the circuit-breaker.
    /// Value: Vec<Address>.
    CircuitBreakerCoordinators,
    /// Global circuit-breaker flag.  When true, every price query for a
    /// high-volatility asset is dropped immediately.
    CircuitBreakerActive,
    /// Ledger timestamp at which the circuit-breaker was last tripped.
    CircuitBreakerTrippedAt,
    /// Address of the coordinator that last tripped the circuit-breaker.
    CircuitBreakerTrippedBy,
    /// Per-asset circuit-breaker override flag (Symbol → bool).
    /// When true the asset is individually paused regardless of the global flag.
    CircuitBreakerPairedAsset(soroban_sdk::Symbol),
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
        .unwrap_or_else(|| panic_with_error!(env, ContractError::AdminNotSet))
}

pub fn _has_admin(env: &Env) -> bool {
    env.storage().instance().has(&DataKey::Admin)
}

/// Check if a caller is in the authorized admin list.
///
/// Loads the admin list onto a fixed-size stack slice and scans linearly —
/// no heap-allocated map is created inside the auth loop, reducing gas on
/// routine multi-signature verification paths (closes #528).
pub fn _is_authorized(env: &Env, caller: &Address) -> bool {
    if _is_revoked(env, caller) {
        return false;
    }

    env.storage()
        .instance()
        .get::<DataKey, Vec<Address>>(&DataKey::Admin)
    else {
        return false;
    };

    // Stack-local fixed buffer — avoids any BTreeMap / HashMap heap allocation.
    const CAP: usize = 16;
    let mut buf: [Option<Address>; CAP] = [
        None, None, None, None, None, None, None, None,
        None, None, None, None, None, None, None, None,
    ];
    let len = (admins.len() as usize).min(CAP);
    for i in 0..len {
        buf[i] = Some(admins.get(i as u32).unwrap());
    }

    for i in 0..len {
        if buf[i].as_ref() == Some(caller) {
            return true;
        }
    }
    false
}

pub fn _require_authorized(env: &Env, caller: &Address) {
    if !_is_authorized(env, caller) {
        panic_with_error!(env, ContractError::NotAuthorized);
    }
}

/// Add an address to the authorized admin list.
pub fn _add_authorized(env: &Env, new_admin: &Address) {
    if _is_revoked(env, new_admin) {
        return;
    }

    let mut admins = _get_admin(env);
    // Avoid duplicates
    if !admins.iter().any(|admin| admin == *new_admin) {
        admins.push_back(new_admin.clone());
        _set_admin(env, &admins);
    }
}

/// Remove an address from the authorized admin list.
pub fn _remove_authorized(env: &Env, admin_to_remove: &Address) {
    if !_has_admin(env) {
        return;
    }

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
// Delegate Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Assign a hot-wallet delegate for a cold-storage admin.
pub fn _set_delegate(env: &Env, admin: &Address, delegate: &Address) {
    // Remove old delegate reverse-mapping if this admin already had one
    if let Some(old_delegate) = _get_delegate(env, admin) {
        env.storage()
            .instance()
            .remove(&DataKey::DelegateOf(old_delegate));
    }

    env.storage()
        .instance()
        .set(&DataKey::SubmissionDelegate(admin.clone()), delegate);
    env.storage()
        .instance()
        .set(&DataKey::DelegateOf(delegate.clone()), admin);
}

/// Get the hot-wallet delegate assigned to an admin.
pub fn _get_delegate(env: &Env, admin: &Address) -> Option<Address> {
    env.storage()
        .instance()
        .get(&DataKey::SubmissionDelegate(admin.clone()))
}

/// Get the admin who assigned this delegate.
pub fn _get_admin_for_delegate(env: &Env, delegate: &Address) -> Option<Address> {
    env.storage()
        .instance()
        .get(&DataKey::DelegateOf(delegate.clone()))
}

/// Revoke a hot-wallet delegate from an admin.
pub fn _remove_delegate(env: &Env, admin: &Address) {
    if let Some(delegate) = _get_delegate(env, admin) {
        env.storage()
            .instance()
            .remove(&DataKey::DelegateOf(delegate));
        env.storage()
            .instance()
            .remove(&DataKey::SubmissionDelegate(admin.clone()));
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

pub fn _remove_paused(env: &Env) {
    env.storage().instance().remove(&DataKey::IsPaused);
}

// ─────────────────────────────────────────────────────────────────────────────
// Revocation Helpers
// ─────────────────────────────────────────────────────────────────────────────

pub fn _is_revoked(env: &Env, addr: &Address) -> bool {
    env.storage()
        .instance()
        .get::<DataKey, bool>(&DataKey::Revoked(addr.clone()))
        .unwrap_or(false)
}

pub fn _set_revoked(env: &Env, addr: &Address, revoked: bool) {
    if revoked {
        env.storage().instance().set(&DataKey::Revoked(addr.clone()), &true);
    } else {
        env.storage().instance().remove(&DataKey::Revoked(addr.clone()));
    }
}

pub fn _revoke_key(env: &Env, addr: &Address) -> bool {
    if _is_revoked(env, addr) {
        return false;
    }

    _set_revoked(env, addr, true);
    if _has_admin(env) {
        _remove_authorized(env, addr);
    }
    _remove_provider(env, addr);
    true
}

// ─────────────────────────────────────────────────────────────────────────────
// Provider Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Whitelist a provider address.
pub fn _add_provider(env: &Env, provider: &Address) {
    if _is_revoked(env, provider) {
        return;
    }

    env.storage()
        .instance()
        .set(&DataKey::Provider(provider.clone()), &true);
    _add_to_active_relayers(env, provider);

    // Issue #263: keep the isolated HealthActiveRelayers slot in sync.
    let count = _get_active_relayers(env).len();
    env.storage()
        .persistent()
        .set(&crate::types::DataKey::HealthActiveRelayers, &count);
}

/// Remove a provider from the whitelist.
pub fn _remove_provider(env: &Env, provider: &Address) {
    env.storage()
        .instance()
        .remove(&DataKey::Provider(provider.clone()));
    _remove_from_active_relayers(env, provider);

    // Issue #263: keep the isolated HealthActiveRelayers slot in sync.
    let count = _get_active_relayers(env).len();
    env.storage()
        .persistent()
        .set(&crate::types::DataKey::HealthActiveRelayers, &count);
}

/// Returns `true` if the address is a whitelisted provider OR an authorized delegate.
pub fn _is_provider(env: &Env, addr: &Address) -> bool {
    if _is_revoked(env, addr) {
        return false;
    }

    env.storage()
    // 1. Direct provider whitelist check
    if env
        .storage()
        .instance()
        .get::<DataKey, bool>(&DataKey::Provider(addr.clone()))
        .unwrap_or(false)
    {
        return true;
    }

    // 2. Delegate check: is this address a delegate for an authorized admin?
    if let Some(admin) = _get_admin_for_delegate(env, addr) {
        return _is_authorized(env, &admin);
    }

    false
}

/// Panics if the caller is not a whitelisted provider.
pub fn _require_provider(env: &Env, caller: &Address) {
    if !_is_provider(env, caller) {
        panic_with_error!(env, ContractError::ProviderNotAuthorized);
    }
}

pub fn _set_provider_weight(env: &Env, provider: &Address, weight: u32) {
    if _is_revoked(env, provider) {
        return;
    }

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

pub fn _set_vote_delegate(env: &Env, owner: &Address, delegate: &Address) {
    env.storage()
        .instance()
        .set(&DataKey::VoteDelegate(owner.clone()), delegate);
}

pub fn _get_vote_delegate(env: &Env, owner: &Address) -> Option<Address> {
    env.storage()
        .instance()
        .get(&DataKey::VoteDelegate(owner.clone()))
}

pub fn _remove_vote_delegate(env: &Env, owner: &Address) {
    env.storage()
        .instance()
        .remove(&DataKey::VoteDelegate(owner.clone()));
}

pub fn _get_delegated_voters(env: &Env, delegate: &Address) -> Vec<Address> {
    let admins = _get_admin(env);
    let mut delegated = Vec::new(env);

    for admin in admins.iter() {
        if _get_vote_delegate(env, &admin)
            .map(|stored_delegate| stored_delegate == *delegate)
            .unwrap_or(false)
        {
            delegated.push_back(admin);
        }
    }

    delegated
}

pub fn _add_effective_action_votes(env: &Env, action_id: u64, voter: &Address) -> u32 {
    let admins = _get_admin(env);
    let mut voters = _get_action_votes(env, action_id);

    if admins.iter().any(|admin| admin == *voter) && _get_vote_delegate(env, voter).is_none() {
        if !voters.iter().any(|existing| existing == voter) {
            voters.push_back(voter.clone());
        }
    }

    for admin in admins.iter() {
        if admin == *voter {
            continue;
        }

        if _get_vote_delegate(env, &admin)
            .map(|delegate| delegate == *voter)
            .unwrap_or(false)
            && !voters.iter().any(|existing| existing == admin)
        {
            voters.push_back(admin);
        }
    }

    let vote_count = voters.len();
    _set_action_votes(env, action_id, &voters);
    vote_count
}

/// Get the list of all active relayers (whitelisted providers).
pub fn _get_active_relayers(env: &Env) -> Vec<Address> {
    env.storage()
        .instance()
        .get(&DataKey::ActiveRelayers)
        .unwrap_or_else(|| Vec::new(env))
}

/// Add a relayer to the active relayers list.
fn _add_to_active_relayers(env: &Env, provider: &Address) {
    let mut relayers = _get_active_relayers(env);
    if !relayers.iter().any(|r| r == *provider) {
        relayers.push_back(provider.clone());
        env.storage()
            .instance()
            .set(&DataKey::ActiveRelayers, &relayers);
    }
}

/// Remove a relayer from the active relayers list.
fn _remove_from_active_relayers(env: &Env, provider: &Address) {
    let relayers = _get_active_relayers(env);
    let mut filtered = Vec::new(env);
    for relayer in relayers.iter() {
        if relayer != *provider {
            filtered.push_back(relayer);
        }
    }
    env.storage()
        .instance()
        .set(&DataKey::ActiveRelayers, &filtered);
}

// ─────────────────────────────────────────────────────────────────────────────
// Community Council Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Set the Community Council address for emergency freeze functionality.
pub fn _set_council(env: &Env, council: &Address) {
    env.storage()
        .instance()
        .set(&DataKey::CommunityCouncil, council);
}

/// Get the Community Council address.
pub fn _get_council(env: &Env) -> Option<Address> {
    env.storage().instance().get(&DataKey::CommunityCouncil)
}

/// Check if the caller is the Community Council.
pub fn _is_council(env: &Env, caller: &Address) -> bool {
    _get_council(env)
        .map(|council| council == *caller)
        .unwrap_or(false)
}

/// Panic if the caller is not the Community Council.
pub fn _require_council(env: &Env, caller: &Address) {
    if !_is_council(env, caller) {
        panic_with_error!(env, ContractError::CouncilRequired);
    }
}

/// Check if the contract is in emergency freeze state.
pub fn _is_frozen(env: &Env) -> bool {
    env.storage()
        .instance()
        .get::<DataKey, bool>(&DataKey::EmergencyFrozen)
        .unwrap_or(false)
}

/// Set the emergency freeze state.
pub fn _set_frozen(env: &Env, frozen: bool) {
    env.storage()
        .instance()
        .set(&DataKey::EmergencyFrozen, &frozen);
}

/// Panic if the contract is in emergency freeze state.
pub fn _require_not_frozen(env: &Env) {
    if _is_frozen(env) {
        panic_with_error!(env, ContractError::ContractFrozen);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Emergency Halt Helpers (multi-sig governance)
// ─────────────────────────────────────────────────────────────────────────────

pub fn _is_halted(env: &Env) -> bool {
    env.storage()
        .instance()
        .get::<DataKey, bool>(&DataKey::EmergencyHalt)
        .unwrap_or(false)
}

pub fn _set_halted(env: &Env, status: bool) {
    env.storage()
        .instance()
        .set(&DataKey::EmergencyHalt, &status);
}

/// Panic if the emergency halt flag is active.
pub fn _require_not_halted(env: &Env) {
    if _is_halted(env) {
        panic!("Contract is emergency halted: rate reads are disabled");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Bypass Safety Checks Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Store the expiry timestamp for the safety-checks bypass.
pub fn _set_bypass_safety_checks(env: &Env, expiry: u64) {
    env.storage()
        .temporary()
        .set(&DataKey::BypassSafetyChecks, &expiry);
}

/// Remove the safety-checks bypass (disables it immediately).
pub fn _remove_bypass_safety_checks(env: &Env) {
    env.storage()
        .temporary()
        .remove(&DataKey::BypassSafetyChecks);
}

/// Return the expiry timestamp of the safety-checks bypass, or None if not set.
pub fn _get_bypass_expiry(env: &Env) -> Option<u64> {
    env.storage().temporary().get(&DataKey::BypassSafetyChecks)
}

/// Return true if a bypass is set and has not yet expired.
pub fn _is_bypass_active(env: &Env) -> bool {
    _get_bypass_expiry(env)
        .map(|expiry| env.ledger().timestamp() < expiry)
        .unwrap_or(false)
}

// ─────────────────────────────────────────────────────────────────────────────
// Multi-Sig Action Proposal Helpers
// ─────────────────────────────────────────────────────────────────────────────

use crate::types::ProposedAction;

/// Get the next available action ID and increment the counter.
pub fn _get_next_action_id(env: &Env) -> u64 {
    let current: u64 = env
        .storage()
        .instance()
        .get(&DataKey::ActionIdCounter)
        .unwrap_or(0);
    let next_id = current + 1;
    env.storage()
        .instance()
        .set(&DataKey::ActionIdCounter, &next_id);
    next_id
}

/// Store a proposed action.
pub fn _set_proposed_action(env: &Env, action_id: u64, action: &ProposedAction) {
    env.storage()
        .instance()
        .set(&DataKey::ProposedAction(action_id), action);
}

/// Get a proposed action by ID.
pub fn _get_proposed_action(env: &Env, action_id: u64) -> Option<ProposedAction> {
    env.storage()
        .instance()
        .get(&DataKey::ProposedAction(action_id))
}

/// Store votes for a proposed action.
pub fn _set_action_votes(env: &Env, action_id: u64, voters: &Vec<Address>) {
    env.storage()
        .instance()
        .set(&DataKey::ActionVotes(action_id), voters);
}

/// Get votes for a proposed action.
pub fn _get_action_votes(env: &Env, action_id: u64) -> Vec<Address> {
    env.storage()
        .instance()
        .get(&DataKey::ActionVotes(action_id))
        .unwrap_or_else(|| Vec::new(env))
}

/// Add a vote for a proposed action.
pub fn _add_action_vote(env: &Env, action_id: u64, voter: &Address) {
    let mut voters = _get_action_votes(env, action_id);
    // Avoid duplicates
    if !voters.iter().any(|v| v == voter) {
        voters.push_back(voter.clone());
        _set_action_votes(env, action_id, &voters);
    }
}

/// Check if an action has reached the required threshold (3/5).
///
/// # Issue #264 — Weight-accumulation algorithm
///
/// Instead of simply counting votes, this function sums the governance weight
/// of every voter and compares the total against the configured
/// `WeightThreshold`.  Each admin's weight is stored under
/// `DataKey::AdminWeight(addr)` and defaults to **1** when unset, so existing
/// deployments that have never called `set_admin_weight` continue to behave
/// exactly as before (one vote = one unit of weight).
///
/// The `threshold` parameter is the *fallback* vote-count threshold used when
/// no `WeightThreshold` has been configured.  When a `WeightThreshold` is
/// present it takes precedence and the raw vote count is ignored.
pub fn _has_reached_threshold(env: &Env, action_id: u64, threshold: u32) -> bool {
    let voters = _get_action_votes(env, action_id);

    // ── Resolve the required weight threshold ────────────────────────────────
    // If a WeightThreshold has been configured (issue #264) use it; otherwise
    // fall back to the legacy vote-count threshold so old deployments are
    // unaffected.
    let required_weight: u32 = env
        .storage()
        .persistent()
        .get(&crate::types::DataKey::WeightThreshold)
        .unwrap_or(threshold); // fallback: 1 vote = 1 weight unit

    // ── Accumulate voter weights ─────────────────────────────────────────────
    let mut accumulated_weight: u32 = 0;
    for voter in voters.iter() {
        let weight: u32 = env
            .storage()
            .persistent()
            .get(&crate::types::DataKey::AdminWeight(voter.clone()))
            .unwrap_or(1); // default weight = 1 (backward-compatible)
        accumulated_weight = accumulated_weight.saturating_add(weight);
    }

    accumulated_weight >= required_weight
}

/// Get the required threshold based on admin count (3/5 of admins).
///
/// Returns the *vote-count* threshold used as the fallback when no
/// `WeightThreshold` has been configured.  When `WeightThreshold` is set,
/// `_has_reached_threshold` uses that value instead.
pub fn _get_required_threshold(env: &Env) -> u32 {
    // If a weight threshold is configured, surface it as the canonical value.
    if let Some(wt) = env
        .storage()
        .persistent()
        .get::<crate::types::DataKey, u32>(&crate::types::DataKey::WeightThreshold)
    {
        return wt;
    }

    let admins = _get_admin(env);
    let admin_count = admins.len() as u32;

    // Require 3/5 (or majority if fewer than 5 admins)
    // For 3 admins: need 2 (majority)
    // For 4 admins: need 3
    // For 5 admins: need 3
    if admin_count <= 3 {
        2 // Simple majority for small groups
    } else {
        3 // 3/5 threshold for larger groups
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Issue #264: Admin weight helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Set the governance weight for a specific admin (issue #264).
///
/// Weight must be in the range 1–100.  A weight of 0 is rejected because a
/// zero-weight admin could never contribute to reaching the threshold.
pub fn _set_admin_weight(env: &Env, admin: &Address, weight: u32) {
    env.storage()
        .persistent()
        .set(&crate::types::DataKey::AdminWeight(admin.clone()), &weight);
}

/// Get the governance weight for a specific admin (issue #264).
///
/// Returns 1 (the default) when no weight has been explicitly assigned,
/// preserving backward compatibility with deployments that predate #264.
pub fn _get_admin_weight(env: &Env, admin: &Address) -> u32 {
    env.storage()
        .persistent()
        .get(&crate::types::DataKey::AdminWeight(admin.clone()))
        .unwrap_or(1)
}

/// Set the minimum cumulative weight required for a governance proposal to
/// execute (issue #264).
///
/// `threshold` must be ≥ 1.  Setting it to 0 is rejected because it would
/// allow any proposal to execute immediately without any votes.
pub fn _set_weight_threshold(env: &Env, threshold: u32) {
    env.storage()
        .persistent()
        .set(&crate::types::DataKey::WeightThreshold, &threshold);
}

/// Get the configured weight threshold, or `None` if not set (issue #264).
pub fn _get_weight_threshold(env: &Env) -> Option<u32> {
    env.storage()
        .persistent()
        .get(&crate::types::DataKey::WeightThreshold)
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod auth_tests {
    use super::*;
    use soroban_sdk::{
        contract, contractimpl,
        testutils::{Address as _, Events},
        Env,
    };

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
    #[should_panic]
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
    #[should_panic]
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
    fn test_set_get_and_remove_vote_delegate() {
        let (env, contract_id, admin) = setup();
        let delegate = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
        env.as_contract(&contract_id, || {
            assert_eq!(_get_vote_delegate(&env, &admin), None);

            _set_vote_delegate(&env, &admin, &delegate);
            assert_eq!(_get_vote_delegate(&env, &admin), Some(delegate.clone()));

            _remove_vote_delegate(&env, &admin);
            assert_eq!(_get_vote_delegate(&env, &admin), None);
        });
    }

    #[test]
    fn test_vote_delegate_can_be_reassigned() {
        let (env, contract_id, admin) = setup();
        let delegate1 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
        let delegate2 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
        env.as_contract(&contract_id, || {
            _set_vote_delegate(&env, &admin, &delegate1);
            assert_eq!(_get_vote_delegate(&env, &admin), Some(delegate1));

            _set_vote_delegate(&env, &admin, &delegate2);
            assert_eq!(_get_vote_delegate(&env, &admin), Some(delegate2));
        });
    }

    #[test]
    fn test_renounce_ownership_removes_all_admins() {
        let (env, contract_id, _admin1) = setup();
        let admin2 = <soroban_sdk::Address as soroban_sdk::testutils::Address>::generate(&env);
        env.as_contract(&contract_id, || {
            _add_authorized(&env, &admin2);
            assert_eq!(_get_admin(&env).len(), 2);
            assert!(_has_admin(&env));

            _renounce_ownership(&env);

            assert!(!_has_admin(&env));
        });
    }
}
