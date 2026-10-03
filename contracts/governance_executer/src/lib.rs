#![no_std]

use soroban_sdk::{contract, contracterror, contractimpl, contracttype, Address, Bytes, Env};


#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProposalPriority {
    Critical,
    High,
    Standard,
}

impl ProposalPriority {
    fn rank(self) -> u8 {
        match self {
            ProposalPriority::Critical => 3,
            ProposalPriority::High => 2,
            ProposalPriority::Standard => 1,
        }
    }
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Proposal {
    pub id: u64,
    pub target: Address,
    pub function: Symbol,
    pub payload: Vec<soroban_sdk::Val>,
    pub executed: bool,
    pub timelock_until: u64,
    pub priority: ProposalPriority,
}

#[contract]
pub struct GovernanceExecuterContract;

#[contractimpl]
impl GovernanceExecuterContract {
    /// Initialize the contract with an administrator
    pub fn initialize(env: Env, admin: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(ContractError::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::ProposalCount, &0u64);
    }

    /// Backward-compatible proposal creation. Defaults to `STANDARD` priority.
    pub fn create_proposal(
        env: Env,
        target: Address,
        function: Symbol,
        payload: Vec<soroban_sdk::Val>,
        timelock_delay_seconds: u64,
    ) -> u64 {
        Self::create_proposal_with_priority(
            env,
            target,
            function,
            payload,
            timelock_delay_seconds,
            ProposalPriority::Standard,
        )
    }

    /// Store a target contract address, function symbol, binary payload, timelock,
    /// and priority classification. Higher-priority proposals are executed first.
    pub fn create_proposal_with_priority(
        env: Env,
        target: Address,
        function: Symbol,
        payload: Vec<soroban_sdk::Val>,
        timelock_delay_seconds: u64,
        priority: ProposalPriority,
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
            priority,
        };

        env.storage().persistent().set(&DataKey::Proposal(count), &proposal);
        env.storage().instance().set(&DataKey::ProposalCount, &count);

        count
    }

    /// Execute a single proposal. This preserves the original contract behavior.
    pub fn execute(env: Env, proposal_id: u64) -> soroban_sdk::Val {
        Self::execute_single(&env, proposal_id)
    }

    /// Execute a batch in priority order: CRITICAL -> HIGH -> STANDARD.
    /// If any CRITICAL proposal fails, the entire batch reverts and the remaining
    /// standard proposals are not executed.
    pub fn execute_batch(env: Env, proposal_ids: Vec<u64>) {
        let mut critical: Vec<u64> = Vec::new(&env);
        let mut high: Vec<u64> = Vec::new(&env);
        let mut standard: Vec<u64> = Vec::new(&env);

        for proposal_id in proposal_ids.iter() {
            let proposal = Self::get_proposal_internal(&env, proposal_id);
            match proposal.priority {
                ProposalPriority::Critical => critical.push_back(proposal_id),
                ProposalPriority::High => high.push_back(proposal_id),
                ProposalPriority::Standard => standard.push_back(proposal_id),
            }
        }

        for proposal_id in critical.iter() {
            Self::execute_single(&env, proposal_id);
        }
        for proposal_id in high.iter() {
            Self::execute_single(&env, proposal_id);
        }
        for proposal_id in standard.iter() {
            Self::execute_single(&env, proposal_id);
        }
    }

    /// Alias for batch execution used by governance queues and tests.
    pub fn execute_proposals(env: Env, proposal_ids: Vec<u64>) {
        Self::execute_batch(env, proposal_ids);
    }

    /// Retrieve proposal details
    pub fn get_proposal(env: Env, proposal_id: u64) -> Proposal {
        Self::get_proposal_internal(&env, proposal_id)
    }

    fn execute_single(env: &Env, proposal_id: u64) -> soroban_sdk::Val {
        let mut proposal: Proposal = env
            .storage()
            .persistent()
            .get(&DataKey::Proposal(proposal_id))
            .expect("proposal not found");

        if proposal.executed {
            return Err(ContractError::ProposalAlreadyExecuted);
        }

        let current_time = env.ledger().timestamp();
        if current_time < proposal.timelock_until {
            return Err(ContractError::TimelockPeriodHasNotExpired);
        }

        proposal.executed = true;
        env.storage().persistent().set(&DataKey::Proposal(proposal_id), &proposal);

        env.invoke_contract(
            &proposal.target,
            &proposal.function,
            proposal.payload,
        )
    }

    fn get_proposal_internal(env: &Env, proposal_id: u64) -> Proposal {
        env.storage()
            .persistent()
            .get(&DataKey::Proposal(proposal_id))
            .expect("proposal not found")
    }

    /// Execute multiple queued governance proposals in a single atomic transaction post-timelock
    pub fn execute_batch(env: Env, proposal_ids: Vec<u64>) -> Vec<soroban_sdk::Val> {
        let current_time = env.ledger().timestamp();
        let mut results = vec![&env];

        // First pass: verify existence, execution state, and timelock for all proposals in the batch
        for proposal_id in proposal_ids.iter() {
            let proposal: Proposal = env
                .storage()
                .persistent()
                .get(&DataKey::Proposal(proposal_id))
                .expect("proposal not found");

            if proposal.executed {
                panic!("proposal already executed");
            }

            if current_time < proposal.timelock_until {
                panic!("timelock period has not expired");
            }
        }

        // Second pass: mark all as executed and execute sequentially in order of submission
        for proposal_id in proposal_ids.iter() {
            let mut proposal: Proposal = env
                .storage()
                .persistent()
                .get(&DataKey::Proposal(proposal_id))
                .expect("proposal not found");

            proposal.executed = true;
            env.storage().persistent().set(&DataKey::Proposal(proposal_id), &proposal);

            let res = env.invoke_contract(
                &proposal.target,
                &proposal.function,
                proposal.payload,
            );
            results.push_back(res);
        }

        results
    }
}
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum ContractError {
    TimelockDelayNotMet = 1,
    ProposalNotFound = 2,
    InvalidPayloadHash = 3,
}

#[contracttype]
#[derive(Clone)]
pub struct GovernanceProposal {
    pub id: u64,
    pub approved_at: u64,
    pub delay: u64,
    pub payload_hash: Bytes,
}

#[contracttype]
#[derive(Clone)]
enum DataKey {
    Proposal(u64),
}

#[contract]
pub struct GovernanceExecuter;

#[contractimpl]
impl GovernanceExecuter {
    pub fn execute_proposal(
        env: Env,
        proposal_id: u64,
        payload_hash: Bytes,
    ) -> Result<(), ContractError> {
        let proposal: GovernanceProposal = env
            .storage()
            .persistent()
            .get(&DataKey::Proposal(proposal_id))
            .ok_or(ContractError::ProposalNotFound)?;

        if proposal.payload_hash != payload_hash {
            return Err(ContractError::InvalidPayloadHash);
        }

        let current_time = env.ledger().timestamp();
        let required_time = proposal.approved_at.saturating_add(proposal.delay);
        if current_time < required_time {
            return Err(ContractError::TimelockDelayNotMet);
        }

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Env, Bytes};
use soroban_sdk::{contract, contractimpl, contracttype, Address, Bytes, Env, Symbol, Vec};

    #[test]
    fn test_timelock_execution_guard()
    {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, GovernanceExecuter);
        let client = GovernanceExecuterClient::new(&env, &contract_id);

        let proposal_id = 1u64;
        let delay = 172800u64; // 48 hours
        let approved_at = 1000u64;
        let payload_hash = Bytes::from_slice(&env, b"payload_tx_hash");

        env.storage().persistent().set(
            &DataKey::Proposal(proposal_id),
            &GovernanceProposal {
                id: proposal_id,
                approved_at,
                delay,
                payload_hash: payload_hash.clone(),
            },
        );

        env.ledger().set_timestamp(approved_at + delay - 1);
        let res = client.try_execute_proposal(&proposal_id, &payload_hash);
        assert_eq!(res, Err(Ok(ContractError::TimelockDelayNotMet)));

        env.ledger().set_timestamp(approved_at + delay);
        let res = client.try_execute_proposal(&proposal_id, &payload_hash);
        assert!(res.is_ok());
    }
}
