use crate::ContractError;
use soroban_sdk::{contracttype, Address, Bytes, BytesN, Env};

#[contracttype]
pub enum NonceKey {
    State(Address),
}

#[contracttype]
#[derive(Clone)]
pub struct NonceState {
    pub nonce: u64,
    pub salt_signature: BytesN<32>,
}

pub fn get_nonce(env: &Env, coordinator: &Address) -> u64 {
    load_state(env, coordinator).nonce
}

pub fn consume_nonce(
    env: &Env,
    coordinator: &Address,
    incoming_nonce: u64,
    salt: Bytes,
    salt_signature: BytesN<32>,
) -> Result<(), ContractError> {
    let state = load_state(env, coordinator);
    if incoming_nonce != state.nonce {
        return Err(ContractError::InvalidNonce);
    }

    let expected_signature = derive_salt_signature(env, incoming_nonce, salt);
    if salt_signature != expected_signature {
        return Err(ContractError::InvalidSaltSignature);
    }

    let next_state = NonceState {
        nonce: state.nonce + 1,
        salt_signature,
    };

    env.storage()
        .persistent()
        .set(&NonceKey::State(coordinator.clone()), &next_state);
    Ok(())
}

fn load_state(env: &Env, coordinator: &Address) -> NonceState {
    env.storage()
        .persistent()
        .get(&NonceKey::State(coordinator.clone()))
        .unwrap_or_else(|| NonceState {
            nonce: 0u64,
            salt_signature: BytesN::from_array(env, &[0u8; 32]),
        })
}

pub fn derive_salt_signature(env: &Env, nonce: u64, salt: Bytes) -> BytesN<32> {
    let mut payload = Bytes::new(env);
    payload.append(&Bytes::from_slice(env, &nonce.to_be_bytes()));
    payload.append(&salt);
    env.crypto().sha256(&payload)
}
