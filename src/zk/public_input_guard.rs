//! ZK Proof Public Input Verification Guard for Deposit Notes (Issue #981).
//!
//! Ensures that public inputs supplied to zero-knowledge proof verifiers strictly
//! match contract state parameters, preventing parameter tampering, spoofed Merkle roots,
//! fee substitution, or redirected recipient attacks on shielded deposit notes.
//!
//! Time Complexity:
//! - Verification: O(1) direct comparisons and historical root lookup.
//!
//! Space Complexity:
//! - O(1) memory overhead.

use soroban_sdk::{contracttype, xdr::ToXdr, Address, Bytes, BytesN, Env, Vec};
use crate::zk::merkle::{validate_root, MerkleStorageKey};
use crate::ContractError;

/// Structured public inputs for a deposit note withdrawal circuit.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DepositNotePublicInputs {
    /// Public Merkle root Rpub.
    pub root: BytesN<32>,
    /// Nullifier hash preventing double-spend.
    pub nullifier_hash: BytesN<32>,
    /// Recipient address receiving unlocked funds.
    pub recipient: Address,
    /// Fee parameter allocated for relayer/protocol.
    pub fee: i128,
}

/// Parameters submitted with the withdrawal transaction to be verified against the ZK public inputs.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubmittedDepositParameters {
    /// Expected active or historical Merkle root.
    pub expected_root: BytesN<32>,
    /// Expected nullifier hash.
    pub expected_nullifier_hash: BytesN<32>,
    /// Expected recipient address.
    pub expected_recipient: Address,
    /// Expected fee parameter.
    pub expected_fee: i128,
}

/// Query whether a Merkle root matches current or historical tree root in instance/persistent memory.
pub fn is_known_merkle_root(env: &Env, root: &BytesN<32>) -> bool {
    // Check if it matches current active root
    if let Some(current) = env
        .storage()
        .persistent()
        .get::<_, BytesN<32>>(&MerkleStorageKey::CurrentRoot)
    {
        if &current == root {
            return true;
        }
    }

    // Check if it exists in the historical root records and is unexpired
    validate_root(env, root).is_ok()
}

/// Convert an i128 fee into a 32-byte big-endian scalar array for circuit input comparison.
pub fn fee_to_scalar_bytes(env: &Env, fee: i128) -> BytesN<32> {
    let mut bytes = [0u8; 32];
    let fee_bytes = fee.to_be_bytes();
    // Place 16-byte i128 at the end (big-endian 256-bit scalar)
    bytes[16..32].copy_from_slice(&fee_bytes);
    BytesN::from_array(env, &bytes)
}

/// Compute recipient hash commitment from Address.
pub fn compute_recipient_commitment(env: &Env, recipient: &Address) -> BytesN<32> {
    let xdr = recipient.to_xdr(env);
    env.crypto().sha256(&xdr)
}

/// Verify that structured public inputs strictly match submitted parameters and contract state.
///
/// Reverts with ContractError::InvalidZKPublicInputs on any mismatch:
/// 1. Assert public input root Rpub matches current or historical Merkle tree root stored in memory.
/// 2. Verify fee parameters, recipient address, and nullifier hashes align with submitted parameters.
pub fn verify_deposit_public_inputs(
    env: &Env,
    public_inputs: &DepositNotePublicInputs,
    submitted_params: &SubmittedDepositParameters,
) -> Result<(), ContractError> {
    // 1. Assert public input root Rpub matches current or historical Merkle tree root
    if !is_known_merkle_root(env, &public_inputs.root) {
        return Err(ContractError::InvalidZKPublicInputs);
    }

    if public_inputs.root != submitted_params.expected_root {
        return Err(ContractError::InvalidZKPublicInputs);
    }

    // 2. Verify nullifier hash aligns with submitted parameter
    if public_inputs.nullifier_hash != submitted_params.expected_nullifier_hash {
        return Err(ContractError::InvalidZKPublicInputs);
    }

    // Reject all-zero nullifiers
    if public_inputs.nullifier_hash == BytesN::from_array(env, &[0u8; 32]) {
        return Err(ContractError::InvalidZKPublicInputs);
    }

    // 3. Verify recipient public key / address aligns with submitted parameter
    if public_inputs.recipient != submitted_params.expected_recipient {
        return Err(ContractError::InvalidZKPublicInputs);
    }

    // 4. Verify fee parameter aligns with submitted parameter and is non-negative
    if public_inputs.fee < 0 || public_inputs.fee != submitted_params.expected_fee {
        return Err(ContractError::InvalidZKPublicInputs);
    }

    Ok(())
}

/// Verify raw circuit public inputs vector [root, nullifier_hash, recipient_hash, fee_scalar].
///
/// Reverts with ContractError::InvalidZKPublicInputs on mismatch:
/// - index 0: Rpub (asserted against current or historical Merkle root)
/// - index 1: nullifier hash
/// - index 2: recipient public key / address commitment
/// - index 3: fee scalar
pub fn verify_raw_zk_public_inputs(
    env: &Env,
    raw_inputs: &Vec<BytesN<32>>,
    submitted_params: &SubmittedDepositParameters,
) -> Result<(), ContractError> {
    if raw_inputs.len() < 4 {
        return Err(ContractError::InvalidZKPublicInputs);
    }

    let r_pub = raw_inputs.get(0).ok_or(ContractError::InvalidZKPublicInputs)?;
    let nullifier = raw_inputs.get(1).ok_or(ContractError::InvalidZKPublicInputs)?;
    let recipient_comm = raw_inputs.get(2).ok_or(ContractError::InvalidZKPublicInputs)?;
    let fee_scalar = raw_inputs.get(3).ok_or(ContractError::InvalidZKPublicInputs)?;

    // 1. Assert public input root Rpub matches current or historical Merkle tree root
    if !is_known_merkle_root(env, &r_pub) || r_pub != submitted_params.expected_root {
        return Err(ContractError::InvalidZKPublicInputs);
    }

    // 2. Verify nullifier hash
    if nullifier != submitted_params.expected_nullifier_hash
        || nullifier == BytesN::from_array(env, &[0u8; 32])
    {
        return Err(ContractError::InvalidZKPublicInputs);
    }

    // 3. Verify recipient commitment
    let expected_recipient_comm = compute_recipient_commitment(env, &submitted_params.expected_recipient);
    if recipient_comm != expected_recipient_comm {
        return Err(ContractError::InvalidZKPublicInputs);
    }

    // 4. Verify fee scalar
    if submitted_params.expected_fee < 0 {
        return Err(ContractError::InvalidZKPublicInputs);
    }
    let expected_fee_scalar = fee_to_scalar_bytes(env, submitted_params.expected_fee);
    if fee_scalar != expected_fee_scalar {
        return Err(ContractError::InvalidZKPublicInputs);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use crate::zk::merkle::{insert_deposit, MerkleStorageKey};

    #[test]
    fn test_valid_deposit_public_inputs_pass() {
        let env = Env::default();
        let recipient = Address::generate(&env);
        let commitment = BytesN::from_array(&env, &[9u8; 32]);

        // Insert deposit to establish legitimate root in state
        let (_, root) = insert_deposit(&env, commitment).unwrap();

        let nullifier_hash = BytesN::from_array(&env, &[7u8; 32]);
        let fee = 250i128;

        let public_inputs = DepositNotePublicInputs {
            root: root.clone(),
            nullifier_hash: nullifier_hash.clone(),
            recipient: recipient.clone(),
            fee,
        };

        let submitted_params = SubmittedDepositParameters {
            expected_root: root.clone(),
            expected_nullifier_hash: nullifier_hash.clone(),
            expected_recipient: recipient.clone(),
            expected_fee: fee,
        };

        assert!(verify_deposit_public_inputs(&env, &public_inputs, &submitted_params).is_ok());

        // Verify raw vector inputs
        let mut raw = Vec::new(&env);
        raw.push_back(root);
        raw.push_back(nullifier_hash);
        raw.push_back(compute_recipient_commitment(&env, &recipient));
        raw.push_back(fee_to_scalar_bytes(&env, fee));

        assert!(verify_raw_zk_public_inputs(&env, &raw, &submitted_params).is_ok());
    }

    #[test]
    fn test_spoofed_root_reverts_with_invalid_zk_public_inputs() {
        let env = Env::default();
        let recipient = Address::generate(&env);
        let fake_root = BytesN::from_array(&env, &[99u8; 32]);
        let nullifier_hash = BytesN::from_array(&env, &[7u8; 32]);
        let fee = 250i128;

        let public_inputs = DepositNotePublicInputs {
            root: fake_root.clone(),
            nullifier_hash: nullifier_hash.clone(),
            recipient: recipient.clone(),
            fee,
        };

        let submitted_params = SubmittedDepositParameters {
            expected_root: fake_root,
            expected_nullifier_hash: nullifier_hash,
            expected_recipient: recipient,
            expected_fee: fee,
        };

        let result = verify_deposit_public_inputs(&env, &public_inputs, &submitted_params);
        assert_eq!(result, Err(ContractError::InvalidZKPublicInputs));
    }

    #[test]
    fn test_mismatched_fee_reverts_with_invalid_zk_public_inputs() {
        let env = Env::default();
        let recipient = Address::generate(&env);
        let commitment = BytesN::from_array(&env, &[5u8; 32]);
        let (_, root) = insert_deposit(&env, commitment).unwrap();

        let nullifier_hash = BytesN::from_array(&env, &[7u8; 32]);

        let public_inputs = DepositNotePublicInputs {
            root: root.clone(),
            nullifier_hash: nullifier_hash.clone(),
            recipient: recipient.clone(),
            fee: 500, // Attacker altered fee
        };

        let submitted_params = SubmittedDepositParameters {
            expected_root: root,
            expected_nullifier_hash: nullifier_hash,
            expected_recipient: recipient,
            expected_fee: 100, // Expected fee
        };

        let result = verify_deposit_public_inputs(&env, &public_inputs, &submitted_params);
        assert_eq!(result, Err(ContractError::InvalidZKPublicInputs));
    }

    #[test]
    fn test_mismatched_recipient_reverts_with_invalid_zk_public_inputs() {
        let env = Env::default();
        let recipient_1 = Address::generate(&env);
        let recipient_2 = Address::generate(&env);
        let commitment = BytesN::from_array(&env, &[5u8; 32]);
        let (_, root) = insert_deposit(&env, commitment).unwrap();

        let nullifier_hash = BytesN::from_array(&env, &[7u8; 32]);

        let public_inputs = DepositNotePublicInputs {
            root: root.clone(),
            nullifier_hash: nullifier_hash.clone(),
            recipient: recipient_1, // Tampered recipient
            fee: 100,
        };

        let submitted_params = SubmittedDepositParameters {
            expected_root: root,
            expected_nullifier_hash: nullifier_hash,
            expected_recipient: recipient_2,
            expected_fee: 100,
        };

        let result = verify_deposit_public_inputs(&env, &public_inputs, &submitted_params);
        assert_eq!(result, Err(ContractError::InvalidZKPublicInputs));
    }

    #[test]
    fn test_mismatched_nullifier_reverts_with_invalid_zk_public_inputs() {
        let env = Env::default();
        let recipient = Address::generate(&env);
        let commitment = BytesN::from_array(&env, &[5u8; 32]);
        let (_, root) = insert_deposit(&env, commitment).unwrap();

        let nullifier_1 = BytesN::from_array(&env, &[1u8; 32]);
        let nullifier_2 = BytesN::from_array(&env, &[2u8; 32]);

        let public_inputs = DepositNotePublicInputs {
            root: root.clone(),
            nullifier_hash: nullifier_1,
            recipient: recipient.clone(),
            fee: 100,
        };

        let submitted_params = SubmittedDepositParameters {
            expected_root: root,
            expected_nullifier_hash: nullifier_2,
            expected_recipient: recipient,
            expected_fee: 100,
        };

        let result = verify_deposit_public_inputs(&env, &public_inputs, &submitted_params);
        assert_eq!(result, Err(ContractError::InvalidZKPublicInputs));
    }
}
