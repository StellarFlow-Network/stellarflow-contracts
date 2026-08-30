#![no_std]

use soroban_sdk::{contract, contractimpl, contracttype, contracterror, token, Address, Env, Symbol, symbol_short};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum VestingError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    NotAdmin = 3,
    VestingNotFound = 4,
    CliffNotReached = 5,
    NothingToClaim = 6,
    InvalidAmount = 7,
    InvalidDuration = 8,
    VestingAlreadyExists = 9,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct VestingSchedule {
    pub beneficiary: Address,
    pub total_amount: i128,
    pub claimed_amount: i128,
    pub start_ledger: u32,
    pub cliff_duration: u32,
    pub vesting_duration: u32,
}

#[contracttype]
pub enum DataKey {
    Admin,
    Token,
    Schedule(Symbol),
}

#[contract]
pub struct LinearVestingContract;

#[contractimpl]
impl LinearVestingContract {
    /// Initialize the vesting contract with admin and token address.
    pub fn initialize(env: Env, admin: Address, token: Address) -> Result<(), VestingError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(VestingError::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Token, &token);
        Ok(())
    }

    /// Create a new vesting schedule for a beneficiary.
    ///
    /// # Parameters
    /// - `identifier`: Unique symbol for this vesting schedule
    /// - `beneficiary`: Address to receive vested tokens
    /// - `total_amount`: Total amount of tokens to vest
    /// - `cliff_duration`: Number of ledgers before any tokens can be claimed
    /// - `vesting_duration`: Total number of ledgers for linear vesting (must be > cliff_duration)
    pub fn create_vesting(
        env: Env,
        admin: Address,
        identifier: Symbol,
        beneficiary: Address,
        total_amount: i128,
        cliff_duration: u32,
        vesting_duration: u32,
    ) -> Result<(), VestingError> {
        let stored_admin: Address = env.storage().instance().get(&DataKey::Admin).ok_or(VestingError::NotInitialized)?;
        if admin != stored_admin {
            return Err(VestingError::NotAdmin);
        }
        admin.require_auth();

        if total_amount <= 0 {
            return Err(VestingError::InvalidAmount);
        }
        if vesting_duration <= cliff_duration {
            return Err(VestingError::InvalidDuration);
        }

        let schedule_key = DataKey::Schedule(identifier.clone());
        if env.storage().instance().has(&schedule_key) {
            return Err(VestingError::VestingAlreadyExists);
        }

        let current_ledger = env.ledger().sequence();
        let schedule = VestingSchedule {
            beneficiary: beneficiary.clone(),
            total_amount,
            claimed_amount: 0,
            start_ledger: current_ledger,
            cliff_duration,
            vesting_duration,
        };

        // Transfer tokens from admin to this contract
        let token_addr: Address = env.storage().instance().get(&DataKey::Token).ok_or(VestingError::NotInitialized)?;
        let token_client = token::Client::new(&env, &token_addr);
        token_client.transfer(&admin, &env.current_contract_address(), &total_amount);

        env.storage().instance().set(&schedule_key, &schedule);

        // Emit event
        env.events().publish(
            (symbol_short!("vest_new"),),
            (identifier, beneficiary, total_amount, cliff_duration, vesting_duration),
        );

        Ok(())
    }

    /// Calculate the amount of tokens currently claimable by a vesting schedule.
    ///
    /// Returns 0 if cliff has not been reached. Otherwise returns the linearly
    /// vested amount minus already claimed tokens.
    pub fn get_claimable(env: Env, identifier: Symbol) -> i128 {
        let schedule_key = DataKey::Schedule(identifier);
        let schedule: VestingSchedule = match env.storage().instance().get(&schedule_key) {
            Some(s) => s,
            None => return 0,
        };

        let current_ledger = env.ledger().sequence();
        let elapsed = current_ledger.saturating_sub(schedule.start_ledger);

        // Before cliff: nothing claimable
        if elapsed < schedule.cliff_duration {
            return 0;
        }

        // Calculate linearly vested amount
        let vested_amount = if elapsed >= schedule.vesting_duration {
            schedule.total_amount
        } else {
            let vesting_range = schedule.vesting_duration - schedule.cliff_duration;
            let post_cliff_elapsed = elapsed - schedule.cliff_duration;
            (schedule.total_amount * (post_cliff_elapsed as i128)) / (vesting_range as i128)
        };

        let claimable = vested_amount - schedule.claimed_amount;
        if claimable <= 0 {
            0
        } else {
            claimable
        }
    }

    /// Claim vested tokens for a given vesting schedule.
    ///
    /// Panics if cliff has not been reached or nothing is claimable.
    pub fn claim_vested(env: Env, identifier: Symbol) -> Result<i128, VestingError> {
        let beneficiary = env.invoker();
        beneficiary.require_auth();

        let schedule_key = DataKey::Schedule(identifier.clone());
        let mut schedule: VestingSchedule = env
            .storage()
            .instance()
            .get(&schedule_key)
            .ok_or(VestingError::VestingNotFound)?;

        // Verify caller is the beneficiary
        if beneficiary != schedule.beneficiary {
            return Err(VestingError::NotAdmin);
        }

        let current_ledger = env.ledger().sequence();
        let elapsed = current_ledger.saturating_sub(schedule.start_ledger);

        // Prevent claims before cliff
        if elapsed < schedule.cliff_duration {
            return Err(VestingError::CliffNotReached);
        }

        let claimable = Self::get_claimable(env.clone(), identifier.clone());
        if claimable <= 0 {
            return Err(VestingError::NothingToClaim);
        }

        schedule.claimed_amount += claimable;
        env.storage().instance().set(&schedule_key, &schedule);

        // Transfer tokens to beneficiary
        let token_addr: Address = env.storage().instance().get(&DataKey::Token).ok_or(VestingError::NotInitialized)?;
        let token_client = token::Client::new(&env, &token_addr);
        token_client.transfer(&env.current_contract_address(), &schedule.beneficiary, &claimable);

        // Emit event
        env.events().publish(
            (symbol_short!("vest_claim"),),
            (identifier, schedule.beneficiary, claimable),
        );

        Ok(claimable)
    }

    /// Get the vesting schedule details.
    pub fn get_schedule(env: Env, identifier: Symbol) -> Option<VestingSchedule> {
        let schedule_key = DataKey::Schedule(identifier);
        env.storage().instance().get(&schedule_key)
    }

    /// Get the admin address.
    pub fn get_admin(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Admin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger, LedgerInfo};
    use soroban_sdk::{symbol_short, Env};

    fn setup() -> (Env, LinearVestingContractClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();
        let id = env.register_contract(None, LinearVestingContract);
        let client = LinearVestingContractClient::new(&env, &id);
        (env, client)
    }

    fn advance_ledgers(env: &Env, count: u32) {
        let info = env.ledger().get();
        env.ledger().set(LedgerInfo {
            sequence: info.sequence + count,
            timestamp: info.timestamp,
            protocol_version: info.protocol_version,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 0,
            min_persistent_entry_ttl: 0,
            max_entry_ttl: u32::MAX,
        });
    }

    #[test]
    fn test_initialize() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let token = Address::generate(&env);

        client.initialize(&admin, &token);
        assert_eq!(client.get_admin(), Some(admin));
    }

    #[test]
    fn test_create_vesting_schedule() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let beneficiary = Address::generate(&env);
        let token = Address::generate(&env);

        client.initialize(&admin, &token);
        let id = symbol_short!("team1");

        client.create_vesting(
            &admin,
            &id,
            &beneficiary,
            &1000_0000000,
            &100,  // cliff: 100 ledgers
            &1000, // vesting: 1000 ledgers
        );

        let schedule = client.get_schedule(&id).unwrap();
        assert_eq!(schedule.beneficiary, beneficiary);
        assert_eq!(schedule.total_amount, 1000_0000000);
        assert_eq!(schedule.claimed_amount, 0);
        assert_eq!(schedule.cliff_duration, 100);
        assert_eq!(schedule.vesting_duration, 1000);
    }

    #[test]
    fn test_no_claimable_before_cliff() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let beneficiary = Address::generate(&env);
        let token = Address::generate(&env);

        client.initialize(&admin, &token);
        let id = symbol_short!("team1");

        client.create_vesting(
            &admin,
            &id,
            &beneficiary,
            &1000_0000000,
            &100,
            &1000,
        );

        // Advance 50 ledgers (still within cliff)
        advance_ledgers(&env, 50);
        assert_eq!(client.get_claimable(&id), 0);
    }

    #[test]
    fn test_claimable_after_cliff() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let beneficiary = Address::generate(&env);
        let token = Address::generate(&env);

        client.initialize(&admin, &token);
        let id = symbol_short!("team1");

        client.create_vesting(
            &admin,
            &id,
            &beneficiary,
            &1000_0000000,
            &100,
            &1000,
        );

        // Advance past cliff (100 ledgers) + halfway through vesting range (450 more = 550 total)
        // Post-cliff range = 900, elapsed post-cliff = 450
        advance_ledgers(&env, 550);
        let claimable = client.get_claimable(&id);
        // 1000_0000000 * 450 / 900 = 500_0000000
        assert_eq!(claimable, 500_0000000);
    }

    #[test]
    fn test_claim_vested_tokens() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let beneficiary = Address::generate(&env);
        let token = Address::generate(&env);

        client.initialize(&admin, &token);
        let id = symbol_short!("team1");

        client.create_vesting(
            &admin,
            &id,
            &beneficiary,
            &1000_0000000,
            &100,
            &1000,
        );

        // Advance fully past vesting
        advance_ledgers(&env, 1000);
        let claimed = client.claim_vested(&id);
        assert_eq!(claimed, 1000_0000000);

        let schedule = client.get_schedule(&id).unwrap();
        assert_eq!(schedule.claimed_amount, 1000_0000000);
    }

    #[test]
    fn test_cannot_claim_before_cliff() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let beneficiary = Address::generate(&env);
        let token = Address::generate(&env);

        client.initialize(&admin, &token);
        let id = symbol_short!("team1");

        client.create_vesting(
            &admin,
            &id,
            &beneficiary,
            &1000_0000000,
            &100,
            &1000,
        );

        advance_ledgers(&env, 50);
        let result = client.try_claim_vested(&id);
        assert_eq!(result, Err(Ok(VestingError::CliffNotReached)));
    }
}
