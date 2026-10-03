#![cfg(test)]

use super::*;
use soroban_sdk::testutils::{Address as _, Events, Ledger};
use soroban_sdk::{token, Bytes, Env, Symbol, TryIntoVal, Vec};

const DAY: u64 = 86_400;

struct Setup {
    env: Env,
    client_id: Address,
    token_id: Address,
    admin: Address,
    sender: Address,
    anchor: Address,
    treasury: Address,
}

fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let sender = Address::generate(&env);
    let anchor = Address::generate(&env);
    let treasury = Address::generate(&env);

    // Deploy a mock SAC token and mint starting balances.
    let token_admin = Address::generate(&env);
    let token_id = env.register_stellar_asset_contract(token_admin.clone());
    let token_sac = token::StellarAssetClient::new(&env, &token_id);
    token_sac.mint(&sender, &1_000_000);
    token_sac.mint(&anchor, &1_000_000);

    let contract_id = env.register_contract(None, RemittanceEscrow);
    let client = RemittanceEscrowClient::new(&env, &contract_id);
    client.initialize(&admin, &token_id);

    // Bond-staking guard (Issue #929): configure the treasury and fund the
    // anchor's minimum liquidity bond so remittance tests can create tasks.
    client.set_treasury(&admin, &treasury);
    client.deposit_bond(&anchor, &BOND_MIN);

    Setup {
        env,
        client_id: contract_id,
        token_id,
        admin,
        sender,
        anchor,
        treasury,
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
#[should_panic(expected = "ContractError(Contract, #2)")]
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
#[should_panic(expected = "ContractError(Contract, #4)")]
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

    // setup() already staked BOND_MIN (20_000) for the anchor.
    assert_eq!(c.get_collateral(&s.anchor), BOND_MIN);

    let anchor_balance_before = tok.balance(&s.anchor);
    let contract_balance_before = tok.balance(&s.client_id);
    c.deposit_collateral(&s.anchor, &5_000);

    assert_eq!(c.get_collateral(&s.anchor), BOND_MIN + 5_000);
    assert_eq!(tok.balance(&s.anchor), anchor_balance_before - 5_000);
    assert_eq!(tok.balance(&s.client_id), contract_balance_before + 5_000);

    // A second deposit accumulates.
    c.deposit_collateral(&s.anchor, &2_500);
    assert_eq!(c.get_collateral(&s.anchor), BOND_MIN + 7_500);
}

#[test]
#[should_panic(expected = "ContractError(Contract, #4)")]
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
#[should_panic(expected = "ContractError(Contract, #3)")]
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
#[should_panic(expected = "ContractError(Contract, #7)")]
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
#[should_panic(expected = "ContractError(Contract, #7)")]
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

    // The 20% bond slash (Issue #929) is paid to the treasury first:
    // 20% × (20_000 setup + 10_000 extra) = 6_000.
    assert_eq!(tok.balance(&s.treasury), 6_000);

    // Remaining collateral (30_000 − 6_000 = 24_000) is seized up to the
    // remittance amount (10_000), leaving 14_000 on the anchor.
    assert_eq!(c.get_collateral(&s.anchor), 14_000);
}

#[test]
#[should_panic(expected = "ContractError(Contract, #3)")]
fn test_dispute_wrong_sender_fails() {
    let s = setup();
    let c = client(&s);

    let id = c.create_remittance(&s.sender, &s.anchor, &10_000, &DAY);
    advance_time(&s.env, DAY + DAY);

    let stranger = Address::generate(&s.env);
    c.open_dispute(&stranger, &id);
}

#[test]
#[should_panic(expected = "ContractError(Contract, #6)")]
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
#[should_panic(expected = "ContractError(Contract, #6)")]
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
    // Anchor only staked a fraction of the remittance amount on top of the
    // minimum bond (20_000 setup + 2_000 extra = 22_000).
    c.deposit_collateral(&s.anchor, &2_000);

    let sender_balance_after_create = tok.balance(&s.sender);

    advance_time(&s.env, DAY + DAY);
    c.open_dispute(&s.sender, &id);

    // 20% slash first: 22_000 × 20% = 4_400 → 17_600 remaining, then the
    // lock seizes up to the remittance amount (10_000) → 7_600 left.
    assert_eq!(tok.balance(&s.treasury), 4_400);
    assert_eq!(c.get_collateral(&s.anchor), 7_600);

    // Sender is still fully refunded regardless of the collateral shortfall.
    assert_eq!(tok.balance(&s.sender), sender_balance_after_create + 10_000);

    let remittance = c.get_remittance(&id);
    assert_eq!(remittance.status, RemittanceStatus::Refunded);
}

#[test]
fn test_dispute_with_minimum_bond_only_slashes_then_refunds_sender() {
    let s = setup();
    let c = client(&s);
    let tok = token_client(&s);

    // The anchor holds exactly the minimum bond (BOND_MIN via setup) — the
    // zero-collateral anchor can no longer accept remittances (Issue #929).
    let id = c.create_remittance(&s.sender, &s.anchor, &10_000, &DAY);

    let sender_balance_after_create = tok.balance(&s.sender);

    advance_time(&s.env, DAY + DAY);
    c.open_dispute(&s.sender, &id);

    // 20% slash of the minimum bond (20_000 × 20% = 4_000) to the treasury,
    // then up to the remittance amount is seized (16_000 − 10_000 = 6_000).
    assert_eq!(tok.balance(&s.treasury), 4_000);
    assert_eq!(c.get_collateral(&s.anchor), 6_000);

    // The sender is still fully refunded.
    assert_eq!(tok.balance(&s.sender), sender_balance_after_create + 10_000);
}

#[test]
#[should_panic(expected = "ContractError(Contract, #5)")]
fn test_get_remittance_not_found() {
    let s = setup();
    let c = client(&s);
    c.get_remittance(&999);
}
#[test]
#[should_panic(expected = "Error(Contract, #9)")]
fn test_create_remittance_requires_minimum_bond() {
    // An anchor with no staked bond may not accept transactions (Issue #929).
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let sender = Address::generate(&env);
    let anchor = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = env.register_stellar_asset_contract(token_admin.clone());
    let token_sac = token::StellarAssetClient::new(&env, &token_id);
    token_sac.mint(&sender, &1_000_000);
    token_sac.mint(&anchor, &1_000_000);

    let contract_id = env.register_contract(None, RemittanceEscrow);
    let client = RemittanceEscrowClient::new(&env, &contract_id);
    client.initialize(&admin, &token_id);

    client.create_remittance(&sender, &anchor, &10_000, &DAY);
}

#[test]
fn test_bond_lock_lifecycle_around_proof() {
    let s = setup();
    let c = client(&s);

    assert!(!c.is_bond_locked(&s.anchor));
    assert_eq!(c.get_pending_remittance_count(&s.anchor), 0);

    let id = c.create_remittance(&s.sender, &s.anchor, &10_000, &DAY);
    assert!(c.is_bond_locked(&s.anchor));
    assert_eq!(c.get_pending_remittance_count(&s.anchor), 1);

    let proof = Bytes::from_slice(&s.env, b"receipt-hash");
    c.submit_payout_proof(&s.anchor, &id, &proof);

    assert!(!c.is_bond_locked(&s.anchor));
    assert_eq!(c.get_pending_remittance_count(&s.anchor), 0);
}

#[test]
fn test_bond_stays_locked_until_all_pending_finalize() {
    let s = setup();
    let c = client(&s);

    let id1 = c.create_remittance(&s.sender, &s.anchor, &10_000, &DAY);
    let id2 = c.create_remittance(&s.sender, &s.anchor, &10_000, &DAY);
    assert!(c.is_bond_locked(&s.anchor));
    assert_eq!(c.get_pending_remittance_count(&s.anchor), 2);

    // One proof keeps the bond locked — a second settlement is still active.
    let proof = Bytes::from_slice(&s.env, b"receipt-hash");
    c.submit_payout_proof(&s.anchor, &id1, &proof);
    assert!(c.is_bond_locked(&s.anchor));
    assert_eq!(c.get_pending_remittance_count(&s.anchor), 1);

    // Finalizing the last task unlocks the bond.
    c.submit_payout_proof(&s.anchor, &id2, &proof);
    assert!(!c.is_bond_locked(&s.anchor));
    assert_eq!(c.get_pending_remittance_count(&s.anchor), 0);
}

#[test]
fn test_dispute_unlocks_bond_after_slash() {
    let s = setup();
    let c = client(&s);

    let id = c.create_remittance(&s.sender, &s.anchor, &10_000, &DAY);
    assert!(c.is_bond_locked(&s.anchor));

    advance_time(&s.env, DAY + DAY);
    c.open_dispute(&s.sender, &id);

    let remittance = c.get_remittance(&id);
    assert_eq!(remittance.status, RemittanceStatus::Refunded);
    assert!(!c.is_bond_locked(&s.anchor));
    assert_eq!(c.get_pending_remittance_count(&s.anchor), 0);
}

#[test]
#[should_panic(expected = "Error(Contract, #10)")]
fn test_dispute_without_treasury_fails() {
    // Treasury never configured → the 20% slash has no sink (Issue #929).
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let sender = Address::generate(&env);
    let anchor = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = env.register_stellar_asset_contract(token_admin.clone());
    let token_sac = token::StellarAssetClient::new(&env, &token_id);
    token_sac.mint(&sender, &1_000_000);
    token_sac.mint(&anchor, &1_000_000);

    let contract_id = env.register_contract(None, RemittanceEscrow);
    let client = RemittanceEscrowClient::new(&env, &contract_id);
    client.initialize(&admin, &token_id);
    client.deposit_bond(&anchor, &BOND_MIN);

    let id = client.create_remittance(&sender, &anchor, &10_000, &DAY);
    advance_time(&env, DAY + DAY);
    client.open_dispute(&sender, &id);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_set_treasury_requires_admin() {
    let s = setup();
    let c = client(&s);
    let impostor = Address::generate(&s.env);
    c.set_treasury(&impostor, &s.treasury);
}

#[test]
fn test_get_treasury_address_after_initialization() {
    let s = setup();
    let c = client(&s);
    assert_eq!(c.get_treasury_address(), Some(s.treasury));
}
