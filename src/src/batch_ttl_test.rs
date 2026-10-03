#![cfg(test)]

use soroban_sdk::{Env, Symbol, Vec, contract, contractimpl};
use crate::storage::bump_persistent_batch;

#[contract]
struct TestBatchContract;

#[contractimpl]
impl TestBatchContract {}

#[test]
fn test_bump_persistent_batch_extends_ttl()
{
    let env = Env::default();
    let cid = env.register(TestBatchContract, ());
    env.as_contract(&cid, || {
        let mut keys = Vec::new(&env);
        let k1 = Symbol::new(&env, "Key1");
        let k2 = Symbol::new(&env, "Key2");
        keys.push_back(k1.clone());
        keys.push_back(k2.clone());

        env.storage().persistent().set(&k1, &42i32);
        env.storage().persistent().set(&k2, &100i32);

        // Simulate long ledger advancement
        env.ledger().with_mut(|li| {
            li.sequence_number += 50_000;
        });

        bump_persistent_batch(&env, &keys);

        assert_eq!(env.storage().persistent().get::<_, i32>(&k1), Some(42));
        assert_eq!(env.storage().persistent().get::<_, i32>(&k2), Some(100));
    });
}
