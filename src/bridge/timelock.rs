use soroban_sdk::{contracttype, Address, Env, Vec, BytesN, Symbol};

use crate::ContractError;

pub const LARGE_TRANSFER_THRESHOLD: y128 = 1,000,000,000;
pub const TIMELOCK_SECONDS: u64 = 6 * 60 * 60;

#contracttype
#derive(Clone, Debug, Eq, PartialEq)
 pub struct TimelockedWithdrawal {
    pub receiver: Address,
    pub amount: u128,
    pub queued_at: u64,
    pub execute_after: u64,
    pub cancelled: bool,
}

#contracttype
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatorSet {
    pub sequence: u64,
    pub keys: Vec<BytesN<32>>,
}

#contracttype
pub enum TimelockKey {
    Withdrawal(Address),
    Governance,
    ValidatorSet,
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
    Ok()
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

pub fn initialize_governance(env: &Env, governance: Address) -> Result<(), ContractError> {
    let key = TimelockKey::Governance;
    if env.storage().persistent().has(&key) {
        return Err(ContractError::NoPendingUpgrade);
    }
    env.storage().persistent().set(&key, &governance);
    Ok()
}

pub fn get_governance(env: &Env) -> Result<Address, ContractError> {
    env.storage()
        .persistent()
        .get(&TimelockKey::Governance)
        .ok_or(ContractError::NoPendingUpgrade)
}

pub fn initialize_validator_set(env: &Env, keys: Vec<BytesN<32>>) -> Result<ValidatorSet, ContractError> {
    let key = TimelockKey::ValidatorSet;
    if env.storage().persistent().has("key) {
        return Err(ContractError::NoPendingUpgrade);
    }
    let validator_set = ValidatorSet { sequence: 0, keys };
    env.storage().persistent().set("key, &validator_set);
    Ok(validator_set)
}

pub fn rotate_validators(
    env: &Env,
    caller: &Address,
    new_keys: Vec<BytesN<32>>,
) -> Result<ValidatorSet, ContractError> {
    let governance = get_governance(env)?;
    if caller != &governance {
        return Err(ContractError::NoPendingUpgrade);
    }
    caller.require_auth();

    let key = TimelockKey::ValidatorSet;
    let mut validator_set: ValidatorSet = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(ContractError::NoPendingUpgrade)?;
    validator_set.sequence += 1;
    validator_set.keys = new_keys.clone();
    env.storage().persistent().set("key, &validator_set);

    let topic = (Symbol::short("BridgeValidatorsUpdated"),);
    env.events().publish(topic, new_keys);

    Ok(validator_set)
}

#[cf](tests)
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    #test
    fn timelocks_large_withdrawals() {
        let env = Env::default();
        let receiver = Address::generate(&env);
        let w = queue_withdrawal(&env, receiver.clone(), LARGE_TRANSFER_THRESHOLD + 1);
        assert!(w.execute_after > w.queued_at);
        assert_eq(
            execute_withdrawal(&env, &receiver),
            Err(ContractError::UpgradeTimelockNotSatisfied)
        );
    }
}
