#![no_std]

mod math;

use soroban_sdk::{
    contract, contractclient, contracterror, contractimpl, contracttype, Address, Env,
};

use crate::math::compute_smoothed_value;

/// Compact 4-byte asset identifier replacing verbose Symbol keys for storage.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub struct AssetId(pub u32);

const ALPHA_SCALE: i128 = 10_000;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum RebalanceError {
    NotProfitable = 1,
}

#[contractclient(name = "YieldStrategyClient")]
pub trait YieldStrategy {
    fn rebalance(env: Env);
}

#[derive(Clone)]
#[contracttype]
pub enum DataKey {
    EmaRecord(AssetId), // Maps an asset id to its EMA
    Alpha,              // The smoothing factor
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct EmaRecord {
    pub value: i128,
    pub last_updated: u64,
}

#[contract]
pub struct AnalyticsEngine;

#[contractimpl]
impl AnalyticsEngine {
    pub fn initialize(env: Env, alpha: i128) {
        if env.storage().instance().has(&DataKey::Alpha) {
            return Err(ContractError::AlreadyInitialized);
        }
        if alpha <= 0 || alpha > ALPHA_SCALE {
            return Err(ContractError::InvalidAlpha);
        }
        env.storage().instance().set(&DataKey::Alpha, &alpha);
    }

    /// Implement an optimized calculation method that updates a single, rolling smoothing metric upon every new price submission.
    /// Store only the finalized moving average record in persistent data slots to minimize long-term storage rent fees.
    pub fn submit_price(env: Env, asset: AssetId, price: i128) {
        if price <= 0 {
            return Err(ContractError::PriceMustBePositive);
        }

        let alpha: i128 = env
            .storage()
            .instance()
            .get(&DataKey::Alpha)
            .unwrap_or_else(|| return Err(ContractError::NotInitialized));
        let key = DataKey::EmaRecord(asset);

        let new_ema = if let Some(record) = env.storage().persistent().get::<_, EmaRecord>(&key) {
            compute_smoothed_value(price, record.value, alpha)
        } else {
            // First price submission becomes the initial EMA
            price
        };

        let new_record = EmaRecord {
            value: new_ema,
            last_updated: env.ledger().timestamp(),
        };

        // Store only the finalized moving average record in persistent data slots
        env.storage().persistent().set(&key, &new_record);
    }

    pub fn get_ema(env: Env, asset: AssetId) -> i128 {
        let key = DataKey::EmaRecord(asset);
        if let Some(record) = env.storage().persistent().get::<_, EmaRecord>(&key) {
            record.value
        } else {
            0
        }
    }

    pub fn is_rebalance_profitable(
        _env: Env,
        expected_yield_delta: i128,
        gas_rebalance: i128,
        safety_multiplier: i128,
    ) -> bool {
        if expected_yield_delta < 0 || gas_rebalance < 0 || safety_multiplier < 0 {
            return false;
        }

        match gas_rebalance.checked_mul(safety_multiplier) {
            Some(threshold) => expected_yield_delta > threshold,
            None => false,
        }
    }

    pub fn rebalance(
        env: Env,
        strategy: Address,
        expected_yield_delta: i128,
        gas_rebalance: i128,
        safety_multiplier: i128,
    ) -> Result<(), RebalanceError> {
        if !Self::is_rebalance_profitable(
            env.clone(),
            expected_yield_delta,
            gas_rebalance,
            safety_multiplier,
        ) {
            return Err(RebalanceError::NotProfitable);
        }

        YieldStrategyClient::new(&env, &strategy).rebalance();

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{contract, contractimpl, contracttype, Env};

    #[contracttype]
    enum MockKey {
        RebalanceCount,
    }

    #[contract]
    struct MockYieldStrategy;

    #[contractimpl]
    impl MockYieldStrategy {
        pub fn rebalance(env: Env) {
            let count: u32 = env
                .storage()
                .instance()
                .get(&MockKey::RebalanceCount)
                .unwrap_or(0);
            env.storage()
                .instance()
                .set(&MockKey::RebalanceCount, &(count + 1));
        }

        pub fn rebalance_count(env: Env) -> u32 {
            env.storage()
                .instance()
                .get(&MockKey::RebalanceCount)
                .unwrap_or(0)
        }
    }

    #[test]
    fn rebalance_requires_yield_delta_above_gas_threshold() {
        let env = Env::default();

        assert!(AnalyticsEngine::is_rebalance_profitable(
            env.clone(),
            101,
            10,
            10
        ));
        assert!(!AnalyticsEngine::is_rebalance_profitable(
            env.clone(),
            100,
            10,
            10
        ));
        assert!(!AnalyticsEngine::is_rebalance_profitable(env, 99, 10, 10));
    }

    #[test]
    fn manual_rebalance_rejects_unprofitable_attempt() {
        let env = Env::default();
        let strategy = env.register_contract(None, MockYieldStrategy);
        let strategy_client = MockYieldStrategyClient::new(&env, &strategy);
        let engine = env.register_contract(None, AnalyticsEngine);
        let engine_client = AnalyticsEngineClient::new(&env, &engine);

        assert_eq!(
            engine_client.try_rebalance(&strategy, &100, &10, &10),
            Err(Ok(RebalanceError::NotProfitable))
        );
        assert_eq!(strategy_client.rebalance_count(), 0);
    }

    #[test]
    fn profitable_rebalance_calls_strategy() {
        let env = Env::default();
        let strategy = env.register_contract(None, MockYieldStrategy);
        let strategy_client = MockYieldStrategyClient::new(&env, &strategy);
        let engine = env.register_contract(None, AnalyticsEngine);
        let engine_client = AnalyticsEngineClient::new(&env, &engine);

        engine_client.rebalance(&strategy, &101, &10, &10);

        assert_eq!(strategy_client.rebalance_count(), 1);
    }

    #[test]
    fn rebalance_rejects_threshold_overflow() {
        let env = Env::default();

        assert!(!AnalyticsEngine::is_rebalance_profitable(
            env,
            i128::MAX,
            i128::MAX,
            2
        ));
    }
}