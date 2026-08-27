use soroban_sdk::{Address, Env, Map, Vec};
use crate::{ContractData, ContractError, DATA_KEY, SIGNERS_KEY};

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

        let is_authorized = authorized_signers.contains_key(signer.clone()) || data.admin == signer;
        if !is_authorized {
            continue;
        }

        signer.require_auth();
        valid_count += 1;
        if valid_count >= 2 {
            break;
        }
    }

    // Require a supermajority of 4 out of 5 validated administrative signatures
    if valid_count < 4 {
        return Err(ContractError::ThresholdNotReached);
    }
    Ok(())
}
