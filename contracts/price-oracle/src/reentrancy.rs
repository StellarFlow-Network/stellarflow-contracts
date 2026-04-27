//! FE-208: Re-entrancy guard for sensitive governance functions.
//! Uses a `lock` flag in instance storage to prevent re-entrant calls.

use soroban_sdk::{contracttype, Env};

#[contracttype]
pub enum LockKey {
    ReentrancyLock,
}

/// Acquires the re-entrancy lock. Panics if already locked.
pub fn acquire_lock(env: &Env) {
    let locked: bool = env
        .storage()
        .instance()
        .get(&LockKey::ReentrancyLock)
        .unwrap_or(false);
    if locked {
        panic!("re-entrancy detected");
    }
    env.storage()
        .instance()
        .set(&LockKey::ReentrancyLock, &true);
}

/// Releases the re-entrancy lock.
pub fn release_lock(env: &Env) {
    env.storage()
        .instance()
        .set(&LockKey::ReentrancyLock, &false);
}