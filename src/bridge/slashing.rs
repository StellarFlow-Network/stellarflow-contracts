//! Bridge Validator Slashing Engine for Double-Sign Proofs (Issue #959).
//!
//! Provides automated verification and slashing mechanisms when cross-chain bridge
//! validators produce conflicting attestations for the same message nonce.
//!
//! Time Complexity:
//! - Verification: O(1) cryptographic verification via Ed25519 signature checks.
//! - Slashing: O(1) state lookup and mutation.
//!
//! Space Complexity:
//! - O(1) memory overhead and instance storage allocations.

use soroban_sdk::{contracttype, symbol_short, Bytes, BytesN, Env, Address};
use crate::bridge::relayer::{bridge_message_digest, is_validator, remove_validator_direct};
use crate::ContractError;

/// Storage keys for bridge validator collateral and slashing states.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SlashingDataKey {
    /// Collateral deposit staked by a bridge validator: (validator_pubkey).
    ValidatorCollateral(BytesN<32>),
    /// Marker indicating a validator has been permanently slashed and banned: (validator_pubkey).
    BannedValidator(BytesN<32>),
    /// Total cumulative stake slashed from malicious bridge validators.
    TotalSlashedCollateral,
}

/// Attestation payload signed by a bridge validator.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttestationPayload {
    pub proof_hash: BytesN<32>,
    pub recipient: Address,
    pub amount: i128,
}

/// Cryptographic double-sign proof containing two conflicting signature payloads for same nonce.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DoubleSignProof {
    /// Offending validator's Ed25519 public key.
    pub validator: BytesN<32>,
    /// Source chain ID common to both conflicting attestations.
    pub source_chain_id: u32,
    /// Message nonce common to both conflicting attestations.
    pub nonce: u64,
    /// First attestation payload.
    pub payload_1: AttestationPayload,
    /// Cryptographic signature for first payload.
    pub signature_1: BytesN<64>,
    /// Second conflicting attestation payload.
    pub payload_2: AttestationPayload,
    /// Cryptographic signature for second payload.
    pub signature_2: BytesN<64>,
}

/// Deposit collateral to stake a bridge validator.
pub fn stake_validator_collateral(
    env: &Env,
    validator: &BytesN<32>,
    amount: i128,
) -> Result<(), ContractError> {
    if amount <= 0 {
        return Err(ContractError::InvalidStakeAmount);
    }

    if is_validator_banned(env, validator) {
        return Err(ContractError::Unauthorized);
    }

    if !is_validator(env, validator) {
        return Err(ContractError::NotRegistered);
    }

    let key = SlashingDataKey::ValidatorCollateral(validator.clone());
    let current_stake: i128 = env.storage().instance().get(&key).unwrap_or(0);
    let new_stake = current_stake.checked_add(amount).ok_or(ContractError::Overflow)?;

    env.storage().instance().set(&key, &new_stake);
    Ok(())
}

/// Get the current staked collateral deposit for a validator.
pub fn get_validator_collateral(env: &Env, validator: &BytesN<32>) -> i128 {
    env.storage()
        .instance()
        .get(&SlashingDataKey::ValidatorCollateral(validator.clone()))
        .unwrap_or(0)
}

/// Query whether a validator has been permanently banned from the consensus set.
pub fn is_validator_banned(env: &Env, validator: &BytesN<32>) -> bool {
    env.storage()
        .instance()
        .has(&SlashingDataKey::BannedValidator(validator.clone()))
}

/// Query total cumulative slashed collateral.
pub fn get_total_slashed_collateral(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&SlashingDataKey::TotalSlashedCollateral)
        .unwrap_or(0)
}

/// Validate cryptographic double-sign proof, slash 100% of collateral, and remove permanently.
///
/// 1. Verifies that the offending validator is registered in the bridge validator set and not yet banned.
/// 2. Asserts that the two payloads are distinct/conflicting for the same (source_chain_id, nonce).
/// 3. Reconstructs message digests for both payloads.
/// 4. Cryptographically validates both Ed25519 signatures against the validator's public key.
/// 5. Slashes 100% of the validator's staked collateral deposit.
/// 6. Permanently removes the validator from active consensus and marks them banned.
pub fn process_double_sign_proof(
    env: &Env,
    proof: &DoubleSignProof,
) -> Result<i128, ContractError> {
    if is_validator_banned(env, &proof.validator) {
        return Err(ContractError::Unauthorized);
    }

    if !is_validator(env, &proof.validator) {
        return Err(ContractError::NotRegistered);
    }

    // Ensure payloads are conflicting for the same nonce
    if proof.payload_1 == proof.payload_2 {
        return Err(ContractError::InvalidArgument);
    }

    // Reconstruct digests
    let digest_1 = bridge_message_digest(
        env,
        proof.source_chain_id,
        proof.nonce,
        &proof.payload_1.proof_hash,
        &proof.payload_1.recipient,
        proof.payload_1.amount,
    );

    let digest_2 = bridge_message_digest(
        env,
        proof.source_chain_id,
        proof.nonce,
        &proof.payload_2.proof_hash,
        &proof.payload_2.recipient,
        proof.payload_2.amount,
    );

    if digest_1 == digest_2 {
        return Err(ContractError::InvalidArgument);
    }

    // Cryptographic verification of both signatures
    env.crypto().ed25519_verify(
        &proof.validator,
        &Bytes::from_slice(env, &digest_1.to_array()),
        &proof.signature_1,
    );

    env.crypto().ed25519_verify(
        &proof.validator,
        &Bytes::from_slice(env, &digest_2.to_array()),
        &proof.signature_2,
    );

    // 100% slash of offending validator's staked collateral deposit
    let collateral_key = SlashingDataKey::ValidatorCollateral(proof.validator.clone());
    let staked_amount: i128 = env.storage().instance().get(&collateral_key).unwrap_or(0);

    // Zero out stake (100% slash)
    env.storage().instance().set(&collateral_key, &0i128);

    // Update global slashed tally
    let total_slashed: i128 = env
        .storage()
        .instance()
        .get(&SlashingDataKey::TotalSlashedCollateral)
        .unwrap_or(0);
    let new_total = total_slashed
        .checked_add(staked_amount)
        .ok_or(ContractError::Overflow)?;
    env.storage()
        .instance()
        .set(&SlashingDataKey::TotalSlashedCollateral, &new_total);

    // Remove validator permanently from active bridge consensus set
    remove_validator_direct(env, &proof.validator);

    // Record permanent ban
    env.storage().instance().set(
        &SlashingDataKey::BannedValidator(proof.validator.clone()),
        &true,
    );

    // Publish event
    env.events().publish(
        (symbol_short!("val_slash"), proof.validator.clone()),
        (proof.source_chain_id, proof.nonce, staked_amount),
    );

    Ok(staked_amount)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use soroban_sdk::testutils::Address as _;
    use crate::bridge::relayer::{add_validator, configure_threshold};
    use crate::{ContractData, DATA_KEY};

    #[test]
    fn test_double_sign_detection_and_slashing() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let recipient_1 = Address::generate(&env);
        let recipient_2 = Address::generate(&env);

        env.storage().instance().set(
            &DATA_KEY,
            &ContractData {
                admin: admin.clone(),
                value: 0,
            },
        );

        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        let pubkey_bytes = signing_key.verifying_key().to_bytes();
        let validator_pubkey = BytesN::from_array(&env, &pubkey_bytes);

        add_validator(&env, &admin, validator_pubkey.clone()).unwrap();
        configure_threshold(&env, &admin, 1).unwrap();

        // Stake 5000 units of collateral
        let collateral = 5_000i128;
        stake_validator_collateral(&env, &validator_pubkey, collateral).unwrap();
        assert_eq!(get_validator_collateral(&env, &validator_pubkey), collateral);

        let source_chain_id = 1u32;
        let nonce = 100u64;

        let payload_1 = AttestationPayload {
            proof_hash: BytesN::from_array(&env, &[1u8; 32]),
            recipient: recipient_1,
            amount: 1_000,
        };
        let digest_1 = bridge_message_digest(
            &env,
            source_chain_id,
            nonce,
            &payload_1.proof_hash,
            &payload_1.recipient,
            payload_1.amount,
        );
        let sig_1 = signing_key.sign(&digest_1.to_array());
        let signature_1 = BytesN::from_array(&env, &sig_1.to_bytes());

        // Conflicting payload 2 for same nonce
        let payload_2 = AttestationPayload {
            proof_hash: BytesN::from_array(&env, &[2u8; 32]),
            recipient: recipient_2,
            amount: 2_000,
        };
        let digest_2 = bridge_message_digest(
            &env,
            source_chain_id,
            nonce,
            &payload_2.proof_hash,
            &payload_2.recipient,
            payload_2.amount,
        );
        let sig_2 = signing_key.sign(&digest_2.to_array());
        let signature_2 = BytesN::from_array(&env, &sig_2.to_bytes());

        let proof = DoubleSignProof {
            validator: validator_pubkey.clone(),
            source_chain_id,
            nonce,
            payload_1,
            signature_1,
            payload_2,
            signature_2,
        };

        let slashed = process_double_sign_proof(&env, &proof).unwrap();
        assert_eq!(slashed, collateral);

        // Stake is 100% slashed
        assert_eq!(get_validator_collateral(&env, &validator_pubkey), 0);
        assert_eq!(get_total_slashed_collateral(&env), collateral);

        // Validator is permanently removed and banned
        assert!(!is_validator(&env, &validator_pubkey));
        assert!(is_validator_banned(&env, &validator_pubkey));

        // Re-adding the validator must fail
        assert_eq!(
            add_validator(&env, &admin, validator_pubkey.clone()),
            Err(ContractError::Unauthorized)
        );
    }
}
