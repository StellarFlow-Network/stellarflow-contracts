use soroban_sdk::{contracttype, BytesN, Env};

use crate::ContractError;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NullifierKey {
    Used(BytesN<32>),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NullifierRecord {
    pub used_at: u64,
}

pub fn register_nullifier(env: &Env, nullifier: BytesN<32>) -> Result<(), ContractError> {
    let key = NullifierKey::Used(nullifier.clone());
    if env.storage().persistent().has(&key) {
        return Err(ContractError::NullifierAlreadyUsed);
    }

    env.storage().persistent().set(
        &key,
        &NullifierRecord {
            used_at: env.ledger().timestamp(),
        },
    );
    env.storage().persistent().extend_ttl(&key, 5_000, 100_000);
    Ok(())
}

pub fn is_nullifier_used(env: &Env, nullifier: &BytesN<32>) -> bool {
    env.storage().persistent().has(&NullifierKey::Used(nullifier.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::BytesN as _;

    #[test]
    fn rejects_duplicates() {
        let env = Env::default();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let nullifier = BytesN::from_array(&env, &[7u8; 32]);
        env.as_contract(&contract_id, || {
            assert!(register_nullifier(&env, nullifier.clone()).is_ok());
            assert_eq!(
                register_nullifier(&env, nullifier.clone()),
                Err(ContractError::NullifierAlreadyUsed)
            );
            assert!(is_nullifier_used(&env, &nullifier));
        });
    }
}
