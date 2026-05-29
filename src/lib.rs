#![no_std]
#![no_std]
use soroban_sdk::{contract, contractimpl, contracttype, symbol_short, Address, Env, BytesN, Map, Symbol, Vec};

// Contract state keys
const DATA_KEY: Symbol = Symbol::short("DATA");
const PENDING_UPGRADE_KEY: Symbol = Symbol::short("PENDING");
const UPGRADE_DELAY_SECONDS: u64 = 48 * 60 * 60; // 48 hours in seconds
// Dedicated initialization flag — separate from DATA_KEY so the guard survives
// partial-write failures and is not sensitive to data structure changes.
const INIT_FLAG_KEY: Symbol = Symbol::short("INITD");

// ── Heartbeat keys (Issue #188) ──────────────────────────────────────────────
/// Per-asset last-update timestamps: Map<Symbol, u64>
const HEARTBEAT_KEY: Symbol = Symbol::short("HBEAT");
/// Configurable heartbeat interval in seconds (default: 5 minutes = 300s)
const HB_INTERVAL_KEY: Symbol = Symbol::short("HBINTV");
/// Default heartbeat interval: 5 minutes
const DEFAULT_HEARTBEAT_INTERVAL: u64 = 5 * 60;

// ── Emergency Key Revocation (Task #revocation) ──────────────────────────────
/// Registered signers list: Vec<Address>
const SIGNERS_KEY: Symbol = Symbol::short("SIGNERS");
/// Active revocation proposal
const REVOCATION_KEY: Symbol = Symbol::short("REVOKE");

/// An active revocation proposal.
#[contracttype]
#[derive(Clone)]
pub struct RevocationProposal {
    /// The compromised admin key to be stripped.
    pub target: Address,
    /// Replacement admin address (takes over after revocation).
    pub replacement: Address,
    /// Signer who opened the proposal.
    pub proposer: Address,
    /// Ledger timestamp when the proposal was created.
    pub proposed_at: u64,
    /// Addresses that have already voted in favour.
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
        ///
        /// Both the token transfer and staking registration succeed together or
        /// neither takes effect — preventing stuck intermediate states.
        pub fn stake_and_register(
            env: Env,
            node: Address,
            amount: u64,
        ) -> StakeRecord {
            // Validate inputs before any state mutation
            if amount == 0 {
                panic!("stake amount must be greater than zero");
            }
    
            node.require_auth();
    
            // Load existing stakes registry
            let mut stakes: Map<Address, u64> = env
                .storage()
                .instance()
                .get(&STAKE_REGISTRY_KEY)
                .unwrap_or_else(|| Map::new(&env));
    
            // Check for duplicate registration
            if stakes.contains_key(node.clone()) {
                panic!("node already registered");
            }
    
            // Update total staked
            let total: u64 = env
                .storage()
                .instance()
                .get(&TOTAL_STAKED_KEY)
                .unwrap_or(0u64);
    
            let new_total = total.checked_add(amount)
                .unwrap_or_else(|| panic!("stake amount overflow"));
    
            // Register the node stake
            stakes.set(node.clone(), amount);
    
            // Commit both writes atomically — if either panics, both roll back
            env.storage().instance().set(&STAKE_REGISTRY_KEY, &stakes);
            env.storage().instance().set(&TOTAL_STAKED_KEY, &new_total);
    
            // Record heartbeat for staking activity
            Self::_record_heartbeat(&env, symbol_short!("STAKE"));
    
            let record = StakeRecord {
                node: node.clone(),
                amount,
                registered_at: env.ledger().timestamp(),
            };
    
            record
        }
    
        /// Get the staked amount for a specific node.
        /// Returns 0 if the node is not registered.
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
    
            // Remove node and update total atomically
            stakes.remove(node.clone());
    
            env.storage().instance().set(&STAKE_REGISTRY_KEY, &stakes);
            env.storage().instance().set(&TOTAL_STAKED_KEY, &new_total);
    
            amount
        }

    /// Get the current contract data
    pub fn get_data(env: Env) -> Result<ContractData, ContractError> {
        env.storage()
            .instance()
            .get(&DATA_KEY)
            .ok_or(ContractError::NotInitialized)
    }

    /// Propose an upgrade with a new WASM hash
    /// This starts the 48-hour timelock period
    pub fn propose_upgrade(
        env: Env,
        new_wasm_hash: BytesN<32>,
        proposer: Address,
    ) -> Result<(), ContractError> {
        let data = Self::get_data(env.clone())?;
        
        // Only admin can propose upgrades
        if data.admin != proposer {
            return Err(ContractError::NotAdmin);
        }
        
        proposer.require_auth();
        consume_nonce(&env, &proposer, nonce);
        let current_time = env.ledger().timestamp();
        
        let pending_upgrade = PendingUpgrade {
            new_wasm_hash,
            proposed_at: current_time,
            proposer: proposer.clone(),
        };
        
        env.storage().instance().set(&PENDING_UPGRADE_KEY, &pending_upgrade);
        Ok(())
    }

    /// Execute a pending upgrade if the timelock period has passed
    pub fn execute_upgrade(env: Env, executor: Address) -> Result<(), ContractError> {
        let data = Self::get_data(env.clone())?;
        
        // Only admin can execute upgrades
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
        
        let current_time = env.ledger().timestamp();
        let time_elapsed = current_time.saturating_sub(pending_upgrade.proposed_at);
        
        // Check if 48 hours have passed
        if time_elapsed < UPGRADE_DELAY_SECONDS {
            return Err(ContractError::UpgradeTimelockNotSatisfied);
        }
        
        // Execute the upgrade
        env.deployer()
            .update_current_contract_wasm(pending_upgrade.new_wasm_hash);
        
        // Clear the pending upgrade
        env.storage().instance().remove(&PENDING_UPGRADE_KEY);
        Ok(())
    }

    /// Cancel a pending upgrade
    pub fn cancel_upgrade(env: Env, canceller: Address) -> Result<(), ContractError> {
        let data = Self::get_data(env.clone())?;
        
        // Only admin can cancel upgrades
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
        if let Some(pending_upgrade) = Self::get_pending_upgrade(env.clone()) {
            let current_time = env.ledger().timestamp();
            let time_elapsed = current_time.saturating_sub(pending_upgrade.proposed_at);
            
            if time_elapsed < UPGRADE_DELAY_SECONDS {
                Some(UPGRADE_DELAY_SECONDS - time_elapsed)
            } else {
                Some(0) // Timelock satisfied
            }
        } else {
            None
        }
    }

    /// Set a simple value for testing purposes.
    ///
    /// Also records a heartbeat for the implicit "VALUE" asset so that
    /// `is_data_fresh` can track when the last state mutation occurred.
    pub fn set_value(env: Env, value: u64, setter: Address) -> Result<(), ContractError> {
        let mut data = Self::get_data(env.clone())?;
        
        // Only admin can set values
        if data.admin != setter {
            return Err(ContractError::NotAdmin);
        }
        
        setter.require_auth();
        consume_nonce(&env, &setter, nonce);
        data.value = value;
        env.storage().instance().set(&DATA_KEY, &data);

        // Auto-record heartbeat for the default "VALUE" asset (Issue #188)
        Self::_record_heartbeat(&env, symbol_short!("VALUE"));
        Ok(())
    }

    // ── Heartbeat Verification (Issue #188) ──────────────────────────────────

    /// Record a heartbeat for a specific asset.
    ///
    /// Stores the current ledger timestamp as the `last_update_timestamp`
    /// for the given asset symbol. Only the admin can call this.
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
    ///
    /// Returns `true` if the time elapsed since the last heartbeat is
    /// within the configured heartbeat interval. Returns `false` if:
    ///   - The asset has never been updated (no heartbeat recorded).
    ///   - The heartbeat interval has been exceeded (data is stale).
    pub fn is_data_fresh(env: Env, asset: Symbol) -> bool {
        let timestamps: Map<Symbol, u64> = env
            .storage()
            .temporary()
            .get(&HEARTBEAT_KEY)
            .unwrap_or_else(|| Map::new(&env));

        match timestamps.get(asset) {
            Some(last_update) => {
                let current_time = env.ledger().timestamp();
                let interval = Self::_get_interval(&env);
                let elapsed = current_time.saturating_sub(last_update);
                elapsed <= interval
            }
            None => false, // Never updated → stale
        }
    }

    /// Get the last update timestamp for a specific asset.
    ///
    /// Returns `None` if no heartbeat has ever been recorded for this asset.
    pub fn get_last_update_timestamp(env: Env, asset: Symbol) -> Option<u64> {
        let timestamps: Map<Symbol, u64> = env
            .storage()
            .temporary()
            .get(&HEARTBEAT_KEY)
            .unwrap_or_else(|| Map::new(&env));

        timestamps.get(asset)
    }

    /// Set the heartbeat interval (in seconds). Admin-only.
    ///
    /// This configures how long the oracle data is considered fresh after
    /// a heartbeat. For example, `300` means data is fresh for 5 minutes.
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
    ///
    /// Returns the configured interval, or the default (300s / 5 min)
    /// if none has been explicitly set.
    pub fn get_heartbeat_interval(env: Env) -> u64 {
        Self::_get_interval(&env)
    }
    pub fn get_coordinator_nonce(env: Env, coordinator: Address) -> u64 {
        get_nonce(&env, &coordinator)
    }

    // ── Signer Management ────────────────────────────────────────────────────

    /// Register a new signer. Admin-only.
    ///
    /// Signers are the addresses eligible to participate in emergency
    /// revocation votes. The admin itself always counts as a signer but
    /// does not need to be explicitly registered.
    pub fn register_signer(env: Env, signer: Address, caller: Address) {
        let data = Self::get_data(env.clone());
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

    /// Remove a registered signer. Admin-only.
    pub fn remove_signer(env: Env, signer: Address, caller: Address) {
        let data = Self::get_data(env.clone());
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

    /// Return the list of registered signers (does not include the admin implicitly).
    pub fn get_signers(env: Env) -> Vec<Address> {
        Self::_get_signers(&env)
    }

    // ── Emergency Revocation Vote Flow ───────────────────────────────────────

    /// Propose revoking the current admin key.
    ///
    /// Any registered signer (or the admin itself) may open a proposal.
    /// `target` must be the current admin. `replacement` will become the
    /// new admin once the vote passes.
    pub fn propose_revocation(
        env: Env,
        target: Address,
        replacement: Address,
        proposer: Address,
    ) {
        proposer.require_auth();
        let data = Self::get_data(env.clone());

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
    ///
    /// When the vote count reaches the majority threshold the admin key is
    /// immediately replaced with `replacement`.
    pub fn vote_revocation(env: Env, voter: Address) {
        voter.require_auth();
        let data = Self::get_data(env.clone());

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
    ///
    /// `vote_revocation` auto-executes on the final vote; this function
    /// exists as an explicit on-chain confirmation path.
    pub fn execute_revocation(env: Env, caller: Address) {
        caller.require_auth();
        let data = Self::get_data(env.clone());

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
    ///
    /// Only the proposer or the current admin (when they are not the target)
    /// may cancel.
    pub fn cancel_revocation(env: Env, caller: Address) {
        caller.require_auth();
        let data = Self::get_data(env.clone());

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

    /// Internal: record the current ledger timestamp for an asset.
    fn _record_heartbeat(env: &Env, asset: Symbol) {
        let mut timestamps: Map<Symbol, u64> = env
            .storage()
            .temporary()
            .get(&HEARTBEAT_KEY)
            .unwrap_or_else(|| Map::new(env));

        timestamps.set(asset, env.ledger().timestamp());
        env.storage().temporary().set(&HEARTBEAT_KEY, &timestamps);
    }

    /// Internal: read the heartbeat interval from storage or return default.
    fn _get_interval(env: &Env) -> u64 {
        env.storage()
            .instance()
            .get(&HB_INTERVAL_KEY)
            .unwrap_or(DEFAULT_HEARTBEAT_INTERVAL)
    }

    /// Internal: return the registered signers list.
    fn _get_signers(env: &Env) -> Vec<Address> {
        env.storage()
            .instance()
            .get(&SIGNERS_KEY)
            .unwrap_or_else(|| Vec::new(env))
    }

    /// Internal: check whether `addr` is a registered signer.
    fn _is_signer(env: &Env, addr: &Address) -> bool {
        Self::_get_signers(env).iter().any(|s| s == *addr)
    }

    /// Internal: majority threshold over registered signers.
    ///
    /// Counts registered signers only (admin is not auto-included).
    /// Threshold = floor(n/2) + 1  (strict majority).
    fn _revocation_threshold(env: &Env) -> u32 {
        let n = Self::_get_signers(env).len();
        n / 2 + 1
    }
}

#[cfg(test)]
mod test;
