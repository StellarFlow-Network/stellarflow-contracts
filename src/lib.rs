use soroban_sdk::{contract, contractimpl, contracttype, Address, Env, Bytes, BytesN, Symbol, Vec, Map};

// Contract state keys
const DATA_KEY: Symbol = Symbol::short("DATA");
const PENDING_UPGRADE_KEY: Symbol = Symbol::short("PENDING");
const PRICE_DATA_KEY: Symbol = Symbol::short("PRICES");
const UPGRADE_DELAY_SECONDS: u64 = 48 * 60 * 60; // 48 hours in seconds

#[contracttype]
pub struct PendingUpgrade {
    pub new_wasm_hash: BytesN<32>,
    pub proposed_at: u64,
    pub proposer: Address,
}

#[contracttype]
#[derive(Clone)]
pub struct ContractData {
    pub admin: Address,
    pub value: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetPrice {
    pub asset_code: Symbol,
    pub price: u64,
    pub timestamp: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceUpdate {
    pub asset_code: Symbol,
    pub price: u64,
}

#[contract]
pub struct TimeLockedUpgradeContract;

#[contractimpl]
impl TimeLockedUpgradeContract {
    /// Initialize the contract with an admin address
    pub fn initialize(env: Env, admin: Address) {
        if env.storage().instance().has(&DATA_KEY) {
            panic!("contract already initialized");
        }
        
        admin.require_auth();
        
        let data = ContractData {
            admin: admin.clone(),
            value: 0,
        };
        
        env.storage().instance().set(&DATA_KEY, &data);
    }

    /// Get the current contract data
    pub fn get_data(env: Env) -> ContractData {
        env.storage()
            .instance()
            .get(&DATA_KEY)
            .unwrap_or_else(|| panic!("contract not initialized"))
    }

    /// Propose an upgrade with a new WASM hash
    /// This starts the 48-hour timelock period
    pub fn propose_upgrade(env: Env, new_wasm_hash: BytesN<32>, proposer: Address) {
        let data = Self::get_data(env.clone());
        
        // Only admin can propose upgrades
        if data.admin != proposer {
            panic!("only admin can propose upgrades");
        }
        
        proposer.require_auth();
        
        let current_time = env.ledger().timestamp();
        
        let pending_upgrade = PendingUpgrade {
            new_wasm_hash,
            proposed_at: current_time,
            proposer: proposer.clone(),
        };
        
        env.storage().instance().set(&PENDING_UPGRADE_KEY, &pending_upgrade);
    }

    /// Execute a pending upgrade if the timelock period has passed
    pub fn execute_upgrade(env: Env, executor: Address) {
        let data = Self::get_data(env.clone());
        
        // Only admin can execute upgrades
        if data.admin != executor {
            panic!("only admin can execute upgrades");
        }
        
        executor.require_auth();
        
        let pending_upgrade: PendingUpgrade = env
            .storage()
            .instance()
            .get(&PENDING_UPGRADE_KEY)
            .unwrap_or_else(|| panic!("no pending upgrade"));
        
        let current_time = env.ledger().timestamp();
        let time_elapsed = current_time.saturating_sub(pending_upgrade.proposed_at);
        
        // Check if 48 hours have passed
        if time_elapsed < UPGRADE_DELAY_SECONDS {
            panic!(
                "upgrade timelock not satisfied: {} seconds remaining",
                UPGRADE_DELAY_SECONDS - time_elapsed
            );
        }
        
        // Execute the upgrade
        env.deployer()
            .update_current_contract(pending_upgrade.new_wasm_hash);
        
        // Clear the pending upgrade
        env.storage().instance().remove(&PENDING_UPGRADE_KEY);
    }

    /// Cancel a pending upgrade
    pub fn cancel_upgrade(env: Env, canceller: Address) {
        let data = Self::get_data(env.clone());
        
        // Only admin can cancel upgrades
        if data.admin != canceller {
            panic!("only admin can cancel upgrades");
        }
        
        canceller.require_auth();
        
        if !env.storage().instance().has(&PENDING_UPGRADE_KEY) {
            panic!("no pending upgrade to cancel");
        }
        
        env.storage().instance().remove(&PENDING_UPGRADE_KEY);
    }

    /// Get the current pending upgrade information
    pub fn get_pending_upgrade(env: Env) -> Option<PendingUpgrade> {
        env.storage().instance().get(&PENDING_UPGRADE_KEY)
    }

    /// Get the remaining time before an upgrade can be executed
    pub fn get_upgrade_timelock_remaining(env: Env) -> Option<u64> {
        if let Some(pending_upgrade) = Self::get_pending_upgrade(env.clone()) {
            let current_time = env.ledger().timestamp();
            let time_elapsed = current_time.saturating_sub(pending_upgrade.proposed_at);
            
            if time_elapsed < UPGRADE_DELAY_SECONDS {
                Some(UPGRADE_DELAY_SECONDS - time_elapsed)
            } else {
                Some(0) // Timelock satisfied
            }
        } else {
            None
        }
    }

    /// Set a simple value for testing purposes
    pub fn set_value(env: Env, value: u64, setter: Address) {
        let mut data = Self::get_data(env.clone());
        
        // Only admin can set values
        if data.admin != setter {
            panic!("only admin can set values");
        }
        
        setter.require_auth();
        
        data.value = value;
        env.storage().instance().set(&DATA_KEY, &data);
    }

    /// Update prices for multiple assets in a single transaction
    /// This allows relayers to update 5+ assets efficiently, saving on submission fees
    pub fn update_prices_batch(env: Env, price_updates: Vec<PriceUpdate>, relayer: Address) {
        let data = Self::get_data(env.clone());
        
        // Only admin can update prices
        if data.admin != relayer {
            panic!("only admin can update prices");
        }
        
        relayer.require_auth();
        
        // Validate that we have at least 5 assets for batch efficiency
        if price_updates.len() < 5 {
            panic!("batch update requires at least 5 assets for efficiency");
        }
        
        // Get current price data or create new map
        let mut price_map: Map<Symbol, AssetPrice> = env.storage()
            .instance()
            .get(&PRICE_DATA_KEY)
            .unwrap_or_else(|| Map::new(&env));
        
        let current_time = env.ledger().timestamp();
        
        // Update each price in the batch
        for price_update in price_updates.iter() {
            let asset_price = AssetPrice {
                asset_code: price_update.asset_code.clone(),
                price: price_update.price,
                timestamp: current_time,
            };
            
            price_map.set(price_update.asset_code.clone(), asset_price);
        }
        
        // Store the updated price map
        env.storage().instance().set(&PRICE_DATA_KEY, &price_map);
    }

    /// Get the price for a specific asset
    pub fn get_price(env: Env, asset_code: Symbol) -> Option<AssetPrice> {
        let price_map: Map<Symbol, AssetPrice> = env.storage()
            .instance()
            .get(&PRICE_DATA_KEY)
            .unwrap_or_else(|| Map::new(&env));
        
        price_map.get(asset_code)
    }

    /// Get all current prices
    pub fn get_all_prices(env: Env) -> Map<Symbol, AssetPrice> {
        env.storage()
            .instance()
            .get(&PRICE_DATA_KEY)
            .unwrap_or_else(|| Map::new(&env))
    }
}

#[cfg(test)]
mod test;
