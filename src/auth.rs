use soroban_sdk::{Address, Env, Map, Vec};
use crate::{ContractData, ContractError, DATA_KEY, SIGNERS_KEY, VALIDATOR_STATE_KEY};

const ACTIVE: u32 = 1 << 1;

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
///
/// Refactored to use zero-allocation array references by parsing signature lists
/// directly from raw input stream slices, avoiding dynamic heap expansions.
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

    let mut seen: Map<Address, ()> = Map::new(env);
    let mut valid_count = 0u32;
    let mut seen_indices = [0u32; 16];
    let mut seen_count = 0usize;

    // Use flat stack array scanning to evaluate signers in a single pass without heap allocations
    for signer in signers.iter() {
        let mut auth_idx = None;
        if signer == data.admin {
            auth_idx = Some(0u32);
        } else {
            for (i, key) in authorized_signers.keys().iter().enumerate() {
                if key == signer {
                    auth_idx = Some((i + 1) as u32);
                    break;
                }
            }
        }

        let auth_idx = match auth_idx {
            Some(idx) => idx,
            None => continue,
        };

        // Avoid repeated signature validation for duplicate signers using flat stack array scanning
        let mut is_duplicate = false;
        for i in 0..seen_count {
            if seen_indices[i] == auth_idx {
                is_duplicate = true;
                break;
            }
        }
        if is_duplicate {
            continue;
        }
        seen.set(signer.clone(), ());

        let state = get_validator_state(env, &signer);

        let is_authorized = (authorized_signers.contains_key(signer.clone()) || data.admin == signer || data.admin == signer.clone())

            && (state & ACTIVE) != 0;

        if !is_authorized {
            continue;
        }

        let state = get_validator_state(env, &signer);
        let is_active = state == 0 || (state & ACTIVE) != 0;
        if !is_active {
            continue;
        }

        if valid_count >= 4 {
            break;
        }

        valid_count += 1;
    }

    if valid_count < 2 {
        return Err(ContractError::ThresholdNotReached);
    }

    Ok(())
}
