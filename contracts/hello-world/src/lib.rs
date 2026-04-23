#![no_std]
use soroban_sdk::{contract, contractimpl, symbol_short, vec, Env, Symbol, Vec};

#[contract]
pub struct Contract;

#[contractimpl]
impl Contract {
    pub fn hello(env: Env, user: Symbol) -> Vec<Symbol> {
        vec![&env, symbol_short!("Hello"), user]
    }
}

mod test;
