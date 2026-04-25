// Example: Lending Protocol with StellarFlow Callback Integration
// This is a complete example showing how to integrate with the callback interface

#![no_std]

use soroban_sdk::{
    contract, contractimpl, contracttype, panic_with_error, Address, Env, Symbol, Vec,
};

// Import the price update payload from the oracle
use price_oracle::types::PriceUpdatePayload;

// ─────────────────────────────────────────────────────────────────────────────
// Types
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
#[contracttype]
pub enum DataKey {
    /// Admin address
    Admin,
    /// Map of asset symbols to their current prices
    AssetPrices,
    /// Map of borrower addresses to their positions
    BorrowerPositions(Address),
    /// Last update timestamp for an asset
    LastPriceUpdate(Symbol),
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct BorrowPosition {
    pub borrower: Address,
    pub asset: Symbol,
    pub borrow_amount: i128,
    pub collateral_asset: Symbol,
    pub collateral_amount: i128,
    pub created_at: u64,
}

#[derive(Clone)]
#[contracttype]
pub enum Error {
    Unauthorized = 1,
    InvalidPrice = 2,
    InsufficientCollateral = 3,
    LiquidationTriggered = 4,
}

// ─────────────────────────────────────────────────────────────────────────────
// Oracle Client Interface
// ─────────────────────────────────────────────────────────────────────────────

#[soroban_sdk::contractclient(name = "PriceOracleClient")]
pub trait PriceOracleInterface {
    fn subscribe_to_price_updates(callback_contract: Address) -> Result<(), soroban_sdk::String>;
    fn get_price(asset: Symbol, verified: bool) -> Result<PriceUpdatePayload, soroban_sdk::String>;
}

// ─────────────────────────────────────────────────────────────────────────────
// Lending Protocol Contract
// ─────────────────────────────────────────────────────────────────────────────

#[contract]
pub struct LendingPool;

#[contractimpl]
impl LendingPool {
    /// Initialize the lending pool
    pub fn initialize(env: Env, admin: Address, oracle: Address) {
        admin.require_auth();

        env.storage().instance().set(&DataKey::Admin, &admin);

        // Subscribe to oracle price updates
        let oracle_client = PriceOracleClient::new(&env, &oracle);
        let my_address = env.current_contract_address();

        match oracle_client.subscribe_to_price_updates(&my_address) {
            Ok(_) => {
                env.events().publish(
                    (Symbol::new(&env, "oracle_subscribed"),),
                    (my_address, oracle),
                );
            }
            Err(e) => {
                panic_with_error!(&env, "Failed to subscribe to oracle");
            }
        }
    }

    /// Standard callback interface - called by oracle on price update
    pub fn on_price_update(env: Env, payload: PriceUpdatePayload) {
        // Verify caller is the expected oracle contract
        // In production, validate that caller is the authorized oracle
        let caller = env.invoker();

        // Store the latest price
        let mut prices: soroban_sdk::Map<Symbol, i128> = env
            .storage()
            .instance()
            .get(&DataKey::AssetPrices)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));

        let old_price = prices.get(payload.asset.clone()).unwrap_or(0);
        prices.set(payload.asset.clone(), payload.price);

        env.storage()
            .instance()
            .set(&DataKey::AssetPrices, &prices);

        // Update timestamp
        env.storage().instance().set(
            &DataKey::LastPriceUpdate(payload.asset.clone()),
            &payload.timestamp,
        );

        // Check for liquidation opportunities
        check_liquidations(&env, &payload.asset, payload.price, old_price);

        // Emit event for monitoring
        env.events().publish(
            (Symbol::new(&env, "price_updated"),),
            (
                payload.asset.clone(),
                payload.price,
                payload.timestamp,
                payload.confidence_score,
            ),
        );
    }

    /// Create a new borrow position with collateral
    pub fn create_position(
        env: Env,
        borrower: Address,
        borrow_asset: Symbol,
        borrow_amount: i128,
        collateral_asset: Symbol,
        collateral_amount: i128,
    ) -> Result<(), Error> {
        borrower.require_auth();

        // Get current prices
        let prices: soroban_sdk::Map<Symbol, i128> = env
            .storage()
            .instance()
            .get(&DataKey::AssetPrices)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));

        let borrow_price = prices
            .get(borrow_asset.clone())
            .ok_or(Error::InvalidPrice)?;
        let collateral_price = prices
            .get(collateral_asset.clone())
            .ok_or(Error::InvalidPrice)?;

        // Check collateralization: collateral_value >= borrow_value * 1.5
        let collateral_value = collateral_amount
            .checked_mul(collateral_price)
            .ok_or(Error::InvalidPrice)?;
        let required_collateral = borrow_amount
            .checked_mul(borrow_price)
            .ok_or(Error::InvalidPrice)?
            .checked_mul(150)
            .ok_or(Error::InvalidPrice)?
            .checked_div(100)
            .ok_or(Error::InvalidPrice)?;

        if collateral_value < required_collateral {
            return Err(Error::InsufficientCollateral);
        }

        // Store position
        let position = BorrowPosition {
            borrower: borrower.clone(),
            asset: borrow_asset.clone(),
            borrow_amount,
            collateral_asset,
            collateral_amount,
            created_at: env.ledger().timestamp(),
        };

        let mut positions: Vec<BorrowPosition> = env
            .storage()
            .instance()
            .get(&DataKey::BorrowerPositions(borrower.clone()))
            .unwrap_or_else(|| Vec::new(&env));

        positions.push_back(position);
        env.storage()
            .instance()
            .set(&DataKey::BorrowerPositions(borrower), &positions);

        env.events().publish(
            (Symbol::new(&env, "position_created"),),
            (borrow_asset, borrow_amount, collateral_amount),
        );

        Ok(())
    }

    /// Get all positions for a borrower
    pub fn get_positions(env: Env, borrower: Address) -> Vec<BorrowPosition> {
        env.storage()
            .instance()
            .get(&DataKey::BorrowerPositions(borrower.clone()))
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Get current asset price (read-only)
    pub fn get_price(env: Env, asset: Symbol) -> Result<i128, Error> {
        let prices: soroban_sdk::Map<Symbol, i128> = env
            .storage()
            .instance()
            .get(&DataKey::AssetPrices)
            .unwrap_or_else(|| soroban_sdk::Map::new(&env));

        prices.get(asset).ok_or(Error::InvalidPrice)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Internal Helper Functions
// ─────────────────────────────────────────────────────────────────────────────

fn check_liquidations(env: &Env, asset: &Symbol, new_price: i128, old_price: i128) {
    // Only check if asset price changed significantly
    if old_price == 0 {
        return; // First update, no comparison
    }

    let price_change_pct = ((new_price - old_price) * 10000) / old_price;

    // Only process if price dropped by more than 5%
    if price_change_pct >= -500 {
        return;
    }

    // TODO: Iterate through all positions and check collateralization
    // This is simplified - in production you'd need position tracking
    env.events().publish(
        (Symbol::new(&env, "liquidation_check"),),
        (asset.clone(), new_price, price_change_pct),
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{
        symbol_short, testutils::Address as TestAddress, Env, Symbol,
    };

    #[test]
    fn test_price_update_callback() {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register_contract(None, LendingPool);
        let client = LendingPoolClient::new(&env, &contract_id);

        let admin = TestAddress::generate(&env);
        let oracle = TestAddress::generate(&env);

        // Initialize
        client.initialize(&admin, &oracle);

        // Simulate oracle calling on_price_update
        let payload = PriceUpdatePayload {
            asset: symbol_short!("NGN"),
            price: 1_500_000_i128,
            timestamp: 1_000_000u64,
            provider: oracle.clone(),
            decimals: 9u32,
            confidence_score: 95u32,
        };

        // In real scenario, oracle would call this
        // client.on_price_update(&payload);

        // Check that price is stored
        // assert_eq!(client.get_price(&symbol_short!("NGN")).unwrap(), 1_500_000);
    }

    #[test]
    fn test_create_position() {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register_contract(None, LendingPool);
        let client = LendingPoolClient::new(&env, &contract_id);

        let admin = TestAddress::generate(&env);
        let oracle = TestAddress::generate(&env);
        let borrower = TestAddress::generate(&env);

        // Initialize
        client.initialize(&admin, &oracle);

        // Manually set prices for test
        // (In production, oracle would push these via callback)
        // client.set_price(&symbol_short!("USDT"), &1_000_000);
        // client.set_price(&symbol_short!("NGN"), &1_500_000);

        // Create position
        // NGN position worth 1M, collateralized with 1.5M USDT
        let result = client.create_position(
            &borrower,
            &symbol_short!("NGN"),
            &1_000_000_i128,
            &symbol_short!("USDT"),
            &1_500_000_i128,
        );

        // Should succeed if prices are set
        // assert!(result.is_ok());

        // Get positions
        // let positions = client.get_positions(&borrower);
        // assert_eq!(positions.len(), 1);
    }
}

// Export for use by other contracts
pub use LendingPool;
