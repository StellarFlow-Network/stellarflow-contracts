use soroban_sdk::{symbol_short, Env, Symbol};
use crate::ContractError;

/// Storage key for the reentrancy lock flag in instance storage.
pub const REENTRANCY_LOCK_KEY: Symbol = symbol_short!("REENTLOCK");

/// Attempt to acquire the reentrancy lock.
///
/// Sets `REENTLOCK` flag to `true` in instance storage.
/// Returns `Err(ContractError::ReentrancyDetected)` if lock is already held.
pub fn lock(env: &Env) -> Result<(), ContractError> {
    let is_locked: bool = env
        .storage()
        .instance()
        .get(&REENTRANCY_LOCK_KEY)
        .unwrap_or(false);

    if is_locked {
        return Err(ContractError::ReentrancyDetected);
    }

    env.storage().instance().set(&REENTRANCY_LOCK_KEY, &true);
    Ok(())
}

/// Release the reentrancy lock.
pub fn unlock(env: &Env) {
    env.storage().instance().set(&REENTRANCY_LOCK_KEY, &false);
}

/// Check if the reentrancy lock is currently active.
pub fn is_locked(env: &Env) -> bool {
    env.storage()
        .instance()
        .get(&REENTRANCY_LOCK_KEY)
        .unwrap_or(false)
}

/// RAII Guard for reentrancy protection.
/// Locks on instantiation and unlocks when dropped.
pub struct ReentrancyGuard<'a> {
    env: &'a Env,
}

impl<'a> ReentrancyGuard<'a> {
    pub fn new(env: &'a Env) -> Result<Self, ContractError> {
        lock(env)?;
        Ok(Self { env })
    }
}

impl<'a> Drop for ReentrancyGuard<'a> {
    fn drop(&mut self) {
        unlock(self.env);
    }
}

/// Execute a closure protected by the reentrancy lock.
pub fn with_reentrancy_guard<F, R>(env: &Env, f: F) -> Result<R, ContractError>
where
    F: FnOnce() -> Result<R, ContractError>,
{
    lock(env)?;
    let result = f();
    unlock(env);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::Env;

    #[test]
    fn test_reentrancy_lock_acquire_and_release() {
        let env = Env::default();

        assert!(!is_locked(&env));
        lock(&env).expect("First lock should succeed");
        assert!(is_locked(&env));

        let reentrant_res = lock(&env);
        assert_eq!(reentrant_res, Err(ContractError::ReentrancyDetected));

        unlock(&env);
        assert!(!is_locked(&env));

        lock(&env).expect("Locking after unlock should succeed");
        unlock(&env);
    }

    #[test]
    fn test_reentrancy_guard_raii() {
        let env = Env::default();

        assert!(!is_locked(&env));
        {
            let _guard = ReentrancyGuard::new(&env).expect("Guard creation should lock");
            assert!(is_locked(&env));
            assert_eq!(lock(&env), Err(ContractError::ReentrancyDetected));
        }
        assert!(!is_locked(&env));
    }

    #[test]
    fn test_with_reentrancy_guard_closure() {
        let env = Env::default();

        let res = with_reentrancy_guard(&env, || {
            assert!(is_locked(&env));
            Ok(42)
        });

        assert_eq!(res, Ok(42));
        assert!(!is_locked(&env));
    }
}
