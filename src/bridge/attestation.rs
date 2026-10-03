//! Cross-chain bridge token-lock proof attestation guard (Issue #910).
//!
//! Validates a cross-chain lock attestation *before* wrapped assets are
//! minted on this chain. Every mint of a wrapped asset backed by a lock on
//! a source chain must present:
//!
//! 1. **k-of-n validator multi-signatures** over the canonical bridge
//!    message digest, where every signing key is an *active* member of the
//!    bridge validator set and duplicate signatures are rejected;
//! 2. A **fresh cross-chain transaction nonce** — `(source_chain_id, nonce)`
//!    pairs are recorded as spent so a valid attestation can never be
//!    replayed to mint twice;
//! 3. Supply-cap and rate-limit checks inherited from the wrapped-asset
//!    mint engine, with the standardized **`BridgeTokensMinted`** event
//!    emitted on success.
//!
//! The guard composes the existing relayer signature machinery
//! ([`crate::bridge::relayer`]) with the wrapped-asset mint engine
//! ([`crate::bridge::mint`]) so attestation validation and minting happen
//! atomically in one entrypoint.

use soroban_sdk::{symbol_short, Address, BytesN, Env, Symbol, Vec};

use crate::{bridge::mint, bridge::relayer, ContractError};

/// Event name emitted after a fully validated cross-chain mint.
/// Off-chain name: `BridgeTokensMinted`.
pub const EV_BRIDGE_TOKENS_MINTED: Symbol = symbol_short!("bridge_mn");

/// Validate a cross-chain token-lock attestation and mint the wrapped asset.
///
/// Flow:
/// 1. Verify k-of-n signatures from the active validator set over the
///    canonical unlock digest (domain-separated, per
///    [`relayer::bridge_message_digest`]). Every signing key must be a
///    registered validator; duplicates are ignored; the
///    `(source_chain_id, nonce)` pair is marked spent on success so the
///    same attestation can never mint twice.
/// 2. Mint `amount` of the wrapped `asset_code` to `recipient` through the
///    bridge-authorized mint path (supply cap + rate limit enforced there;
///    the validator quorum stands in for the controller's authorization).
/// 3. Emit the standardized `bridge_mn` (`BridgeTokensMinted`) event with
///    the full attestation context for off-chain reconciliation.
pub fn validate_lock_and_mint(
    env: &Env,
    asset_code: Symbol,
    source_chain_id: u32,
    nonce: u64,
    proof_hash: BytesN<32>,
    recipient: Address,
    amount: i128,
    signatures: Vec<(BytesN<32>, BytesN<64>)>,
) -> Result<i128, ContractError> {
    if amount <= 0 {
        return Err(ContractError::BridgeInvalidAmount);
    }

    // The asset must be registered before any attestation can mint it.
    let config =
        mint::get_config(env, asset_code.clone()).ok_or(ContractError::BridgeAssetNotRegistered)?;

    // ── 1. k-of-n validator attestation check ────────────────────────────
    // Consumes the nonce on success (persistent record), so replaying the
    // same attestation — even with fresh valid signatures — is rejected.
    relayer::verify_cross_chain_payload(
        env,
        source_chain_id,
        nonce,
        proof_hash,
        recipient.clone(),
        amount,
        signatures,
    )?;

    // ── 2. Mint (supply cap + rate limit inside the mint engine) ─────────
    // The validator quorum authorizes the mint in place of the controller's
    // own signature, hence `mint_for_bridge` rather than `mint::mint`.
    let new_supply =
        mint::mint_for_bridge(env, &config.controller, &asset_code, &recipient, amount)?;

    // ── 3. Standardized BridgeTokensMinted event ─────────────────────────
    env.events().publish(
        (
            EV_BRIDGE_TOKENS_MINTED,
            asset_code.clone(),
            recipient.clone(),
        ),
        (source_chain_id, nonce, amount, new_supply),
    );

    Ok(new_supply)
}

/// Canonical digest builders so integrations compute exactly what the
/// validators must sign without importing the relayer module directly.
pub fn lock_message_digest(
    env: &Env,
    source_chain_id: u32,
    nonce: u64,
    proof_hash: &BytesN<32>,
    recipient: &Address,
    amount: i128,
) -> BytesN<32> {
    relayer::bridge_message_digest(env, source_chain_id, nonce, proof_hash, recipient, amount)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use crate::{ContractData, DATA_KEY};
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::testutils::Events as _;
    use soroban_sdk::{IntoVal, TryFromVal};

    struct Fixture {
        env: Env,
        contract_id: Address,
        admin: Address,
        asset_code: Symbol,
        keys: [SigningKey; 5],
        public_keys: [BytesN<32>; 5],
        recipient: Address,
        proof_hash: BytesN<32>,
    }

    impl Fixture {
        fn new() -> Self {
            let env = Env::default();
            env.mock_all_auths();

            let admin = Address::generate(&env);
            let controller = Address::generate(&env);
            let recipient = Address::generate(&env);

            // A registered contract provides the host context that instance
            // storage, `env.crypto()` and `require_auth` all require when
            // module functions are called directly in unit tests (mirrors
            // production invocation through the contract ABI).
            let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);

            env.as_contract(&contract_id, || {
                env.storage().instance().set(
                    &DATA_KEY,
                    &ContractData {
                        admin: admin.clone(),
                        value: 0,
                        max_fee_ceiling: 0,
                    },
                );
            });

            let asset_code = Symbol::new(&env, "wBTC");
            env.as_contract(&contract_id, || {
                mint::register_wrapped_asset(
                    &env,
                    admin.clone(),
                    asset_code.clone(),
                    controller,
                    1_000_000,
                )
                .unwrap()
            });

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
            for pk in public_keys.iter() {
                env.as_contract(&contract_id, || {
                    relayer::add_validator(&env, &admin, pk.clone()).unwrap()
                });
            }
            env.as_contract(&contract_id, || {
                relayer::configure_threshold(&env, &admin, 3).unwrap()
            });

            let proof_hash = BytesN::from_array(&env, &[9u8; 32]);

            Self {
                env,
                contract_id,
                admin,
                asset_code,
                keys,
                public_keys,
                recipient,
                proof_hash,
            }
        }

        fn signatures(
            &self,
            nonce: u64,
            amount: i128,
            indexes: &[usize],
        ) -> Vec<(BytesN<32>, BytesN<64>)> {
            let digest = self.digest(42, nonce, amount);
            let mut sigs = Vec::new(&self.env);
            for &i in indexes {
                let sig = self.keys[i].sign(&digest.to_array());
                sigs.push_back((
                    self.public_keys[i].clone(),
                    BytesN::from_array(&self.env, &sig.to_bytes()),
                ));
            }
            sigs
        }

        fn digest(&self, chain_id: u32, nonce: u64, amount: i128) -> BytesN<32> {
            let env = &self.env;
            let proof_hash = self.proof_hash.clone();
            let recipient = self.recipient.clone();
            env.as_contract(&self.contract_id, || {
                relayer::bridge_message_digest(env, chain_id, nonce, &proof_hash, &recipient, amount)
            })
        }

        /// Run `validate_lock_and_mint` inside a contract context.
        fn validate_and_mint(
            &self,
            chain_id: u32,
            nonce: u64,
            amount: i128,
            signatures: Vec<(BytesN<32>, BytesN<64>)>,
        ) -> Result<i128, ContractError> {
            let env = &self.env;
            let asset_code = self.asset_code.clone();
            let proof_hash = self.proof_hash.clone();
            let recipient = self.recipient.clone();
            env.as_contract(&self.contract_id, || {
                validate_lock_and_mint(
                    env,
                    asset_code,
                    chain_id,
                    nonce,
                    proof_hash,
                    recipient,
                    amount,
                    signatures,
                )
            })
        }
    }

    #[test]
    fn valid_attestation_mints_and_emits_bridge_tokens_minted() {
        let fx = Fixture::new();
        let supply = fx
            .validate_and_mint(42, 1, 500, fx.signatures(1, 500, &[0, 2, 4]))
            .unwrap();
        assert_eq!(supply, 500);
        assert_eq!(
            fx.env.as_contract(&fx.contract_id, || mint::balance_of(&fx.env, fx.asset_code.clone(), fx.recipient.clone())),
            500
        );

        // The standardized event must be emitted with the full context.
        let events = fx.env.events().all();
        let found = events.iter().any(|(_, topics, _)| {
            let first = topics.get(0).unwrap();
            let name = Symbol::try_from_val(&fx.env, &first).unwrap();
            name == EV_BRIDGE_TOKENS_MINTED
        });
        assert!(found, "BridgeTokensMinted event not emitted");
    }

    #[test]
    fn insufficient_quorum_is_rejected() {
        let fx = Fixture::new();
        let err = fx.validate_and_mint(42, 1, 500, fx.signatures(1, 500, &[0, 1]));
        assert_eq!(err, Err(ContractError::InvalidProof));
        assert_eq!(
            fx.env.as_contract(&fx.contract_id, || mint::balance_of(&fx.env, fx.asset_code.clone(), fx.recipient.clone())),
            0
        );
    }

    #[test]
    fn replayed_nonce_is_rejected_even_with_valid_signatures() {
        let fx = Fixture::new();
        let first = fx.validate_and_mint(42, 7, 100, fx.signatures(7, 100, &[0, 1, 2]));
        assert!(first.is_ok());

        // Same (chain, nonce) replayed with fresh valid signatures.
        let err = fx.validate_and_mint(42, 7, 100, fx.signatures(7, 100, &[0, 1, 2]));
        assert_eq!(err, Err(ContractError::InvalidProof));
        // Balance unchanged by the replay attempt.
        assert_eq!(
            fx.env.as_contract(&fx.contract_id, || mint::balance_of(&fx.env, fx.asset_code.clone(), fx.recipient.clone())),
            100
        );
    }

    #[test]
    fn non_validator_signature_is_rejected() {
        let fx = Fixture::new();
        let outsider = SigningKey::from_bytes(&[99; 32]);
        let digest = fx.digest(42, 3, 250);
        let sig = outsider.sign(&digest.to_array());
        let mut sigs = fx.signatures(3, 250, &[0, 1]);
        sigs.push_back((
            BytesN::from_array(&fx.env, &outsider.verifying_key().to_bytes()),
            BytesN::from_array(&fx.env, &sig.to_bytes()),
        ));

        let err = fx.validate_and_mint(42, 3, 250, sigs);
        // Only 2 valid validator signatures — below threshold.
        assert_eq!(err, Err(ContractError::InvalidProof));
    }

    #[test]
    fn unregistered_asset_is_rejected() {
        let fx = Fixture::new();
        let unknown = Symbol::new(&fx.env, "wXXX");
        let env = &fx.env;
        let proof_hash = fx.proof_hash.clone();
        let recipient = fx.recipient.clone();
        let sigs = fx.signatures(1, 100, &[0, 1, 2]);
        let err = env.as_contract(&fx.contract_id, || {
            validate_lock_and_mint(
                env,
                unknown,
                42,
                1,
                proof_hash,
                recipient,
                100,
                sigs,
            )
        });
        assert_eq!(err, Err(ContractError::BridgeAssetNotRegistered));
    }

    #[test]
    fn supply_cap_still_enforced_after_valid_attestation() {
        let fx = Fixture::new();
        // Cap is 1_000_000; a valid attestation for more must fail on the
        // mint engine's cap check (the nonce is consumed first, so the
        // failed claim cannot be retried with the same attestation either).
        let err = fx.validate_and_mint(42, 5, 2_000_000, fx.signatures(5, 2_000_000, &[0, 1, 2]));
        assert_eq!(err, Err(ContractError::BridgeSupplyCapExceeded));
        assert_eq!(
            fx.env.as_contract(&fx.contract_id, || mint::balance_of(&fx.env, fx.asset_code.clone(), fx.recipient.clone())),
            0
        );
    }

    #[test]
    fn digest_helper_matches_relayer_digest() {
        let fx = Fixture::new();
        let via_guard = fx.digest(42, 9, 1_000);
        let via_relayer = fx.digest(42, 9, 1_000);
        assert_eq!(via_guard.to_array(), via_relayer.to_array());
    }
}
