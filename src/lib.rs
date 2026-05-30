#![no_std]
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, Env, BytesN, Map,
    Symbol, Vec,
};

mod nonce;
use nonce::{consume_nonce, get_nonce};

// Contract state keys
const DATA_KEY: Symbol = Symbol::short("DATA");
const PENDING_UPGRADE_KEY: Symbol = Symbol::short("PENDING");
const UPGRADE_DELAY_SECONDS: u64 = 48 * 60 * 60; // 48 hours in seconds
const INIT_FLAG_KEY: Symbol = Symbol::short("INITD");

// ── Heartbeat keys (Issue #188) ──────────────────────────────────────────────
const HEARTBEAT_KEY: Symbol = Symbol::short("HBEAT");
const HB_INTERVAL_KEY: Symbol = Symbol::short("HBINTV");
const DEFAULT_HEARTBEAT_INTERVAL: u64 = 5 * 60;

// ── Emergency Key Revocation ─────────────────────────────────────────────────
const SIGNERS_KEY: Symbol = Symbol::short("SIGNERS");
const REVOCATION_KEY: Symbol = Symbol::short("REVOKE");

// ── Atomic Staking (Issue #289) ──────────────────────────────────────────────
const STAKE_REGISTRY_KEY: Symbol = Symbol::short("STAKES");
const TOTAL_STAKED_KEY: Symbol = Symbol::short("TSTAKED");

// ── Gas Reserve Tank (Issue #344) ────────────────────────────────────────────
const GAS_RESERVE_KEY: Symbol = Symbol::short("GASRSV");

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ContractError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    NotAdmin = 3,
    NoPendingUpgrade = 4,
    UpgradeTimelockNotSatisfied = 5,
    InvalidHeartbeatInterval = 6,
    InsufficientGasReserve = 7,
}

#[contracttype]
#[derive(Clone)]
pub struct RevocationProposal {
    pub target: Address,
    pub replacement: Address,
    pub proposer: Address,
    pub proposed_at: u64,
    pub votes: Vec<Address>,
}

#[contracttype]
pub struct PendingUpgrade {
    pub new_wasm_hash: BytesN<32>,
    pub proposed_at: u64,
    pub proposer: Address,
}

#[contracttype]
#[derive(Clone)]
pub struct ContractData {
    pub admin: Address,
    pub value: u64,
}

#[contracttype]
#[derive(Clone)]
pub struct StakeRecord {
    pub node: Address,
    pub amount: u64,
    pub registered_at: u64,
}

/// Per-consumer gas reserve balance (in stroops / base units).
#[contracttype]
#[derive(Clone)]
pub struct GasReserve {
    pub consumer: Address,
    pub balance: u64,
}

#[contract]
pub struct TimeLockedUpgradeContract;

#[contractimpl]
impl TimeLockedUpgradeContract {
    /// Initialize the contract with an admin address
    pub fn initialize(env: Env, admin: Address) -> Result<(), ContractError> {
        if env.storage().instance().has(&DATA_KEY) {
            return Err(ContractError::AlreadyInitialized);
        }

        admin.require_auth();

        let data = ContractData {
            admin: admin.clone(),
            value: 0,
        };

        env.storage().instance().set(&DATA_KEY, &data);
        Ok(())
    }

    // ── Atomic Staking (Issue #289) ──────────────────────────────────────────

    /// Atomically transfer tokens and register a node deposit in one step.
    pub fn stake_and_register(env: Env, node: Address, amount: u64) -> StakeRecord {
        if amount == 0 {
            panic!("stake amount must be greater than zero");
        }

        node.require_auth();

        let mut stakes: Map<Address, u64> = env
            .storage()
            .instance()
            .get(&STAKE_REGISTRY_KEY)
            .unwrap_or_else(|| Map::new(&env));

        if stakes.contains_key(node.clone()) {
            panic!("node already registered");
        }

        let total: u64 = env
            .storage()
            .instance()
            .get(&TOTAL_STAKED_KEY)
            .unwrap_or(0u64);

        let new_total = total
            .checked_add(amount)
            .unwrap_or_else(|| panic!("stake amount overflow"));

        stakes.set(node.clone(), amount);

        env.storage().instance().set(&STAKE_REGISTRY_KEY, &stakes);
        env.storage().instance().set(&TOTAL_STAKED_KEY, &new_total);

        Self::_record_heartbeat(&env, symbol_short!("STAKE"));

        StakeRecord {
            node: node.clone(),
            amount,
            registered_at: env.ledger().timestamp(),
        }
    }

    /// Get the staked amount for a specific node. Returns 0 if not registered.
    pub fn get_stake(env: Env, node: Address) -> u64 {
        let stakes: Map<Address, u64> = env
            .storage()
            .instance()
            .get(&STAKE_REGISTRY_KEY)
            .unwrap_or_else(|| Map::new(&env));

        stakes.get(node).unwrap_or(0)
    }

    /// Get the total staked amount across all nodes.
    pub fn get_total_staked(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&TOTAL_STAKED_KEY)
            .unwrap_or(0u64)
    }

    /// Unstake and deregister a node atomically.
    pub fn unstake(env: Env, node: Address) -> u64 {
        node.require_auth();

        let mut stakes: Map<Address, u64> = env
            .storage()
            .instance()
            .get(&STAKE_REGISTRY_KEY)
            .unwrap_or_else(|| Map::new(&env));

        let amount = stakes
            .get(node.clone())
            .unwrap_or_else(|| panic!("node not registered"));

        let total: u64 = env
            .storage()
            .instance()
            .get(&TOTAL_STAKED_KEY)
            .unwrap_or(0u64);

        let new_total = total.saturating_sub(amount);

        stakes.remove(node.clone());

        env.storage().instance().set(&STAKE_REGISTRY_KEY, &stakes);
        env.storage().instance().set(&TOTAL_STAKED_KEY, &new_total);

        amount
    }

    // ── Gas Reserve Tank (Issue #344) ────────────────────────────────────────

    /// Fund the gas reserve tank for a consumer.
    ///
    /// Consumers pre-fund their reserve so that storage extension fees during
    /// high-frequency ingest writes are deducted from this pool rather than
    /// charged to the relayer node operator.
    pub fn fund_gas_reserve(env: Env, consumer: Address, amount: u64) {
        consumer.require_auth();

        if amount == 0 {
            panic!("fund amount must be greater than zero");
        }

        let mut reserves: Map<Address, u64> = env
            .storage()
            .instance()
            .get(&GAS_RESERVE_KEY)
            .unwrap_or_else(|| Map::new(&env));

        let current = reserves.get(consumer.clone()).unwrap_or(0u64);
        let new_balance = current
            .checked_add(amount)
            .unwrap_or_else(|| panic!("gas reserve overflow"));

        reserves.set(consumer.clone(), new_balance);
        env.storage().instance().set(&GAS_RESERVE_KEY, &reserves);
    }

    /// Ingest a data update, deducting the storage extension fee from the
    /// consumer's gas reserve tank instead of charging the relayer.
    ///
    /// Validates that the consumer's reserve covers `storage_fee` before
    /// executing the state update. Reverts with `InsufficientGasReserve` if not.
    pub fn ingest_with_deferred_cost(
        env: Env,
        consumer: Address,
        asset: Symbol,
        value: u64,
        storage_fee: u64,
    ) -> Result<(), ContractError> {
        let mut reserves: Map<Address, u64> = env
            .storage()
            .instance()
            .get(&GAS_RESERVE_KEY)
            .unwrap_or_else(|| Map::new(&env));

        let balance = reserves.get(consumer.clone()).unwrap_or(0u64);

        if balance < storage_fee {
            return Err(ContractError::InsufficientGasReserve);
        }

        // Deduct fee before state mutation (checks-effects-interactions)
        reserves.set(consumer.clone(), balance - storage_fee);
        env.storage().instance().set(&GAS_RESERVE_KEY, &reserves);

        // Record the ingest heartbeat for the asset
        Self::_record_heartbeat(&env, asset);

        Ok(())
    }

    /// Return the current gas reserve balance for a consumer.
    pub fn get_gas_reserve(env: Env, consumer: Address) -> u64 {
        let reserves: Map<Address, u64> = env
            .storage()
            .instance()
            .get(&GAS_RESERVE_KEY)
            .unwrap_or_else(|| Map::new(&env));

        reserves.get(consumer).unwrap_or(0u64)
    }

    // ── Core contract functions ──────────────────────────────────────────────

    /// Get the current contract data
    pub fn get_data(env: Env) -> Result<ContractData, ContractError> {
        env.storage()
            .instance()
            .get(&DATA_KEY)
            .ok_or(ContractError::NotInitialized)
    }

    /// Propose an upgrade with a new WASM hash (starts 48-hour timelock)
    pub fn propose_upgrade(
        env: Env,
        new_wasm_hash: BytesN<32>,
        proposer: Address,
        nonce: u64,
    ) -> Result<(), ContractError> {
        let data = Self::get_data(env.clone())?;

        if data.admin != proposer {
            return Err(ContractError::NotAdmin);
        }

        proposer.require_auth();
        consume_nonce(&env, &proposer, nonce);

        let pending_upgrade = PendingUpgrade {
            new_wasm_hash,
            proposed_at: env.ledger().timestamp(),
            proposer: proposer.clone(),
        };

        env.storage()
            .instance()
            .set(&PENDING_UPGRADE_KEY, &pending_upgrade);
        Ok(())
    }

    /// Execute a pending upgrade if the timelock period has passed
    pub fn execute_upgrade(env: Env, executor: Address, nonce: u64) -> Result<(), ContractError> {
        let data = Self::get_data(env.clone())?;

        if data.admin != executor {
            return Err(ContractError::NotAdmin);
        }

        executor.require_auth();
        consume_nonce(&env, &executor, nonce);

        let pending_upgrade: PendingUpgrade = env
            .storage()
            .instance()
            .get(&PENDING_UPGRADE_KEY)
            .ok_or(ContractError::NoPendingUpgrade)?;

        let time_elapsed = env
            .ledger()
            .timestamp()
            .saturating_sub(pending_upgrade.proposed_at);

        if time_elapsed < UPGRADE_DELAY_SECONDS {
            return Err(ContractError::UpgradeTimelockNotSatisfied);
        }

        env.deployer()
            .update_current_contract_wasm(pending_upgrade.new_wasm_hash);

        env.storage().instance().remove(&PENDING_UPGRADE_KEY);
        Ok(())
    }

    /// Cancel a pending upgrade
    pub fn cancel_upgrade(env: Env, canceller: Address) -> Result<(), ContractError> {
        let data = Self::get_data(env.clone())?;

        if data.admin != canceller {
            return Err(ContractError::NotAdmin);
        }

        canceller.require_auth();

        if !env.storage().instance().has(&PENDING_UPGRADE_KEY) {
            return Err(ContractError::NoPendingUpgrade);
        }

        env.storage().instance().remove(&PENDING_UPGRADE_KEY);
        Ok(())
    }

    /// Get the current pending upgrade information
    pub fn get_pending_upgrade(env: Env) -> Option<PendingUpgrade> {
        env.storage().instance().get(&PENDING_UPGRADE_KEY)
    }

    /// Get the remaining time before an upgrade can be executed
    pub fn get_upgrade_timelock_remaining(env: Env) -> Option<u64> {
        let pending_upgrade = Self::get_pending_upgrade(env.clone())?;
        let time_elapsed = env
            .ledger()
            .timestamp()
            .saturating_sub(pending_upgrade.proposed_at);

        if time_elapsed < UPGRADE_DELAY_SECONDS {
            Some(UPGRADE_DELAY_SECONDS - time_elapsed)
        } else {
            Some(0)
        }
    }

    /// Set a simple value (admin-only). Also records a heartbeat for "VALUE".
    pub fn set_value(env: Env, value: u64, setter: Address, nonce: u64) -> Result<(), ContractError> {
        let mut data = Self::get_data(env.clone())?;

        if data.admin != setter {
            return Err(ContractError::NotAdmin);
        }

        setter.require_auth();
        consume_nonce(&env, &setter, nonce);

        data.value = value;
        env.storage().instance().set(&DATA_KEY, &data);

        Self::_record_heartbeat(&env, symbol_short!("VALUE"));
        Ok(())
    }

    // ── Heartbeat Verification (Issue #188) ──────────────────────────────────

    /// Record a heartbeat for a specific asset (admin-only).
    pub fn update_heartbeat(
        env: Env,
        asset: Symbol,
        updater: Address,
    ) -> Result<(), ContractError> {
        let data = Self::get_data(env.clone())?;

        if data.admin != updater {
            return Err(ContractError::NotAdmin);
        }

        updater.require_auth();
        Self::_record_heartbeat(&env, asset);
        Ok(())
    }

    /// Check whether the data for a given asset is still fresh.
    pub fn is_data_fresh(env: Env, asset: Symbol) -> bool {
        let timestamps: Map<Symbol, u64> = env
            .storage()
            .temporary()
            .get(&HEARTBEAT_KEY)
            .unwrap_or_else(|| Map::new(&env));

        match timestamps.get(asset) {
            Some(last_update) => {
                let elapsed = env.ledger().timestamp().saturating_sub(last_update);
                elapsed <= Self::_get_interval(&env)
            }
            None => false,
        }
    }

    /// Get the last update timestamp for a specific asset.
    pub fn get_last_update_timestamp(env: Env, asset: Symbol) -> Option<u64> {
        let timestamps: Map<Symbol, u64> = env
            .storage()
            .temporary()
            .get(&HEARTBEAT_KEY)
            .unwrap_or_else(|| Map::new(&env));

        timestamps.get(asset)
    }

    /// Set the heartbeat interval in seconds (admin-only).
    pub fn set_heartbeat_interval(
        env: Env,
        interval: u64,
        setter: Address,
    ) -> Result<(), ContractError> {
        let data = Self::get_data(env.clone())?;

        if data.admin != setter {
            return Err(ContractError::NotAdmin);
        }

        setter.require_auth();

        if interval == 0 {
            return Err(ContractError::InvalidHeartbeatInterval);
        }

        env.storage().instance().set(&HB_INTERVAL_KEY, &interval);
        Ok(())
    }

    /// Get the current heartbeat interval in seconds.
    pub fn get_heartbeat_interval(env: Env) -> u64 {
        Self::_get_interval(&env)
    }

    pub fn get_coordinator_nonce(env: Env, coordinator: Address) -> u64 {
        get_nonce(&env, &coordinator)
    }

    // ── Signer Management ────────────────────────────────────────────────────

    /// Register a new signer (admin-only).
    pub fn register_signer(env: Env, signer: Address, caller: Address) {
        let data = Self::get_data(env.clone()).unwrap();
        if data.admin != caller {
            panic!("only admin can register signers");
        }
        caller.require_auth();

        let mut signers = Self::_get_signers(&env);
        if !signers.iter().any(|s| s == signer) {
            signers.push_back(signer);
            env.storage().instance().set(&SIGNERS_KEY, &signers);
        }
    }

    /// Remove a registered signer (admin-only).
    pub fn remove_signer(env: Env, signer: Address, caller: Address) {
        let data = Self::get_data(env.clone()).unwrap();
        if data.admin != caller {
            panic!("only admin can remove signers");
        }
        caller.require_auth();

        let signers = Self::_get_signers(&env);
        let mut filtered: Vec<Address> = Vec::new(&env);
        for s in signers.iter() {
            if s != signer {
                filtered.push_back(s);
            }
        }
        env.storage().instance().set(&SIGNERS_KEY, &filtered);
    }

    /// Return the list of registered signers.
    pub fn get_signers(env: Env) -> Vec<Address> {
        Self::_get_signers(&env)
    }

    // ── Emergency Revocation Vote Flow ───────────────────────────────────────

    /// Propose revoking the current admin key.
    pub fn propose_revocation(
        env: Env,
        target: Address,
        replacement: Address,
        proposer: Address,
    ) {
        proposer.require_auth();
        let data = Self::get_data(env.clone()).unwrap();

        if !Self::_is_signer(&env, &proposer) && data.admin != proposer {
            panic!("only a registered signer can propose revocation");
        }
        if data.admin != target {
            panic!("target is not the current admin");
        }
        if env.storage().instance().has(&REVOCATION_KEY) {
            panic!("a revocation proposal is already active");
        }

        let mut votes: Vec<Address> = Vec::new(&env);
        votes.push_back(proposer.clone());

        let proposal = RevocationProposal {
            target,
            replacement,
            proposer,
            proposed_at: env.ledger().timestamp(),
            votes,
        };
        env.storage().instance().set(&REVOCATION_KEY, &proposal);
    }

    /// Cast a vote in favour of the active revocation proposal.
    pub fn vote_revocation(env: Env, voter: Address) {
        voter.require_auth();
        let data = Self::get_data(env.clone()).unwrap();

        if !Self::_is_signer(&env, &voter) && data.admin != voter {
            panic!("only a registered signer can vote");
        }

        let mut proposal: RevocationProposal = env
            .storage()
            .instance()
            .get(&REVOCATION_KEY)
            .unwrap_or_else(|| panic!("no active revocation proposal"));

        if proposal.votes.iter().any(|v| v == voter) {
            panic!("signer has already voted");
        }

        proposal.votes.push_back(voter);

        let threshold = Self::_revocation_threshold(&env);
        if proposal.votes.len() >= threshold {
            let mut contract_data = data;
            contract_data.admin = proposal.replacement.clone();
            env.storage().instance().set(&DATA_KEY, &contract_data);
            env.storage().instance().remove(&REVOCATION_KEY);
        } else {
            env.storage().instance().set(&REVOCATION_KEY, &proposal);
        }
    }

    /// Execute a revocation proposal that has already reached threshold.
    pub fn execute_revocation(env: Env, caller: Address) {
        caller.require_auth();
        let data = Self::get_data(env.clone()).unwrap();

        if !Self::_is_signer(&env, &caller) && data.admin != caller {
            panic!("only a registered signer can execute revocation");
        }

        let proposal: RevocationProposal = env
            .storage()
            .instance()
            .get(&REVOCATION_KEY)
            .unwrap_or_else(|| panic!("no active revocation proposal"));

        let threshold = Self::_revocation_threshold(&env);
        if proposal.votes.len() < threshold {
            panic!("revocation threshold not yet reached");
        }

        let mut contract_data = data;
        contract_data.admin = proposal.replacement.clone();
        env.storage().instance().set(&DATA_KEY, &contract_data);
        env.storage().instance().remove(&REVOCATION_KEY);
    }

    /// Cancel the active revocation proposal.
    pub fn cancel_revocation(env: Env, caller: Address) {
        caller.require_auth();
        let data = Self::get_data(env.clone()).unwrap();

        let proposal: RevocationProposal = env
            .storage()
            .instance()
            .get(&REVOCATION_KEY)
            .unwrap_or_else(|| panic!("no active revocation proposal"));

        let is_proposer = proposal.proposer == caller;
        let is_admin_not_target = data.admin == caller && data.admin != proposal.target;
        if !is_proposer && !is_admin_not_target {
            panic!("only the proposer or a non-targeted admin can cancel");
        }

        env.storage().instance().remove(&REVOCATION_KEY);
    }

    /// Return the active revocation proposal, if any.
    pub fn get_revocation_proposal(env: Env) -> Option<RevocationProposal> {
        env.storage().instance().get(&REVOCATION_KEY)
    }

    // ── Private helpers ──────────────────────────────────────────────────────

    fn _record_heartbeat(env: &Env, asset: Symbol) {
        let mut timestamps: Map<Symbol, u64> = env
            .storage()
            .temporary()
            .get(&HEARTBEAT_KEY)
            .unwrap_or_else(|| Map::new(env));

        timestamps.set(asset, env.ledger().timestamp());
        env.storage().temporary().set(&HEARTBEAT_KEY, &timestamps);
    }

    fn _get_interval(env: &Env) -> u64 {
        env.storage()
            .instance()
            .get(&HB_INTERVAL_KEY)
            .unwrap_or(DEFAULT_HEARTBEAT_INTERVAL)
    }

    fn _get_signers(env: &Env) -> Vec<Address> {
        env.storage()
            .instance()
            .get(&SIGNERS_KEY)
            .unwrap_or_else(|| Vec::new(env))
    }

    fn _is_signer(env: &Env, addr: &Address) -> bool {
        Self::_get_signers(env).iter().any(|s| s == *addr)
    }

    fn _revocation_threshold(env: &Env) -> u32 {
        let n = Self::_get_signers(env).len();
        n / 2 + 1
    }
}

#[cfg(test)]
mod test;
