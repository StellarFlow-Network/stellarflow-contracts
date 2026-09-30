use soroban_sdk::{Address, Env, Map, Vec};
use crate::{ContractData, ContractError, DATA_KEY, SIGNERS_KEY, VALIDATOR_STATE_KEY};

// Bit positions for ValidatorState mask
const ONLINE: u32 = 1 << 0;
const ACTIVE: u32 = 1 << 1;
const SUSPENDED: u32 = 1 << 2;
const JAILED: u32 = 1 << 3;

fn get_validator_state(env: &Env, addr: &Address) -> u32 {
    let states: Map<Address, u32> = env
        .storage()
        .instance()
        .get(&VALIDATOR_STATE_KEY)
        .unwrap_or_else(|| Map::new(env));
    states.get(addr.clone()).unwrap_or(0u32)
}

fn set_validator_flag(env: &Env, addr: &Address, flag: u32, value: bool) {
    let mut states: Map<Address, u32> = env
        .storage()
        .instance()
        .get(&VALIDATOR_STATE_KEY)
        .unwrap_or_else(|| Map::new(env));
    let current = states.get(addr.clone()).unwrap_or(0u32);
    let updated = if value { current | flag } else { current & !flag };
    states.set(addr.clone(), updated);
    env.storage().instance().set(&VALIDATOR_STATE_KEY, &states);
}

fn has_validator_flag(env: &Env, addr: &Address, flag: u32) -> bool {
    get_validator_state(env, addr) & flag != 0
}

/// Rigid multi-signature confirmation barrier for parameter shift actions.
/// Requires a supermajority of 4 out of 5 validated administrative signatures
/// before approving changes to system boundary configurations.
pub fn require_multisig(env: &Env, signers: &Vec<Address>) -> Result<(), ContractError> {
    let authorized_signers: Map<Address, ()> = env
        .storage()
        .instance()
        .get(&SIGNERS_KEY)
        .unwrap_or_else(|| Map::new(env));

    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;

    let mut valid_count = 0u32;

    for (idx, signer) in signers.iter().enumerate() {
        // Avoid repeated signature validation for duplicate signers in the same request.
        if signers.iter().take(idx).any(|previous| previous == signer) {
            continue;
        }

        let state = get_validator_state(env, &signer);
        let is_authorized = (authorized_signers.contains_key(signer.clone()) || data.admin == signer)
            && (state & ACTIVE) != 0;
        if !is_authorized {
            continue;
        }

        signer.require_auth();
        valid_count += 1;
        set_validator_flag(env, &signer, ONLINE, true);

        if valid_count >= 2 {
            break;
        }
    }

    // Clear ONLINE flags for all signers after the check completes
    for signer in signers.iter() {
        set_validator_flag(env, &signer, ONLINE, false);
    }

    // Require a supermajority of 4 out of 5 validated administrative signatures
    if valid_count < 4 {
        return Err(ContractError::ThresholdNotReached);
    }

    Ok(())
}
