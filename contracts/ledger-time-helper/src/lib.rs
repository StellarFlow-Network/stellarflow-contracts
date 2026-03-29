//! Helpers for reading Soroban ledger (blockchain) time.

use soroban_sdk::Env;

/// Returns the current ledger close time as a Unix timestamp in seconds.
///
/// This is the "blockchain time" from the ledger header—the time at which the
/// ledger was closed—not wall-clock time on the host.
pub fn current_ledger_timestamp(env: &Env) -> u64 {
    env.ledger().timestamp()
}

mod test;
