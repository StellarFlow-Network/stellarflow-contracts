use soroban_sdk::{
    testutils::Address as _,
    Address, Env,
};
use soroban_sdk::token;

/// Represents the state of a mock token contract.
pub struct MockTokenState {
    pub token_id: Address,
}

/// Set up mock token state with initial balances for given addresses.
/// Uses `env.register_stellar_asset_contract` to create a mock Stellar token contract
/// and `StellarAssetClient::mint` to set initial balances.
pub fn setup_mock_token_state(
    env: &Env,
    admin: &Address,
    initial_balances: &[(Address, i128)],
) -> MockTokenState {
    env.mock_all_auths();

    let token_id = env.register_stellar_asset_contract(admin.clone());
    let client = soroban_sdk::token::StellarAssetClient::new(env, &token_id);

    for (account, balance) in initial_balances {
        client.mint(account, balance);
    }

    MockTokenState { token_id }
}

/// Simulate a token approval: `owner` approves `spender` to spend `amount`.
pub fn mock_approve(env: &Env, token_id: &Address, owner: &Address, spender: &Address, amount: i128) {
    env.mock_all_auths();
    let client = token::Client::new(env, token_id);
    client.approve(owner, spender, &amount);
}

/// Simulate a token transfer: move `amount` from `from` to `to`.
pub fn mock_transfer(env: &Env, token_id: &Address, from: &Address, to: &Address, amount: i128) {
    env.mock_all_auths();
    let client = token::Client::new(env, token_id);
    client.transfer(from, to, &amount);
}

/// Simulate a token transfer from `from` to `to` on behalf of `spender` using an allowance.
pub fn mock_transfer_from(
    env: &Env,
    token_id: &Address,
    spender: &Address,
    from: &Address,
    to: &Address,
    amount: i128,
) {
    env.mock_all_auths();
    let client = token::Client::new(env, token_id);
    client.transfer_from(spender, from, to, &amount);
}

/// Set the balance of `account` to exactly `amount` by minting additional tokens.
/// Panics if the account already has a balance (minting to an existing balance
/// is not supported by Stellar asset contracts in the mock environment).
pub fn mock_set_balance(env: &Env, token_id: &Address, account: &Address, amount: i128) {
    env.mock_all_auths();
    let client = soroban_sdk::token::StellarAssetClient::new(env, token_id);
    client.mint(account, &amount);
}

/// Query the balance of `account` for the mock token contract.
pub fn mock_balance_of(env: &Env, token_id: &Address, account: &Address) -> i128 {
    let client = token::Client::new(env, token_id);
    client.balance(account)
}

/// Query the allowance that `owner` has granted to `spender`.
pub fn mock_allowance(env: &Env, token_id: &Address, owner: &Address, spender: &Address) -> i128 {
    let client = token::Client::new(env, token_id);
    client.allowance(owner, spender)
}

/// Set up a mock token with admin, initial balances, and pre-approved allowances.
/// Convenience function that combines setup and approval in one call.
pub fn setup_mock_token_with_allowances(
    env: &Env,
    admin: &Address,
    initial_balances: &[(Address, i128)],
    allowances: &[(Address, Address, i128)],
) -> MockTokenState {
    let state = setup_mock_token_state(env, admin, initial_balances);

    for (owner, spender, amount) in allowances {
        mock_approve(env, &state.token_id, owner, spender, *amount);
    }

    state
}