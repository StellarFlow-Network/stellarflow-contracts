use soroban_sdk::xdr::ToXdr;
use soroban_sdk::{contracttype, Address, Bytes, BytesN, Env, Map, Vec};

use crate::{ContractData, ContractError, DATA_KEY};

#[contracttype]
pub enum RelayerStorageKey {
    Threshold,
    Validator(BytesN<32>),
    SpentNonce(u32, u64),
}

fn require_protocol_admin(env: &Env, caller: &Address) -> Result<(), ContractError> {
    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;

    if data.admin != *caller {
        return Err(ContractError::NotAdmin);
    }

    caller.require_auth();
    Ok(())
}

pub fn configure_threshold(
    env: &Env,
    admin: &Address,
    threshold: u32,
) -> Result<(), ContractError> {
    require_protocol_admin(env, admin)?;

    if threshold == 0 {
        return Err(ContractError::InvalidProof);
    }

    env.storage()
        .instance()
        .set(&RelayerStorageKey::Threshold, &threshold);

    Ok(())
}

pub fn add_validator(env: &Env, admin: &Address, pubkey: BytesN<32>) -> Result<(), ContractError> {
    require_protocol_admin(env, admin)?;

    let key = RelayerStorageKey::Validator(pubkey);

    if env.storage().instance().has(&key) {
        return Err(ContractError::AlreadyRegistered);
    }

    env.storage().instance().set(&key, &true);

    Ok(())
}

pub fn remove_validator(
    env: &Env,
    admin: &Address,
    pubkey: BytesN<32>,
) -> Result<(), ContractError> {
    require_protocol_admin(env, admin)?;

    let key = RelayerStorageKey::Validator(pubkey);

    if !env.storage().instance().has(&key) {
        return Err(ContractError::NotRegistered);
    }

    env.storage().instance().remove(&key);

    Ok(())
}

/// Build the exact digest validators must sign for a bridge unlock.
///
/// The digest commits to:
/// - protocol/domain tag
/// - source chain
/// - message nonce
/// - bridge proof hash
/// - recipient address
/// - unlock amount
pub fn bridge_message_digest(
    env: &Env,
    source_chain_id: u32,
    nonce: u64,
    proof_hash: &BytesN<32>,
    recipient: &Address,
    amount: i128,
) -> BytesN<32> {
    let mut payload = Bytes::new(env);

    payload.append(&Bytes::from_slice(env, b"stellarflow:bridge:unlock:v1"));
    payload.append(&Bytes::from_slice(env, &source_chain_id.to_be_bytes()));
    payload.append(&Bytes::from_slice(env, &nonce.to_be_bytes()));
    payload.append(&Bytes::from_slice(env, &proof_hash.to_array()));

    let recipient_hash = env.crypto().sha256(&recipient.to_xdr(env));
    payload.append(&Bytes::from_slice(env, &recipient_hash.to_array()));

    payload.append(&Bytes::from_slice(env, &amount.to_be_bytes()));

    env.crypto().sha256(&payload)
}

/// Verify a complete bridge unlock proof against the registered validator set.
pub fn verify_cross_chain_payload(
    env: &Env,
    source_chain_id: u32,
    nonce: u64,
    proof_hash: BytesN<32>,
    recipient: Address,
    amount: i128,
    signatures: Vec<(BytesN<32>, BytesN<64>)>,
) -> Result<(), ContractError> {
    if amount <= 0 {
        return Err(ContractError::BridgeInvalidAmount);
    }

    let threshold: u32 = env
        .storage()
        .instance()
        .get(&RelayerStorageKey::Threshold)
        .unwrap_or(0);

    if threshold == 0 {
        return Err(ContractError::InvalidProof);
    }

    if signatures.len() < threshold {
        return Err(ContractError::InvalidProof);
    }

    let nonce_key = RelayerStorageKey::SpentNonce(source_chain_id, nonce);

    if env.storage().persistent().has(&nonce_key) {
        return Err(ContractError::InvalidProof);
    }

    let digest =
        bridge_message_digest(env, source_chain_id, nonce, &proof_hash, &recipient, amount);

    let mut valid_count = 0u32;
    let mut seen_validators: Map<BytesN<32>, ()> = Map::new(env);

    for entry in signatures.iter() {
        let (pubkey, signature) = entry;
        let validator_key = RelayerStorageKey::Validator(pubkey.clone());

        if !env.storage().instance().has(&validator_key) {
            continue;
        }

        if seen_validators.contains_key(pubkey.clone()) {
            continue;
        }

        env.crypto().ed25519_verify(
            &pubkey,
            &Bytes::from_slice(env, &digest.to_array()),
            &signature,
        );

        seen_validators.set(pubkey, ());
        valid_count += 1;

        if valid_count >= threshold {
            break;
        }
    }

    if valid_count < threshold {
        return Err(ContractError::InvalidProof);
    }

    env.storage().persistent().set(&nonce_key, &true);
    crate::storage::extend_persistent_ttl(env, &nonce_key);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use soroban_sdk::testutils::Address as _;

    struct BridgeFixture {
        env: Env,
        admin: Address,
        keys: [SigningKey; 5],
        public_keys: [BytesN<32>; 5],
        recipient: Address,
        proof_hash: BytesN<32>,
    }

    impl BridgeFixture {
        fn new() -> Self {
            let env = Env::default();
            env.mock_all_auths();

            let admin = Address::generate(&env);
            let recipient = Address::generate(&env);

            env.storage().instance().set(
                &DATA_KEY,
                &ContractData {
                    admin: admin.clone(),
                    value: 0,
                },
            );

            let keys = [
                SigningKey::from_bytes(&[1; 32]),
                SigningKey::from_bytes(&[2; 32]),
                SigningKey::from_bytes(&[3; 32]),
                SigningKey::from_bytes(&[4; 32]),
                SigningKey::from_bytes(&[5; 32]),
            ];

            let public_keys = [
                BytesN::from_array(&env, &keys[0].verifying_key().to_bytes()),
                BytesN::from_array(&env, &keys[1].verifying_key().to_bytes()),
                BytesN::from_array(&env, &keys[2].verifying_key().to_bytes()),
                BytesN::from_array(&env, &keys[3].verifying_key().to_bytes()),
                BytesN::from_array(&env, &keys[4].verifying_key().to_bytes()),
            ];

            for public_key in public_keys.iter() {
                add_validator(&env, &admin, public_key.clone()).unwrap();
            }

            configure_threshold(&env, &admin, 3).unwrap();

            let proof_hash = BytesN::from_array(&env, &[7u8; 32]);

            Self {
                env,
                admin,
                keys,
                public_keys,
                recipient,
                proof_hash,
            }
        }

        fn digest(&self, nonce: u64, recipient: &Address, amount: i128) -> BytesN<32> {
            bridge_message_digest(&self.env, 42, nonce, &self.proof_hash, recipient, amount)
        }

        fn signatures(
            &self,
            nonce: u64,
            recipient: &Address,
            amount: i128,
            indexes: &[usize],
        ) -> Vec<(BytesN<32>, BytesN<64>)> {
            let digest = self.digest(nonce, recipient, amount);
            let mut signatures = Vec::new(&self.env);

            for &index in indexes {
                let signature = self.keys[index].sign(&digest.to_array());
                signatures.push_back((
                    self.public_keys[index].clone(),
                    BytesN::from_array(&self.env, &signature.to_bytes()),
                ));
            }

            signatures
        }
    }

    #[test]
    fn valid_three_of_five_threshold_is_accepted() {
        let fixture = BridgeFixture::new();
        let amount = 1_000i128;
        let signatures = fixture.signatures(1, &fixture.recipient, amount, &[0, 2, 4]);

        assert_eq!(
            verify_cross_chain_payload(
                &fixture.env,
                42,
                1,
                fixture.proof_hash.clone(),
                fixture.recipient.clone(),
                amount,
                signatures,
            ),
            Ok(())
        );
    }

    #[test]
    fn two_of_five_is_insufficient_for_three_of_five_threshold() {
        let fixture = BridgeFixture::new();
        let amount = 1_000i128;
        let signatures = fixture.signatures(2, &fixture.recipient, amount, &[0, 1]);

        assert_eq!(
            verify_cross_chain_payload(
                &fixture.env,
                42,
                2,
                fixture.proof_hash.clone(),
                fixture.recipient.clone(),
                amount,
                signatures,
            ),
            Err(ContractError::InvalidProof)
        );
    }

    #[test]
    fn duplicate_validator_does_not_count_twice() {
        let fixture = BridgeFixture::new();
        let amount = 1_000i128;
        let signatures = fixture.signatures(3, &fixture.recipient, amount, &[0, 0, 1]);

        assert_eq!(
            verify_cross_chain_payload(
                &fixture.env,
                42,
                3,
                fixture.proof_hash.clone(),
                fixture.recipient.clone(),
                amount,
                signatures,
            ),
            Err(ContractError::InvalidProof)
        );
    }

    #[test]
    fn unauthorized_valid_signature_does_not_count() {
        let fixture = BridgeFixture::new();
        let amount = 1_000i128;
        let outsider = SigningKey::from_bytes(&[99; 32]);
        let outsider_public =
            BytesN::from_array(&fixture.env, &outsider.verifying_key().to_bytes());

        let digest = fixture.digest(4, &fixture.recipient, amount);

        let mut signatures = Vec::new(&fixture.env);
        for index in [0usize, 1usize] {
            let signature = fixture.keys[index].sign(&digest.to_array());
            signatures.push_back((
                fixture.public_keys[index].clone(),
                BytesN::from_array(&fixture.env, &signature.to_bytes()),
            ));
        }

        let outsider_signature = outsider.sign(&digest.to_array());
        signatures.push_back((
            outsider_public,
            BytesN::from_array(&fixture.env, &outsider_signature.to_bytes()),
        ));

        assert_eq!(
            verify_cross_chain_payload(
                &fixture.env,
                42,
                4,
                fixture.proof_hash.clone(),
                fixture.recipient.clone(),
                amount,
                signatures,
            ),
            Err(ContractError::InvalidProof)
        );
    }

    #[test]
    #[should_panic]
    fn invalid_registered_validator_signature_is_rejected() {
        let fixture = BridgeFixture::new();
        let amount = 1_000i128;

        let mut signatures = Vec::new(&fixture.env);
        let digest = fixture.digest(5, &fixture.recipient, amount);

        let valid = fixture.keys[0].sign(&digest.to_array());

        signatures.push_back((
            fixture.public_keys[0].clone(),
            BytesN::from_array(&fixture.env, &valid.to_bytes()),
        ));
        signatures.push_back((
            fixture.public_keys[1].clone(),
            BytesN::from_array(&fixture.env, &[0u8; 64]),
        ));
        signatures.push_back((
            fixture.public_keys[2].clone(),
            BytesN::from_array(&fixture.env, &[0u8; 64]),
        ));

        let _ = verify_cross_chain_payload(
            &fixture.env,
            42,
            5,
            fixture.proof_hash.clone(),
            fixture.recipient.clone(),
            amount,
            signatures,
        );
    }

    #[test]
    fn failed_verification_does_not_consume_nonce() {
        let fixture = BridgeFixture::new();
        let amount = 1_000i128;

        let insufficient = fixture.signatures(9, &fixture.recipient, amount, &[0, 1]);

        assert_eq!(
            verify_cross_chain_payload(
                &fixture.env,
                42,
                9,
                fixture.proof_hash.clone(),
                fixture.recipient.clone(),
                amount,
                insufficient,
            ),
            Err(ContractError::InvalidProof)
        );

        let valid = fixture.signatures(9, &fixture.recipient, amount, &[0, 1, 2]);

        assert_eq!(
            verify_cross_chain_payload(
                &fixture.env,
                42,
                9,
                fixture.proof_hash.clone(),
                fixture.recipient.clone(),
                amount,
                valid,
            ),
            Ok(())
        );
    }

    #[test]
    fn replayed_nonce_is_rejected() {
        let fixture = BridgeFixture::new();
        let amount = 1_000i128;
        let signatures = fixture.signatures(6, &fixture.recipient, amount, &[0, 1, 2]);

        assert_eq!(
            verify_cross_chain_payload(
                &fixture.env,
                42,
                6,
                fixture.proof_hash.clone(),
                fixture.recipient.clone(),
                amount,
                signatures.clone(),
            ),
            Ok(())
        );

        assert_eq!(
            verify_cross_chain_payload(
                &fixture.env,
                42,
                6,
                fixture.proof_hash.clone(),
                fixture.recipient.clone(),
                amount,
                signatures,
            ),
            Err(ContractError::InvalidProof)
        );
    }

    #[test]
    fn source_chain_is_bound_to_signature_digest() {
        let fixture = BridgeFixture::new();
        let amount = 1_000i128;
        let signatures = fixture.signatures(10, &fixture.recipient, amount, &[0, 1, 2]);

        assert_eq!(
            verify_cross_chain_payload(
                &fixture.env,
                43,
                10,
                fixture.proof_hash.clone(),
                fixture.recipient.clone(),
                amount,
                signatures,
            ),
            Err(ContractError::InvalidProof)
        );
    }

    #[test]
    fn changed_recipient_invalidates_existing_signatures() {
        let fixture = BridgeFixture::new();
        let amount = 1_000i128;
        let signatures = fixture.signatures(7, &fixture.recipient, amount, &[0, 1, 2]);
        let changed_recipient = Address::generate(&fixture.env);

        assert!(verify_cross_chain_payload(
            &fixture.env,
            42,
            7,
            fixture.proof_hash.clone(),
            changed_recipient,
            amount,
            signatures,
        )
        .is_err());
    }

    #[test]
    fn changed_amount_invalidates_existing_signatures() {
        let fixture = BridgeFixture::new();
        let signatures = fixture.signatures(8, &fixture.recipient, 1_000, &[0, 1, 2]);

        assert!(verify_cross_chain_payload(
            &fixture.env,
            42,
            8,
            fixture.proof_hash.clone(),
            fixture.recipient.clone(),
            2_000,
            signatures,
        )
        .is_err());
    }

    #[test]
    fn zero_threshold_is_rejected() {
        let fixture = BridgeFixture::new();

        assert_eq!(
            configure_threshold(&fixture.env, &fixture.admin, 0),
            Err(ContractError::InvalidProof)
        );
    }

    #[test]
    fn duplicate_validator_registration_is_rejected() {
        let fixture = BridgeFixture::new();

        assert_eq!(
            add_validator(&fixture.env, &fixture.admin, fixture.public_keys[0].clone()),
            Err(ContractError::AlreadyRegistered)
        );
    }

    #[test]
    fn non_admin_cannot_modify_validator_set() {
        let fixture = BridgeFixture::new();
        let attacker = Address::generate(&fixture.env);
        let key = SigningKey::from_bytes(&[55; 32]);
        let public_key = BytesN::from_array(&fixture.env, &key.verifying_key().to_bytes());

        assert_eq!(
            add_validator(&fixture.env, &attacker, public_key),
            Err(ContractError::NotAdmin)
        );
    }
}
