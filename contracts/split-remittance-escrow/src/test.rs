#![cfg(test)]

use super::*;
use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{token, Env, Vec};

struct Setup {
    env: Env,
    client_id: Address,
    token_id: Address,
    admin: Address,
    sender: Address,
    anchor_a: Address,
    anchor_b: Address,
    anchor_c: Address,
}

fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let sender = Address::generate(&env);
    let anchor_a = Address::generate(&env);
    let anchor_b = Address::generate(&env);
    let anchor_c = Address::generate(&env);

    let token_admin = Address::generate(&env);
    let token_id = env.register_stellar_asset_contract(token_admin.clone());
    let token_sac = token::StellarAssetClient::new(&env, &token_id);
    token_sac.mint(&sender, &1_000_000);

    let contract_id = env.register_contract(None, SplitRemittanceEscrow);
    let client = SplitRemittanceEscrowClient::new(&env, &contract_id);
    client.initialize(&admin, &token_id);

    Setup {
        env,
        client_id: contract_id,
        token_id,
        admin,
        sender,
        anchor_a,
        anchor_b,
        anchor_c,
    }
}

fn client(s: &Setup) -> SplitRemittanceEscrowClient<'static> {
    SplitRemittanceEscrowClient::new(&s.env, &s.client_id)
}

fn token_client(s: &Setup) -> token::Client<'static> {
    token::Client::new(&s.env, &s.token_id)
}

fn advance_time(env: &Env, delta: u64) {
    env.ledger().with_mut(|li| {
        li.timestamp = li.timestamp.saturating_add(delta);
    });
}

fn two_way_dests(s: &Setup) -> Vec<(Address, u32)> {
    let mut d = Vec::new(&s.env);
    d.push_back((s.anchor_a.clone(), 6000u32)); // 60%
    d.push_back((s.anchor_b.clone(), 4000u32)); // 40%
    d
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
fn test_create_split_order_locks_and_allocates_partials() {
    let s = setup();
    let c = client(&s);
    let tok = token_client(&s);

    let before = tok.balance(&s.sender);
    let id = c.create_split_order(&s.sender, &10_000, &two_way_dests(&s));
    assert_eq!(id, 0);
    assert_eq!(tok.balance(&s.sender), before - 10_000);
    assert_eq!(tok.balance(&s.client_id), 10_000);

    let order = c.get_order(&id);
    assert_eq!(order.total_amount, 10_000);
    assert_eq!(order.status, OrderStatus::Open);
    assert_eq!(order.legs.len(), 2);
    // E_partial = E_total × p / 10000
    assert_eq!(order.legs.get(0).unwrap().amount, 6_000);
    assert_eq!(order.legs.get(1).unwrap().amount, 4_000);
    assert_eq!(c.get_partial_released(&id), 0);
    assert_eq!(c.get_remaining_locked(&id), 10_000);
}

#[test]
#[should_panic(expected = "Error(Contract, #9)")]
fn test_create_invalid_proportions_fails() {
    let s = setup();
    let c = client(&s);
    let mut d = Vec::new(&s.env);
    d.push_back((s.anchor_a.clone(), 5000u32));
    d.push_back((s.anchor_b.clone(), 4000u32)); // sum 9000 != 10000
    c.create_split_order(&s.sender, &10_000, &d);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_create_zero_amount_fails() {
    let s = setup();
    let c = client(&s);
    c.create_split_order(&s.sender, &0, &two_way_dests(&s));
}

#[test]
fn test_settle_leg_releases_partial_and_updates_instance_state() {
    let s = setup();
    let c = client(&s);
    let tok = token_client(&s);

    let id = c.create_split_order(&s.sender, &10_000, &two_way_dests(&s));
    c.settle_leg(&s.anchor_a, &id);

    assert_eq!(tok.balance(&s.anchor_a), 6_000);
    assert_eq!(tok.balance(&s.client_id), 4_000);
    assert_eq!(c.get_partial_released(&id), 6_000);
    assert_eq!(c.get_remaining_locked(&id), 4_000);

    let order = c.get_order(&id);
    assert_eq!(order.legs.get(0).unwrap().status, LegStatus::Settled);
    assert_eq!(order.status, OrderStatus::Open);
}

#[test]
fn test_all_legs_settled_completes_order() {
    let s = setup();
    let c = client(&s);
    let tok = token_client(&s);

    let id = c.create_split_order(&s.sender, &10_000, &two_way_dests(&s));
    c.settle_leg(&s.anchor_a, &id);
    c.settle_leg(&s.anchor_b, &id);

    assert_eq!(c.get_order(&id).status, OrderStatus::Completed);
    assert_eq!(c.get_partial_released(&id), 10_000);
    assert_eq!(c.get_remaining_locked(&id), 0);
    assert_eq!(tok.balance(&s.client_id), 0);
}

#[test]
#[should_panic(expected = "Error(Contract, #12)")]
fn test_double_settle_fails() {
    let s = setup();
    let c = client(&s);
    let id = c.create_split_order(&s.sender, &10_000, &two_way_dests(&s));
    c.settle_leg(&s.anchor_a, &id);
    c.settle_leg(&s.anchor_a, &id);
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_refund_before_12h_fails() {
    let s = setup();
    let c = client(&s);
    let id = c.create_split_order(&s.sender, &10_000, &two_way_dests(&s));
    c.refund_unsettled(&s.sender, &id);
}

#[test]
fn test_refund_unsettled_after_12h() {
    let s = setup();
    let c = client(&s);
    let tok = token_client(&s);

    let id = c.create_split_order(&s.sender, &10_000, &two_way_dests(&s));
    // One anchor settles; the other times out.
    c.settle_leg(&s.anchor_a, &id);

    advance_time(&s.env, SETTLE_WINDOW_SECS);

    let sender_before = tok.balance(&s.sender);
    c.refund_unsettled(&s.sender, &id);

    assert_eq!(tok.balance(&s.sender), sender_before + 4_000);
    assert_eq!(c.get_remaining_locked(&id), 0);
    assert_eq!(c.get_partial_released(&id), 6_000);

    let order = c.get_order(&id);
    assert_eq!(order.status, OrderStatus::Refunded);
    assert_eq!(order.legs.get(0).unwrap().status, LegStatus::Settled);
    assert_eq!(order.legs.get(1).unwrap().status, LegStatus::Refunded);
}

#[test]
fn test_three_way_split_partial_formula() {
    let s = setup();
    let c = client(&s);

    let mut d = Vec::new(&s.env);
    d.push_back((s.anchor_a.clone(), 3333u32));
    d.push_back((s.anchor_b.clone(), 3333u32));
    d.push_back((s.anchor_c.clone(), 3334u32));

    let id = c.create_split_order(&s.sender, &10_000, &d);
    let order = c.get_order(&id);
    let a0 = order.legs.get(0).unwrap().amount;
    let a1 = order.legs.get(1).unwrap().amount;
    let a2 = order.legs.get(2).unwrap().amount;
    assert_eq!(a0, 3_333); // 10000 * 3333 / 10000
    assert_eq!(a1, 3_333);
    assert_eq!(a2, 3_334); // remainder to last leg
    assert_eq!(a0 + a1 + a2, 10_000);
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_refund_after_completed_fails() {
    let s = setup();
    let c = client(&s);
    let id = c.create_split_order(&s.sender, &10_000, &two_way_dests(&s));
    c.settle_leg(&s.anchor_a, &id);
    c.settle_leg(&s.anchor_b, &id);
    advance_time(&s.env, SETTLE_WINDOW_SECS);
    c.refund_unsettled(&s.sender, &id);
}
