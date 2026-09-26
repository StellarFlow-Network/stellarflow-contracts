#![no_std]

use soroban_sdk::{contract, contracterror, contractimpl, contracttype, token, Address, Env, Vec};

const HEARTBEAT_TIMEOUT: u64 = 24 * 60 * 60;

#[contracttype]
#[derive(Clone)]
pub struct Order {
    pub id: u64,
    pub maker: Address,
    pub token: Address,
    pub amount: i128,
}

#[contracttype]
#[derive(Clone)]
enum DataKey {
    NextOrderId,
    Order(u64),
    MakerOrders(Address),
    MakerHeartbeat(Address),
}

#[contracterror]
#[derive(Copy, Clone, PartialEq, Eq)]
#[repr(u32)]
pub enum ContractError {
    InvalidAmount = 1,
    OrderNotFound = 2,
    NotOrderMaker = 3,
    NoHeartbeat = 4,
    HeartbeatStillFresh = 5,
}

#[contract]
pub struct OrderBook;

#[contractimpl]
impl OrderBook {
    pub fn place_order(
        env: Env,
        maker: Address,
        token_address: Address,
        amount: i128,
    ) -> Result<u64, ContractError> {
        if amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }
        maker.require_auth();

        token::Client::new(&env, &token_address).transfer(
            &maker,
            &env.current_contract_address(),
            &amount,
        );

        let order_id = env
            .storage()
            .persistent()
            .get(&DataKey::NextOrderId)
            .unwrap_or(0u64);
        env.storage()
            .persistent()
            .set(&DataKey::NextOrderId, &(order_id + 1));
        env.storage().persistent().set(
            &DataKey::Order(order_id),
            &Order {
                id: order_id,
                maker: maker.clone(),
                token: token_address,
                amount,
            },
        );

        let mut orders = Self::maker_orders(&env, &maker);
        orders.push_back(order_id);
        env.storage()
            .persistent()
            .set(&DataKey::MakerOrders(maker.clone()), &orders);
        Self::record_heartbeat(&env, &maker);

        Ok(order_id)
    }

    pub fn heartbeat(env: Env, maker: Address) {
        maker.require_auth();
        Self::record_heartbeat(&env, &maker);
    }

    pub fn cancel_order(
        env: Env,
        maker: Address,
        order_id: u64,
    ) -> Result<(), ContractError> {
        maker.require_auth();
        Self::refund_order(&env, &maker, order_id)?;
        Self::remove_order_id(&env, &maker, order_id);
        Self::record_heartbeat(&env, &maker);
        Ok(())
    }

    pub fn cancel_stale_orders(env: Env, maker: Address) -> Result<u32, ContractError> {
        let heartbeat = env
            .storage()
            .persistent()
            .get::<DataKey, u64>(&DataKey::MakerHeartbeat(maker.clone()))
            .ok_or(ContractError::NoHeartbeat)?;
        if env.ledger().timestamp().saturating_sub(heartbeat) <= HEARTBEAT_TIMEOUT {
            return Err(ContractError::HeartbeatStillFresh);
        }

        let orders = Self::maker_orders(&env, &maker);
        let mut cancelled = 0u32;
        for order_id in orders.iter() {
            Self::refund_order(&env, &maker, order_id)?;
            cancelled += 1;
        }
        env.storage()
            .persistent()
            .remove(&DataKey::MakerOrders(maker));
        Ok(cancelled)
    }

    pub fn get_heartbeat(env: Env, maker: Address) -> Option<u64> {
        env.storage()
            .persistent()
            .get(&DataKey::MakerHeartbeat(maker))
    }

    pub fn get_order(env: Env, order_id: u64) -> Option<Order> {
        env.storage().persistent().get(&DataKey::Order(order_id))
    }

    fn maker_orders(env: &Env, maker: &Address) -> Vec<u64> {
        env.storage()
            .persistent()
            .get(&DataKey::MakerOrders(maker.clone()))
            .unwrap_or_else(|| Vec::new(env))
    }

    fn record_heartbeat(env: &Env, maker: &Address) {
        env.storage().persistent().set(
            &DataKey::MakerHeartbeat(maker.clone()),
            &env.ledger().timestamp(),
        );
    }

    fn remove_order_id(env: &Env, maker: &Address, order_id: u64) {
        let orders = Self::maker_orders(env, maker);
        let mut remaining = Vec::new(env);
        for id in orders.iter() {
            if id != order_id {
                remaining.push_back(id);
            }
        }
        env.storage()
            .persistent()
            .set(&DataKey::MakerOrders(maker.clone()), &remaining);
    }

    fn refund_order(env: &Env, maker: &Address, order_id: u64) -> Result<(), ContractError> {
        let order = env
            .storage()
            .persistent()
            .get::<DataKey, Order>(&DataKey::Order(order_id))
            .ok_or(ContractError::OrderNotFound)?;
        if order.maker != *maker {
            return Err(ContractError::NotOrderMaker);
        }

        token::Client::new(env, &order.token).transfer(
            &env.current_contract_address(),
            maker,
            &order.amount,
        );
        env.storage()
            .persistent()
            .remove(&DataKey::Order(order_id));
        Ok(())
    }
}

#[cfg(test)]
mod test;