use soroban_sdk::{contracttype, Address, Env};

use crate::ContractError;

pub const LARGE_TRANSFER_THRESHOLD: u128 = 1_000_000_000;
pub const TIMELOCK_SECONDS: u64 = 6 * 60 * 60;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimelockedWithdrawal {
    pub receiver: Address,
    pub amount: u128,
    pub queued_at: u64,
    pub execute_after: u64,
    pub cancelled: bool,
}

#[contracttype]
pub enum TimelockKey {
    Withdrawal(Address),
}

pub fn queue_withdrawal(env: &Env, receiver: Address, amount: u128) -> TimelockedWithdrawal {
    let queued_at = env.ledger().timestamp();
    let withdrawal = TimelockedWithdrawal {
        receiver: receiver.clone(),
        amount,
        queued_at,
        execute_after: if amount > LARGE_TRANSFER_THRESHOLD {
            queued_at + TIMELOCK_SECONDS
        } else {
            queued_at
        },
        cancelled: false,
    };
    env.storage()
        .persistent()
        .set(&TimelockKey::Withdrawal(receiver), &withdrawal);
    withdrawal
}

pub fn cancel_withdrawal(env: &Env, receiver: &Address) -> Result<(), ContractError> {
    let key = TimelockKey::Withdrawal(receiver.clone());
    let mut withdrawal: TimelockedWithdrawal = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(ContractError::NoPendingUpgrade)?;
    withdrawal.cancelled = true;
    env.storage().persistent().set(&key, &withdrawal);
    Ok(())
}

pub fn execute_withdrawal(env: &Env, receiver: &Address) -> Result<TimelockedWithdrawal, ContractError> {
    let key = TimelockKey::Withdrawal(receiver.clone());
    let withdrawal: TimelockedWithdrawal = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(ContractError::NoPendingUpgrade)?;
    if withdrawal.cancelled {
        return Err(ContractError::NoPendingUpgrade);
    }
    if env.ledger().timestamp() < withdrawal.execute_after {
        return Err(ContractError::UpgradeTimelockNotSatisfied);
    }
    env.storage().persistent().remove(&key);
    Ok(withdrawal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    #[test]
    fn timelocks_large_withdrawals() {
        let env = Env::default();
        let cid = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let receiver = Address::generate(&env);
        let w = env.as_contract(&cid, || queue_withdrawal(&env, receiver.clone(), LARGE_TRANSFER_THRESHOLD + 1));
        assert!(w.execute_after > w.queued_at);
        assert_eq!(
            env.as_contract(&cid, || execute_withdrawal(&env, &receiver)),
            Err(ContractError::UpgradeTimelockNotSatisfied)
        );
    }
}
