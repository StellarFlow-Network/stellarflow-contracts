//! Test-only minimal token contract.
//!
//! The pinned `soroban-sdk 20.0.0` test harness raises a host `InternalError`
//! when the built-in Stellar asset contract is invoked *after* the ledger
//! sequence number has been jumped forward (crash documented in
//! `src/test.rs::advance_ledger_timestamp_only`). Vote-escrow withdrawals must
//! be driven past `MIN_LOCK_DURATION_LEDGERS` via such a jump, so we back the
//! contract with this in-crate token that stores balances in plain persistent
//! storage (which is stable across sequence jumps in the same harness).

use soroban_sdk::{contract, contractimpl, contracttype, Address, Env};

#[contracttype]
#[derive(Clone)]
pub enum MockTokenKey {
    Bal(Address),
}

#[contract]
pub struct MockToken;

#[contractimpl]
impl MockToken {
    pub fn mint(env: Env, to: Address, amount: i128) {
        let balance = read_balance(&env, &to);
        write_balance(&env, &to, balance + amount);
    }

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        let from_balance = read_balance(&env, &from);
        let to_balance = read_balance(&env, &to);
        write_balance(&env, &from, from_balance - amount);
        write_balance(&env, &to, to_balance + amount);
    }

    pub fn balance(env: Env, addr: Address) -> i128 {
        read_balance(&env, &addr)
    }
}

fn read_balance(env: &Env, addr: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&MockTokenKey::Bal(addr.clone()))
        .unwrap_or(0)
}

fn write_balance(env: &Env, addr: &Address, amount: i128) {
    env.storage().persistent().set(&MockTokenKey::Bal(addr.clone()), &amount);
}