//! On-chain limit order book matching logic (Issue #701).
//!
//! Makers post limit orders that lock their `sell_asset` in the contract
//! until a fill keeper matches them at (or better than) their `price_tick`.
//! Orders support partial fills — remaining balance state is tracked on the
//! order itself — and the maker can cancel a still-open order at any time to
//! recover whatever quantity has not yet been filled.

use soroban_sdk::{contracttype, token, Address, Env, Vec};

use crate::ContractError;

/// Fixed-point scale for `price_tick`: units of `buy_asset` per 1 unit of
/// `sell_asset`, scaled by 10^7 (matches the protocol's standard fixed-point
/// footprint used elsewhere in the contract, see `fees::FIXED_POINT_SCALE`).
pub const PRICE_SCALE: i128 = 10_000_000;
pub const PROTOCOL_FEE_BPS: i128 = 30;
const BPS_SCALE: i128 = 10_000;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetPair {
    pub sell_asset: Address,
    pub buy_asset: Address,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct LimitOrder {
    pub id: u64,
    pub maker: Address,
    pub pair: AssetPair,
    pub sell_asset: Address,
    pub buy_asset: Address,
    /// Price in `buy_asset` per unit of `sell_asset`, fixed-point at `PRICE_SCALE`.
    pub price_tick: i128,
    /// Remaining sell-asset collateral locked by this order.
    pub amount: i128,
    pub original_amount: i128,
    pub remaining_amount: i128,
    pub filled_amount: i128,
    pub created_at_ledger: u32,
    /// Expiry ledger sequence; zero means the order does not expire.
    pub expiry: u32,
    pub active: bool,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OrderSide {
    Sell,
    Buy,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct FillResult {
    pub order_id: u64,
    pub filled_amount: i128,
    pub paid_amount: i128,
    pub remaining_amount: i128,
    pub order_closed: bool,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct SettlementResult {
    pub seller_order_id: u64,
    pub buyer_order_id: u64,
    pub filled_amount: i128,
    pub quote_amount: i128,
    pub seller_net_amount: i128,
    pub buyer_net_amount: i128,
    pub quote_fee_amount: i128,
    pub base_fee_amount: i128,
    pub seller_order_closed: bool,
    pub buyer_order_closed: bool,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OrderStorageKey {
    NextOrderId,
    /// Index from the public order id to its canonical composite storage key.
    OrderIndex(u64),
    /// Order struct keyed by `(AssetPair, PriceTick, OrderID)`.
    Order(AssetPair, i128, u64),
    /// Resting-order index bucket keyed by `(AssetPair, PriceTick)`, listing
    /// the ids of every order posted at that exact tick for that pair — this
    /// is the `(AssetPair, PriceTick, OrderID)` addressing scheme fill
    /// keepers walk to find matchable liquidity.
    Bucket(AssetPair, i128),
    Balance(Address, Address),
}

fn next_order_id(env: &Env) -> u64 {
    let id: u64 = env
        .storage()
        .instance()
        .get(&OrderStorageKey::NextOrderId)
        .unwrap_or(0);
    env.storage().instance().set(&OrderStorageKey::NextOrderId, &(id + 1));
    id
}

fn credit_balance(env: &Env, owner: &Address, asset: &Address, amount: i128) -> Result<(), ContractError> {
    if amount == 0 {
        return Ok(());
    }
    let key = OrderStorageKey::Balance(owner.clone(), asset.clone());
    let current: i128 = env.storage().persistent().get(&key).unwrap_or(0);
    let updated = current.checked_add(amount).ok_or(ContractError::MathOverflow)?;
    env.storage().persistent().set(&key, &updated);
    env.storage()
        .persistent()
        .extend_ttl(&key, crate::storage::PERSISTENT_TTL_THRESHOLD, crate::storage::PERSISTENT_TTL_THRESHOLD);
    Ok(())
}

fn debit_balance(env: &Env, owner: &Address, asset: &Address, amount: i128) -> Result<(), ContractError> {
    let key = OrderStorageKey::Balance(owner.clone(), asset.clone());
    let current: i128 = env.storage().persistent().get(&key).unwrap_or(0);
    if amount > current {
        return Err(ContractError::BridgeInsufficientBalance);
    }
    let updated = current.checked_sub(amount).ok_or(ContractError::MathOverflow)?;
    if updated == 0 {
        env.storage().persistent().remove(&key);
    } else {
        env.storage().persistent().set(&key, &updated);
    }
    Ok(())
}

fn quote_amount(base_amount: i128, price_tick: i128) -> Result<i128, ContractError> {
    base_amount
        .checked_mul(price_tick)
        .ok_or(ContractError::MathOverflow)?
        .checked_div(PRICE_SCALE)
        .ok_or(ContractError::DivisionByZero)
}

fn protocol_fee(amount: i128) -> Result<i128, ContractError> {
    amount
        .checked_mul(PROTOCOL_FEE_BPS)
        .ok_or(ContractError::MathOverflow)?
        .checked_div(BPS_SCALE)
        .ok_or(ContractError::DivisionByZero)
}

fn load_order(env: &Env, order_id: u64) -> Result<LimitOrder, ContractError> {
    let index: (AssetPair, i128) = env
        .storage()
        .persistent()
        .get(&OrderStorageKey::OrderIndex(order_id))
        .ok_or(ContractError::OrderNotFound)?;
    env.storage()
        .persistent()
        .get(&OrderStorageKey::Order(index.0, index.1, order_id))
        .ok_or(ContractError::OrderNotFound)
}

fn save_order(env: &Env, order: &LimitOrder) {
    let key = OrderStorageKey::Order(
        order.pair.clone(),
        order.price_tick,
        order.id,
    );
    env.storage().persistent().set(&key, order);
    env.storage().persistent().extend_ttl(
        &key,
        crate::storage::PERSISTENT_TTL_THRESHOLD,
        crate::storage::PERSISTENT_TTL_THRESHOLD,
    );
    let index_key = OrderStorageKey::OrderIndex(order.id);
    env.storage()
        .persistent()
        .set(&index_key, &(order.pair.clone(), order.price_tick));
    env.storage().persistent().extend_ttl(
        &index_key,
        crate::storage::PERSISTENT_TTL_THRESHOLD,
        crate::storage::PERSISTENT_TTL_THRESHOLD,
    );
}

fn bucket_push(env: &Env, pair: &AssetPair, price_tick: i128, order_id: u64) {
    let key = OrderStorageKey::Bucket(pair.clone(), price_tick);
    let mut bucket: Vec<u64> = env.storage().persistent().get(&key).unwrap_or_else(|| Vec::new(env));
    bucket.push_back(order_id);
    env.storage().persistent().set(&key, &bucket);
    env.storage()
        .persistent()
        .extend_ttl(&key, crate::storage::PERSISTENT_TTL_THRESHOLD, crate::storage::PERSISTENT_TTL_THRESHOLD);
}

fn bucket_remove(env: &Env, pair: &AssetPair, price_tick: i128, order_id: u64) {
    let key = OrderStorageKey::Bucket(pair.clone(), price_tick);
    if let Some(bucket) = env.storage().persistent().get::<_, Vec<u64>>(&key) {
        let mut updated: Vec<u64> = Vec::new(env);
        for existing in bucket.iter() {
            if existing != order_id {
                updated.push_back(existing);
            }
        }
        if updated.is_empty() {
            env.storage().persistent().remove(&key);
        } else {
            env.storage().persistent().set(&key, &updated);
        }
    }
}

/// Post a new limit order: locks `sell_amount` of `pair.sell_asset` from
/// `maker` into the contract until filled or cancelled.
pub fn place_order(
    env: &Env,
    maker: Address,
    pair: AssetPair,
    price_tick: i128,
    sell_amount: i128,
) -> Result<LimitOrder, ContractError> {
    place_order_with_expiry(env, maker, pair, price_tick, sell_amount, 0)
}

/// Post a limit order with an optional ledger-sequence expiry.
pub fn place_order_with_expiry(
    env: &Env,
    maker: Address,
    pair: AssetPair,
    price_tick: i128,
    sell_amount: i128,
    expiry: u32,
) -> Result<LimitOrder, ContractError> {
    if sell_amount <= 0 {
        return Err(ContractError::OrderZeroAmount);
    }
    if price_tick <= 0 {
        return Err(ContractError::OrderInvalidPrice);
    }
    maker.require_auth();

    let token_client = token::Client::new(env, &pair.sell_asset);
    token_client.transfer(&maker, &env.current_contract_address(), &sell_amount);

    let order = LimitOrder {
        id: next_order_id(env),
        maker,
        pair: pair.clone(),
        sell_asset: pair.sell_asset.clone(),
        buy_asset: pair.buy_asset.clone(),
        price_tick,
        amount: sell_amount,
        original_amount: sell_amount,
        remaining_amount: sell_amount,
        filled_amount: 0,
        created_at_ledger: env.ledger().sequence(),
        expiry,
        active: true,
    };

    save_order(env, &order);
    bucket_push(env, &pair, price_tick, order.id);

    let is_bid = pair.sell_asset > pair.buy_asset;
    add_tick_liquidity(env, &pair, price_tick, sell_amount, is_bid);

    Ok(order)
}

/// Post a buy-side limit order: locks the maximum `buy_asset` spend required
/// for `buy_amount` units of `sell_asset` at `price_tick`.
pub fn place_buy_order(
    env: &Env,
    maker: Address,
    pair: AssetPair,
    price_tick: i128,
    buy_amount: i128,
) -> Result<LimitOrder, ContractError> {
    if buy_amount <= 0 {
        return Err(ContractError::OrderZeroAmount);
    }
    if price_tick <= 0 {
        return Err(ContractError::OrderInvalidPrice);
    }
    maker.require_auth();

    let locked_quote = quote_amount(buy_amount, price_tick)?;
    let token_client = token::Client::new(env, &pair.buy_asset);
    token_client.transfer(&maker, &env.current_contract_address(), &locked_quote);

    let order = LimitOrder {
        id: next_order_id(env),
        maker,
        pair: pair.clone(),
        side: OrderSide::Buy,
        price_tick,
        original_amount: buy_amount,
        remaining_amount: buy_amount,
        filled_amount: 0,
        created_at_ledger: env.ledger().sequence(),
        active: true,
    };

    save_order(env, &order);
    bucket_push(env, &pair, price_tick, order.id);

    Ok(order)
}

/// Fill up to `fill_amount` of `order_id`'s remaining balance. The filler
/// pays the maker `fill_amount * price_tick` of `pair.buy_asset` and
/// receives `fill_amount` of `pair.sell_asset` released from escrow.
/// Partial fills leave the order open with a reduced `remaining_amount`.
pub fn fill_order(env: &Env, filler: Address, order_id: u64, fill_amount: i128) -> Result<FillResult, ContractError> {
    if fill_amount <= 0 {
        return Err(ContractError::OrderZeroAmount);
    }
    filler.require_auth();

    let mut order = load_order(env, order_id)?;
    if !order.active {
        return Err(ContractError::OrderAlreadyClosed);
    }
    if order.expiry != 0 && env.ledger().sequence() > order.expiry {
        return Err(ContractError::OrderAlreadyClosed);
    }
    if fill_amount > order.remaining_amount {
        return Err(ContractError::OrderInsufficientRemaining);
    }

    let paid_amount = fill_amount
        .checked_mul(order.price_tick)
        .ok_or(ContractError::MathOverflow)?
        .checked_div(PRICE_SCALE)
        .ok_or(ContractError::DivisionByZero)?;

    let buy_client = token::Client::new(env, &order.pair.buy_asset);
    buy_client.transfer(&filler, &order.maker, &paid_amount);

    let sell_client = token::Client::new(env, &order.pair.sell_asset);
    sell_client.transfer(&env.current_contract_address(), &filler, &fill_amount);

    order.remaining_amount = order
        .remaining_amount
        .checked_sub(fill_amount)
        .ok_or(ContractError::MathOverflow)?;
    order.amount = order.remaining_amount;
    order.filled_amount = order
        .filled_amount
        .checked_add(fill_amount)
        .ok_or(ContractError::MathOverflow)?;

    let order_closed = order.remaining_amount == 0;
    if order_closed {
        order.active = false;
        bucket_remove(env, &order.pair, order.price_tick, order.id);
    }
    save_order(env, &order);

    let is_bid = order.pair.sell_asset > order.pair.buy_asset;
    remove_tick_liquidity(env, &order.pair, order.price_tick, fill_amount, is_bid);

    env.events().publish(
        (soroban_sdk::symbol_short!("ord_fill"), order.id),
        (filler, fill_amount, paid_amount, order.remaining_amount),
    );

    Ok(FillResult {
        order_id: order.id,
        filled_amount: fill_amount,
        paid_amount,
        remaining_amount: order.remaining_amount,
        order_closed,
    })
}

/// Atomically settle a seller order against a buyer order when the buyer's
/// limit price crosses the seller's tick. Filled proceeds are credited into
/// per-user contract balances after protocol fees are skimmed to treasury.
pub fn match_orders(
    env: &Env,
    seller_order_id: u64,
    buyer_order_id: u64,
    fill_amount: i128,
) -> Result<SettlementResult, ContractError> {
    if fill_amount <= 0 {
        return Err(ContractError::OrderZeroAmount);
    }

    let mut seller = load_order(env, seller_order_id)?;
    let mut buyer = load_order(env, buyer_order_id)?;
    if !seller.active || !buyer.active {
        return Err(ContractError::OrderAlreadyClosed);
    }
    if seller.side != OrderSide::Sell || buyer.side != OrderSide::Buy {
        return Err(ContractError::OrderSideMismatch);
    }
    if seller.pair != buyer.pair {
        return Err(ContractError::OrderPairMismatch);
    }
    if buyer.price_tick < seller.price_tick {
        return Err(ContractError::OrderPriceNotCrossed);
    }
    if fill_amount > seller.remaining_amount || fill_amount > buyer.remaining_amount {
        return Err(ContractError::OrderInsufficientRemaining);
    }

    let quote_paid = quote_amount(fill_amount, seller.price_tick)?;
    let buyer_locked_quote = quote_amount(fill_amount, buyer.price_tick)?;
    let price_improvement = buyer_locked_quote
        .checked_sub(quote_paid)
        .ok_or(ContractError::MathOverflow)?;

    let quote_fee = protocol_fee(quote_paid)?;
    let base_fee = protocol_fee(fill_amount)?;
    let seller_net = quote_paid.checked_sub(quote_fee).ok_or(ContractError::MathOverflow)?;
    let buyer_net = fill_amount.checked_sub(base_fee).ok_or(ContractError::MathOverflow)?;

    seller.remaining_amount = seller
        .remaining_amount
        .checked_sub(fill_amount)
        .ok_or(ContractError::MathOverflow)?;
    buyer.remaining_amount = buyer
        .remaining_amount
        .checked_sub(fill_amount)
        .ok_or(ContractError::MathOverflow)?;
    seller.filled_amount = seller
        .filled_amount
        .checked_add(fill_amount)
        .ok_or(ContractError::MathOverflow)?;
    buyer.filled_amount = buyer
        .filled_amount
        .checked_add(fill_amount)
        .ok_or(ContractError::MathOverflow)?;

    let seller_closed = seller.remaining_amount == 0;
    let buyer_closed = buyer.remaining_amount == 0;
    if seller_closed {
        seller.active = false;
        bucket_remove(env, &seller.pair, seller.price_tick, seller.id);
    }
    if buyer_closed {
        buyer.active = false;
        bucket_remove(env, &buyer.pair, buyer.price_tick, buyer.id);
    }

    credit_balance(env, &seller.maker, &seller.pair.buy_asset, seller_net)?;
    credit_balance(env, &buyer.maker, &buyer.pair.sell_asset, buyer_net)?;
    credit_balance(env, &buyer.maker, &buyer.pair.buy_asset, price_improvement)?;

    if let Some(treasury) = env.storage().instance().get::<_, Address>(&crate::TREASURY_KEY) {
        credit_balance(env, &treasury, &seller.pair.buy_asset, quote_fee)?;
        credit_balance(env, &treasury, &seller.pair.sell_asset, base_fee)?;
    }

    save_order(env, &seller);
    save_order(env, &buyer);

    env.events().publish(
        (soroban_sdk::symbol_short!("ord_mtch"), seller.id, buyer.id),
        (fill_amount, quote_paid, seller_net, buyer_net, quote_fee, base_fee),
    );

    Ok(SettlementResult {
        seller_order_id: seller.id,
        buyer_order_id: buyer.id,
        filled_amount: fill_amount,
        quote_amount: quote_paid,
        seller_net_amount: seller_net,
        buyer_net_amount: buyer_net,
        quote_fee_amount: quote_fee,
        base_fee_amount: base_fee,
        seller_order_closed: seller_closed,
        buyer_order_closed: buyer_closed,
    })
}

/// Cancel a still-open order and return its unfilled balance to the maker.
/// Callable by the maker at any time — no expiry or keeper approval needed.
pub fn cancel_order(env: &Env, maker: Address, order_id: u64) -> Result<i128, ContractError> {
    maker.require_auth();

    let mut order = load_order(env, order_id)?;
    if order.maker != maker {
        return Err(ContractError::OrderNotMaker);
    }
    if !order.active {
        return Err(ContractError::OrderAlreadyClosed);
    }

    let recovered = if order.side == OrderSide::Buy {
        quote_amount(order.remaining_amount, order.price_tick)?
    } else {
        order.remaining_amount
    };
    order.remaining_amount = 0;
    order.amount = 0;
    order.active = false;
    bucket_remove(env, &order.pair, order.price_tick, order.id);
    save_order(env, &order);

    let is_bid = order.pair.sell_asset > order.pair.buy_asset;
    remove_tick_liquidity(env, &order.pair, order.price_tick, recovered, is_bid);

    if recovered > 0 {
        let asset = if order.side == OrderSide::Buy {
            order.pair.buy_asset.clone()
        } else {
            order.pair.sell_asset.clone()
        };
        let token_client = token::Client::new(env, &asset);
        token_client.transfer(&env.current_contract_address(), &maker, &recovered);
    }

    Ok(recovered)
}

pub fn get_balance(env: &Env, owner: Address, asset: Address) -> i128 {
    env.storage()
        .persistent()
        .get(&OrderStorageKey::Balance(owner, asset))
        .unwrap_or(0)
}

pub fn withdraw_balance(env: &Env, owner: Address, asset: Address, amount: i128) -> Result<i128, ContractError> {
    if amount <= 0 {
        return Err(ContractError::OrderZeroAmount);
    }
    owner.require_auth();
    debit_balance(env, &owner, &asset, amount)?;
    let token_client = token::Client::new(env, &asset);
    token_client.transfer(&env.current_contract_address(), &owner, &amount);
    Ok(amount)
}

pub fn get_order(env: &Env, order_id: u64) -> Option<LimitOrder> {
    load_order(env, order_id).ok()
}

/// List the ids of every order currently resting at `(pair, price_tick)`.
pub fn get_orders_at_tick(env: &Env, pair: AssetPair, price_tick: i128) -> Vec<u64> {
    env.storage()
        .persistent()
        .get(&OrderStorageKey::Bucket(pair, price_tick))
        .unwrap_or_else(|| Vec::new(env))
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LiquidityStorageKey {
    ActiveTicks(AssetPair, bool),
    TickVolume(AssetPair, i128, bool),
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct LiquidityLevel {
    pub price_tick: i128,
    pub volume: i128,
}

pub fn add_tick_liquidity(env: &Env, pair: &AssetPair, price_tick: i128, amount: i128, is_bid: bool) {
    if amount <= 0 {
        return;
    }
    let vol_key = LiquidityStorageKey::TickVolume(pair.clone(), price_tick, is_bid);
    let current_vol: i128 = env.storage().persistent().get(&vol_key).unwrap_or(0);
    let new_vol = current_vol + amount;
    env.storage().persistent().set(&vol_key, &new_vol);
    env.storage().persistent().extend_ttl(&vol_key, crate::storage::PERSISTENT_TTL_THRESHOLD, crate::storage::PERSISTENT_TTL_THRESHOLD);

    if current_vol == 0 {
        let ticks_key = LiquidityStorageKey::ActiveTicks(pair.clone(), is_bid);
        let mut ticks: Vec<i128> = env.storage().persistent().get(&ticks_key).unwrap_or_else(|| Vec::new(env));
        ticks.push_back(price_tick);
        
        let mut temp_rust_vec = soroban_sdk::vec![env];
        for t in ticks.iter() {
            temp_rust_vec.push_back(t);
        }
        
        let len = temp_rust_vec.len();
        for i in 1..len {
            let key_val = temp_rust_vec.get(i).unwrap();
            let mut j = i;
            if is_bid {
                while j > 0 && temp_rust_vec.get(j - 1).unwrap() < key_val {
                    temp_rust_vec.set(j, temp_rust_vec.get(j - 1).unwrap());
                    j -= 1;
                }
            } else {
                while j > 0 && temp_rust_vec.get(j - 1).unwrap() > key_val {
                    temp_rust_vec.set(j, temp_rust_vec.get(j - 1).unwrap());
                    j -= 1;
                }
            }
            temp_rust_vec.set(j, key_val);
        }

        let mut sorted_ticks = Vec::new(env);
        for t in temp_rust_vec.iter() {
            sorted_ticks.push_back(t);
        }
        env.storage().persistent().set(&ticks_key, &sorted_ticks);
        env.storage().persistent().extend_ttl(&ticks_key, crate::storage::PERSISTENT_TTL_THRESHOLD, crate::storage::PERSISTENT_TTL_THRESHOLD);
    }
}

pub fn remove_tick_liquidity(env: &Env, pair: &AssetPair, price_tick: i128, amount: i128, is_bid: bool) {
    if amount <= 0 {
        return;
    }
    let vol_key = LiquidityStorageKey::TickVolume(pair.clone(), price_tick, is_bid);
    let current_vol: i128 = env.storage().persistent().get(&vol_key).unwrap_or(0);
    let new_vol = if current_vol <= amount { 0 } else { current_vol - amount };
    
    if new_vol == 0 {
        env.storage().persistent().remove(&vol_key);
        let ticks_key = LiquidityStorageKey::ActiveTicks(pair.clone(), is_bid);
        if let Some(ticks) = env.storage().persistent().get::<_, Vec<i128>>(&ticks_key) {
            let mut updated = Vec::new(env);
            for t in ticks.iter() {
                if t != price_tick {
                    updated.push_back(t);
                }
            }
            if updated.is_empty() {
                env.storage().persistent().remove(&ticks_key);
            } else {
                env.storage().persistent().set(&ticks_key, &updated);
                env.storage().persistent().extend_ttl(&ticks_key, crate::storage::PERSISTENT_TTL_THRESHOLD, crate::storage::PERSISTENT_TTL_THRESHOLD);
            }
        }
    } else {
        env.storage().persistent().set(&vol_key, &new_vol);
        env.storage().persistent().extend_ttl(&vol_key, crate::storage::PERSISTENT_TTL_THRESHOLD, crate::storage::PERSISTENT_TTL_THRESHOLD);
    }
}

pub fn get_liquidity_depth(env: &Env, pair: AssetPair, is_bid: bool) -> Vec<LiquidityLevel> {
    let ticks_key = LiquidityStorageKey::ActiveTicks(pair.clone(), is_bid);
    let ticks: Vec<i128> = env.storage().persistent().get(&ticks_key).unwrap_or_else(|| Vec::new(env));
    let mut levels = Vec::new(env);
    
    let count = if ticks.len() > 20 { 20 } else { ticks.len() };
    for i in 0..count {
        let price_tick = ticks.get(i).unwrap();
        let vol_key = LiquidityStorageKey::TickVolume(pair.clone(), price_tick, is_bid);
        if let Some(volume) = env.storage().persistent().get::<_, i128>(&vol_key) {
            levels.push_back(LiquidityLevel { price_tick, volume });
        }
    }
    levels
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    fn setup() -> (Env, crate::TimeLockedUpgradeContractClient<'static>, Address, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let client = crate::TimeLockedUpgradeContractClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        client.initialize(&admin, &treasury);
        let sell_issuer = Address::generate(&env);
        let buy_issuer = Address::generate(&env);
        let sell_asset = env.register_stellar_asset_contract(sell_issuer);
        let buy_asset = env.register_stellar_asset_contract(buy_issuer);
        (env, client, sell_asset, buy_asset, treasury)
    }

    fn mint(env: &Env, asset: &Address, to: &Address, amount: i128) {
        soroban_sdk::token::StellarAssetClient::new(env, asset).mint(to, &amount);
    }

    #[test]
    fn place_order_locks_sell_asset() {
        let (env, client, sell_asset, buy_asset, _) = setup();
        let maker = Address::generate(&env);
        mint(&env, &sell_asset, &maker, 1_000);
        let pair = AssetPair { sell_asset: sell_asset.clone(), buy_asset };
        let order = client.place_limit_order(&maker, &pair, &(2 * PRICE_SCALE), &1_000);
        assert_eq!(order.remaining_amount, 1_000);
        let token_client = soroban_sdk::token::Client::new(&env, &sell_asset);
        assert_eq!(token_client.balance(&maker), 0);
    }

    #[test]
    fn partial_fill_keeps_order_open_with_reduced_remainder() {
        let (env, client, sell_asset, buy_asset, _) = setup();
        let maker = Address::generate(&env);
        let filler = Address::generate(&env);
        mint(&env, &sell_asset, &maker, 1_000);
        mint(&env, &buy_asset, &filler, 10_000);
        let pair = AssetPair { sell_asset: sell_asset.clone(), buy_asset: buy_asset.clone() };
        let order = client.place_limit_order(&maker, &pair, &(2 * PRICE_SCALE), &1_000);

        let result = client.fill_limit_order(&filler, &order.id, &400);
        assert_eq!(result.remaining_amount, 600);
        assert!(!result.order_closed);
        assert_eq!(result.paid_amount, 800); // 400 * 2

        let sell_client = soroban_sdk::token::Client::new(&env, &sell_asset);
        assert_eq!(sell_client.balance(&filler), 400);
        let buy_client = soroban_sdk::token::Client::new(&env, &buy_asset);
        assert_eq!(buy_client.balance(&maker), 800);
    }

    #[test]
    fn full_fill_closes_order_and_clears_book_bucket() {
        let (env, client, sell_asset, buy_asset, _) = setup();
        let maker = Address::generate(&env);
        let filler = Address::generate(&env);
        mint(&env, &sell_asset, &maker, 500);
        mint(&env, &buy_asset, &filler, 5_000);
        let pair = AssetPair { sell_asset, buy_asset };
        let order = client.place_limit_order(&maker, &pair, &PRICE_SCALE, &500);

        let result = client.fill_limit_order(&filler, &order.id, &500);
        assert!(result.order_closed);
        assert_eq!(client.get_orders_at_tick(&pair, &PRICE_SCALE).len(), 0);
    }

    #[test]
    fn maker_can_cancel_and_recover_locked_assets() {
        let (env, client, sell_asset, buy_asset, _) = setup();
        let maker = Address::generate(&env);
        mint(&env, &sell_asset, &maker, 1_000);
        let pair = AssetPair { sell_asset: sell_asset.clone(), buy_asset };
        let order = client.place_limit_order(&maker, &pair, &PRICE_SCALE, &1_000);

        let recovered = client.cancel_limit_order(&maker, &order.id);
        assert_eq!(recovered, 1_000);
        let sell_client = soroban_sdk::token::Client::new(&env, &sell_asset);
        assert_eq!(sell_client.balance(&maker), 1_000);
    }

    #[test]
    fn non_maker_cannot_cancel_order() {
        let (env, client, sell_asset, buy_asset, _) = setup();
        let maker = Address::generate(&env);
        let attacker = Address::generate(&env);
        mint(&env, &sell_asset, &maker, 1_000);
        let pair = AssetPair { sell_asset, buy_asset };
        let order = client.place_limit_order(&maker, &pair, &PRICE_SCALE, &1_000);

        let result = client.try_cancel_limit_order(&attacker, &order.id);
        assert_eq!(result, Err(Ok(ContractError::OrderNotMaker)));
    }

    #[test]
    fn fill_exceeding_remaining_amount_fails() {
        let (env, client, sell_asset, buy_asset, _) = setup();
        let maker = Address::generate(&env);
        let filler = Address::generate(&env);
        mint(&env, &sell_asset, &maker, 100);
        mint(&env, &buy_asset, &filler, 10_000);
        let pair = AssetPair { sell_asset, buy_asset };
        let order = client.place_limit_order(&maker, &pair, &PRICE_SCALE, &100);

        let result = client.try_fill_limit_order(&filler, &order.id, &200);
        assert_eq!(result, Err(Ok(ContractError::OrderInsufficientRemaining)));
    }

    #[test]
    fn cancelled_order_cannot_be_filled() {
        let (env, client, sell_asset, buy_asset, _) = setup();
        let maker = Address::generate(&env);
        let filler = Address::generate(&env);
        mint(&env, &sell_asset, &maker, 100);
        mint(&env, &buy_asset, &filler, 1_000);
        let pair = AssetPair { sell_asset, buy_asset };
        let order = client.place_limit_order(&maker, &pair, &PRICE_SCALE, &100);
        client.cancel_limit_order(&maker, &order.id);

        let result = client.try_fill_limit_order(&filler, &order.id, &10);
        assert_eq!(result, Err(Ok(ContractError::OrderAlreadyClosed)));
    }

    #[test]
    fn crossed_buy_and_sell_orders_settle_to_balances_with_protocol_fees() {
        let (env, client, sell_asset, buy_asset, treasury) = setup();
        let seller = Address::generate(&env);
        let buyer = Address::generate(&env);
        mint(&env, &sell_asset, &seller, 1_000);
        mint(&env, &buy_asset, &buyer, 3_000);

        let pair = AssetPair { sell_asset: sell_asset.clone(), buy_asset: buy_asset.clone() };
        let sell_order = client.place_limit_order(&seller, &pair, &(2 * PRICE_SCALE), &1_000);
        let buy_order = client.place_buy_limit_order(&buyer, &pair, &(3 * PRICE_SCALE), &1_000);

        let quote_client = soroban_sdk::token::Client::new(&env, &buy_asset);
        assert_eq!(quote_client.balance(&buyer), 0);

        let result = client.match_limit_orders(&sell_order.id, &buy_order.id, &1_000);
        assert!(result.seller_order_closed);
        assert!(result.buyer_order_closed);
        assert_eq!(result.quote_amount, 2_000);
        assert_eq!(result.quote_fee_amount, 6);
        assert_eq!(result.base_fee_amount, 3);

        assert_eq!(client.get_order_balance(&seller, &buy_asset), 1_994);
        assert_eq!(client.get_order_balance(&buyer, &sell_asset), 997);
        assert_eq!(client.get_order_balance(&buyer, &buy_asset), 1_000);
        assert_eq!(client.get_order_balance(&treasury, &buy_asset), 6);
        assert_eq!(client.get_order_balance(&treasury, &sell_asset), 3);

        client.withdraw_order_balance(&seller, &buy_asset, &1_994);
        assert_eq!(quote_client.balance(&seller), 1_994);
    }

    #[test]
    fn non_crossed_orders_do_not_settle() {
        let (env, client, sell_asset, buy_asset, _) = setup();
        let seller = Address::generate(&env);
        let buyer = Address::generate(&env);
        mint(&env, &sell_asset, &seller, 100);
        mint(&env, &buy_asset, &buyer, 100);
        let pair = AssetPair { sell_asset, buy_asset };
        let sell_order = client.place_limit_order(&seller, &pair, &(2 * PRICE_SCALE), &100);
        let buy_order = client.place_buy_limit_order(&buyer, &pair, &PRICE_SCALE, &100);

        let result = client.try_match_limit_orders(&sell_order.id, &buy_order.id, &100);
        assert_eq!(result, Err(Ok(ContractError::OrderPriceNotCrossed)));
    }
}
