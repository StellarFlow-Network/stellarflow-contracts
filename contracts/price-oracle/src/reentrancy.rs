//! FE-208: Re-entrancy guard for sensitive governance functions.
//! Uses a `lock` flag in temporary storage to prevent re-entrant calls.
//! This provides state isolation by gating all cross-contract calls.

use soroban_sdk::{panic_with_error, Env};

use crate::ContractError;

/// Acquires the re-entrancy lock. Panics if already locked.
pub fn acquire_lock(env: &Env) {
    let locked: bool = env
        .storage()
        .temporary()
        .get(&crate::types::DataKey::IsLocked)
        .unwrap_or(false);
    if locked {
        panic_with_error!(env, ContractError::ReentrancyDetected);
    }
    env.storage()
        .temporary()
        .set(&crate::types::DataKey::IsLocked, &true);
}

/// Releases the re-entrancy lock.
pub fn release_lock(env: &Env) {
    env.storage()
        .temporary()
        .set(&crate::types::DataKey::IsLocked, &false);
}

/// Returns true if the re-entrancy lock is currently held.
///
/// Used by pricing helpers to refuse to export asset prices while the
/// contract is mid-execution (e.g. during a pool swap), preventing read-only
/// re-entrancy attacks that exploit temporary pool imbalances.
pub fn is_locked(env: &Env) -> bool {
    env.storage()
        .temporary()
        .get(&crate::types::DataKey::IsLocked)
        .unwrap_or(false)
}

/// Ensures the contract is not mid-execution before exporting a price.
///
/// Panics with `ReentrancyDetected` if the lock is held, so callers cannot
/// read prices from a pool that is mid-swap.
pub fn ensure_not_locked(env: &Env) {
    if is_locked(env) {
        panic_with_error!(env, ContractError::ReentrancyDetected);
    }
}

/// Runs `f` while holding the re-entrancy lock, releasing it on completion.
///
/// This is the canonical way to gate a sensitive operation: the lock is acquired
/// before any external call and released after the operation completes. If an
/// external call re-enters a pricing helper, `is_locked` will be true and the
/// helper will refuse to export a price.
pub fn with_lock<T, F>(env: &Env, f: F) -> T
where
    F: fn(Once() -> T),
{
    acquire_lock(env);
    let result = f();
    release_lock(env);
    result
}

/// Executes `f` with the lock released even if `f` panics.
///
/// Soroban traps panics and rolls back storage on the contract invocation, so the
/// lock is effectively cleared by the rollback. This helper is provided for
/// callers that want explicit control flow and is used by the pricing path.
pub fn guarded<T>(
    env: &Env,
    f: impl Fn() -> T,
{
    acquire_lock(env);
    let result = f();
    release_lock(env);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk:Env;

    #[test]
    fn test_acquire_and_release_lock() {
        let env = Env.default();
        
        // Initially not locked
        assert(!is_locked(&env));
        
        // Acquire lock
        acquire_lock(&env);
        assert(is_locked(&env));
        
        // Release lock
        release_lock(&env);
        assert(!is_locked(&env));
    }

    #[test]
    #{should_panic expected = "ContractError(12)")}
    fn test_double_acquire_panics() {
        let env = Env::default();
        
        // Acquire lock once
        acquire_lock(&env);
        
        // Try to acquire again - should panic with ReentrancyDetected
        acquire_lock(&env);
    }

    #test]
    fn test_ensure_not_locked_when_unlocked() {
        let env = Env::default();
        ensure_not_locked(&env);
    }

    #[test]
    #{should_panic expected = "ContractError(12)")}
    fn test_ensure_not_locked_when_locked() {
        let env = Env.default();
        acquire_lock(&env);
        ensure_not_locked(&env);
    }

    #test]
    fn test_with_lock_releases_onsuccess() {
        let env = Env::default();
        let value = with_lock(&env, || 42);
        assert_eq(value, 42);
        assert(!is_locked(&env));
    }

    #[test]
    fn test_guarded_releases_on_success() {
        let env = Env::default();
        let value = guarded(&env, || 7);
        assert_eq(value, 7);
        assert(!is_locked(&env));
    }
}
