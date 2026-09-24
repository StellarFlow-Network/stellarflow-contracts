use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger},
    Address, Env,
};

mod mocks;
use mocks::oracle_mocks::{
    mock_oracle_advance_time, mock_oracle_get_price, mock_oracle_has_price, mock_oracle_set_prices,
    mock_oracle_update_price, setup_mock_oracle,
};
use mocks::token_mocks::{
    mock_allowance, mock_approve, mock_balance_of, mock_set_balance, mock_transfer,
    mock_transfer_from, setup_mock_token_state, MockTokenState,
};

/// Integration test: mock token setup, approval, and transfer work
/// entirely offline without any live network connectivity.
#[test]
fn test_mock_token_approval_and_transfer_offline() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let owner = Address::generate(&env);
    let spender = Address::generate(&env);
    let recipient = Address::generate(&env);

    // Set up a mock token contract with an initial balance for `owner`.
    let token_state = setup_mock_token_state(&env, &admin, &[(owner.clone(), 1_000_000_i128)]);

    // Verify the initial balance was minted correctly.
    assert_eq!(
        mock_balance_of(&env, &token_state.token_id, &owner),
        1_000_000_i128
    );
    assert_eq!(
        mock_balance_of(&env, &token_state.token_id, &spender),
        0_i128
    );
    assert_eq!(
        mock_balance_of(&env, &token_state.token_id, &recipient),
        0_i128
    );

    // Simulate owner approving spender to spend up to 500_000 tokens.
    mock_approve(&env, &token_state.token_id, &owner, &spender, 500_000_i128);
    assert_eq!(
        mock_allowance(&env, &token_state.token_id, &owner, &spender),
        500_000_i128
    );

    // Simulate spender transferring 200_000 tokens from owner to recipient.
    mock_transfer_from(
        &env,
        &token_state.token_id,
        &spender,
        &owner,
        &recipient,
        200_000_i128,
    );

    // Verify balances after the transfer.
    assert_eq!(
        mock_balance_of(&env, &token_state.token_id, &owner),
        800_000_i128
    );
    assert_eq!(
        mock_balance_of(&env, &token_state.token_id, &spender),
        0_i128
    );
    assert_eq!(
        mock_balance_of(&env, &token_state.token_id, &recipient),
        200_000_i128
    );
}

/// Integration test: mock oracle price updates work entirely offline.
#[test]
fn test_mock_oracle_price_updates_offline() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);

    // Deploy the mock oracle contract.
    let (oracle_id, _client) = setup_mock_oracle(&env);

    let ngn = symbol_short!("NGN");
    let kes = symbol_short!("KES");
    let ghs = symbol_short!("GHS");

    // Initially no prices are set.
    assert!(mock_oracle_get_price(&env, &oracle_id, ngn.clone()).is_none());
    assert!(mock_oracle_get_price(&env, &oracle_id, kes.clone()).is_none());

    // Simulate price feed updates.
    mock_oracle_update_price(&env, &oracle_id, ngn.clone(), 1_500_000_i128);
    mock_oracle_update_price(&env, &oracle_id, kes.clone(), 50_000_000_000_i128);

    // Verify prices were stored correctly.
    let ngn_price = mock_oracle_get_price(&env, &oracle_id, ngn.clone());
    assert!(ngn_price.is_some());
    assert_eq!(ngn_price.unwrap(), 1_500_000_i128);

    let kes_price = mock_oracle_get_price(&env, &oracle_id, kes.clone());
    assert!(kes_price.is_some());
    assert_eq!(kes_price.unwrap(), 50_000_000_000_i128);

    // Verify a missing price returns None.
    assert!(mock_oracle_get_price(&env, &oracle_id, ghs.clone()).is_none());
    assert!(!mock_oracle_has_price(&env, &oracle_id, ghs.clone()));

    // Batch price update.
    mock_oracle_set_prices(
        &env,
        &oracle_id,
        &[(ghs.clone(), 4_500_000_i128), (ngn.clone(), 1_600_000_i128)],
    );

    assert_eq!(
        mock_oracle_get_price(&env, &oracle_id, ghs.clone()).unwrap(),
        4_500_000_i128
    );
    assert_eq!(
        mock_oracle_get_price(&env, &oracle_id, ngn.clone()).unwrap(),
        1_600_000_i128
    );

    // Advance ledger time and verify the oracle still works.
    mock_oracle_advance_time(&env, 3600);
    assert_eq!(env.ledger().timestamp(), 3600);
}

/// Integration test: combine token mocks and oracle mocks in a single
/// offline scenario simulating a trade that checks a price feed before
/// executing a token transfer.
#[test]
fn test_offline_trade_with_token_and_oracle_mocks() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let trader = Address::generate(&env);
    let counterparty = Address::generate(&env);

    // Deploy a mock token contract and mint tokens to the trader.
    let token_state = setup_mock_token_state(&env, &admin, &[(trader.clone(), 10_000_000_i128)]);

    // Deploy a mock oracle and set the NGN/USDC price.
    let (oracle_id, _oracle_client) = setup_mock_oracle(&env);
    let ngn = symbol_short!("NGN");
    let usdc = symbol_short!("USDC");

    mock_oracle_update_price(&env, &oracle_id, ngn.clone(), 1_500_000_i128);
    mock_oracle_update_price(&env, &oracle_id, usdc.clone(), 1_000_000_000_i128);

    // Simulate a trade: trader wants to sell 100 NGN to receive USDC.
    // The price is 1_500_000 NGN per base unit, so 100 NGN = 150_000_000_000 base units
    // of quote (simplified for this mock scenario).
    let sell_amount: i128 = 100;

    // Check the oracle price before executing the transfer.
    let ngn_price = mock_oracle_get_price(&env, &oracle_id, ngn.clone()).unwrap();
    assert_eq!(ngn_price, 1_500_000_i128);

    // Verify trader has sufficient balance.
    assert_eq!(
        mock_balance_of(&env, &token_state.token_id, &trader),
        10_000_000_i128
    );

    // Approve the counterparty to receive tokens on behalf of the trader.
    mock_approve(
        &env,
        &token_state.token_id,
        &trader,
        &counterparty,
        sell_amount,
    );
    assert_eq!(
        mock_allowance(&env, &token_state.token_id, &trader, &counterparty),
        sell_amount
    );

    // Execute the transfer (simulating a trade settlement).
    mock_transfer_from(
        &env,
        &token_state.token_id,
        &counterparty,
        &trader,
        &counterparty,
        sell_amount,
    );

    // Verify post-trade balances.
    assert_eq!(
        mock_balance_of(&env, &token_state.token_id, &trader),
        10_000_000 - sell_amount
    );
    assert_eq!(
        mock_balance_of(&env, &token_state.token_id, &counterparty),
        sell_amount
    );

    // Verify the oracle price is still accessible (no side effects).
    assert_eq!(
        mock_oracle_get_price(&env, &oracle_id, ngn.clone()).unwrap(),
        1_500_000_i128
    );
}
