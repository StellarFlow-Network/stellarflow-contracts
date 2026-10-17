//! Off-chain EIP-712-style meta-transaction payload verification (Issue #1253).
//!
//! Enforces an explicit `deadline` parameter on meta-transaction execution.
//! Transactions whose signatures have exceeded their deadline are strictly rejected
//! to prevent indefinitely valid authorizations and delayed relayer griefing.

use soroban_sdk::{contracttype, symbol_short, Address, BytesN, Env, Symbol};
use crate::ContractError;

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct MetaTxPayload {
    pub from: Address,
    pub to: Address,
    pub value: u64,
    pub nonce: u64,
    pub deadline: u64,
}

/// Validates a meta-transaction's expiry and nonce replay constraints.
///
/// Remediation for Issue #1253:
/// Reverts with `ContractError::SignatureExpired` if current ledger timestamp exceeds `deadline`.
pub fn validate_meta_tx_execution(
    current_timestamp: u64,
    current_nonce: u64,
    payload: &MetaTxPayload,
) -> Result<u64, ContractError> {
    // ── 1. Validate Expiry Timestamp (Issue #1253) ───────────────────────────
    if current_timestamp > payload.deadline {
        return Err(ContractError::SignatureExpired);
    }

    // ── 2. Validate Replay Nonce ─────────────────────────────────────────────
    if payload.nonce != current_nonce {
        return Err(ContractError::InvalidNonce);
    }

    // Return the incremented nonce for storage update
    Ok(current_nonce + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_meta_tx_passes() {
        let payload = MetaTxPayload {
            from: Address::generate(&Env::default()),
            to: Address::generate(&Env::default()),
            value: 100,
            nonce: 0,
            deadline: 1500,
        };

        // Current time is 1000, deadline is 1500 -> Valid
        let next_nonce = validate_meta_tx_execution(1000, 0, &payload);
        assert_eq!(next_nonce, Ok(1));
    }

    #[test]
    fn test_exact_deadline_timestamp_passes() {
        let payload = MetaTxPayload {
            from: Address::generate(&Env::default()),
            to: Address::generate(&Env::default()),
            value: 100,
            nonce: 0,
            deadline: 1500,
        };

        // Current time is 1500, deadline is 1500 -> Valid
        let next_nonce = validate_meta_tx_execution(1500, 0, &payload);
        assert_eq!(next_nonce, Ok(1));
    }

    #[test]
    fn test_expired_meta_tx_reverts() {
        let payload = MetaTxPayload {
            from: Address::generate(&Env::default()),
            to: Address::generate(&Env::default()),
            value: 100,
            nonce: 0,
            deadline: 1500,
        };

        // Current time is 1501, deadline is 1500 -> Reverts with SignatureExpired
        let res = validate_meta_tx_execution(1501, 0, &payload);
        assert_eq!(res, Err(ContractError::SignatureExpired));
    }

    #[test]
    fn test_invalid_nonce_reverts() {
        let payload = MetaTxPayload {
            from: Address::generate(&Env::default()),
            to: Address::generate(&Env::default()),
            value: 100,
            nonce: 5,
            deadline: 1500,
        };

        // Expected nonce is 0, payload nonce is 5 -> InvalidNonce
        let res = validate_meta_tx_execution(1000, 0, &payload);
        assert_eq!(res, Err(ContractError::InvalidNonce));
    }
}
