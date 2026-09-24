#![cfg(test)]

use super::*;
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short,
    testutils::{Address as _, Events},
    token::{Client as TokenClient, StellarAssetClient},
    Address, Env, Symbol, TryFromVal, Vec,
};

// ─────────────────────────────────────────────────────────────────────────────
// Mock TWAP oracle
// ─────────────────────────────────────────────────────────────────────────────
//
// Minimal, faithful stand-in for the workspace `price-oracle` contract used by
// the tests: it exposes the exact same `get_twap` cross-contract interface the
// handler calls (last-10 verified price updates averaged, `None` when the
// buffer is empty, `Err` when halted).

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
enum MockDataKey {
    TwapBuffer(Symbol),
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
enum MockOracleError {
    EmergencyHalted = 1,
}

#[contract]
struct MockTwapOracle;

#[contractimpl]
impl MockTwapOracle {
    pub fn set_price(env: Env, asset: Symbol, price: i128) -> Result<(), MockOracleError> {
        let key = MockDataKey::TwapBuffer(asset);
        let mut buffer: Vec<(u64, i128)> = env
            .storage()
            .temporary()
            .get(&key)
            .unwrap_or_else(|| Vec::new(&env));
        buffer.push_back((env.ledger().timestamp(), price));
        if buffer.len() > 10 {
            buffer.pop_front();
        }
        env.storage().temporary().set(&key, &buffer);
        Ok(())
    }

    pub fn get_twap(env: Env, asset: Symbol) -> Result<Option<i128>, MockOracleError> {
        let key = MockDataKey::TwapBuffer(asset);
        let buffer: Option<Vec<(u64, i128)>> = env.storage().temporary().get(&key);
        match buffer {
            None => Ok(None),
            Some(buf) if buf.len() == 0 => Ok(None),
            Some(buf) => {
                let mut sum: i128 = 0;
                for (_, price) in buf.iter() {
                    sum += price;
                }
                Ok(Some(sum / buf.len() as i128))
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Test helpers
// ─────────────────────────────────────────────────────────────────────────────

fn setup() -> (
    Env,
    StopLossTriggerContractClient<'static>,
    MockTwapOracleClient<'static>,
    Address,
) {
    let env = Env::default();
    env.mock_all_auths();

    // Register the mock TWAP oracle (stand-in for the price-oracle contract).
    let oracle_id = env.register_contract(None, MockTwapOracle);
    let oracle_client = MockTwapOracleClient::new(&env, &oracle_id);

    // Register and initialize the stop-loss trigger handler.
    let admin = Address::generate(&env);
    let handler_id = env.register_contract(None, StopLossTriggerContract);
    let client = StopLossTriggerContractClient::new(&env, &handler_id);
    client.initialize(&admin, &oracle_id);

    (env, client, oracle_client, admin)
}

/// Push `prices` into the mock oracle's verified TWAP buffer.
fn seed_twap(oracle_client: &MockTwapOracleClient, symbol: &Symbol, prices: &[i128]) {
    for price in prices.iter() {
        oracle_client.set_price(symbol, &price);
    }
}

fn mint(env: &Env, asset: &Address, to: &Address, amount: i128) {
    StellarAssetClient::new(env, asset).mint(to, &amount);
}

fn balance(env: &Env, asset: &Address, holder: &Address) -> i128 {
    TokenClient::new(env, asset).balance(holder)
}

/// True when any event emitted so far carries `topic` as its first topic.
fn event_has_topic(env: &Env, topic: &Symbol) -> bool {
    env.events().all().iter().any(|(_, topics, _)| {
        if topics.len() == 0 {
            return false;
        }
        match Symbol::try_from_val(env, &topics.get(0).unwrap()) {
            Ok(s) => s == *topic,
            Err(_) => false,
        }
    })
}

/// Create the sell/buy tokens and a standard stop-loss trigger:
/// sell 1_000 SELL, stop at 1.20e9, TWAP seeded at 1.00e9 (breached),
/// slippage corridor [9.0e8, 1.10e9], min proceeds 90_000.
fn register_standard_trigger(
    client: &StopLossTriggerContractClient,
    owner: &Address,
    sell_asset: &Address,
    buy_asset: &Address,
) -> StopLossTrigger {
    client.register_trigger(
        owner,
        sell_asset,
        buy_asset,
        &symbol_short!("NGN"),
        &1_000i128,
        &1_200_000_000i128,
        &TriggerCondition::PriceBelow,
        &900_000_000i128,
        &1_100_000_000i128,
        &90_000i128,
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn initialize_sets_config_and_rejects_double_init() {
    let (env, client, _, admin) = setup();
    let config = client.get_config().unwrap();
    assert_eq!(config.admin, admin);

    let second = Address::generate(&env);
    let other_oracle = Address::generate(&env);
    let result = client.try_initialize(&second, &other_oracle);
    assert_eq!(result, Err(Ok(ContractError::AlreadyInitialized)));
}

#[test]
fn set_oracle_requires_current_admin() {
    let (env, client, _, _) = setup();
    let new_oracle = Address::generate(&env);

    let intruder = Address::generate(&env);
    let result = client.try_set_oracle(&intruder, &new_oracle);
    assert_eq!(result, Err(Ok(ContractError::NotAdmin)));
}

#[test]
fn register_trigger_escrows_sell_asset() {
    let (env, client, _, _) = setup();
    let owner = Address::generate(&env);
    let sell_issuer = Address::generate(&env);
    let buy_issuer = Address::generate(&env);
    let sell_asset = env.register_stellar_asset_contract(sell_issuer);
    let buy_asset = env.register_stellar_asset_contract(buy_issuer);
    mint(&env, &sell_asset, &owner, 1_000);

    let trigger = register_standard_trigger(&client, &owner, &sell_asset, &buy_asset);

    assert_eq!(trigger.owner, owner);
    assert_eq!(trigger.sell_amount, 1_000);
    assert!(trigger.active);
    assert_eq!(balance(&env, &sell_asset, &owner), 0);
    assert_eq!(client.get_pool_balance(&sell_asset), 1_000);

    let stored = client.get_trigger(&trigger.id).unwrap();
    assert_eq!(stored, trigger);
}

#[test]
fn register_rejects_invalid_params() {
    let (env, client, _, _) = setup();
    let owner = Address::generate(&env);
    let sell_issuer = Address::generate(&env);
    let buy_issuer = Address::generate(&env);
    let sell_asset = env.register_stellar_asset_contract(sell_issuer);
    let buy_asset = env.register_stellar_asset_contract(buy_issuer);
    mint(&env, &sell_asset, &owner, 1_000);

    let zero_amount = client.try_register_trigger(
        &owner,
        &sell_asset,
        &buy_asset,
        &symbol_short!("NGN"),
        &0i128,
        &1_200_000_000i128,
        &TriggerCondition::PriceBelow,
        &900_000_000i128,
        &1_100_000_000i128,
        &0i128,
    );
    assert_eq!(zero_amount, Err(Ok(ContractError::InvalidAmount)));

    let zero_stop = client.try_register_trigger(
        &owner,
        &sell_asset,
        &buy_asset,
        &symbol_short!("NGN"),
        &1_000i128,
        &0i128,
        &TriggerCondition::PriceBelow,
        &900_000_000i128,
        &1_100_000_000i128,
        &0i128,
    );
    assert_eq!(zero_stop, Err(Ok(ContractError::InvalidTriggerPrice)));

    let inverted_bounds = client.try_register_trigger(
        &owner,
        &sell_asset,
        &buy_asset,
        &symbol_short!("NGN"),
        &1_000i128,
        &1_200_000_000i128,
        &TriggerCondition::PriceBelow,
        &1_100_000_000i128,
        &900_000_000i128,
        &0i128,
    );
    assert_eq!(
        inverted_bounds,
        Err(Ok(ContractError::InvalidSlippageBounds))
    );

    let negative_min_out = client.try_register_trigger(
        &owner,
        &sell_asset,
        &buy_asset,
        &symbol_short!("NGN"),
        &1_000i128,
        &1_200_000_000i128,
        &TriggerCondition::PriceBelow,
        &900_000_000i128,
        &1_100_000_000i128,
        &-1i128,
    );
    assert_eq!(
        negative_min_out,
        Err(Ok(ContractError::InvalidSlippageBounds))
    );
}

#[test]
fn execute_on_stop_loss_breach_swaps_position() {
    let (env, client, oracle_client, _) = setup();
    let handler = client.address.clone();
    let owner = Address::generate(&env);
    let keeper = Address::generate(&env);
    let sell_issuer = Address::generate(&env);
    let buy_issuer = Address::generate(&env);
    let sell_asset = env.register_stellar_asset_contract(sell_issuer);
    let buy_asset = env.register_stellar_asset_contract(buy_issuer);
    mint(&env, &sell_asset, &owner, 1_000);
    mint(&env, &buy_asset, &handler, 500_000);

    // Verified TWAP = 1.00e9 — below the 1.20e9 stop → breach.
    seed_twap(
        &oracle_client,
        &symbol_short!("NGN"),
        &[1_000_000_000, 1_000_000_000, 1_000_000_000],
    );

    let trigger = register_standard_trigger(&client, &owner, &sell_asset, &buy_asset);

    let result = client.execute_trigger(&keeper, &trigger.id, &1_000_000_000i128);

    assert_eq!(result.twap_price, 1_000_000_000);
    assert_eq!(result.fill_price, 1_000_000_000);
    // proceeds = 1_000 * 1.00e9 / 1e7 = 100_000
    assert_eq!(result.proceeds, 100_000);

    // Position swapped: owner now holds buy_asset, pool absorbed sell_asset.
    assert_eq!(balance(&env, &buy_asset, &owner), 100_000);
    assert_eq!(balance(&env, &sell_asset, &handler), 1_000);
    assert!(!client.get_trigger(&trigger.id).unwrap().active);
}

#[test]
fn execute_fires_at_exact_stop_price_boundary() {
    let (env, client, oracle_client, _) = setup();
    let handler = client.address.clone();
    let owner = Address::generate(&env);
    let keeper = Address::generate(&env);
    let sell_issuer = Address::generate(&env);
    let buy_issuer = Address::generate(&env);
    let sell_asset = env.register_stellar_asset_contract(sell_issuer);
    let buy_asset = env.register_stellar_asset_contract(buy_issuer);
    mint(&env, &sell_asset, &owner, 1_000);
    mint(&env, &buy_asset, &handler, 500_000);

    // TWAP exactly equals the stop price → inclusive breach.
    seed_twap(
        &oracle_client,
        &symbol_short!("NGN"),
        &[1_200_000_000, 1_200_000_000, 1_200_000_000],
    );

    // Slippage corridor [1.00e9, 1.20e9] accepts the 1.20e9 fill so the
    // boundary condition itself is what is exercised.
    let trigger = client.register_trigger(
        &owner,
        &sell_asset,
        &buy_asset,
        &symbol_short!("NGN"),
        &1_000i128,
        &1_200_000_000i128,
        &TriggerCondition::PriceBelow,
        &1_000_000_000i128,
        &1_200_000_000i128,
        &100_000i128,
    );

    let result = client.execute_trigger(&keeper, &trigger.id, &1_200_000_000i128);
    assert_eq!(result.proceeds, 120_000);
    assert!(!client.get_trigger(&trigger.id).unwrap().active);
}

#[test]
fn execute_price_above_direction_fires_on_rise() {
    let (env, client, oracle_client, _) = setup();
    let handler = client.address.clone();
    let owner = Address::generate(&env);
    let keeper = Address::generate(&env);
    let sell_issuer = Address::generate(&env);
    let buy_issuer = Address::generate(&env);
    let sell_asset = env.register_stellar_asset_contract(sell_issuer);
    let buy_asset = env.register_stellar_asset_contract(buy_issuer);
    mint(&env, &sell_asset, &owner, 1_000);
    mint(&env, &buy_asset, &handler, 500_000);

    seed_twap(
        &oracle_client,
        &symbol_short!("NGN"),
        &[1_200_000_000, 1_200_000_000, 1_200_000_000],
    );

    let trigger = client.register_trigger(
        &owner,
        &sell_asset,
        &buy_asset,
        &symbol_short!("NGN"),
        &1_000i128,
        &1_000_000_000i128,
        &TriggerCondition::PriceAbove,
        &1_100_000_000i128,
        &1_300_000_000i128,
        &100_000i128,
    );

    let result = client.execute_trigger(&keeper, &trigger.id, &1_200_000_000i128);
    assert_eq!(result.proceeds, 120_000);
    assert!(!client.get_trigger(&trigger.id).unwrap().active);
}

#[test]
fn execute_without_breach_keeps_trigger_active_and_untouched() {
    let (env, client, oracle_client, _) = setup();
    let handler = client.address.clone();
    let owner = Address::generate(&env);
    let keeper = Address::generate(&env);
    let sell_issuer = Address::generate(&env);
    let buy_issuer = Address::generate(&env);
    let sell_asset = env.register_stellar_asset_contract(sell_issuer);
    let buy_asset = env.register_stellar_asset_contract(buy_issuer);
    mint(&env, &sell_asset, &owner, 1_000);
    mint(&env, &buy_asset, &handler, 500_000);

    // TWAP = 1.30e9 is ABOVE the 1.20e9 stop → no breach for PriceBelow.
    seed_twap(
        &oracle_client,
        &symbol_short!("NGN"),
        &[1_300_000_000, 1_300_000_000, 1_300_000_000],
    );

    let trigger = register_standard_trigger(&client, &owner, &sell_asset, &buy_asset);

    let result = client.try_execute_trigger(&keeper, &trigger.id, &1_300_000_000i128);
    assert_eq!(result, Err(Ok(ContractError::TriggerConditionNotMet)));

    assert!(client.get_trigger(&trigger.id).unwrap().active);
    assert_eq!(balance(&env, &buy_asset, &owner), 0);
    assert_eq!(balance(&env, &sell_asset, &handler), 1_000);
}

#[test]
fn keeper_claimed_price_must_match_verified_twap() {
    let (env, client, oracle_client, _) = setup();
    let handler = client.address.clone();
    let owner = Address::generate(&env);
    let keeper = Address::generate(&env);
    let sell_issuer = Address::generate(&env);
    let buy_issuer = Address::generate(&env);
    let sell_asset = env.register_stellar_asset_contract(sell_issuer);
    let buy_asset = env.register_stellar_asset_contract(buy_issuer);
    mint(&env, &sell_asset, &owner, 1_000);
    mint(&env, &buy_asset, &handler, 500_000);

    seed_twap(
        &oracle_client,
        &symbol_short!("NGN"),
        &[1_000_000_000, 1_000_000_000, 1_000_000_000],
    );

    let trigger = register_standard_trigger(&client, &owner, &sell_asset, &buy_asset);

    // Keeper quotes a stale/fabricated price one unit off the verified TWAP.
    let result = client.try_execute_trigger(&keeper, &trigger.id, &1_000_000_001i128);
    assert_eq!(result, Err(Ok(ContractError::TriggerPriceNotVerified)));

    assert!(client.get_trigger(&trigger.id).unwrap().active);
    assert_eq!(balance(&env, &buy_asset, &owner), 0);
}

#[test]
fn execute_without_oracle_data_fails_cleanly() {
    let (env, client, _, _) = setup();
    let owner = Address::generate(&env);
    let keeper = Address::generate(&env);
    let sell_issuer = Address::generate(&env);
    let buy_issuer = Address::generate(&env);
    let sell_asset = env.register_stellar_asset_contract(sell_issuer);
    let buy_asset = env.register_stellar_asset_contract(buy_issuer);
    mint(&env, &sell_asset, &owner, 1_000);

    // Feed "UNFED" has no TWAP buffer at all.
    let trigger = client.register_trigger(
        &owner,
        &sell_asset,
        &buy_asset,
        &symbol_short!("UNFED"),
        &1_000i128,
        &1_200_000_000i128,
        &TriggerCondition::PriceBelow,
        &900_000_000i128,
        &1_100_000_000i128,
        &0i128,
    );

    let result = client.try_execute_trigger(&keeper, &trigger.id, &1_000_000_000i128);
    assert_eq!(result, Err(Ok(ContractError::OraclePriceUnavailable)));
    assert!(client.get_trigger(&trigger.id).unwrap().active);
}

#[test]
fn fill_price_above_max_fill_price_reverts_with_slippage() {
    let (env, client, oracle_client, _) = setup();
    let handler = client.address.clone();
    let owner = Address::generate(&env);
    let keeper = Address::generate(&env);
    let sell_issuer = Address::generate(&env);
    let buy_issuer = Address::generate(&env);
    let sell_asset = env.register_stellar_asset_contract(sell_issuer);
    let buy_asset = env.register_stellar_asset_contract(buy_issuer);
    mint(&env, &sell_asset, &owner, 1_000);
    mint(&env, &buy_asset, &handler, 500_000);

    // TWAP = 1.10e9 breaches the 1.20e9 stop.
    seed_twap(
        &oracle_client,
        &symbol_short!("NGN"),
        &[1_100_000_000, 1_100_000_000, 1_100_000_000],
    );

    // max_fill_price = 1.00e9 < fill 1.10e9 → fill price exceeds the bound.
    let trigger = client.register_trigger(
        &owner,
        &sell_asset,
        &buy_asset,
        &symbol_short!("NGN"),
        &1_000i128,
        &1_200_000_000i128,
        &TriggerCondition::PriceBelow,
        &900_000_000i128,
        &1_000_000_000i128,
        &0i128,
    );

    let result = client.try_execute_trigger(&keeper, &trigger.id, &1_100_000_000i128);
    assert_eq!(result, Err(Ok(ContractError::SlippageExceeded)));

    // The revert must leave every piece of state untouched.
    assert!(client.get_trigger(&trigger.id).unwrap().active);
    assert_eq!(balance(&env, &buy_asset, &owner), 0);
    assert_eq!(balance(&env, &sell_asset, &handler), 1_000);
}

#[test]
fn fill_price_below_min_fill_price_reverts_with_slippage() {
    let (env, client, oracle_client, _) = setup();
    let handler = client.address.clone();
    let owner = Address::generate(&env);
    let keeper = Address::generate(&env);
    let sell_issuer = Address::generate(&env);
    let buy_issuer = Address::generate(&env);
    let sell_asset = env.register_stellar_asset_contract(sell_issuer);
    let buy_asset = env.register_stellar_asset_contract(buy_issuer);
    mint(&env, &sell_asset, &owner, 1_000);
    mint(&env, &buy_asset, &handler, 500_000);

    seed_twap(
        &oracle_client,
        &symbol_short!("NGN"),
        &[1_000_000_000, 1_000_000_000, 1_000_000_000],
    );

    // min_fill_price = 1.05e9 > fill 1.00e9 → fill price falls below the floor.
    let trigger = client.register_trigger(
        &owner,
        &sell_asset,
        &buy_asset,
        &symbol_short!("NGN"),
        &1_000i128,
        &1_200_000_000i128,
        &TriggerCondition::PriceBelow,
        &1_050_000_000i128,
        &1_200_000_000i128,
        &0i128,
    );

    let result = client.try_execute_trigger(&keeper, &trigger.id, &1_000_000_000i128);
    assert_eq!(result, Err(Ok(ContractError::SlippageExceeded)));
    assert!(client.get_trigger(&trigger.id).unwrap().active);
}

#[test]
fn proceeds_below_min_amount_out_reverts_with_slippage() {
    let (env, client, oracle_client, _) = setup();
    let handler = client.address.clone();
    let owner = Address::generate(&env);
    let keeper = Address::generate(&env);
    let sell_issuer = Address::generate(&env);
    let buy_issuer = Address::generate(&env);
    let sell_asset = env.register_stellar_asset_contract(sell_issuer);
    let buy_asset = env.register_stellar_asset_contract(buy_issuer);
    mint(&env, &sell_asset, &owner, 1_000);
    mint(&env, &buy_asset, &handler, 500_000);

    seed_twap(
        &oracle_client,
        &symbol_short!("NGN"),
        &[1_000_000_000, 1_000_000_000, 1_000_000_000],
    );

    // Proceeds would be 100_000, but the owner demands at least 150_000.
    let trigger = client.register_trigger(
        &owner,
        &sell_asset,
        &buy_asset,
        &symbol_short!("NGN"),
        &1_000i128,
        &1_200_000_000i128,
        &TriggerCondition::PriceBelow,
        &900_000_000i128,
        &1_100_000_000i128,
        &150_000i128,
    );

    let result = client.try_execute_trigger(&keeper, &trigger.id, &1_000_000_000i128);
    assert_eq!(result, Err(Ok(ContractError::SlippageExceeded)));
    assert!(client.get_trigger(&trigger.id).unwrap().active);
    assert_eq!(balance(&env, &buy_asset, &owner), 0);
}

#[test]
fn second_execution_is_rejected() {
    let (env, client, oracle_client, _) = setup();
    let handler = client.address.clone();
    let owner = Address::generate(&env);
    let keeper = Address::generate(&env);
    let sell_issuer = Address::generate(&env);
    let buy_issuer = Address::generate(&env);
    let sell_asset = env.register_stellar_asset_contract(sell_issuer);
    let buy_asset = env.register_stellar_asset_contract(buy_issuer);
    mint(&env, &sell_asset, &owner, 1_000);
    mint(&env, &buy_asset, &handler, 500_000);

    seed_twap(
        &oracle_client,
        &symbol_short!("NGN"),
        &[1_000_000_000, 1_000_000_000, 1_000_000_000],
    );

    let trigger = register_standard_trigger(&client, &owner, &sell_asset, &buy_asset);
    client.execute_trigger(&keeper, &trigger.id, &1_000_000_000i128);

    let replay = client.try_execute_trigger(&keeper, &trigger.id, &1_000_000_000i128);
    assert_eq!(replay, Err(Ok(ContractError::TriggerNotActive)));

    // No double payout: the owner still only received the first fill.
    assert_eq!(balance(&env, &buy_asset, &owner), 100_000);
}

#[test]
fn insufficient_pool_liquidity_reverts_and_keeps_trigger_active() {
    let (env, client, oracle_client, _) = setup();
    let handler = client.address.clone();
    let owner = Address::generate(&env);
    let keeper = Address::generate(&env);
    let sell_issuer = Address::generate(&env);
    let buy_issuer = Address::generate(&env);
    let sell_asset = env.register_stellar_asset_contract(sell_issuer);
    let buy_asset = env.register_stellar_asset_contract(buy_issuer);
    mint(&env, &sell_asset, &owner, 1_000);
    // Pool holds only 10_000 — less than the 100_000 proceeds.
    mint(&env, &buy_asset, &handler, 10_000);

    seed_twap(
        &oracle_client,
        &symbol_short!("NGN"),
        &[1_000_000_000, 1_000_000_000, 1_000_000_000],
    );

    let trigger = register_standard_trigger(&client, &owner, &sell_asset, &buy_asset);

    let result = client.try_execute_trigger(&keeper, &trigger.id, &1_000_000_000i128);
    assert_eq!(result, Err(Ok(ContractError::InsufficientLiquidity)));
    assert!(client.get_trigger(&trigger.id).unwrap().active);
    assert_eq!(balance(&env, &buy_asset, &owner), 0);

    // Top the pool up and the same trigger now executes cleanly.
    let funder = Address::generate(&env);
    mint(&env, &buy_asset, &funder, 200_000);
    client.fund_pool(&funder, &buy_asset, &200_000i128);

    let result = client.execute_trigger(&keeper, &trigger.id, &1_000_000_000i128);
    assert_eq!(result.proceeds, 100_000);
    assert_eq!(balance(&env, &buy_asset, &owner), 100_000);
}

#[test]
fn cancel_refunds_escrow_and_blocks_execution() {
    let (env, client, oracle_client, _) = setup();
    let handler = client.address.clone();
    let owner = Address::generate(&env);
    let keeper = Address::generate(&env);
    let sell_issuer = Address::generate(&env);
    let buy_issuer = Address::generate(&env);
    let sell_asset = env.register_stellar_asset_contract(sell_issuer);
    let buy_asset = env.register_stellar_asset_contract(buy_issuer);
    mint(&env, &sell_asset, &owner, 1_000);
    mint(&env, &buy_asset, &handler, 500_000);

    seed_twap(
        &oracle_client,
        &symbol_short!("NGN"),
        &[1_000_000_000, 1_000_000_000, 1_000_000_000],
    );

    let trigger = register_standard_trigger(&client, &owner, &sell_asset, &buy_asset);

    let refund = client.cancel_trigger(&owner, &trigger.id);
    assert_eq!(refund, 1_000);
    assert_eq!(balance(&env, &sell_asset, &owner), 1_000);
    assert!(!client.get_trigger(&trigger.id).unwrap().active);

    let result = client.try_execute_trigger(&keeper, &trigger.id, &1_000_000_000i128);
    assert_eq!(result, Err(Ok(ContractError::TriggerNotActive)));
    assert_eq!(balance(&env, &buy_asset, &owner), 0);
}

#[test]
fn non_owner_cannot_cancel_trigger() {
    let (env, client, _, _) = setup();
    let handler = client.address.clone();
    let owner = Address::generate(&env);
    let attacker = Address::generate(&env);
    let sell_issuer = Address::generate(&env);
    let buy_issuer = Address::generate(&env);
    let sell_asset = env.register_stellar_asset_contract(sell_issuer);
    let buy_asset = env.register_stellar_asset_contract(buy_issuer);
    mint(&env, &sell_asset, &owner, 1_000);

    let trigger = register_standard_trigger(&client, &owner, &sell_asset, &buy_asset);

    let result = client.try_cancel_trigger(&attacker, &trigger.id);
    assert_eq!(result, Err(Ok(ContractError::NotTriggerOwner)));
    assert!(client.get_trigger(&trigger.id).unwrap().active);
    assert_eq!(balance(&env, &sell_asset, &handler), 1_000);
}

#[test]
fn unknown_trigger_id_fails() {
    let (env, client, _, _) = setup();
    let keeper = Address::generate(&env);

    let exec = client.try_execute_trigger(&keeper, &42u64, &1_000_000_000i128);
    assert_eq!(exec, Err(Ok(ContractError::TriggerNotFound)));

    let cancel = client.try_cancel_trigger(&keeper, &42u64);
    assert_eq!(cancel, Err(Ok(ContractError::TriggerNotFound)));
}

#[test]
fn any_keeper_may_execute() {
    let (env, client, oracle_client, _) = setup();
    let handler = client.address.clone();
    let owner = Address::generate(&env);
    // Keeper is an unrelated third party, not the owner.
    let keeper = Address::generate(&env);
    let sell_issuer = Address::generate(&env);
    let buy_issuer = Address::generate(&env);
    let sell_asset = env.register_stellar_asset_contract(sell_issuer);
    let buy_asset = env.register_stellar_asset_contract(buy_issuer);
    mint(&env, &sell_asset, &owner, 1_000);
    mint(&env, &buy_asset, &handler, 500_000);

    seed_twap(
        &oracle_client,
        &symbol_short!("NGN"),
        &[1_000_000_000, 1_000_000_000, 1_000_000_000],
    );

    let trigger = register_standard_trigger(&client, &owner, &sell_asset, &buy_asset);
    let result = client.execute_trigger(&keeper, &trigger.id, &1_000_000_000i128);
    assert_eq!(result.proceeds, 100_000);
    assert_eq!(balance(&env, &buy_asset, &owner), 100_000);
}

#[test]
fn execution_emits_registration_and_execution_events() {
    let (env, client, oracle_client, _) = setup();
    let handler = client.address.clone();
    let owner = Address::generate(&env);
    let keeper = Address::generate(&env);
    let sell_issuer = Address::generate(&env);
    let buy_issuer = Address::generate(&env);
    let sell_asset = env.register_stellar_asset_contract(sell_issuer);
    let buy_asset = env.register_stellar_asset_contract(buy_issuer);
    mint(&env, &sell_asset, &owner, 1_000);
    mint(&env, &buy_asset, &handler, 500_000);

    seed_twap(
        &oracle_client,
        &symbol_short!("NGN"),
        &[1_000_000_000, 1_000_000_000, 1_000_000_000],
    );

    let trigger = register_standard_trigger(&client, &owner, &sell_asset, &buy_asset);
    client.execute_trigger(&keeper, &trigger.id, &1_000_000_000i128);

    assert!(event_has_topic(&env, &symbol_short!("trig_reg")));
    assert!(event_has_topic(&env, &symbol_short!("trig_exec")));
}

#[test]
fn cancel_emits_cancellation_event() {
    let (env, client, _, _) = setup();
    let owner = Address::generate(&env);
    let sell_issuer = Address::generate(&env);
    let buy_issuer = Address::generate(&env);
    let sell_asset = env.register_stellar_asset_contract(sell_issuer);
    let buy_asset = env.register_stellar_asset_contract(buy_issuer);
    mint(&env, &sell_asset, &owner, 1_000);

    let trigger = register_standard_trigger(&client, &owner, &sell_asset, &buy_asset);
    client.cancel_trigger(&owner, &trigger.id);

    assert!(event_has_topic(&env, &symbol_short!("trig_cxl")));
}

#[test]
fn fund_pool_adds_liquidity_and_rejects_zero() {
    let (env, client, _, _) = setup();
    let funder = Address::generate(&env);
    let issuer = Address::generate(&env);
    let asset = env.register_stellar_asset_contract(issuer);
    mint(&env, &asset, &funder, 5_000);

    assert_eq!(client.get_pool_balance(&asset), 0);
    client.fund_pool(&funder, &asset, &5_000i128);
    assert_eq!(client.get_pool_balance(&asset), 5_000);
    assert_eq!(balance(&env, &asset, &funder), 0);

    let zero = client.try_fund_pool(&funder, &asset, &0i128);
    assert_eq!(zero, Err(Ok(ContractError::InvalidAmount)));
}
