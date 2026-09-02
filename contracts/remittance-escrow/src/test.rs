#![cfg(test)]

use super::*;
use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{token, Bytes, Env};

const DAY: u64 = 86_400;

struct Setup {
    env: Env,
    client_id: Address,
    token_id: Address,
    admin: Address,
    sender: Address,
    anchor: Address,
}

fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let sender = Address::generate(&env);
    let anchor = Address::generate(&env);

    // Deploy a mock SAC token and mint starting balances.
    let token_admin = Address::generate(&env);
    let token_id = env.register_stellar_asset_contract(token_admin.clone());
    let token_sac = token::StellarAssetClient::new(&env, &token_id);
    token_sac.mint(&sender, &1_000_000);
    token_sac.mint(&anchor, &1_000_000);

    let contract_id = env.register_contract(None, RemittanceEscrow);
    let client = RemittanceEscrowClient::new(&env, &contract_id);
    client.initialize(&admin, &token_id);

    Setup {
        env,
        client_id: contract_id,
        token_id,
        admin,
        sender,
        anchor,
    }
}

fn client(s: &Setup) -> RemittanceEscrowClient<'static> {
    RemittanceEscrowClient::new(&s.env, &s.client_id)
}

fn token_client(s: &Setup) -> token::Client<'static> {
    token::Client::new(&s.env, &s.token_id)
}

fn advance_time(env: &Env, delta: u64) {
    env.ledger().with_mut(|li| {
        li.timestamp = li.timestamp.saturating_add(delta);
    });
}

#[test]
fn test_initialize_sets_admin_and_token() {
    let s = setup();
    let c = client(&s);
    assert_eq!(c.get_admin(), s.admin);
    assert_eq!(c.get_token(), s.token_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_initialize_twice_fails() {
    let s = setup();
    let c = client(&s);
    c.initialize(&s.admin, &s.token_id);
}

#[test]
fn test_create_remittance_escrows_funds() {
    let s = setup();
    let c = client(&s);
    let tok = token_client(&s);

    let sender_balance_before = tok.balance(&s.sender);

    let id = c.create_remittance(&s.sender, &s.anchor, &10_000, &DAY);
    assert_eq!(id, 0);

    assert_eq!(tok.balance(&s.sender), sender_balance_before - 10_000);
    assert_eq!(tok.balance(&s.client_id), 10_000);

    let remittance = c.get_remittance(&id);
    assert_eq!(remittance.sender, s.sender);
    assert_eq!(remittance.anchor, s.anchor);
    assert_eq!(remittance.amount, 10_000);
    assert_eq!(remittance.status, RemittanceStatus::Pending);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_create_remittance_zero_amount_fails() {
    let s = setup();
    let c = client(&s);
    c.create_remittance(&s.sender, &s.anchor, &0, &DAY);
}

#[test]
fn test_deposit_collateral() {
    let s = setup();
    let c = client(&s);
    let tok = token_client(&s);

    let anchor_balance_before = tok.balance(&s.anchor);
    c.deposit_collateral(&s.anchor, &5_000);

    assert_eq!(c.get_collateral(&s.anchor), 5_000);
    assert_eq!(tok.balance(&s.anchor), anchor_balance_before - 5_000);
    assert_eq!(tok.balance(&s.client_id), 5_000);

    // A second deposit accumulates.
    c.deposit_collateral(&s.anchor, &2_500);
    assert_eq!(c.get_collateral(&s.anchor), 7_500);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_deposit_collateral_zero_amount_fails() {
    let s = setup();
    let c = client(&s);
    c.deposit_collateral(&s.anchor, &0);
}

#[test]
fn test_submit_payout_proof_completes_remittance() {
    let s = setup();
    let c = client(&s);

    let id = c.create_remittance(&s.sender, &s.anchor, &10_000, &DAY);
    let proof = Bytes::from_slice(&s.env, b"receipt-hash");
    c.submit_payout_proof(&s.anchor, &id, &proof);

    let remittance = c.get_remittance(&id);
    assert_eq!(remittance.status, RemittanceStatus::Completed);
    assert_eq!(remittance.proof, proof);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_submit_payout_proof_wrong_anchor_fails() {
    let s = setup();
    let c = client(&s);

    let id = c.create_remittance(&s.sender, &s.anchor, &10_000, &DAY);
    let stranger = Address::generate(&s.env);
    let proof = Bytes::from_slice(&s.env, b"receipt-hash");
    c.submit_payout_proof(&stranger, &id, &proof);
}

#[test]
fn test_proof_before_deadline_prevents_later_dispute() {
    let s = setup();
    let c = client(&s);

    let id = c.create_remittance(&s.sender, &s.anchor, &10_000, &DAY);

    // Anchor proves the payout well before the deadline.
    let proof = Bytes::from_slice(&s.env, b"receipt-hash");
    c.submit_payout_proof(&s.anchor, &id, &proof);

    // Fast-forward well past deadline + 24h dispute window.
    advance_time(&s.env, DAY + DAY + 1);

    let result = c.try_open_dispute(&s.sender, &id);
    assert!(result.is_err());

    let remittance = c.get_remittance(&id);
    assert_eq!(remittance.status, RemittanceStatus::Completed);
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_dispute_rejected_before_window_elapsed() {
    let s = setup();
    let c = client(&s);

    let id = c.create_remittance(&s.sender, &s.anchor, &10_000, &DAY);
    c.deposit_collateral(&s.anchor, &10_000);

    // Right at the deadline, the 24h dispute window has not started yet.
    advance_time(&s.env, DAY);
    c.open_dispute(&s.sender, &id);
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_dispute_rejected_one_second_before_window_closes() {
    let s = setup();
    let c = client(&s);

    let id = c.create_remittance(&s.sender, &s.anchor, &10_000, &DAY);
    c.deposit_collateral(&s.anchor, &10_000);

    // deadline + 24h - 1s: window has not *fully* elapsed yet.
    advance_time(&s.env, DAY + DAY - 1);
    c.open_dispute(&s.sender, &id);
}

#[test]
fn test_dispute_succeeds_after_window_refunds_and_locks_collateral() {
    let s = setup();
    let c = client(&s);
    let tok = token_client(&s);

    let id = c.create_remittance(&s.sender, &s.anchor, &10_000, &DAY);
    c.deposit_collateral(&s.anchor, &10_000);

    let sender_balance_after_create = tok.balance(&s.sender);

    // Exactly at deadline + 24h: the window has fully elapsed.
    advance_time(&s.env, DAY + DAY);
    c.open_dispute(&s.sender, &id);

    let remittance = c.get_remittance(&id);
    assert_eq!(remittance.status, RemittanceStatus::Refunded);

    // Sender refunded the full remittance amount.
    assert_eq!(tok.balance(&s.sender), sender_balance_after_create + 10_000);

    // Anchor's collateral was seized (locked) by the remittance amount.
    assert_eq!(c.get_collateral(&s.anchor), 0);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_dispute_wrong_sender_fails() {
    let s = setup();
    let c = client(&s);

    let id = c.create_remittance(&s.sender, &s.anchor, &10_000, &DAY);
    advance_time(&s.env, DAY + DAY);

    let stranger = Address::generate(&s.env);
    c.open_dispute(&stranger, &id);
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_double_dispute_rejected() {
    let s = setup();
    let c = client(&s);

    let id = c.create_remittance(&s.sender, &s.anchor, &10_000, &DAY);
    c.deposit_collateral(&s.anchor, &10_000);
    advance_time(&s.env, DAY + DAY);

    c.open_dispute(&s.sender, &id);
    // Second attempt must fail: remittance is already Refunded.
    c.open_dispute(&s.sender, &id);
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_dispute_after_completion_rejected() {
    let s = setup();
    let c = client(&s);

    let id = c.create_remittance(&s.sender, &s.anchor, &10_000, &DAY);
    let proof = Bytes::from_slice(&s.env, b"receipt-hash");
    c.submit_payout_proof(&s.anchor, &id, &proof);

    advance_time(&s.env, DAY + DAY);
    c.open_dispute(&s.sender, &id);
}

#[test]
fn test_dispute_with_insufficient_collateral_locks_available_only() {
    let s = setup();
    let c = client(&s);
    let tok = token_client(&s);

    let id = c.create_remittance(&s.sender, &s.anchor, &10_000, &DAY);
    // Anchor only staked a fraction of the remittance amount.
    c.deposit_collateral(&s.anchor, &2_000);

    let sender_balance_after_create = tok.balance(&s.sender);

    advance_time(&s.env, DAY + DAY);
    c.open_dispute(&s.sender, &id);

    // All available collateral (2_000) is seized, not the full 10_000.
    assert_eq!(c.get_collateral(&s.anchor), 0);

    // Sender is still fully refunded regardless of the collateral shortfall.
    assert_eq!(tok.balance(&s.sender), sender_balance_after_create + 10_000);

    let remittance = c.get_remittance(&id);
    assert_eq!(remittance.status, RemittanceStatus::Refunded);
}

#[test]
fn test_dispute_with_zero_collateral_still_refunds_sender() {
    let s = setup();
    let c = client(&s);
    let tok = token_client(&s);

    let id = c.create_remittance(&s.sender, &s.anchor, &10_000, &DAY);
    // No collateral deposited at all.

    let sender_balance_after_create = tok.balance(&s.sender);

    advance_time(&s.env, DAY + DAY);
    c.open_dispute(&s.sender, &id);

    assert_eq!(c.get_collateral(&s.anchor), 0);
    assert_eq!(tok.balance(&s.sender), sender_balance_after_create + 10_000);
}

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_get_remittance_not_found() {
    let s = setup();
    let c = client(&s);
    c.get_remittance(&999);
}
