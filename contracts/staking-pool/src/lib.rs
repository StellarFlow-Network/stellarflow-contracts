#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, Address, Env,
};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    Unauthorized = 2,
    InvalidParameters = 3,
    EmissionCapReached = 4,
}

#[derive(Clone)]
#[contracttype]
pub enum DataKey {
    Admin,
    StartLedger,
    InitialRate,
    EpochLength,
    DecayRateBps,
    MaxEmissionCap,
    TotalEmitted,
    LastUpdateLedger,
    CurrentRate,
}

#[contract]
pub struct StakingPoolYieldEmission;

#[contractimpl]
impl StakingPoolYieldEmission {
    pub fn initialize(
        env: Env,
        admin: Address,
        start_ledger: u32,
        initial_rate: u128,
        epoch_length: u32,
        decay_rate_bps: u32,
        max_emission_cap: u128,
    ) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }

        if decay_rate_bps > 10000 || epoch_length == 0 {
            panic_with_error!(&env, Error::InvalidParameters);
        }

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::StartLedger, &start_ledger);
        env.storage().instance().set(&DataKey::InitialRate, &initial_rate);
        env.storage().instance().set(&DataKey::EpochLength, &epoch_length);
        env.storage().instance().set(&DataKey::DecayRateBps, &decay_rate_bps);
        env.storage().instance().set(&DataKey::MaxEmissionCap, &max_emission_cap);
        
        env.storage().instance().set(&DataKey::TotalEmitted, &0u128);
        env.storage().instance().set(&DataKey::LastUpdateLedger, &start_ledger);
        env.storage().instance().set(&DataKey::CurrentRate, &initial_rate);
    }

    pub fn get_current_epoch(env: Env) -> u32 {
        let current_ledger = env.ledger().sequence();
        let start_ledger: u32 = env.storage().instance().get(&DataKey::StartLedger).unwrap_or(0);
        
        if current_ledger < start_ledger {
            return 0;
        }

        let epoch_length: u32 = env.storage().instance().get(&DataKey::EpochLength).unwrap();
        (current_ledger - start_ledger) / epoch_length
    }

    pub fn get_reward_rate(env: Env) -> u128 {
        let epoch = Self::get_current_epoch(env.clone());
        let initial_rate: u128 = env.storage().instance().get(&DataKey::InitialRate).unwrap();
        let decay_rate_bps: u32 = env.storage().instance().get(&DataKey::DecayRateBps).unwrap();
        
        let mut rate = initial_rate;
        for _ in 0..epoch {
            rate = rate.saturating_mul((10000 - decay_rate_bps) as u128) / 10000;
        }
        rate
    }

    pub fn update_emissions(env: Env) -> u128 {
        let current_ledger = env.ledger().sequence();
        let last_update_ledger: u32 = env.storage().instance().get(&DataKey::LastUpdateLedger).unwrap_or(current_ledger);
        
        if current_ledger <= last_update_ledger {
            return env.storage().instance().get(&DataKey::TotalEmitted).unwrap_or(0);
        }

        let start_ledger: u32 = env.storage().instance().get(&DataKey::StartLedger).unwrap();
        
        if current_ledger < start_ledger {
            return 0;
        }
        
        let effective_last = if last_update_ledger < start_ledger { start_ledger } else { last_update_ledger };
        let epoch_length: u32 = env.storage().instance().get(&DataKey::EpochLength).unwrap();
        let initial_rate: u128 = env.storage().instance().get(&DataKey::InitialRate).unwrap();
        let decay_rate_bps: u32 = env.storage().instance().get(&DataKey::DecayRateBps).unwrap();
        let max_emission_cap: u128 = env.storage().instance().get(&DataKey::MaxEmissionCap).unwrap();
        let mut total_emitted: u128 = env.storage().instance().get(&DataKey::TotalEmitted).unwrap_or(0);

        let mut temp_ledger = effective_last;

        while temp_ledger < current_ledger && total_emitted < max_emission_cap {
            let current_epoch = (temp_ledger - start_ledger) / epoch_length;
            let next_epoch_ledger = start_ledger + (current_epoch + 1) * epoch_length;
            
            let end_ledger = if current_ledger < next_epoch_ledger { current_ledger } else { next_epoch_ledger };
            let ledgers_in_span = (end_ledger - temp_ledger) as u128;
            
            let mut rate = initial_rate;
            for _ in 0..current_epoch {
                rate = rate.saturating_mul((10000 - decay_rate_bps) as u128) / 10000;
            }

            let mut emission_for_span = rate.saturating_mul(ledgers_in_span);
            if total_emitted.saturating_add(emission_for_span) > max_emission_cap {
                emission_for_span = max_emission_cap - total_emitted;
            }

            total_emitted = total_emitted.saturating_add(emission_for_span);
            temp_ledger = end_ledger;
        }

        env.storage().instance().set(&DataKey::TotalEmitted, &total_emitted);
        env.storage().instance().set(&DataKey::LastUpdateLedger, &current_ledger);
        
        // Update current rate at the end
        let current_epoch = Self::get_current_epoch(env.clone());
        let mut final_rate = initial_rate;
        for _ in 0..current_epoch {
            final_rate = final_rate.saturating_mul((10000 - decay_rate_bps) as u128) / 10000;
        }
        env.storage().instance().set(&DataKey::CurrentRate, &final_rate);

        total_emitted
    }

    pub fn get_total_emitted(env: Env) -> u128 {
        env.storage().instance().get(&DataKey::TotalEmitted).unwrap_or(0)
    }
}
