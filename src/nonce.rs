use soroban_sdk::{contracttype, Address, Env};

#[contracttype]
pub enum NonceKey {
    Nonce(Address),
}

pub fn get_nonce(env: &Env, coordinator: &Address) -> u64 {
    env.storage()
        .persistent()
        .get(&NonceKey::Nonce(coordinator.clone()))
        .unwrap_or(0u64)
}

pub fn consume_nonce(env: &Env, coordinator: &Address, incoming_nonce: u64) {
    let expected = get_nonce(env, coordinator);
    if incoming_nonce != expected {
        panic!("Invalid nonce: expected {}, got {}", expected, incoming_nonce);
    }
    env.storage()
        .persistent()
        .set(&NonceKey::Nonce(coordinator.clone()), &(expected + 1));
}
