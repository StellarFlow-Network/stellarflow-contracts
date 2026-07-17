use soroban_sdk::{contractimpl, Address, Env, Symbol};

use crate::types::DataKey;

#[soroban_sdk::contractevent]
pub struct RewardClaimedEvent {
    pub relayer: Address,
    pub amount: i128,
    pub timestamp: u64,
}

/// Internal helpers for reward accounting. Designed for O(1) updates per relayer.
pub struct Rewards;

impl Rewards {
    /// Add `amount` to `relayer` balance. O(1) storage write.
    pub fn add_to_balance(env: &Env, relayer: &Address, amount: i128) {
        let storage = env.storage().persistent();
        let mut map: soroban_sdk::Map<Address, i128> = storage
            .get(&DataKey::Rewards)
            .unwrap_or_else(|| soroban_sdk::Map::new(env));

        let prev = map.get(relayer).unwrap_or(0_i128);
        // Safe add - in production consider saturating arithmetic or checks
        let new_bal = prev.saturating_add(amount);
        map.set(relayer.clone(), new_bal);
        storage.set(&DataKey::Rewards, &map);
    }

    /// Read current claimable balance for a relayer.
    pub fn get_balance(env: &Env, relayer: &Address) -> i128 {
        let storage = env.storage().persistent();
        let map: soroban_sdk::Map<Address, i128> = storage
            .get(&DataKey::Rewards)
            .unwrap_or_else(|| soroban_sdk::Map::new(env));
        map.get(relayer).unwrap_or(0_i128)
    }
}

#[contractimpl]
impl Rewards {
    /// Claim rewards for the caller `relayer` by pulling accumulated balance.
    /// Implements Checks-Effects-Interactions: zeroes storage balance before external transfer.
    ///
    /// `token_contract` should implement a `transfer(from: Address, to: Address, amount: i128)` entrypoint
    /// following the Soroban token interface. For tests we use a dummy token contract.
    pub fn claim_rewards(env: Env, relayer: Address, token_contract: Address) -> i128 {
        relayer.require_auth();

        // CHECK: read current balance
        let storage = env.storage().persistent();
        let mut map: soroban_sdk::Map<Address, i128> = storage
            .get(&DataKey::Rewards)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));

        let balance = map.get(relayer.clone()).unwrap_or(0_i128);
        if balance == 0_i128 {
            return 0_i128;
        }

        // EFFECT: zero out on-chain balance BEFORE interaction
        map.set(relayer.clone(), 0_i128);
        storage.set(&DataKey::Rewards, &map);

        // INTERACTION: perform token transfer from contract -> relayer
        // We use the standard token client so any token-compatible contract can be used.
        // Acquire reentrancy lock before cross-contract call
        crate::reentrancy::acquire_lock(&env);
        
        let token = soroban_sdk::token::Client::new(&env, &token_contract);
        token.transfer(&env.current_contract_address(), &relayer, &balance);
        
        // Release reentrancy lock after cross-contract call
        crate::reentrancy::release_lock(&env);

        // Emit event for observability
        env.events().publish((Symbol::new(&env, "RewardClaimed"),), RewardClaimedEvent {
            relayer: relayer.clone(),
            amount: balance,
            timestamp: env.ledger().timestamp(),
        });

        balance
    }
}
