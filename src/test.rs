use soroban_sdk::{Address, BytesN, Env, Symbol};
use crate::{TimeLockedUpgradeContract, PendingUpgrade, UPGRADE_DELAY_SECONDS};

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
