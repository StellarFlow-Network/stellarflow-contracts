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

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::Env;

    #[test]
    fn test_acquire_and_release_lock() {
        let env = Env::default();
        
        // Initially not locked
        assert!(!env.storage().temporary().get::<_, bool>(&crate::types::DataKey::IsLocked).unwrap_or(false));
        
        // Acquire lock
        acquire_lock(&env);
        assert!(env.storage().temporary().get::<_, bool>(&crate::types::DataKey::IsLocked).unwrap_or(false));
        
        // Release lock
        release_lock(&env);
        assert!(!env.storage().temporary().get::<_, bool>(&crate::types::DataKey::IsLocked).unwrap_or(false));
    }

    #[test]
    #[should_panic(expected = "ContractError(12)")]
    fn test_double_acquire_panics() {
        let env = Env::default();
        
        // Acquire lock once
        acquire_lock(&env);
        
        // Try to acquire again - should panic with ReentrancyDetected
        acquire_lock(&env);
    }
}
