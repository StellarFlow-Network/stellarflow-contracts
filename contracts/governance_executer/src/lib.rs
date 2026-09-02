#![no_std]
use soroban_sdk::{contract, contractimpl, contracttype, Address, Bytes, Env, Symbol, Vec};

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Proposal {
    pub id: u64,
    pub target: Address,
    pub function: Symbol,
    pub payload: Vec<soroban_sdk::Val>,
    pub executed: bool,
    pub timelock_until: u64,
}

#[contracttype]
pub enum DataKey {
    Proposal(u64),
    ProposalCount,
    Admin,
}

#[contract]
pub struct GovernanceExecuterContract;

#[contractimpl]
impl GovernanceExecuterContract {
    /// Initialize the contract with an administrator
    pub fn initialize(env: Env, admin: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("already initialized");
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::ProposalCount, &0u64);
    }

    /// Store target contract address, function symbol, binary argument payload, and timelock
    pub fn create_proposal(
        env: Env,
        target: Address,
        function: Symbol,
        payload: Vec<soroban_sdk::Val>,
        timelock_delay_seconds: u64,
    ) -> u64 {
        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        admin.require_auth();

        let mut count: u64 = env.storage().instance().get(&DataKey::ProposalCount).unwrap_or(0);
        count += 1;

        let current_time = env.ledger().timestamp();
        let timelock_until = current_time + timelock_delay_seconds;

        let proposal = Proposal {
            id: count,
            target,
            function,
            payload,
            executed: false,
            timelock_until,
        };

        env.storage().persistent().set(&DataKey::Proposal(count), &proposal);
        env.storage().instance().set(&DataKey::ProposalCount, &count);

        count
    }

    /// Execute transaction via dynamic call dispatch when execute() is invoked
    pub fn execute(env: Env, proposal_id: u64) -> soroban_sdk::Val {
        let mut proposal: Proposal = env
            .storage()
            .persistent()
            .get(&DataKey::Proposal(proposal_id))
            .expect("proposal not found");

        // Prevent duplicate invocation & ensure execution state
        if proposal.executed {
            panic!("proposal already executed");
        }

        // Verify timelock check
        let current_time = env.ledger().timestamp();
        if current_time < proposal.timelock_until {
            panic!("timelock period has not expired");
        }

        // Mark proposal state as Executed before dispatch to prevent re-entrancy
        proposal.executed = true;
        env.storage().persistent().set(&DataKey::Proposal(proposal_id), &proposal);

        // Execute transaction via dynamic call dispatch
        env.invoke_contract(
            &proposal.target,
            &proposal.function,
            proposal.payload,
        )
    }

    /// Retrieve proposal details
    pub fn get_proposal(env: Env, proposal_id: u64) -> Proposal {
        env.storage()
            .persistent()
            .get(&DataKey::Proposal(proposal_id))
            .expect("proposal not found")
    }
}

mod test;
