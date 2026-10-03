#![no_std]

use soroban_sdk::{contract, contracterror, contractimpl, contracttype, Address, Env};

pub const MAX_LOCK_DURATION: u64 = 4 * 365 * 24 * 60 * 60;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ContractError {
    InvalidLockDuration = 1,
    InvalidAmount = 2,
    LockNotFound = 3,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct VeLock {
    pub user: Address,
    pub amount: i128,
    pub lock_duration: u64,
    pub yield_rewards: i128,
}

#[contract]
pub struct VoteEscrowExtension;

#[contractimpl]
impl VoteEscrowExtension {
    pub fn calculate_vote_weight(amount: i128, lock_duration: u64) -> Result<i128, ContractError> {
        if lock_duration > MAX_LOCK_DURATION {
            return Err(ContractError::InvalidLockDuration);
        }
        if amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }
        let weight = amount * (lock_duration as i128) / (MAX_LOCK_DURATION as i128);
        Ok(weight)
    }

    pub fn extend_lock(
        env: Env,
        user: Address,
        additional_duration: u64,
    ) -> Result<VeLock, ContractError> {
        user.require_auth();
        let key = (
            soroban_sdk::symbol_short!("VeLock"),
            user.clone(),
        );
        let mut lock: VeLock = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::LockNotFound)?;

        let new_duration = lock.lock_duration + additional_duration;
        if new_duration > MAX_LOCK_DURATION {
            return Err(ContractError::InvalidLockDuration);
        }

        lock.lock_duration = new_duration;
        env.storage().persistent().set(&key, &lock);
        Ok(lock)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::Env;

    #[test]
    fn test_calculate_vote_weight() {
        let amount = 1000i128;
        let max_duration = MAX_LOCK_DURATION;
        let weight = VoteEscrowExtension::calculate_vote_weight(amount, max_duration).unwrap();
        assert_eq!(weight, 1000);

        let half_duration = max_duration / 2;
        let weight_half = VoteEscrowExtension::calculate_vote_weight(amount, half_duration).unwrap();
        assert_eq!(weight_half, 500);

        let invalid = VoteEscrowExtension::calculate_vote_weight(amount, max_duration + 1);
        assert_eq!(invalid, Err(ContractError::InvalidLockDuration));
    }
}
