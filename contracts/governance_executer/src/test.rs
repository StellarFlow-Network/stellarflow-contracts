use super::*;
use soroban_sdk::{Env, Symbol, vec};

#[test]
fn test_proposal_lifecycle() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(GovernanceExecuterContract, ());
    let client = GovernanceExecuterContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    client.initialize(&admin);

    let target = Address::generate(&env);
    let function = Symbol::new(&env, "some_func");
    let payload = vec![&env];

    // Create proposal with 0 delay for testing
    let proposal_id = client.create_proposal(&target, &function, &payload, &0);
    assert_eq!(proposal_id, 1);

    let proposal = client.get_proposal(&1);
    assert!(!proposal.executed);

    // Note: In a full integration test, target contract would be invoked here.
}
