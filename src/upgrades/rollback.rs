use soroban_sdk::{symbol_short, Address, Bytes, BytesN, Env, Symbol};
use crate::ContractError;
use crate::nonce::consume_nonce;

pub const CURRENT_WASM_KEY: Symbol = symbol_short!("CURR_WASM");
pub const PREVIOUS_WASM_KEY: Symbol = symbol_short!("PREV_WASM");
pub const UPGRADE_TIMESTAMP_KEY: Symbol = symbol_short!("UPG_TIME");

pub fn preserve_current_wasm(env: &Env, new_wasm: &BytesN<32>) {
    if let Some(current_wasm) = env.storage().instance().get::<_, BytesN<32>>(&CURRENT_WASM_KEY) {
        env.storage().instance().set(&PREVIOUS_WASM_KEY, &current_wasm);
        env.storage().instance().set(&UPGRADE_TIMESTAMP_KEY, &env.ledger().timestamp());
    }
    env.storage().instance().set(&CURRENT_WASM_KEY, new_wasm);
}

pub fn execute_rollback(
    env: Env,
    admin: Address,
    nonce: u64,
    salt: Bytes,
    signature: BytesN<32>,
    sig_expires_at: u64,
) -> Result<(), ContractError> {
    if env.ledger().timestamp() > sig_expires_at {
        return Err(ContractError::SignatureExpired);
    }
    
    // Load admin config
    let data: crate::ContractData = env
        .storage()
        .instance()
        .get(&crate::DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;
        
    if data.admin != admin {
        return Err(ContractError::NotAdmin);
    }
    admin.require_auth();
    consume_nonce(&env, &admin, nonce, salt, signature)?;

    let prev_wasm: BytesN<32> = env
        .storage()
        .instance()
        .get(&PREVIOUS_WASM_KEY)
        .ok_or(ContractError::NoPreviousUpgrade)?;
    let upgrade_time: u64 = env
        .storage()
        .instance()
        .get(&UPGRADE_TIMESTAMP_KEY)
        .ok_or(ContractError::NoPreviousUpgrade)?;

    if env.ledger().timestamp().saturating_sub(upgrade_time) > 72 * 60 * 60 {
        return Err(ContractError::RollbackWindowExpired);
    }

    // Perform rollback
    env.deployer().update_current_contract_wasm(prev_wasm.clone());

    // Update keys
    env.storage().instance().set(&CURRENT_WASM_KEY, &prev_wasm);
    env.storage().instance().remove(&PREVIOUS_WASM_KEY);
    env.storage().instance().remove(&UPGRADE_TIMESTAMP_KEY);

    Ok(())
}
