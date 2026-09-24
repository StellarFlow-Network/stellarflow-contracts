#![no_std]

//! # Split Remittance Escrow — Cross-Border Multi-Anchor State Machine
//!
//! Escrows a single remittance (`E_total`) and splits it across multiple
//! payout destination anchors. Each anchor is assigned a proportional
//! partial release:
//!
//! ```text
//! E_partial = E_total × p_anchor / 10_000
//! ```
//!
//! where `p_anchor` is the anchor's share in basis points.
//!
//! Partial releases and remaining locked funds are tracked in **instance**
//! storage so indexers / callers can read the live split state cheaply.
//!
//! If any destination anchor fails to settle within **12 hours**, the
//! sender may refund the remaining locked (unsettled) funds.

mod types;

pub use types::{DataKey, DestinationLeg, LegStatus, OrderStatus, SplitOrder};

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, token, Address, Env, Vec,
};

/// Settlement window: anchors must settle within 12 hours of order creation.
pub const SETTLE_WINDOW_SECS: u64 = 43_200;

/// Basis-point denominator (100% = 10_000).
pub const BPS_DENOM: u32 = 10_000;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    NotInitialized = 1,
    AlreadyInitialized = 2,
    Unauthorized = 3,
    ZeroAmount = 4,
    OrderNotFound = 5,
    AlreadyResolved = 6,
    TooEarlyToRefund = 7,
    ArithmeticOverflow = 8,
    InvalidProportions = 9,
    NoDestinations = 10,
    LegNotFound = 11,
    LegAlreadySettled = 12,
    NothingToRefund = 13,
}

#[contract]
pub struct SplitRemittanceEscrow;

#[contracttype]
pub struct ContractInitializedEvent {
    pub admin: Address,
    pub token: Address,
}

#[contracttype]
pub struct SplitOrderCreatedEvent {
    pub id: u64,
    pub sender: Address,
    pub total_amount: i128,
    pub deadline: u64,
    pub leg_count: u32,
}

#[contracttype]
pub struct PartialReleasedEvent {
    pub id: u64,
    pub anchor: Address,
    pub amount: i128,
    pub partial_released_total: i128,
    pub remaining_locked: i128,
}

#[contracttype]
pub struct UnsettledRefundedEvent {
    pub id: u64,
    pub sender: Address,
    pub refunded_amount: i128,
}

fn require_initialized(env: &Env) -> Result<(), Error> {
    if !env.storage().instance().has(&DataKey::Initialized) {
        return Err(Error::NotInitialized);
    }
    Ok(())
}

fn get_token(env: &Env) -> Result<Address, Error> {
    env.storage()
        .instance()
        .get(&DataKey::Token)
        .ok_or(Error::NotInitialized)
}

fn get_order(env: &Env, id: u64) -> Result<SplitOrder, Error> {
    env.storage()
        .persistent()
        .get(&DataKey::Order(id))
        .ok_or(Error::OrderNotFound)
}

fn set_order(env: &Env, order: &SplitOrder) {
    env.storage()
        .persistent()
        .set(&DataKey::Order(order.id), order);
}

fn set_instance_balances(env: &Env, id: u64, partial_released: i128, remaining_locked: i128) {
    env.storage()
        .instance()
        .set(&DataKey::PartialReleased(id), &partial_released);
    env.storage()
        .instance()
        .set(&DataKey::RemainingLocked(id), &remaining_locked);
}

fn get_partial_released(env: &Env, id: u64) -> i128 {
    env.storage()
        .instance()
        .get(&DataKey::PartialReleased(id))
        .unwrap_or(0)
}

fn get_remaining_locked(env: &Env, id: u64) -> i128 {
    env.storage()
        .instance()
        .get(&DataKey::RemainingLocked(id))
        .unwrap_or(0)
}

fn checked_add(a: i128, b: i128) -> Result<i128, Error> {
    a.checked_add(b).ok_or(Error::ArithmeticOverflow)
}

fn checked_sub(a: i128, b: i128) -> Result<i128, Error> {
    a.checked_sub(b).ok_or(Error::ArithmeticOverflow)
}

fn checked_mul_div(amount: i128, bps: u32) -> Result<i128, Error> {
    let numer = amount
        .checked_mul(bps as i128)
        .ok_or(Error::ArithmeticOverflow)?;
    Ok(numer / (BPS_DENOM as i128))
}

/// Build destination legs from (anchor, proportion_bps) pairs.
/// Proportions must sum to exactly 10_000. Remainder from integer division
/// is assigned to the last leg so Σ E_partial == E_total.
fn build_legs(
    env: &Env,
    total: i128,
    destinations: &Vec<(Address, u32)>,
) -> Result<Vec<DestinationLeg>, Error> {
    let n = destinations.len();
    if n == 0 {
        return Err(Error::NoDestinations);
    }

    let mut sum_bps: u32 = 0;
    let mut allocated: i128 = 0;
    let mut legs: Vec<DestinationLeg> = Vec::new(env);

    for i in 0..n {
        let (anchor, bps) = destinations.get(i).unwrap();
        if bps == 0 || bps > BPS_DENOM {
            return Err(Error::InvalidProportions);
        }
        sum_bps = sum_bps
            .checked_add(bps)
            .ok_or(Error::ArithmeticOverflow)?;

        let amount = if i == n - 1 {
            // Last leg absorbs rounding remainder.
            checked_sub(total, allocated)?
        } else {
            let partial = checked_mul_div(total, bps)?;
            allocated = checked_add(allocated, partial)?;
            partial
        };

        if amount <= 0 {
            return Err(Error::ZeroAmount);
        }

        legs.push_back(DestinationLeg {
            anchor,
            proportion_bps: bps,
            amount,
            status: LegStatus::Pending,
        });
    }

    if sum_bps != BPS_DENOM {
        return Err(Error::InvalidProportions);
    }

    Ok(legs)
}

#[contractimpl]
impl SplitRemittanceEscrow {
    /// Initialize once with admin + SEP-41/SAC token used for escrow.
    pub fn initialize(env: Env, admin: Address, token: Address) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Initialized) {
            return Err(Error::AlreadyInitialized);
        }

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage().instance().set(&DataKey::NextOrderId, &0u64);
        env.storage().instance().set(&DataKey::Initialized, &true);

        env.events().publish(
            (symbol_short!("cinit"),),
            ContractInitializedEvent { admin, token },
        );
        Ok(())
    }

    /// Create a split remittance: lock `E_total` and allocate `E_partial`
    /// legs across `destinations` (anchor, proportion_bps). Proportions must
    /// sum to 10_000. Settlement deadline is now + 12 hours.
    pub fn create_split_order(
        env: Env,
        sender: Address,
        total_amount: i128,
        destinations: Vec<(Address, u32)>,
    ) -> Result<u64, Error> {
        require_initialized(&env)?;
        sender.require_auth();

        if total_amount <= 0 {
            return Err(Error::ZeroAmount);
        }

        let legs = build_legs(&env, total_amount, &destinations)?;
        let now = env.ledger().timestamp();
        let deadline = now
            .checked_add(SETTLE_WINDOW_SECS)
            .ok_or(Error::ArithmeticOverflow)?;

        let token_client = token::Client::new(&env, &get_token(&env)?);
        token_client.transfer(&sender, &env.current_contract_address(), &total_amount);

        let id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextOrderId)
            .unwrap_or(0);
        let next_id = id.checked_add(1).ok_or(Error::ArithmeticOverflow)?;
        env.storage()
            .instance()
            .set(&DataKey::NextOrderId, &next_id);

        let order = SplitOrder {
            id,
            sender: sender.clone(),
            total_amount,
            created_at: now,
            deadline,
            status: OrderStatus::Open,
            legs: legs.clone(),
        };
        set_order(&env, &order);

        // Instance-state tracking: E_partial released = 0, remaining = E_total.
        set_instance_balances(&env, id, 0, total_amount);

        env.events().publish(
            (symbol_short!("spltord"),),
            SplitOrderCreatedEvent {
                id,
                sender,
                total_amount,
                deadline,
                leg_count: legs.len(),
            },
        );

        Ok(id)
    }

    /// Destination anchor settles its leg: releases `E_partial` to the
    /// anchor and updates instance partial / remaining balances.
    pub fn settle_leg(env: Env, anchor: Address, order_id: u64) -> Result<(), Error> {
        require_initialized(&env)?;
        anchor.require_auth();

        let mut order = get_order(&env, order_id)?;
        if order.status != OrderStatus::Open {
            return Err(Error::AlreadyResolved);
        }

        let n = order.legs.len();
        let mut found = false;
        let mut release_amount: i128 = 0;
        let mut updated_legs: Vec<DestinationLeg> = Vec::new(&env);

        for i in 0..n {
            let mut leg = order.legs.get(i).unwrap();
            if leg.anchor == anchor {
                if leg.status != LegStatus::Pending {
                    return Err(Error::LegAlreadySettled);
                }
                release_amount = leg.amount;
                leg.status = LegStatus::Settled;
                found = true;
            }
            updated_legs.push_back(leg);
        }

        if !found {
            return Err(Error::LegNotFound);
        }

        order.legs = updated_legs;

        let token_client = token::Client::new(&env, &get_token(&env)?);
        token_client.transfer(
            &env.current_contract_address(),
            &anchor,
            &release_amount,
        );

        let partial = checked_add(get_partial_released(&env, order_id), release_amount)?;
        let remaining = checked_sub(get_remaining_locked(&env, order_id), release_amount)?;
        set_instance_balances(&env, order_id, partial, remaining);

        // If every leg is settled, mark order Completed.
        let mut all_settled = true;
        for i in 0..order.legs.len() {
            if order.legs.get(i).unwrap().status != LegStatus::Settled {
                all_settled = false;
                break;
            }
        }
        if all_settled {
            order.status = OrderStatus::Completed;
        }
        set_order(&env, &order);

        env.events().publish(
            (symbol_short!("partrel"),),
            PartialReleasedEvent {
                id: order_id,
                anchor,
                amount: release_amount,
                partial_released_total: partial,
                remaining_locked: remaining,
            },
        );

        Ok(())
    }

    /// After the 12-hour window, refund remaining locked funds for any
    /// unsettled destination legs back to the sender.
    pub fn refund_unsettled(env: Env, sender: Address, order_id: u64) -> Result<(), Error> {
        require_initialized(&env)?;
        sender.require_auth();

        let mut order = get_order(&env, order_id)?;
        if order.sender != sender {
            return Err(Error::Unauthorized);
        }
        if order.status != OrderStatus::Open {
            return Err(Error::AlreadyResolved);
        }

        let now = env.ledger().timestamp();
        if now < order.deadline {
            return Err(Error::TooEarlyToRefund);
        }

        let remaining = get_remaining_locked(&env, order_id);
        if remaining <= 0 {
            return Err(Error::NothingToRefund);
        }

        let mut updated_legs: Vec<DestinationLeg> = Vec::new(&env);
        for i in 0..order.legs.len() {
            let mut leg = order.legs.get(i).unwrap();
            if leg.status == LegStatus::Pending {
                leg.status = LegStatus::Refunded;
            }
            updated_legs.push_back(leg);
        }
        order.legs = updated_legs;
        order.status = OrderStatus::Refunded;
        set_order(&env, &order);

        let partial = get_partial_released(&env, order_id);
        set_instance_balances(&env, order_id, partial, 0);

        let token_client = token::Client::new(&env, &get_token(&env)?);
        token_client.transfer(&env.current_contract_address(), &sender, &remaining);

        env.events().publish(
            (symbol_short!("unrefnd"),),
            UnsettledRefundedEvent {
                id: order_id,
                sender,
                refunded_amount: remaining,
            },
        );

        Ok(())
    }

    pub fn get_admin(env: Env) -> Result<Address, Error> {
        require_initialized(&env)?;
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)
    }

    pub fn get_token(env: Env) -> Result<Address, Error> {
        get_token(&env)
    }

    pub fn get_order(env: Env, order_id: u64) -> Result<SplitOrder, Error> {
        get_order(&env, order_id)
    }

    /// Instance-state: cumulative `E_partial` released for this order.
    pub fn get_partial_released(env: Env, order_id: u64) -> Result<i128, Error> {
        require_initialized(&env)?;
        if !env.storage().persistent().has(&DataKey::Order(order_id)) {
            return Err(Error::OrderNotFound);
        }
        Ok(get_partial_released(&env, order_id))
    }

    /// Instance-state: remaining locked escrow for this order.
    pub fn get_remaining_locked(env: Env, order_id: u64) -> Result<i128, Error> {
        require_initialized(&env)?;
        if !env.storage().persistent().has(&DataKey::Order(order_id)) {
            return Err(Error::OrderNotFound);
        }
        Ok(get_remaining_locked(&env, order_id))
    }
}

#[cfg(test)]
mod test;
