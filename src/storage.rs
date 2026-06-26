//! Storage utilities for consumer lease management.
//!
//! This module provides an automated lifetime extension utility that
//! increases a consumer profile's ledger lease allocation proportionally
//! to their interaction frequency. This helps prevent unexpected eviction
//! events during high‑use periods.

use soroban_sdk::{symbol_short, Env, Symbol, Address};

/// Error type used by storage utility functions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractError {
    /// Generic error placeholder.
    Generic,
    // Add more variants as needed.
}

/// Symbol used to store the lease value in contract instance storage.
const LEASE_KEY: Symbol = symbol_short!("LEASE");

/// Extend the lease for a given consumer based on interaction frequency.
///
/// * `env` – The contract execution environment.
/// * `consumer` – The address of the consumer profile.
/// * `frequency` – Number of interactions (e.g., requests) performed in the
///   current period. The lease will be increased proportionally to this value.
///
/// The function reads the current lease value from instance storage, adds a
/// proportional amount (`frequency * EXTENSION_FACTOR`), and writes the new
/// lease back. The factor is deliberately chosen to be small to avoid
/// runaway growth; adjust as needed for business requirements.
///
/// Returns `Ok(())` on success or a `ContractError` on failure.
pub fn extend_consumer_lease(env: &Env, consumer: &Address, frequency: u64) -> Result<(), ContractError> {
    // Extension factor – can be tuned based on desired lease scaling.
    const EXTENSION_FACTOR: u64 = 10;

    // Retrieve the current lease for the consumer; default to 0 if not set.
    let mut lease: u64 = env
        .storage()
        .instance()
        .get(&(LEASE_KEY, consumer.clone()))
        .unwrap_or(0);

    // Compute the additional lease based on interaction frequency.
    // Use saturating arithmetic to guard against overflow.
    let addition = frequency.saturating_mul(EXTENSION_FACTOR);
    lease = lease.saturating_add(addition);

    // Persist the updated lease.
    env.storage()
        .instance()
        .set(&(LEASE_KEY, consumer.clone()), &lease);

    Ok(())
}
