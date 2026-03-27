//! Helpers for reading Soroban ledger (blockchain) time.

use soroban_sdk::Env;

/// Returns the current ledger close time as a Unix timestamp in seconds.
///
/// This is the "blockchain time" from the ledger header—the time at which the
/// ledger was closed—not wall-clock time on the host.
pub fn current_ledger_timestamp(env: &Env) -> u64 {
    env.ledger().timestamp()
}

#[cfg(test)]
mod tests {
    use super::current_ledger_timestamp;
    use soroban_sdk::{testutils::Ledger, Env};

    #[test]
    fn returns_mock_ledger_timestamp() {
        let env = Env::default();
        env.ledger().set_timestamp(1_700_000_123);
        assert_eq!(current_ledger_timestamp(&env), 1_700_000_123);
    }
}
