use soroban_sdk::{Address, BytesN, Env, Symbol, Vec, Map};
use crate::{TimeLockedUpgradeContract, PendingUpgrade, UPGRADE_DELAY_SECONDS, AssetPrice, PriceUpdate};

pub struct TimeLockedUpgradeClient<'a> {
    env: &'a Env,
    contract_id: &'a Address,
}

impl<'a> TimeLockedUpgradeClient<'a> {
    pub fn new(env: &'a Env, contract_id: &'a Address) -> Self {
        Self { env, contract_id }
    }

    pub fn initialize(&self, admin: &Address) {
        self.env.invoke_contract(
            self.contract_id,
            &Symbol::short("initialize"),
            (admin,),
        );
    }

    pub fn get_data(&self) -> crate::ContractData {
        self.env.invoke_contract(
            self.contract_id,
            &Symbol::short("get_data"),
            (),
        )
    }

    pub fn propose_upgrade(&self, new_wasm_hash: &BytesN<32>, proposer: &Address) {
        self.env.invoke_contract(
            self.contract_id,
            &Symbol::short("propose_upgrade"),
            (new_wasm_hash, proposer),
        );
    }

    pub fn execute_upgrade(&self, executor: &Address) {
        self.env.invoke_contract(
            self.contract_id,
            &Symbol::short("execute_upgrade"),
            (executor,),
        );
    }

    pub fn cancel_upgrade(&self, canceller: &Address) {
        self.env.invoke_contract(
            self.contract_id,
            &Symbol::short("cancel_upgrade"),
            (canceller,),
        );
    }

    pub fn get_pending_upgrade(&self) -> Option<PendingUpgrade> {
        self.env.invoke_contract(
            self.contract_id,
            &Symbol::short("get_pending_upgrade"),
            (),
        )
    }

    pub fn get_upgrade_timelock_remaining(&self) -> Option<u64> {
        self.env.invoke_contract(
            self.contract_id,
            &Symbol::short("get_upgrade_timelock_remaining"),
            (),
        )
    }

    pub fn set_value(&self, value: u64, setter: &Address) {
        self.env.invoke_contract(
            self.contract_id,
            &Symbol::short("set_value"),
            (value, setter),
        );
    }

    pub fn update_prices_batch(&self, price_updates: &Vec<PriceUpdate>, relayer: &Address) {
        self.env.invoke_contract(
            self.contract_id,
            &Symbol::short("update_prices_batch"),
            (price_updates, relayer),
        );
    }

    pub fn get_price(&self, asset_code: &Symbol) -> Option<AssetPrice> {
        self.env.invoke_contract(
            self.contract_id,
            &Symbol::short("get_price"),
            (asset_code,),
        )
    }

    pub fn get_all_prices(&self) -> Map<Symbol, AssetPrice> {
        self.env.invoke_contract(
            self.contract_id,
            &Symbol::short("get_all_prices"),
            (),
        )
    }
}

#[test]
fn test_initialize_and_basic_functionality() {
    let env = Env::default();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeClient::new(&env, &contract_id);
    
    let admin = Address::generate(&env);
    
    // Test initialization
    client.initialize(&admin);
    
    // Test getting data
    let data = client.get_data();
    assert_eq!(data.admin, admin);
    assert_eq!(data.value, 0);
    
    // Test setting value
    client.set_value(&42, &admin);
    let data = client.get_data();
    assert_eq!(data.value, 42);
}

#[test]
fn test_propose_upgrade() {
    let env = Env::default();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeClient::new(&env, &contract_id);
    
    let admin = Address::generate(&env);
    client.initialize(&admin);
    
    // Create a fake WASM hash for testing
    let new_wasm_hash = BytesN::from_array(&env, &[1u8; 32]);
    
    // Propose upgrade
    client.propose_upgrade(&new_wasm_hash, &admin);
    
    // Check pending upgrade
    let pending = client.get_pending_upgrade();
    assert!(pending.is_some());
    
    let pending_upgrade = pending.unwrap();
    assert_eq!(pending_upgrade.new_wasm_hash, new_wasm_hash);
    assert_eq!(pending_upgrade.proposer, admin);
    
    // Check timelock remaining
    let remaining = client.get_upgrade_timelock_remaining();
    assert!(remaining.is_some());
    assert_eq!(remaining.unwrap(), UPGRADE_DELAY_SECONDS);
}

#[test]
fn test_execute_upgrade_after_timelock() {
    let env = Env::default();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeClient::new(&env, &contract_id);
    
    let admin = Address::generate(&env);
    client.initialize(&admin);
    
    let new_wasm_hash = BytesN::from_array(&env, &[1u8; 32]);
    
    // Propose upgrade
    client.propose_upgrade(&new_wasm_hash, &admin);
    
    // Try to execute immediately - should fail
    let result = env.try_invoke_contract::<(), (
        soroban_sdk::xdr::ScVal,
        soroban_sdk::xdr::ScVal,
    )>(
        &contract_id,
        &Symbol::short("execute_upgrade"),
        (&admin,).into_val(&env),
    );
    assert!(result.is_err());
    
    // Fast forward time by 48 hours
    env.ledger().set_timestamp(env.ledger().timestamp() + UPGRADE_DELAY_SECONDS);
    
    // Now execution should work (though we can't actually test the upgrade in unit tests)
    let remaining = client.get_upgrade_timelock_remaining();
    assert_eq!(remaining.unwrap(), 0);
}

#[test]
fn test_cancel_upgrade() {
    let env = Env::default();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeClient::new(&env, &contract_id);
    
    let admin = Address::generate(&env);
    client.initialize(&admin);
    
    let new_wasm_hash = BytesN::from_array(&env, &[1u8; 32]);
    
    // Propose upgrade
    client.propose_upgrade(&new_wasm_hash, &admin);
    
    // Verify pending upgrade exists
    assert!(client.get_pending_upgrade().is_some());
    
    // Cancel upgrade
    client.cancel_upgrade(&admin);
    
    // Verify pending upgrade is gone
    assert!(client.get_pending_upgrade().is_none());
    assert!(client.get_upgrade_timelock_remaining().is_none());
}

#[test]
fn test_unauthorized_operations() {
    let env = Env::default();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeClient::new(&env, &contract_id);
    
    let admin = Address::generate(&env);
    let unauthorized_user = Address::generate(&env);
    
    client.initialize(&admin);
    
    let new_wasm_hash = BytesN::from_array(&env, &[1u8; 32]);
    
    // Try to propose upgrade as unauthorized user - should fail
    let result = env.try_invoke_contract::<(), (
        soroban_sdk::xdr::ScVal,
        soroban_sdk::xdr::ScVal,
    )>(
        &contract_id,
        &Symbol::short("propose_upgrade"),
        (&new_wasm_hash, &unauthorized_user).into_val(&env),
    );
    assert!(result.is_err());
    
    // Try to set value as unauthorized user - should fail
    let result = env.try_invoke_contract::<(), (
        soroban_sdk::xdr::ScVal,
        soroban_sdk::xdr::ScVal,
    )>(
        &contract_id,
        &Symbol::short("set_value"),
        (&42, &unauthorized_user).into_val(&env),
    );
    assert!(result.is_err());
}

#[test]
fn test_timelock_countdown() {
    let env = Env::default();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeClient::new(&env, &contract_id);
    
    let admin = Address::generate(&env);
    client.initialize(&admin);
    
    let new_wasm_hash = BytesN::from_array(&env, &[1u8; 32]);
    
    // Propose upgrade
    client.propose_upgrade(&new_wasm_hash, &admin);
    
    // Check initial timelock
    let remaining = client.get_upgrade_timelock_remaining().unwrap();
    assert_eq!(remaining, UPGRADE_DELAY_SECONDS);
    
    // Fast forward by 24 hours
    env.ledger().set_timestamp(env.ledger().timestamp() + (24 * 60 * 60));
    
    // Check remaining time
    let remaining = client.get_upgrade_timelock_remaining().unwrap();
    assert_eq!(remaining, 24 * 60 * 60);
    
    // Fast forward by another 24 hours
    env.ledger().set_timestamp(env.ledger().timestamp() + (24 * 60 * 60));
    
    // Timelock should be satisfied
    let remaining = client.get_upgrade_timelock_remaining().unwrap();
    assert_eq!(remaining, 0);
}

#[test]
fn test_batch_price_update_success() {
    let env = Env::default();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeClient::new(&env, &contract_id);
    
    let admin = Address::generate(&env);
    client.initialize(&admin);
    
    // Create price updates for 5+ assets
    let mut price_updates = Vec::new(&env);
    
    // Add 5 different assets with their prices
    price_updates.push_back(PriceUpdate {
        asset_code: Symbol::short("BTC"),
        price: 50000,
    });
    price_updates.push_back(PriceUpdate {
        asset_code: Symbol::short("ETH"),
        price: 3000,
    });
    price_updates.push_back(PriceUpdate {
        asset_code: Symbol::short("USDC"),
        price: 1000,
    });
    price_updates.push_back(PriceUpdate {
        asset_code: Symbol::short("USDT"),
        price: 1000,
    });
    price_updates.push_back(PriceUpdate {
        asset_code: Symbol::short("XLM"),
        price: 100,
    });
    
    // Update prices in batch
    client.update_prices_batch(&price_updates, &admin);
    
    // Verify all prices were updated
    let btc_price = client.get_price(&Symbol::short("BTC"));
    assert!(btc_price.is_some());
    assert_eq!(btc_price.unwrap().price, 50000);
    
    let eth_price = client.get_price(&Symbol::short("ETH"));
    assert!(eth_price.is_some());
    assert_eq!(eth_price.unwrap().price, 3000);
    
    let all_prices = client.get_all_prices();
    assert_eq!(all_prices.len(), 5);
}

#[test]
fn test_batch_price_update_minimum_assets() {
    let env = Env::default();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeClient::new(&env, &contract_id);
    
    let admin = Address::generate(&env);
    client.initialize(&admin);
    
    // Try with only 4 assets (should fail)
    let mut price_updates = Vec::new(&env);
    price_updates.push_back(PriceUpdate {
        asset_code: Symbol::short("BTC"),
        price: 50000,
    });
    price_updates.push_back(PriceUpdate {
        asset_code: Symbol::short("ETH"),
        price: 3000,
    });
    price_updates.push_back(PriceUpdate {
        asset_code: Symbol::short("USDC"),
        price: 1000,
    });
    price_updates.push_back(PriceUpdate {
        asset_code: Symbol::short("USDT"),
        price: 1000,
    });
    
    // Should fail with less than 5 assets
    let result = env.try_invoke_contract::<(), (
        soroban_sdk::xdr::ScVal,
        soroban_sdk::xdr::ScVal,
    )>(
        &contract_id,
        &Symbol::short("update_prices_batch"),
        (&price_updates, &admin).into_val(&env),
    );
    assert!(result.is_err());
}

#[test]
fn test_batch_price_update_unauthorized() {
    let env = Env::default();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeClient::new(&env, &contract_id);
    
    let admin = Address::generate(&env);
    let unauthorized_user = Address::generate(&env);
    client.initialize(&admin);
    
    // Create price updates
    let mut price_updates = Vec::new(&env);
    for i in 0..5 {
        price_updates.push_back(PriceUpdate {
            asset_code: Symbol::short(&format!("Asset{}", i)),
            price: 1000 + i,
        });
    }
    
    // Try to update as unauthorized user (should fail)
    let result = env.try_invoke_contract::<(), (
        soroban_sdk::xdr::ScVal,
        soroban_sdk::xdr::ScVal,
    )>(
        &contract_id,
        &Symbol::short("update_prices_batch"),
        (&price_updates, &unauthorized_user).into_val(&env),
    );
    assert!(result.is_err());
}

#[test]
fn test_batch_price_update_timestamps() {
    let env = Env::default();
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeClient::new(&env, &contract_id);
    
    let admin = Address::generate(&env);
    client.initialize(&admin);
    
    // Set initial timestamp
    let initial_time = 1000000;
    env.ledger().set_timestamp(initial_time);
    
    // Create price updates
    let mut price_updates = Vec::new(&env);
    for i in 0..5 {
        price_updates.push_back(PriceUpdate {
            asset_code: Symbol::short(&format!("Asset{}", i)),
            price: 1000 + i,
        });
    }
    
    // Update prices
    client.update_prices_batch(&price_updates, &admin);
    
    // Check that timestamps are set correctly
    let btc_price = client.get_price(&Symbol::short("Asset0"));
    assert!(btc_price.is_some());
    assert_eq!(btc_price.unwrap().timestamp, initial_time);
    
    // Advance time and update again
    let new_time = initial_time + 3600; // 1 hour later
    env.ledger().set_timestamp(new_time);
    
    // Update with new prices
    let mut new_price_updates = Vec::new(&env);
    new_price_updates.push_back(PriceUpdate {
        asset_code: Symbol::short("Asset0"),
        price: 2000,
    });
    new_price_updates.push_back(PriceUpdate {
        asset_code: Symbol::short("Asset1"),
        price: 3000,
    });
    new_price_updates.push_back(PriceUpdate {
        asset_code: Symbol::short("Asset2"),
        price: 4000,
    });
    new_price_updates.push_back(PriceUpdate {
        asset_code: Symbol::short("Asset3"),
        price: 5000,
    });
    new_price_updates.push_back(PriceUpdate {
        asset_code: Symbol::short("Asset4"),
        price: 6000,
    });
    
    client.update_prices_batch(&new_price_updates, &admin);
    
    // Verify timestamps were updated
    let updated_price = client.get_price(&Symbol::short("Asset0"));
    assert!(updated_price.is_some());
    assert_eq!(updated_price.unwrap().timestamp, new_time);
    assert_eq!(updated_price.unwrap().price, 2000);
}
