use soroban_sdk::{
    testutils::Address as _,
    Address, Env, Symbol,
};

/// A minimal mock price oracle contract for offline integration tests.
/// Deployed with `env.register_contract` so that contract calls behave as
/// they would against a real external Stellar Asset Contract or price feed.
#[soroban_sdk::contract]
pub struct MockPriceOracle;

#[soroban_sdk::contractimpl]
impl MockPriceOracle {
    /// Record a price for the given asset. Any address may call this in the
    /// mock environment (auth is provided by `env.mock_all_auths()`).
    pub fn set_price(env: Env, asset: Symbol, price: i128) {
        env.storage().persistent().set(&asset, &price);
    }

    /// Retrieve the price for an asset. Returns `None` when no price has been set.
    pub fn get_price(env: Env, asset: Symbol) -> Option<i128> {
        env.storage().persistent().get::<_, i128>(&asset)
    }
}

/// Deploy a fresh `MockPriceOracle` contract and return its address and client.
pub fn setup_mock_oracle(env: &Env) -> (Address, MockPriceOracleClient) {
    env.mock_all_auths();
    let oracle_id = env.register_contract(None, MockPriceOracle);
    let client = MockPriceOracleClient::new(env, &oracle_id);
    (oracle_id, client)
}

/// Update the price for `asset` to `price`, simulating a price-feed provider
/// submitting a new value.
pub fn mock_oracle_update_price(env: &Env, oracle_id: &Address, asset: Symbol, price: i128) {
    env.mock_all_auths();
    let client = MockPriceOracleClient::new(env, oracle_id);
    env.as_contract(oracle_id, || {
        MockPriceOracle::set_price(env.clone(), asset, price);
    });
}

/// Query the latest price for `asset` from the mock oracle.
pub fn mock_oracle_get_price(env: &Env, oracle_id: &Address, asset: Symbol) -> Option<i128> {
    let client = MockPriceOracleClient::new(env, oracle_id);
    client.get_price(&asset)
}

/// Update several prices in a single batch inside the mock oracle.
pub fn mock_oracle_set_prices(env: &Env, oracle_id: &Address, prices: &[(Symbol, i128)]) {
    env.mock_all_auths();
    env.as_contract(oracle_id, || {
        for (asset, price) in prices {
            MockPriceOracle::set_price(env.clone(), asset.clone(), *price);
        }
    });
}

/// Return `true` if a price exists for `asset` in the mock oracle.
pub fn mock_oracle_has_price(env: &Env, oracle_id: &Address, asset: Symbol) -> bool {
    mock_oracle_get_price(env, oracle_id, asset).is_some()
}

/// Advance the mock ledger timestamp by `seconds`, useful for TTL / staleness tests.
pub fn mock_oracle_advance_time(env: &Env, seconds: u64) {
    let current_ts = env.ledger().timestamp();
    env.ledger().with_mut(|li| li.timestamp = current_ts + seconds);
}