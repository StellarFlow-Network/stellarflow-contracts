//! Keys for calculation state that must not incur durable storage rent.

use soroban_sdk::{contracttype, symbol_short, Symbol};

#[contracttype]
#[derive(Clone)]
pub enum EphemeralStorageKey {
    ActiveRoute,
}

pub const ACTIVE_ROUTE_LABEL: Symbol = symbol_short!("RTEXEC");
