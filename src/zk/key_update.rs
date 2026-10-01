//! Timelocked update handler for ZK verification keys (Issue #931).
//!
//! Private remittance deposits are verified on-chain against a Groth16
//! verification key (`VK`). Rotating that key is a high-privilege operation:
//! a malicious or malformed key could let forged deposit notes pass
//! verification. This module wraps every rotation in the protocol's governance
//! timelock and validates the structural integrity of the uploaded key
//! material both when it is queued and again immediately before it is committed
//! to persistent storage.
//!
//! # Flow
//!
//! 1. `queue_verification_key_update` validates the new verification key and
//!    its associated proving key, assigns the next monotonic key version, and
//!    stores the proposal with `execute_not_before = now + 48h`.
//! 2. `cancel_verification_key_update` lets the admin veto the pending update
//!    at any time before it executes.
//! 3. After the timelock elapses, `execute_verification_key_update` re-runs
//!    every structural check, commits the key through the existing
//!    [`crate::zk::verifier`] registry, bumps the stored version, and emits a
//!    `ZKVerificationKeysUpdated` event carrying the version identifier.
//!
//! Only one update per circuit may be in flight at a time; a second queue call
//! for the same circuit returns [`ContractError::AdminChangePending`].

use soroban_sdk::{contracttype, Address, BytesN, Env, Symbol};

use crate::zk::proving_key::{self, ProvingKeySchema, UploadedProvingKey};
use crate::zk::verifier::{self, VerificationKey};
use crate::ContractError;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Mandatory governance delay between queueing and executing a key update.
///
/// Kept in lock-step with the protocol's shared admin-action timelock
/// (`crate::admin::action_queue::ADMIN_ACTION_DELAY_SECONDS`) so every
/// high-privilege parameter change observes the same 48-hour review window.
pub const ZK_KEY_UPDATE_DELAY_SECONDS: u64 = 48 * 60 * 60;

/// Persistent-storage TTL threshold (ledgers) for queued updates and versions.
const ZK_KEY_TTL_THRESHOLD: u32 = 5_000;

/// Persistent-storage TTL (ledgers) for queued updates and versions.
const ZK_KEY_TTL_LEDGERS: u32 = 100_000;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Storage keys for the timelocked verification-key update handler.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ZKKeyUpdateStorageKey {
    /// Pending (queued but not yet executed) update for a circuit.
    Pending(BytesN<32>),
    /// Latest committed key version for a circuit.
    Version(BytesN<32>),
}

/// A queued, not-yet-executed verification-key rotation.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZKVerificationKeyUpdate {
    /// Circuit whose verification key is being rotated.
    pub circuit_id: BytesN<32>,
    /// Version identifier assigned to this rotation (monotonic per circuit).
    pub version: u32,
    /// The new verification key parameters `VK_new`.
    pub vkey: VerificationKey,
    /// Proving key uploaded alongside the verification key, whose structural
    /// integrity is checked before committing.
    pub proving_key: UploadedProvingKey,
    /// Circuit dimensions used to validate the proving-key payload length.
    pub schema: ProvingKeySchema,
    /// Governance address that queued the update.
    pub proposer: Address,
    /// Ledger timestamp at which the update was queued.
    pub queued_at: u64,
    /// Earliest ledger timestamp at which the update may execute.
    pub execute_not_before: u64,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Queue a timelocked verification-key update.
///
/// The new verification key and its accompanying proving key are validated
/// *before* anything is written to persistent storage. A per-circuit monotonic
/// version is assigned at queue time so consumers can reason about ordering.
///
/// # Arguments
/// * `env` – The Soroban environment.
/// * `proposer` – Governance address that requested the rotation.
/// * `vkey` – The new verification key `VK_new`.
/// * `proving_key` – The proving key uploaded with this rotation.
/// * `schema` – Circuit dimensions for the proving-key payload.
///
/// # Returns
/// * `Ok(ZKVerificationKeyUpdate)` describing the queued rotation.
/// * `Err(ContractError::InvalidArgument)` if the verification key is malformed.
/// * `Err(ContractError::InvalidProvingKey)` if the proving key is malformed.
/// * `Err(ContractError::AdminChangePending)` if an update is already queued.
/// * `Err(ContractError::Overflow)` on version or deadline arithmetic overflow.
pub fn queue_verification_key_update(
    env: &Env,
    proposer: Address,
    vkey: VerificationKey,
    proving_key: UploadedProvingKey,
    schema: ProvingKeySchema,
) -> Result<ZKVerificationKeyUpdate, ContractError> {
    // ── Structural integrity: reject malformed material up-front. ─────────
    verifier::validate_verification_key(&vkey)?;
    proving_key::validate_proving_key(&proving_key, &schema)?;

    // ── One in-flight update per circuit. ────────────────────────────────
    let pending_key = ZKKeyUpdateStorageKey::Pending(vkey.circuit_id.clone());
    if env.storage().persistent().has(&pending_key) {
        return Err(ContractError::AdminChangePending);
    }

    // ── Assign the next monotonic version. ───────────────────────────────
    let current_version = get_verification_key_version(env, &vkey.circuit_id);
    let version = current_version
        .checked_add(1)
        .ok_or(ContractError::Overflow)?;

    let queued_at = env.ledger().timestamp();
    let execute_not_before = queued_at
        .checked_add(ZK_KEY_UPDATE_DELAY_SECONDS)
        .ok_or(ContractError::Overflow)?;

    let update = ZKVerificationKeyUpdate {
        circuit_id: vkey.circuit_id.clone(),
        version,
        vkey,
        proving_key,
        schema,
        proposer,
        queued_at,
        execute_not_before,
    };

    env.storage().persistent().set(&pending_key, &update);
    env.storage()
        .persistent()
        .extend_ttl(&pending_key, ZK_KEY_TTL_THRESHOLD, ZK_KEY_TTL_LEDGERS);

    Ok(update)
}

/// Cancel a pending verification-key update.
///
/// # Returns
/// * `Ok(())` if a pending update existed and was removed.
/// * `Err(ContractError::NoAdminChangePending)` if nothing was queued.
pub fn cancel_verification_key_update(
    env: &Env,
    circuit_id: &BytesN<32>,
) -> Result<(), ContractError> {
    let pending_key = ZKKeyUpdateStorageKey::Pending(circuit_id.clone());
    if !env.storage().persistent().has(&pending_key) {
        return Err(ContractError::NoAdminChangePending);
    }
    env.storage().persistent().remove(&pending_key);
    Ok(())
}

/// Execute a queued verification-key update once its timelock has elapsed.
///
/// Every structural check performed at queue time is repeated here — state may
/// have evolved and the payload must never reach persistent storage unverified.
/// On success the key is committed through [`verifier::register_verification_key`],
/// the per-circuit version is advanced, and a `ZKVerificationKeysUpdated` event
/// carrying the version identifier is emitted.
///
/// # Returns
/// * `Ok(u32)` – the newly committed version identifier.
/// * `Err(ContractError::NoAdminChangePending)` if nothing is queued.
/// * `Err(ContractError::AdminTimelockNotSatisfied)` if called too early.
/// * `Err(ContractError::InvalidArgument | InvalidProvingKey)` if the queued
///   material fails structural validation.
pub fn execute_verification_key_update(
    env: &Env,
    circuit_id: &BytesN<32>,
) -> Result<u32, ContractError> {
    let pending_key = ZKKeyUpdateStorageKey::Pending(circuit_id.clone());
    let update: ZKVerificationKeyUpdate = env
        .storage()
        .persistent()
        .get(&pending_key)
        .ok_or(ContractError::NoAdminChangePending)?;

    // ── Timelock gate. ───────────────────────────────────────────────────
    if env.ledger().timestamp() < update.execute_not_before {
        return Err(ContractError::AdminTimelockNotSatisfied);
    }

    // ── Re-validate structural integrity before committing. ──────────────
    verifier::validate_verification_key(&update.vkey)?;
    proving_key::validate_proving_key(&update.proving_key, &update.schema)?;

    // ── Commit to the on-chain verification-key registry. ────────────────
    verifier::register_verification_key(env, &update.vkey)?;

    // ── Persist the committed version identifier. ────────────────────────
    let version_key = ZKKeyUpdateStorageKey::Version(circuit_id.clone());
    env.storage()
        .persistent()
        .set(&version_key, &update.version);
    env.storage()
        .persistent()
        .extend_ttl(&version_key, ZK_KEY_TTL_THRESHOLD, ZK_KEY_TTL_LEDGERS);

    // ── Clear the queue entry. ───────────────────────────────────────────
    env.storage().persistent().remove(&pending_key);

    // ── Emit `ZKVerificationKeysUpdated` with the version identifier. ────
    env.events().publish(
        (
            Symbol::new(env, "ZKVerificationKeysUpdated"),
            circuit_id.clone(),
        ),
        update.version,
    );

    Ok(update.version)
}

/// Read the pending update for a circuit, if one is queued.
pub fn get_pending_verification_key_update(
    env: &Env,
    circuit_id: &BytesN<32>,
) -> Option<ZKVerificationKeyUpdate> {
    env.storage()
        .persistent()
        .get(&ZKKeyUpdateStorageKey::Pending(circuit_id.clone()))
}

/// Return the latest committed key version for a circuit (`0` if none).
pub fn get_verification_key_version(env: &Env, circuit_id: &BytesN<32>) -> u32 {
    env.storage()
        .persistent()
        .get(&ZKKeyUpdateStorageKey::Version(circuit_id.clone()))
        .unwrap_or(0u32)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Events as _, Ledger};
    use soroban_sdk::{Env, TryFromVal};

    /// Register the contract so tests run inside a real contract context —
    /// Soroban host storage is only reachable with an active contract ID.
    fn setup() -> (Env, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        (env, contract_id)
    }

    fn make_bytes32(env: &Env, seed: u8) -> BytesN<32> {
        let mut arr = [0u8; 32];
        arr[0] = seed;
        BytesN::from_array(env, &arr)
    }

    fn sample_vkey(env: &Env) -> VerificationKey {
        VerificationKey {
            alpha_beta_hash: make_bytes32(env, 0xAA),
            gamma_hash: make_bytes32(env, 0xBB),
            delta_hash: make_bytes32(env, 0xCC),
            ic_count: 1,
            ic_hash: make_bytes32(env, 0xDD),
            circuit_id: make_bytes32(env, 0x01),
        }
    }

    fn sample_schema() -> ProvingKeySchema {
        ProvingKeySchema {
            public_input_count: 1,
            variable_count: 3,
            constraint_count: 3,
        }
    }

    fn sample_proving_key(env: &Env, schema: &ProvingKeySchema) -> UploadedProvingKey {
        let mut generator_x = [0u8; 32];
        generator_x[31] = 1;
        let mut generator_y = [0u8; 32];
        generator_y[31] = 2;

        let len = proving_key::expected_payload_len(schema).unwrap();
        let mut payload = soroban_sdk::Bytes::new(env);
        for _ in 0..len {
            payload.push_back(0xA5);
        }

        UploadedProvingKey {
            generator_x: BytesN::from_array(env, &generator_x),
            generator_y: BytesN::from_array(env, &generator_y),
            payload,
        }
    }

    fn advance(env: &Env, delta: u64) {
        env.ledger().with_mut(|l| {
            l.timestamp += delta;
        });
    }

    #[test]
    fn queue_assigns_first_version_and_stores_pending() {
        let (env, contract_id) = setup();
        let proposer = Address::generate(&env);
        let vkey = sample_vkey(&env);
        let schema = sample_schema();
        let proving_key = sample_proving_key(&env, &schema);
        let circuit_id = vkey.circuit_id.clone();

        env.as_contract(&contract_id, || {
            let update = queue_verification_key_update(
                &env,
                proposer.clone(),
                vkey.clone(),
                proving_key,
                schema,
            )
            .unwrap();

            assert_eq!(update.version, 1);
            assert_eq!(
                update.execute_not_before,
                update.queued_at + ZK_KEY_UPDATE_DELAY_SECONDS
            );
            assert_eq!(update.proposer, proposer);
            assert!(get_pending_verification_key_update(&env, &circuit_id).is_some());
            assert_eq!(get_verification_key_version(&env, &circuit_id), 0);
        });
    }

    #[test]
    fn execute_before_timelock_is_rejected() {
        let (env, contract_id) = setup();
        let proposer = Address::generate(&env);
        let vkey = sample_vkey(&env);
        let schema = sample_schema();
        let proving_key = sample_proving_key(&env, &schema);
        let circuit_id = vkey.circuit_id.clone();

        env.as_contract(&contract_id, || {
            queue_verification_key_update(&env, proposer, vkey, proving_key, schema).unwrap();
            assert_eq!(
                execute_verification_key_update(&env, &circuit_id),
                Err(ContractError::AdminTimelockNotSatisfied)
            );
        });
    }

    #[test]
    fn execute_after_timelock_commits_key_and_bumps_version() {
        let (env, contract_id) = setup();
        let proposer = Address::generate(&env);
        let vkey = sample_vkey(&env);
        let schema = sample_schema();
        let proving_key = sample_proving_key(&env, &schema);
        let circuit_id = vkey.circuit_id.clone();

        env.as_contract(&contract_id, || {
            queue_verification_key_update(&env, proposer, vkey, proving_key, schema).unwrap();
        });
        advance(&env, ZK_KEY_UPDATE_DELAY_SECONDS + 1);
        env.as_contract(&contract_id, || {
            let version = execute_verification_key_update(&env, &circuit_id).unwrap();
            assert_eq!(version, 1);
            assert_eq!(get_verification_key_version(&env, &circuit_id), 1);
            assert!(get_pending_verification_key_update(&env, &circuit_id).is_none());

            // The key is now committed to the on-chain registry.
            assert!(verifier::get_verification_key(&env, &circuit_id).is_some());
        });
    }

    #[test]
    fn second_update_advances_version() {
        let (env, contract_id) = setup();
        let proposer = Address::generate(&env);
        let vkey = sample_vkey(&env);
        let schema = sample_schema();
        let circuit_id = vkey.circuit_id.clone();

        env.as_contract(&contract_id, || {
            queue_verification_key_update(
                &env,
                proposer.clone(),
                vkey.clone(),
                sample_proving_key(&env, &schema),
                schema.clone(),
            )
            .unwrap();
        });
        advance(&env, ZK_KEY_UPDATE_DELAY_SECONDS + 1);
        env.as_contract(&contract_id, || {
            assert_eq!(execute_verification_key_update(&env, &circuit_id), Ok(1));
            let second = queue_verification_key_update(
                &env,
                proposer,
                vkey,
                sample_proving_key(&env, &schema),
                schema,
            )
            .unwrap();
            assert_eq!(second.version, 2);
        });
    }

    #[test]
    fn duplicate_pending_update_is_rejected() {
        let (env, contract_id) = setup();
        let proposer = Address::generate(&env);
        let vkey = sample_vkey(&env);
        let schema = sample_schema();

        env.as_contract(&contract_id, || {
            queue_verification_key_update(
                &env,
                proposer.clone(),
                vkey.clone(),
                sample_proving_key(&env, &schema),
                schema.clone(),
            )
            .unwrap();

            let err = queue_verification_key_update(
                &env,
                proposer,
                vkey,
                sample_proving_key(&env, &schema),
                schema,
            )
            .unwrap_err();
            assert_eq!(err, ContractError::AdminChangePending);
        });
    }

    #[test]
    fn malformed_verification_key_is_rejected_at_queue_time() {
        let (env, contract_id) = setup();
        let proposer = Address::generate(&env);
        let mut vkey = sample_vkey(&env);
        vkey.gamma_hash = make_bytes32(&env, 0x00);
        let schema = sample_schema();
        let circuit_id = vkey.circuit_id.clone();

        env.as_contract(&contract_id, || {
            assert_eq!(
                queue_verification_key_update(
                    &env,
                    proposer,
                    vkey,
                    sample_proving_key(&env, &schema),
                    schema,
                ),
                Err(ContractError::InvalidArgument)
            );
            assert!(get_pending_verification_key_update(&env, &circuit_id).is_none());
        });
    }

    #[test]
    fn malformed_proving_key_is_rejected_at_queue_time() {
        let (env, contract_id) = setup();
        let proposer = Address::generate(&env);
        let vkey = sample_vkey(&env);
        let schema = sample_schema();
        let mut proving_key = sample_proving_key(&env, &schema);
        // Truncate the payload so it no longer matches the schema dimensions.
        proving_key.payload.pop_back();
        let circuit_id = vkey.circuit_id.clone();

        env.as_contract(&contract_id, || {
            assert_eq!(
                queue_verification_key_update(&env, proposer, vkey, proving_key, schema),
                Err(ContractError::InvalidProvingKey)
            );
            assert!(get_pending_verification_key_update(&env, &circuit_id).is_none());
        });
    }

    #[test]
    fn cancel_removes_pending_update() {
        let (env, contract_id) = setup();
        let proposer = Address::generate(&env);
        let vkey = sample_vkey(&env);
        let schema = sample_schema();
        let circuit_id = vkey.circuit_id.clone();

        env.as_contract(&contract_id, || {
            queue_verification_key_update(
                &env,
                proposer,
                vkey,
                sample_proving_key(&env, &schema),
                schema,
            )
            .unwrap();

            cancel_verification_key_update(&env, &circuit_id).unwrap();
            assert!(get_pending_verification_key_update(&env, &circuit_id).is_none());
            assert_eq!(
                cancel_verification_key_update(&env, &circuit_id),
                Err(ContractError::NoAdminChangePending)
            );
        });
    }

    #[test]
    fn execute_without_pending_update_is_rejected() {
        let (env, contract_id) = setup();
        let circuit_id = make_bytes32(&env, 0x42);
        env.as_contract(&contract_id, || {
            assert_eq!(
                execute_verification_key_update(&env, &circuit_id),
                Err(ContractError::NoAdminChangePending)
            );
        });
    }

    #[test]
    fn expected_payload_len_matches_groth16_layout() {
        // Sanity guard tying the schema-driven length to the fixed layout the
        // handler relies on (3 G1 + 2 G2 fixed, A + B-G1 + B-G2 + H + L queries).
        assert_eq!(
            proving_key::expected_payload_len(&sample_schema()),
            Ok(1_408)
        );
    }

    #[test]
    fn version_event_is_published_on_execute() {
        let (env, contract_id) = setup();
        let proposer = Address::generate(&env);
        let vkey = sample_vkey(&env);
        let schema = sample_schema();
        let circuit_id = vkey.circuit_id.clone();

        env.as_contract(&contract_id, || {
            queue_verification_key_update(
                &env,
                proposer,
                vkey,
                sample_proving_key(&env, &schema),
                schema,
            )
            .unwrap();
        });
        advance(&env, ZK_KEY_UPDATE_DELAY_SECONDS + 1);
        env.as_contract(&contract_id, || {
            execute_verification_key_update(&env, &circuit_id).unwrap();
        });

        let events = env.events().all();
        let expected = Symbol::new(&env, "ZKVerificationKeysUpdated");
        let found = events.iter().any(|(_, topics, _)| {
            topics.iter().any(|topic| {
                Symbol::try_from_val(&env, &topic)
                    .map(|symbol| symbol == expected)
                    .unwrap_or(false)
            })
        });
        assert!(found, "ZKVerificationKeysUpdated event was not emitted");
    }
}
