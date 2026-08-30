use soroban_sdk::{contracttype, Address, Bytes, BytesN, Env, Map, Vec};

use crate::ContractError;

#[contracttype]
pub enum RelayerStorageKey {
    Threshold,
    Validator(BytesN<32>),
    Nonce(BytesN<32>),
}

pub fn configure_threshold(env: &Env, admin: &Address, threshold: u32) {
    admin.require_auth();
    env.storage().instance().set(&RelayerStorageKey::Threshold, &threshold);
}

pub fn add_validator(env: &Env, admin: &Address, pubkey: BytesN<32>) {
    admin.require_auth();
    env.storage().instance().set(&RelayerStorageKey::Validator(pubkey), &true);
}

pub fn remove_validator(env: &Env, admin: &Address, pubkey: BytesN<32>) {
    admin.require_auth();
    env.storage().instance().remove(&RelayerStorageKey::Validator(pubkey));
}

/// Verify a cross-chain payload hash against a list of multi-sig signatures.
/// 
/// Prevents message replay and enforces a minimum required threshold of authorized validators.
pub fn verify_cross_chain_payload(
    env: &Env,
    payload_hash: BytesN<32>,
    signatures: Vec<(BytesN<32>, BytesN<64>)>,
) -> Result<(), ContractError> {
    // 1. Prevent replay attacks
    let nonce_key = RelayerStorageKey::Nonce(payload_hash.clone());
    if env.storage().persistent().has(&nonce_key) {
        return Err(ContractError::InvalidProof);
    }

    // 2. Load threshold
    let threshold: u32 = env
        .storage()
        .instance()
        .get(&RelayerStorageKey::Threshold)
        .unwrap_or(0);

    if threshold == 0 || signatures.len() < threshold {
        return Err(ContractError::InvalidProof);
    }

    // 3. Verify signatures
    let mut valid_count = 0;
    let mut seen_validators: Map<BytesN<32>, ()> = Map::new(env);
    let mut payload_bytes = Bytes::new(env);
    payload_bytes.append(&Bytes::from_slice(env, &payload_hash.to_array()));

    for sig in signatures.iter() {
        let (pubkey, signature) = sig;

        let val_key = RelayerStorageKey::Validator(pubkey.clone());
        if !env.storage().instance().has(&val_key) {
            continue;
        }
        if seen_validators.contains_key(pubkey.clone()) {
            continue;
        }

        // Verify the signature using the native SDK.
        env.crypto().ed25519_verify(&pubkey, &payload_bytes, &signature);

        seen_validators.set(pubkey.clone(), ());
        valid_count += 1;
        if valid_count >= threshold {
            break;
        }
    }

    if valid_count < threshold {
        return Err(ContractError::InvalidProof);
    }

    // 4. Record the nonce to prevent replay attacks
    env.storage().persistent().set(&nonce_key, &true);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use soroban_sdk::testutils::Address as _;

    struct BridgeFixture {
        env: Env,
        keys: [SigningKey; 3],
        public_keys: [BytesN<32>; 3],
    }

    impl BridgeFixture {
        fn new() -> Self {
            let env = Env::default();
            env.mock_all_auths();
            let admin = Address::generate(&env);
            let keys = [
                SigningKey::from_bytes(&[1; 32]),
                SigningKey::from_bytes(&[2; 32]),
                SigningKey::from_bytes(&[3; 32]),
            ];
            let public_keys = [
                BytesN::from_array(&env, &keys[0].verifying_key().to_bytes()),
                BytesN::from_array(&env, &keys[1].verifying_key().to_bytes()),
                BytesN::from_array(&env, &keys[2].verifying_key().to_bytes()),
            ];

            for public_key in public_keys.iter() {
                add_validator(&env, &admin, public_key);
            }
            configure_threshold(&env, &admin, 2);

            Self {
                env,
                keys,
                public_keys,
            }
        }

        fn proof(&self, seed: u8) -> BytesN<32> {
            BytesN::from_array(&self.env, &[seed; 32])
        }

        fn signatures(
            &self,
            proof: &BytesN<32>,
            indexes: &[usize],
        ) -> Vec<(BytesN<32>, BytesN<64>)> {
            let mut signatures = Vec::new(&self.env);
            for &index in indexes {
                let signature = self.keys[index].sign(&proof.to_array());
                signatures.push((
                    self.public_keys[index].clone(),
                    BytesN::from_array(&self.env, &signature.to_bytes()),
                ));
            }
            signatures
        }
    }

    #[test]
    fn valid_threshold_proof_is_accepted() {
        let fixture = BridgeFixture::new();
        let proof = fixture.proof(1);

        assert_eq!(
            verify_cross_chain_payload(
                &fixture.env,
                proof.clone(),
                fixture.signatures(&proof, &[0, 1])
            ),
            Ok(())
        );
    }

    #[test]
    fn proof_with_insufficient_valid_signatures_returns_invalid_proof() {
        let fixture = BridgeFixture::new();
        let proof = fixture.proof(2);
        let signatures = fixture.signatures(&proof, &[0]);

        assert_eq!(
            verify_cross_chain_payload(&fixture.env, proof, signatures),
            Err(ContractError::InvalidProof)
        );
    }

    #[test]
    fn signature_from_unknown_validator_returns_invalid_proof() {
        let fixture = BridgeFixture::new();
        let proof = fixture.proof(5);
        let mut signatures = Vec::new(&fixture.env);
        signatures.push((
            BytesN::from_array(&fixture.env, &[99; 32]),
            BytesN::from_array(&fixture.env, &[0; 64]),
        ));
        signatures.push((
            BytesN::from_array(&fixture.env, &[98; 32]),
            BytesN::from_array(&fixture.env, &[0; 64]),
        ));

        assert_eq!(
            verify_cross_chain_payload(&fixture.env, proof, signatures),
            Err(ContractError::InvalidProof)
        );
    }

    #[test]
    fn duplicate_validator_signatures_do_not_satisfy_threshold() {
        let fixture = BridgeFixture::new();
        let proof = fixture.proof(3);
        let signatures = fixture.signatures(&proof, &[0, 0]);

        assert_eq!(
            verify_cross_chain_payload(&fixture.env, proof, signatures),
            Err(ContractError::InvalidProof)
        );
    }

    #[test]
    fn replayed_proof_returns_invalid_proof() {
        let fixture = BridgeFixture::new();
        let proof = fixture.proof(4);
        let signatures = fixture.signatures(&proof, &[0, 1]);

        assert_eq!(
            verify_cross_chain_payload(
                &fixture.env,
                proof.clone(),
                signatures.clone()
            ),
            Ok(())
        );
        assert_eq!(
            verify_cross_chain_payload(&fixture.env, proof, signatures),
            Err(ContractError::InvalidProof)
        );
    }
}
