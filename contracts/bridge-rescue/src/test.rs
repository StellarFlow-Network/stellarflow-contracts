#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::Address as _, testutils::Events, token, Env, String, Vec,
};

/// Test fixture: deploys the contract, a mock SAC token, and a 2-of-3 admin
/// committee alongside a 2-of-3 validator set.
///
/// soroban-sdk 20.x splits the token interface across two clients: the
/// admin-only `StellarAssetClient` (mint, etc.) used to fund test accounts,
/// and the regular `token::Client` (balance, transfer) used to make
/// assertions — a single combined client only exists in later SDK versions.
struct Fixture {
    env: Env,
    client: BridgeRescueClient<'static>,
    token_balance: token::Client<'static>,
    admins: Vec<Address>,
    validators: Vec<Address>,
    sender: Address,
}

fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, BridgeRescue);
    let client = BridgeRescueClient::new(&env, &contract_id);

    let token_admin = Address::generate(&env);
    let token_contract_id = env.register_stellar_asset_contract(token_admin);
    let token_client = token::StellarAssetClient::new(&env, &token_contract_id);
    let token_balance = token::Client::new(&env, &token_contract_id);

    let admin1 = Address::generate(&env);
    let admin2 = Address::generate(&env);
    let admin3 = Address::generate(&env);
    let admins = soroban_sdk::vec![&env, admin1, admin2, admin3];

    let val1 = Address::generate(&env);
    let val2 = Address::generate(&env);
    let val3 = Address::generate(&env);
    let validators = soroban_sdk::vec![&env, val1, val2, val3];

    client.initialize(&admins, &2, &validators, &2, &token_contract_id);

    let sender = Address::generate(&env);
    token_client.mint(&sender, &1_000_000);

    Fixture {
        env,
        client,
        token_balance,
        admins,
        validators,
        sender,
    }
}

fn lock(fx: &Fixture, amount: i128) -> u64 {
    let dest_ref = String::from_str(&fx.env, "eth:0xdeadbeef");
    fx.client.lock_tokens(&fx.sender, &amount, &dest_ref)
}

#[test]
fn test_initialize_rejects_bad_thresholds() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, BridgeRescue);
    let client = BridgeRescueClient::new(&env, &contract_id);

    let token = Address::generate(&env);
    let admins = soroban_sdk::vec![&env, Address::generate(&env)];
    let validators = soroban_sdk::vec![&env, Address::generate(&env)];

    // threshold 0 is invalid
    let res = client.try_initialize(&admins, &0, &validators, &1, &token);
    assert!(res.is_err());

    // threshold > admins.len() is invalid
    let res = client.try_initialize(&admins, &2, &validators, &1, &token);
    assert!(res.is_err());

    // validator_threshold > validators.len() is invalid
    let res = client.try_initialize(&admins, &1, &validators, &2, &token);
    assert!(res.is_err());
}

#[test]
fn test_initialize_rejects_duplicate_addresses() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, BridgeRescue);
    let client = BridgeRescueClient::new(&env, &contract_id);

    let token = Address::generate(&env);
    let dup = Address::generate(&env);
    let admins = soroban_sdk::vec![&env, dup.clone(), dup];
    let validators = soroban_sdk::vec![&env, Address::generate(&env)];

    let res = client.try_initialize(&admins, &1, &validators, &1, &token);
    assert!(res.is_err());
}

#[test]
fn test_lock_tokens_transfers_in_and_creates_lock() {
    let fx = setup();
    let lock_id = lock(&fx, 500);

    assert_eq!(fx.token_balance.balance(&fx.sender), 1_000_000 - 500);
    assert_eq!(
        fx.token_balance.balance(&fx.client.address),
        500
    );

    let stored = fx.client.get_lock(&lock_id).unwrap();
    assert_eq!(stored.sender, fx.sender);
    assert_eq!(stored.amount, 500);
    assert_eq!(stored.status, LockStatus::Locked);
    assert!(!stored.validator_confirmed);
}

#[test]
fn test_lock_tokens_rejects_zero_amount() {
    let fx = setup();
    let dest_ref = String::from_str(&fx.env, "eth:0xdeadbeef");
    let res = fx.client.try_lock_tokens(&fx.sender, &0, &dest_ref);
    assert!(res.is_err());
}

#[test]
fn test_rescue_blocked_when_validator_consensus_not_reached() {
    let fx = setup();
    let lock_id = lock(&fx, 500);

    // Only one of two required validator attestations.
    let att = String::from_str(&fx.env, "attest");
    fx.client
        .submit_failure_proof(&fx.validators.get(0).unwrap(), &lock_id, &att);

    // Admins approve with full threshold met.
    fx.client.approve_rescue(&fx.admins.get(0).unwrap(), &lock_id);
    fx.client.approve_rescue(&fx.admins.get(1).unwrap(), &lock_id);

    // Rescue must NOT have executed: lock still Locked, funds still in contract.
    let stored = fx.client.get_lock(&lock_id).unwrap();
    assert_eq!(stored.status, LockStatus::Locked);
    assert_eq!(fx.token_balance.balance(&fx.client.address), 500);

    // Explicit execute must fail too.
    let res = fx.client.try_execute_rescue(&lock_id);
    assert!(res.is_err());
}

#[test]
fn test_rescue_blocked_when_admin_threshold_not_reached() {
    let fx = setup();
    let lock_id = lock(&fx, 500);

    let att = String::from_str(&fx.env, "attest");
    fx.client
        .submit_failure_proof(&fx.validators.get(0).unwrap(), &lock_id, &att);
    fx.client
        .submit_failure_proof(&fx.validators.get(1).unwrap(), &lock_id, &att);

    // Validator consensus is now confirmed.
    let stored = fx.client.get_lock(&lock_id).unwrap();
    assert!(stored.validator_confirmed);
    assert_eq!(stored.status, LockStatus::Locked);

    // Only one of two required admin approvals.
    fx.client.approve_rescue(&fx.admins.get(0).unwrap(), &lock_id);

    let stored = fx.client.get_lock(&lock_id).unwrap();
    assert_eq!(stored.status, LockStatus::Locked);
    assert_eq!(fx.token_balance.balance(&fx.client.address), 500);

    let res = fx.client.try_execute_rescue(&lock_id);
    assert!(res.is_err());
}

#[test]
fn test_rescue_succeeds_and_emits_event_once_both_thresholds_met() {
    let fx = setup();
    let lock_id = lock(&fx, 777);

    let att = String::from_str(&fx.env, "attest");
    fx.client
        .submit_failure_proof(&fx.validators.get(0).unwrap(), &lock_id, &att);
    fx.client
        .submit_failure_proof(&fx.validators.get(1).unwrap(), &lock_id, &att);

    fx.client.approve_rescue(&fx.admins.get(0).unwrap(), &lock_id);
    // The second approval crosses the admin threshold and triggers execution.
    fx.client.approve_rescue(&fx.admins.get(1).unwrap(), &lock_id);

    let stored = fx.client.get_lock(&lock_id).unwrap();
    assert_eq!(stored.status, LockStatus::Rescued);

    // Funds landed back with the original sender; contract no longer holds them.
    assert_eq!(fx.token_balance.balance(&fx.sender), 1_000_000 - 777 + 777);
    assert_eq!(fx.token_balance.balance(&fx.client.address), 0);

    // BridgeTokensRescued event was emitted.
    let events = fx.env.events().all();
    let found = events.iter().any(|(contract_id, _topics, _data)| {
        contract_id == fx.client.address
    });
    assert!(found, "expected an event from the bridge-rescue contract");
}

#[test]
fn test_duplicate_validator_vote_does_not_double_count() {
    let fx = setup();
    let lock_id = lock(&fx, 500);

    let att = String::from_str(&fx.env, "attest");
    let validator = fx.validators.get(0).unwrap();
    fx.client.submit_failure_proof(&validator, &lock_id, &att);

    let res = fx.client.try_submit_failure_proof(&validator, &lock_id, &att);
    assert!(res.is_err());

    assert_eq!(fx.client.get_validator_vote_count(&lock_id), 1);
}

#[test]
fn test_duplicate_admin_approval_does_not_double_count() {
    let fx = setup();
    let lock_id = lock(&fx, 500);

    let admin = fx.admins.get(0).unwrap();
    fx.client.approve_rescue(&admin, &lock_id);

    let res = fx.client.try_approve_rescue(&admin, &lock_id);
    assert!(res.is_err());

    assert_eq!(fx.client.get_admin_approval_count(&lock_id), 1);
}

#[test]
fn test_second_approve_after_rescue_is_rejected_and_does_not_transfer_again() {
    let fx = setup();
    let lock_id = lock(&fx, 900);

    let att = String::from_str(&fx.env, "attest");
    fx.client
        .submit_failure_proof(&fx.validators.get(0).unwrap(), &lock_id, &att);
    fx.client
        .submit_failure_proof(&fx.validators.get(1).unwrap(), &lock_id, &att);

    fx.client.approve_rescue(&fx.admins.get(0).unwrap(), &lock_id);
    fx.client.approve_rescue(&fx.admins.get(1).unwrap(), &lock_id);

    // Rescue has now executed exactly once.
    let stored = fx.client.get_lock(&lock_id).unwrap();
    assert_eq!(stored.status, LockStatus::Rescued);
    let sender_balance_after_first_rescue = fx.token_balance.balance(&fx.sender);

    // A third, previously-unused admin tries to approve after the rescue already
    // executed. This must be rejected (lock is no longer `Locked`) and must not
    // move any additional funds.
    let res = fx
        .client
        .try_approve_rescue(&fx.admins.get(2).unwrap(), &lock_id);
    assert!(res.is_err());

    // Direct double-rescue attempt via the explicit execute entry point too.
    let res2 = fx.client.try_execute_rescue(&lock_id);
    assert!(res2.is_err());

    // No additional funds moved; contract balance stays at zero for this lock's amount.
    assert_eq!(
        fx.token_balance.balance(&fx.sender),
        sender_balance_after_first_rescue
    );
    assert_eq!(fx.token_balance.balance(&fx.client.address), 0);

    let stored_final = fx.client.get_lock(&lock_id).unwrap();
    assert_eq!(stored_final.status, LockStatus::Rescued);
}

#[test]
fn test_non_validator_cannot_vote() {
    let fx = setup();
    let lock_id = lock(&fx, 500);

    let outsider = Address::generate(&fx.env);
    let att = String::from_str(&fx.env, "attest");
    let res = fx.client.try_submit_failure_proof(&outsider, &lock_id, &att);
    assert!(res.is_err());
}

#[test]
fn test_non_admin_cannot_approve() {
    let fx = setup();
    let lock_id = lock(&fx, 500);

    let outsider = Address::generate(&fx.env);
    let res = fx.client.try_approve_rescue(&outsider, &lock_id);
    assert!(res.is_err());
}

#[test]
fn test_approve_rescue_on_unknown_lock_fails() {
    let fx = setup();
    let res = fx.client.try_approve_rescue(&fx.admins.get(0).unwrap(), &999);
    assert!(res.is_err());
}

#[test]
fn test_getters_reflect_vote_state() {
    let fx = setup();
    let lock_id = lock(&fx, 500);

    let admin = fx.admins.get(0).unwrap();
    let validator = fx.validators.get(0).unwrap();

    assert!(!fx.client.has_admin_approved(&lock_id, &admin));
    assert!(!fx.client.has_validator_voted(&lock_id, &validator));

    fx.client.approve_rescue(&admin, &lock_id);
    let att = String::from_str(&fx.env, "attest");
    fx.client.submit_failure_proof(&validator, &lock_id, &att);

    assert!(fx.client.has_admin_approved(&lock_id, &admin));
    assert!(fx.client.has_validator_voted(&lock_id, &validator));
    assert_eq!(fx.client.get_admin_approval_count(&lock_id), 1);
    assert_eq!(fx.client.get_validator_vote_count(&lock_id), 1);
}
