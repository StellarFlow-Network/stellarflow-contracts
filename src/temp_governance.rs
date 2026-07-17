//! ── Temporary Storage Management for Voting Proposals ──
//!
//! This module provides utilities for storing voting proposals in Soroban's
//! Temporary storage bucket instead of persistent storage. This approach:
//!
//! - Reduces gas overhead by avoiding persistent storage writes
//! - Automatically purges expired proposals via Soroban's TTL mechanism
//! - Keeps storage indices clean and performant
//! - Enables programmatic cleanup of stale voting structures
//!
//! ## Storage Lifecycle
//!
//! 1. Proposal is created and written to temporary storage with TTL
//! 2. During voting window, updates extend the TTL
//! 3. Upon execution, proposal is explicitly removed
//! 4. If voting expires without reaching threshold, network auto-purges after TTL

use soroban_sdk::{symbol_short, Env, Symbol, contracttype};

// ── Temporary proposal storage keys ──────────────────────────────────────

pub const EMERGENCY_REVOCATION_TEMP_KEY: Symbol = symbol_short!("EMREV_T");
pub const REVOCATION_TEMP_KEY: Symbol = symbol_short!("REVOK_T");

// ── TTL Configuration ────────────────────────────────────────────────────
// 
// The TTL values determine how long proposals remain accessible in temporary
// storage before being auto-purged by the network. These should be set to
// accommodate reasonable voting windows while avoiding unnecessary storage bloat.

/// Default TTL for temporary proposals (in ledgers).
/// Approximately 10 days at 5-second block time (~172,800 ledgers)
pub const DEFAULT_PROPOSAL_TTL: u32 = 172_800;

/// Extended TTL after receiving a vote (prevents premature expiration during voting)
pub const EXTENDED_PROPOSAL_TTL: u32 = 259_200; // ~15 days

// ── Generic temporary proposal storage ───────────────────────────────────

/// Store a proposal in temporary storage with automatic TTL management.
///
/// Proposals stored here will be automatically purged by the Soroban network
/// after the TTL expires, eliminating the need for manual cleanup.
///
/// # Arguments
/// * `env` - The contract environment
/// * `key` - Storage key for the proposal
/// * `proposal` - The proposal data to store
/// * `ttl` - Time-to-live in ledgers (auto-cleanup after this duration)
pub fn store_temp_proposal<T: soroban_sdk::IntoVal<Env, soroban_sdk::Val>>(
    env: &Env,
    key: &Symbol,
    proposal: &T,
    ttl: u32,
) {
    env.storage().temporary().set(key, proposal);
    // Note: Temporary storage TTL is managed automatically by the Soroban network.
    // The `extend_ttl` method keeps the entry alive by preventing expiration.
    env.storage().temporary().extend_ttl(key, ttl, ttl);
}

/// Retrieve a proposal from temporary storage.
///
/// Returns None if the proposal has expired or doesn't exist.
pub fn get_temp_proposal<T: soroban_sdk::TryFromVal<Env, soroban_sdk::Val>>(
    env: &Env,
    key: &Symbol,
) -> Option<T> {
    env.storage().temporary().get(key)
}

/// Check if a proposal exists in temporary storage.
pub fn has_temp_proposal(env: &Env, key: &Symbol) -> bool {
    env.storage().temporary().has(key)
}

/// Remove a proposal from temporary storage (executed or rejected proposals).
///
/// This is used to clean up proposals that have reached a terminal state
/// (either executed or explicitly rejected). While the Soroban network will
/// eventually auto-purge after TTL, explicit removal frees resources sooner.
pub fn remove_temp_proposal(env: &Env, key: &Symbol) {
    env.storage().temporary().remove(key);
}

/// Extend the TTL of an active proposal to prevent premature expiration.
///
/// Call this whenever a proposal receives a vote or undergoes state changes
/// to ensure it remains accessible throughout the voting window.
pub fn extend_temp_proposal_ttl(env: &Env, key: &Symbol, ttl: u32) {
    env.storage().temporary().extend_ttl(key, ttl, ttl);
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    #[test]
    fn test_temp_proposal_storage() {
        let env = Env::default();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        
        env.as_contract(&contract_id, || {
            let test_key = symbol_short!("TEST");
            let test_value = 42u64;

            // Store proposal
            store_temp_proposal(&env, &test_key, &test_value, DEFAULT_PROPOSAL_TTL);

            // Verify it exists
            assert!(has_temp_proposal(&env, &test_key));

            // Retrieve proposal
            let retrieved: u64 = get_temp_proposal(&env, &test_key).expect("proposal should exist");
            assert_eq!(retrieved, test_value);

            // Remove proposal
            remove_temp_proposal(&env, &test_key);

            // Verify it's gone
            assert!(!has_temp_proposal(&env, &test_key));
        });
    }
}
