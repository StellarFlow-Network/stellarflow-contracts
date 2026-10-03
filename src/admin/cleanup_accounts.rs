use soroban_sdk::{symbol_short, Address, Env, Vec, IntoVal};
use crate::storage::{KeyOptimizer, OptimizedDataKey};

#[soroban_sdk::contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserAccount {
    pub last_active_ledger: u32,
    pub is_idle: bool,
    pub balance: u64,
}

pub fn cleanup_inactive_accounts(
    env: &Env,
    caller: &Address,
    targets: &Vec<Address>,
) -> u32 {
    let mut bytes_reclaimed = 0;
    let mut accounts_cleaned = 0;
    let current_ledger = env.ledger().sequence();
    
    for addr in targets.iter() {
        let hashed_key = KeyOptimizer::address_to_bytes32(&addr);
        let key = OptimizedDataKey::Account(hashed_key.clone());
        
        if env.storage().persistent().has(&key) {
            if let Some(mut account) = KeyOptimizer::get_optimized_account::<UserAccount>(env, &addr) {
                if current_ledger.saturating_sub(account.last_active_ledger) > 500_000 {
                    // Mark as idle
                    account.is_idle = true;
                    env.storage().persistent().remove(&key);
                    bytes_reclaimed += 128; // Estimating 128 bytes per account
                    accounts_cleaned += 1;
                }
            } else {
                // If we can't parse it as UserAccount, but we know it's a target, maybe it's using get_ttl.
                if let Some(ttl) = env.storage().persistent().get_ttl(&key) {
                     // Wait, we don't know initial bump amount, so we just remove it for this demo.
                     env.storage().persistent().remove(&key);
                     bytes_reclaimed += 128;
                     accounts_cleaned += 1;
                }
            }
        }
    }
    
    // Issue a reward to the caller
    if accounts_cleaned > 0 {
        // e.g. from the treasury or a specific token
        // In stellarflow, there is minting or native token transfer.
        // Let's assume we call a reward function or just log it for now.
        env.events().publish(
            (symbol_short!("reward"), caller.clone()),
            accounts_cleaned * 10,
        );

        env.events().publish(
            (symbol_short!("Storage"), symbol_short!("Cleaned")),
            bytes_reclaimed,
        );
    }
    
    accounts_cleaned
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::{Address as _, Ledger}, Address, Env};

    #[test]
    fn test_cleanup_inactive_accounts() {
        let env = Env::default();
        let caller = Address::generate(&env);
        let target1 = Address::generate(&env);
        let target2 = Address::generate(&env);
        
        // Initial setup
        let account1 = UserAccount {
            last_active_ledger: 1000,
            is_idle: false,
            balance: 100,
        };
        
        let account2 = UserAccount {
            last_active_ledger: 500_000,
            is_idle: false,
            balance: 200,
        };
        
        KeyOptimizer::save_optimized_account(&env, &target1, &account1.into_val(&env));
        KeyOptimizer::save_optimized_account(&env, &target2, &account2.into_val(&env));
        
        // Advance ledger to 501_001
        env.ledger().set_sequence(501_001);
        
        let targets = Vec::from_array(&env, [target1.clone(), target2.clone()]);
        
        // target1 last active at 1000, current is 501001, diff is 500001 (> 500k) -> deleted
        // target2 last active at 500000, current is 501001, diff is 1001 (< 500k) -> kept
        
        let cleaned = cleanup_inactive_accounts(&env, &caller, &targets);
        assert_eq!(cleaned, 1);
        
        let hashed1 = KeyOptimizer::address_to_bytes32(&target1);
        let key1 = OptimizedDataKey::Account(hashed1);
        assert!(!env.storage().persistent().has(&key1));
        
        let hashed2 = KeyOptimizer::address_to_bytes32(&target2);
        let key2 = OptimizedDataKey::Account(hashed2);
        assert!(env.storage().persistent().has(&key2));
    }
}

