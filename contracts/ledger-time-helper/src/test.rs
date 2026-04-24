#![cfg(test)]

use super::*;
use soroban_sdk::{testutils::Ledger, Env};

#[test]
fn test_current_ledger_timestamp() {
    let env = Env::default();
    env.ledger().set_timestamp(1_700_000_123);
    assert_eq!(current_ledger_timestamp(&env), 1_700_000_123);
}
