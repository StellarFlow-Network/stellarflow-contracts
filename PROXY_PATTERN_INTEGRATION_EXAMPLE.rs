// Example: Integrating Storage Contract with Logic Contract
// This demonstrates how the price-oracle logic contract would use the storage contract

#![no_std]

use soroban_sdk::{
    contract, contractimpl, Address, Env, Symbol, Vec, panic_with_error,
};

// Import the storage contract client
use price_oracle_storage::{
    PriceOracleStorageClient, PriceData, AssetMeta, Error as StorageError,
};

#[contract]
pub struct PriceOracleLogic;

/// Example: Update price using storage contract
#[contractimpl]
impl PriceOracleLogic {
    /// Update a verified price (admin only)
    pub fn update_price(
        env: Env,
        storage_address: Address,
        asset: Symbol,
        price: i128,
        decimals: u32,
    ) -> Result<(), String> {
        // Create storage client
        let storage_client = PriceOracleStorageClient::new(&env, &storage_address);

        // Verify caller is admin
        let admin = storage_client
            .get_admin()
            .map_err(|_| "Failed to get admin".to_string())?;

        let caller = env.invoker();
        if caller != admin {
            return Err("Unauthorized: caller is not admin".to_string());
        }

        // Create price data
        let price_data = PriceData {
            price,
            timestamp: env.ledger().timestamp(),
            provider: caller,
            decimals,
        };

        // Store verified price
        storage_client.set_verified_price(&asset, &price_data);

        Ok(())
    }

    /// Get current price for an asset
    pub fn get_price(
        env: Env,
        storage_address: Address,
        asset: Symbol,
    ) -> Result<i128, String> {
        let storage_client = PriceOracleStorageClient::new(&env, &storage_address);

        storage_client
            .get_verified_price(&asset)
            .map(|price_data| price_data.price)
            .map_err(|_| "Price not found".to_string())
    }

    /// Add a new asset to track
    pub fn add_asset(
        env: Env,
        storage_address: Address,
        asset: Symbol,
    ) -> Result<(), String> {
        let storage_client = PriceOracleStorageClient::new(&env, &storage_address);

        // Verify caller is admin
        let admin = storage_client
            .get_admin()
            .map_err(|_| "Failed to get admin".to_string())?;

        let caller = env.invoker();
        if caller != admin {
            return Err("Unauthorized: caller is not admin".to_string());
        }

        // Add asset to storage
        storage_client
            .add_asset(&asset)
            .map_err(|e| match e {
                StorageError::AlreadyExists => "Asset already exists".to_string(),
                _ => "Failed to add asset".to_string(),
            })?;

        Ok(())
    }

    /// Get all tracked assets
    pub fn get_all_assets(
        env: Env,
        storage_address: Address,
    ) -> Vec<Symbol> {
        let storage_client = PriceOracleStorageClient::new(&env, &storage_address);
        storage_client.get_all_assets()
    }

    /// Subscribe a contract to price updates
    pub fn subscribe(
        env: Env,
        storage_address: Address,
        callback_contract: Address,
    ) -> Result<(), String> {
        let storage_client = PriceOracleStorageClient::new(&env, &storage_address);

        storage_client
            .subscribe(&callback_contract)
            .map_err(|e| match e {
                StorageError::AlreadyExists => "Already subscribed".to_string(),
                _ => "Failed to subscribe".to_string(),
            })?;

        Ok(())
    }

    /// Unsubscribe a contract from price updates
    pub fn unsubscribe(
        env: Env,
        storage_address: Address,
        callback_contract: Address,
    ) -> Result<(), String> {
        let storage_client = PriceOracleStorageClient::new(&env, &storage_address);

        storage_client
            .unsubscribe(&callback_contract)
            .map_err(|e| match e {
                StorageError::NotFound => "Not subscribed".to_string(),
                _ => "Failed to unsubscribe".to_string(),
            })?;

        Ok(())
    }

    /// Get all subscribed contracts
    pub fn get_subscribers(
        env: Env,
        storage_address: Address,
    ) -> Vec<Address> {
        let storage_client = PriceOracleStorageClient::new(&env, &storage_address);
        storage_client.get_subscribers()
    }

    /// Notify all subscribers of a price update
    pub fn notify_subscribers(
        env: Env,
        storage_address: Address,
        asset: Symbol,
        price: i128,
    ) -> Result<(), String> {
        let storage_client = PriceOracleStorageClient::new(&env, &storage_address);

        // Get all subscribers
        let subscribers = storage_client.get_subscribers();

        // Notify each subscriber (non-blocking)
        for subscriber in subscribers.iter() {
            // In a real implementation, this would invoke the callback
            // env.invoke_contract(&subscriber, &Symbol::new(&env, "on_price_update"), &args);
            // For now, just log the notification
            env.events().publish(
                ("price_update_notification",),
                (asset.clone(), price, subscriber.clone()),
            );
        }

        Ok(())
    }

    /// Initialize the storage contract
    pub fn initialize_storage(
        env: Env,
        storage_address: Address,
        admin: Address,
    ) -> Result<(), String> {
        let storage_client = PriceOracleStorageClient::new(&env, &storage_address);

        storage_client
            .initialize(&admin)
            .map_err(|e| match e {
                StorageError::AlreadyExists => "Already initialized".to_string(),
                _ => "Failed to initialize".to_string(),
            })?;

        Ok(())
    }

    /// Check if storage is initialized
    pub fn is_storage_initialized(
        env: Env,
        storage_address: Address,
    ) -> bool {
        let storage_client = PriceOracleStorageClient::new(&env, &storage_address);
        storage_client.is_initialized()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Example Usage Patterns
// ─────────────────────────────────────────────────────────────────────────────

/*
// Pattern 1: Update Price
let storage_address = Address::from_contract_id(&env, &storage_contract_id);
PriceOracleLogic::update_price(
    env.clone(),
    storage_address,
    Symbol::new(&env, "NGN"),
    100_000_000,  // price
    9,            // decimals
)?;

// Pattern 2: Get Price
let price = PriceOracleLogic::get_price(
    env.clone(),
    storage_address,
    Symbol::new(&env, "NGN"),
)?;

// Pattern 3: Add Asset
PriceOracleLogic::add_asset(
    env.clone(),
    storage_address,
    Symbol::new(&env, "GHS"),
)?;

// Pattern 4: Subscribe to Updates
PriceOracleLogic::subscribe(
    env.clone(),
    storage_address,
    callback_contract_address,
)?;

// Pattern 5: Notify Subscribers
PriceOracleLogic::notify_subscribers(
    env.clone(),
    storage_address,
    Symbol::new(&env, "NGN"),
    100_000_000,
)?;
*/

// ─────────────────────────────────────────────────────────────────────────────
// Key Points
// ─────────────────────────────────────────────────────────────────────────────

/*
1. Storage Contract Separation:
   - All storage operations go through the storage contract client
   - Logic contract focuses on business logic
   - Storage contract is immutable and auditable

2. Cross-Contract Communication:
   - Uses PriceOracleStorageClient generated by #[contractclient]
   - Type-safe interface for storage operations
   - Automatic error handling

3. Error Handling:
   - Storage operations return Result<T, StorageError>
   - Map storage errors to logic contract errors
   - Provide meaningful error messages

4. Authorization:
   - Admin checks are performed in logic contract
   - Storage contract validates data
   - Clear separation of concerns

5. Gas Optimization:
   - Batch operations where possible
   - Minimize cross-contract calls
   - Use appropriate storage types (instance, persistent, temporary)

6. Testing:
   - Unit test storage contract independently
   - Integration test logic contract with storage
   - End-to-end tests for full workflows
*/
