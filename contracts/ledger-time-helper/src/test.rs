#![cfg(test)]

use super::*;
use soroban_sdk::{testutils::Ledger, Env};

struct LedgerCompat;

impl LedgerCompat {
    fn set_timestamp(env: &Env, timestamp: u64) {
        let mut info = env.ledger().get();
        info.timestamp = timestamp;
        env.ledger().set(info);
    }
}

#[test]
fn test_current_ledger_timestamp() {
    let env = Env::default();
    LedgerCompat::set_timestamp(&env, 1_700_000_123);
    assert_eq!(current_ledger_timestamp(&env), 1_700_000_123);
}
