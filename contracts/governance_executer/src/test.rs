use super::*;
use soroban_sdk::{contract, contractimpl, contracttype, vec, Env, Symbol, Vec};

#[contracttype]
#[derive(Clone)]
enum ProbeKey {
    History,
}

#[contract]
pub struct PriorityProbe;

#[contractimpl]
impl PriorityProbe {
    pub fn record(env: Env, value: u32) {
        let mut history: Vec<u32> = env
            .storage()
            .instance()
            .get(&ProbeKey::History)
            .unwrap_or_else(|| Vec::new(&env));
        history.push_back(value);
        env.storage().instance().set(&ProbeKey::History, &history);
    }

    pub fn fail(_env: Env) {
        panic!("critical proposal failed");
    }

    pub fn history(env: Env) -> Vec<u32> {
        env.storage()
            .instance()
            .get(&ProbeKey::History)
            .unwrap_or_else(|| Vec::new(&env))
    }
}

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

    let proposal_id = client.create_proposal(&target, &function, &payload, &0);
    assert_eq!(proposal_id, 1);

    let proposal = client.get_proposal(&proposal_id);
    assert_eq!(proposal.priority, ProposalPriority::Standard);
    assert!(!proposal.executed);
}

#[test]
fn test_execute_batch_prioritizes_critical_first() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(GovernanceExecuterContract, ());
    let client = GovernanceExecuterContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let probe_id = env.register_contract(None, PriorityProbe);
    let probe = PriorityProbeClient::new(&env, &probe_id);

    let standard = client.create_proposal_with_priority(
        &probe_id,
        &Symbol::new(&env, "record"),
        &vec![&env, 10u32.into_val(&env)],
        &0,
        &ProposalPriority::Standard,
    );
    let critical = client.create_proposal_with_priority(
        &probe_id,
        &Symbol::new(&env, "record"),
        &vec![&env, 30u32.into_val(&env)],
        &0,
        &ProposalPriority::Critical,
    );
    let high = client.create_proposal_with_priority(
        &probe_id,
        &Symbol::new(&env, "record"),
        &vec![&env, 20u32.into_val(&env)],
        &0,
        &ProposalPriority::High,
    );

    client.execute_batch(&vec![&env, standard, critical, high]);

    let history = probe.history();
    assert_eq!(history.len(), 3);
    assert_eq!(history.get(0).unwrap(), 30u32);
    assert_eq!(history.get(1).unwrap(), 20u32);
    assert_eq!(history.get(2).unwrap(), 10u32);
}

#[test]
fn test_execute_batch_reverts_when_critical_proposal_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(GovernanceExecuterContract, ());
    let client = GovernanceExecuterContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let probe_id = env.register_contract(None, PriorityProbe);
    let probe = PriorityProbeClient::new(&env, &probe_id);

    let critical = client.create_proposal_with_priority(
        &probe_id,
        &Symbol::new(&env, "fail"),
        &vec![&env],
        &0,
        &ProposalPriority::Critical,
    );
    let standard = client.create_proposal_with_priority(
        &probe_id,
        &Symbol::new(&env, "record"),
        &vec![&env, 99u32.into_val(&env)],
        &0,
        &ProposalPriority::Standard,
    );

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.execute_batch(&vec![&env, standard, critical]);
    }));

    assert!(result.is_err());
    assert_eq!(probe.history().len(), 0);
}
