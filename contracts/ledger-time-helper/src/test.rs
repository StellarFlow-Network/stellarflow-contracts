#![cfg(test)]

use super::*;
use soroban_sdk::{testutils::{Ledger, LedgerInfo}, Env};

#[test]
fn test_current_ledger_timestamp() {
    let env = Env::default();
    env.ledger().set(LedgerInfo {
        timestamp: 1_700_000_123,
        ..env.ledger().get()
    });
    assert_eq!(current_ledger_timestamp(&env), 1_700_000_123);
}

#[test]
fn test_current_ledger_sequence() {
    let env = Env::default();
    env.ledger().set(LedgerInfo {
        sequence_number: 42_000,
        ..env.ledger().get()
    });
    assert_eq!(current_ledger_sequence(&env), 42_000);
}
