#![no_std]
use soroban_sdk::{contract, contractimpl, contracttype, Address, Env, Symbol};

// ─────────────────────────────────────────────
// 1. STORAGE KEYS
// Each variant becomes a unique key in the
// on-chain key-value store.
// ─────────────────────────────────────────────
#[contracttype]
pub enum DataKey {
    Admin,              // stores the admin Address
    AssetPair(Symbol),  // stores an AssetPairConfig, keyed by symbol e.g. "GHS"
}

// ─────────────────────────────────────────────
// 2. DATA SHAPE
// What we save for every whitelisted pair.
// ─────────────────────────────────────────────
#[contracttype]
#[derive(Clone)]
pub struct AssetPairConfig {
    pub symbol: Symbol,         // e.g. "GHS"
    pub enabled: bool,          // true = active
    pub added_at_ledger: u32,   // ledger number when it was added
}

// ─────────────────────────────────────────────
// 3. ADMIN HELPER
// Called at the start of any admin-only function.
// Panics if the transaction isn't signed by the admin.
// ─────────────────────────────────────────────
fn require_admin(env: &Env) {
    let admin: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .expect("contract not initialized");
    admin.require_auth();
}

// ─────────────────────────────────────────────
// 4. CONTRACT + ENTRY POINTS
// ─────────────────────────────────────────────
#[contract]
pub struct StellarFlowOracle;

#[contractimpl]
impl StellarFlowOracle {

    /// Called once when the contract is first deployed.
    /// Saves the admin address so we know who can add pairs later.
    pub fn initialize(env: Env, admin: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("already initialized");
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
    }

    /// Admin-only: whitelist a new fiat asset pair.
    /// Panics if the caller is not admin, or the pair already exists.
    pub fn add_asset_pair(env: Env, symbol: Symbol) {
        // Block non-admins
        require_admin(&env);

        // Build the key we'll look up in storage
        let key = DataKey::AssetPair(symbol.clone());

        // Reject duplicates
        if env.storage().persistent().has(&key) {
            panic!("asset pair already exists");
        }

        // Build and save the config
        let config = AssetPairConfig {
            symbol: symbol.clone(),
            enabled: true,
            added_at_ledger: env.ledger().sequence(),
        };
        env.storage().persistent().set(&key, &config);

        // Emit an event (useful for off-chain listeners)
        env.events().publish(
            (Symbol::new(&env, "asset_pair_added"),),
            symbol,
        );
    }

    /// Anyone can call this to check if a pair is whitelisted.
    pub fn is_whitelisted(env: Env, symbol: Symbol) -> bool {
        env.storage()
            .persistent()
            .has(&DataKey::AssetPair(symbol))
    }

    /// Anyone can call this to read a pair's config.
    pub fn get_asset_pair(env: Env, symbol: Symbol) -> AssetPairConfig {
        env.storage()
            .persistent()
            .get(&DataKey::AssetPair(symbol))
            .expect("asset pair not found")
    }
}

// ─────────────────────────────────────────────
// 5. TESTS
// Run with: cargo test
// ─────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Env, Symbol};

    fn setup() -> (Env, StellarFlowOracleClient<'static>, Address) {
        let env = Env::default();
        env.mock_all_auths(); // skips real signature checks in tests
        let contract_id = env.register_contract(None, StellarFlowOracle);
        let client = StellarFlowOracleClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        client.initialize(&admin);
        (env, client, admin)
    }

    #[test]
    fn test_add_ghs_succeeds() {
        let (env, client, _) = setup();
        client.add_asset_pair(&Symbol::new(&env, "GHS"));
        assert!(client.is_whitelisted(&Symbol::new(&env, "GHS")));
    }

    #[test]
    #[should_panic(expected = "asset pair already exists")]
    fn test_duplicate_rejected() {
        let (env, client, _) = setup();
        client.add_asset_pair(&Symbol::new(&env, "KES"));
        client.add_asset_pair(&Symbol::new(&env, "KES")); // must panic
    }

    #[test]
    fn test_get_config_enabled() {
        let (env, client, _) = setup();
        client.add_asset_pair(&Symbol::new(&env, "NGN"));
        let config = client.get_asset_pair(&Symbol::new(&env, "NGN"));
        assert!(config.enabled);
    }
}