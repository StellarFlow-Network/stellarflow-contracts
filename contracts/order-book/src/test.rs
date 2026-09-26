#![cfg(test)]

use super::*;
use soroban_sdk::{testutils::Address as _, Address, Env};

fn setup(env: &Env, maker: &Address, amount: i128) -> (Address, Address, OrderBookClient<'_>) {
    let contract_id = env.register_contract(None, OrderBook);
    let token_id = env.register_stellar_asset_contract(maker.clone());
    soroban_sdk::token::StellarAssetClient::new(env, &token_id).mint(maker, &amount);
    let client = OrderBookClient::new(env, &contract_id);
    (contract_id, token_id, client)
}

#[test]
fn stale_orders_are_cancelled_and_refunded() {
    let env = Env::default();
    env.mock_all_auths();
    let maker = Address::generate(&env);
    let (contract_id, token_id, book) = setup(&env, &maker, 100);

    let order_id = book.place_order(&maker, &token_id, &100);
    assert_eq!(soroban_sdk::token::Client::new(&env, &token_id).balance(&contract_id), 100);
    env.ledger().set_timestamp(24 * 60 * 60 + 1);

    assert_eq!(book.cancel_stale_orders(&maker), 1);
    assert_eq!(soroban_sdk::token::Client::new(&env, &token_id).balance(&maker), 100);
    assert!(book.get_order(&order_id).is_none());
}

#[test]
fn order_interactions_refresh_heartbeat() {
    let env = Env::default();
    env.mock_all_auths();
    let maker = Address::generate(&env);
    let (_, token_id, book) = setup(&env, &maker, 100);

    env.ledger().set_timestamp(10);
    let order_id = book.place_order(&maker, &token_id, &10);
    assert_eq!(book.get_heartbeat(&maker), Some(10));
    env.ledger().set_timestamp(20);
    book.cancel_order(&maker, &order_id);
    assert_eq!(book.get_heartbeat(&maker), Some(20));
}