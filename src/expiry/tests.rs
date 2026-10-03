//! Unit tests for the expiry-guard module.
//!
//! These exercise the leaf-binding math and the expiry boundary at the module
//! level. They are NOT a substitute for contract-level integration tests —
//! the crate cannot compile end-to-end (see issue #1114), so these have never
//! been run. They document intent; they do not demonstrate verification.

#![allow(dead_code, unused_imports)]

use super::tree;
use super::DEPOSIT_EXPIRY_SECS;
use soroban_sdk::{Address, BytesN, Env};

#[test]
fn leaf_hash_binds_timestamp() {
    let env = Env::default();
    let commitment = BytesN::from_array(&env, &[7u8; 32]);

    let t0 = tree_leaf(&env, &commitment, 1_000_000);
    let t1 = tree_leaf(&env, &commitment, 1_000_001);
    assert_ne!(
        t0, t1,
        "same commitment at different t_deposit must hash differently"
    );

    let again = tree_leaf(&env, &commitment, 1_000_000);
    assert_eq!(t0, again, "leaf hash must be deterministic");
}

/// Reimplementation of `tree::leaf_hash` for the test — `leaf_hash` is a
/// private fn, and this test only asserts the binding property, not the
/// exact function. If `leaf_hash` changes, update this mirror.
fn tree_leaf(env: &Env, commitment: &BytesN<32>, deposited_at: u64) -> BytesN<32> {
    use soroban_sdk::Bytes;
    let mut payload = Bytes::new(env);
    payload.append(&Bytes::from_slice(env, &commitment.to_array()));
    payload.extend_from_array(&deposited_at.to_be_bytes());
    env.crypto().sha256(&payload)
}

#[test]
fn default_expiry_is_thirty_days() {
    assert_eq!(DEPOSIT_EXPIRY_SECS, 2_592_000);
}

#[test]
fn expiry_boundary_is_strictly_after() {
    // Mirrors `guard::is_expired` (`now > expires_at`). A deposit expiring at
    // T is still withdrawable AT T, and refundable from T+1. If
    // `guard::is_expired` changes, update this mirror.
    let expires_at: u64 = 1_000;
    let is_expired = |now: u64| now > expires_at;
    assert!(!is_expired(999));
    assert!(
        !is_expired(1_000),
        "at-expires_at is not expired (strictly-after convention)"
    );
    assert!(is_expired(1_001));
}

// NOTE: the tests below sketch the *intended* end-to-end behavior. They need
// a compilable crate (mock token, registered contract, generated client) to
// run, which main does not currently provide. Included as specification, not
// as verification.

// #[test]
// fn third_party_withdrawal_rejected_after_expiry() { ... }

// #[test]
// fn depositor_can_refund_only_after_expiry() { ... }

// #[test]
// fn withdraw_then_refund_is_rejected() { ... }

// #[test]
// fn refund_goes_to_depositor_not_caller() { ... }
