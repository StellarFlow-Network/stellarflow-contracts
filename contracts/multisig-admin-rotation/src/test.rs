use super::*;
use soroban-sdk::{Env, Vec};
use soroban-sdk::testutils::{Address as _, Ledger};

#[test]
fn test_multisig_admin_rotation_flow() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, MultisigAdminRotationContract);
    let client = MultisigAdminRotationContractClient::new(&env, &contract_id);

    let key1 = Address::generate(&env);
    let key2 = Address::generate(&env);
    let mut initial_keys = Vec::new(&env);
    initial_keys.push_back(key1.clone());
    initial_keys.push_back(key2.clone());

    client.initialize(&initial_keys);
    assert_eq!(client.get_admin_keys(), initial_keys);

    let new_key1 = Address::generate(&env);
    let new_key2 = Address::generate(&env);
    let mut new_keys = Vec::new(&env);
    new_keys.push_back(new_key1.clone());
    new_keys.push_back(new_key2.clone());

    let mut approvers = Vec::new(&env);
    approvers.push_back(key1.clone());
    approvers.push_back(key2.clone());

    // Propose rotation
    client.propose_rotation(&approvers, &new_keys);

    let pending = client.get_pending_rotation().unwrap();
    assert_eq!(pending.new_admin_keys, new_keys);

    // Attempt applying before 24 hours should fail
    let result = std::panic::catch_unwind(|| {
        client.apply_rotation();
    });
    assert!(result.is_err());

    // Advance ledger timestamp by 24 hours (86400 seconds)
    env.ledger().set_timestamp(env.ledger().timestamp() + 86400);

    // Apply rotation successfully
    client.apply_rotation();

    assert_eq!(client.get_admin_keys(), new_keys);
    assert!(client.get_pending_rotation().is_none());
}
