//! Poseidon Commitment Validation & Private Remittance Deposit Handler.
//!
//! This module implements the on-chain side of the ZK private remittance
//! deposit flow:
//!
//! 1. **Validate** the Poseidon commitment `C = Poseidon(r, v, k)` supplied
//!    by the depositor, where:
//!    - `r` – randomness / blinding factor (32 bytes)
//!    - `v` – transfer value / denomination (32-byte big-endian encoding)
//!    - `k` – secret spending key fragment (32 bytes)
//!
//!    On-chain we cannot run a native Poseidon permutation (no BN254 field
//!    arithmetic built-in to Soroban's host). The simulation used here
//!    mirrors the domain-separated SHA-256 construction that the off-chain
//!    circuits use as an equivalent: `SHA256("poseidon:commitment" || r || v || k)`.
//!    This is a **collision-resistant commitment scheme** — the prover cannot
//!    open a single `C` to two different `(r, v, k)` triplets.  When a real
//!    Poseidon host function is available the `poseidon_hash_rvk` function
//!    below is the only site that needs changing.
//!
//! 2. **Validate** structural constraints:
//!    - Amount `v` must be > 0.
//!    - The commitment must be non-zero (the zero leaf is reserved as the
//!      canonical empty tree node).
//!    - The commitment must not already exist in persistent storage (replay
//!      protection; one note per commitment).
//!
//! 3. **Compute the Merkle inclusion proof** for the newly inserted leaf:
//!    after inserting the leaf into the incremental Merkle tree the sibling
//!    path is reconstructed from the packed subtree state so it is returned
//!    to the caller and can be handed to the ZK circuit for proof generation.
//!
//! 4. **Insert** the commitment into the on-chain incremental Merkle tree
//!    managed by `crate::zk::merkle`.
//!
//! 5. **Emit** an anonymous deposit commitment event so indexers can track
//!    the anonymity set size without learning the note contents.

use soroban_sdk::{contracttype, symbol_short, Bytes, BytesN, Env, Symbol, Vec};

use crate::ContractError;
use crate::zk::merkle::{
    get_zero_hash, insert_deposit, verify_merkle_proof, get_packed_filled_subtree,
    get_current_root, TREE_DEPTH,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Domain separator prepended before hashing `(r, v, k)` to simulate the
/// Poseidon domain-separation tag used by the off-chain circuit.
const POSEIDON_DOMAIN: &[u8] = b"poseidon:commitment";

/// Maximum denominated deposit amount (2^63 - 1 stroops to stay in i128
/// safe-positive range while being far larger than any realistic deposit).
pub const MAX_DEPOSIT_AMOUNT: i128 = i64::MAX as i128;

/// Event topic for anonymous private deposit commitments.
/// `sym!("zkpc_dep")` — zk poseidon commitment deposit.
pub const EV_ZK_POSEIDON_DEPOSIT: Symbol = symbol_short!("zkpc_dep");

/// Storage key namespace for commitment replay-protection set.
const COMMITMENT_SET_PREFIX: &[u8] = b"zkpc";

// ---------------------------------------------------------------------------
// Storage Keys
// ---------------------------------------------------------------------------

/// Persistent storage key for individual commitment records.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PoseidonStorageKey {
    /// Boolean presence flag: commitment has been deposited.
    /// Key: `CommitmentDeposited(commitment_bytes)`.
    CommitmentDeposited(BytesN<32>),
}

// ---------------------------------------------------------------------------
// Public Input / Output Types
// ---------------------------------------------------------------------------

/// Parameters required to submit a private remittance deposit.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct DepositParams {
    /// Blinding factor `r` (32 bytes, chosen randomly by the depositor).
    pub randomness: BytesN<32>,
    /// Transfer value `v` encoded as a 32-byte big-endian unsigned integer.
    /// The actual token amount is encoded in `amount` below — this field is
    /// the circuit-visible denomination commitment input.
    pub value_commitment: BytesN<32>,
    /// Secret key fragment `k` (32 bytes, derived from the depositor's ZK key).
    pub spending_key: BytesN<32>,
    /// On-chain amount in stroops (must match the value encoded in `value_commitment`).
    pub amount: i128,
}

/// Result returned by a successful private deposit submission.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct DepositResult {
    /// The validated Poseidon commitment `C = Poseidon(r, v, k)`.
    pub commitment: BytesN<32>,
    /// Leaf index at which the commitment was inserted into the Merkle tree.
    pub leaf_index: u32,
    /// The new Merkle root after this commitment was inserted.
    pub new_root: BytesN<32>,
    /// Inclusion proof path: `TREE_DEPTH` sibling hashes from the leaf to the
    /// root. The depositor must hold onto this to generate their withdrawal ZK
    /// proof at spend time.
    pub proof_path: Vec<BytesN<32>>,
}

// ---------------------------------------------------------------------------
// Core: Poseidon Commitment Hash
// ---------------------------------------------------------------------------

/// Simulate `Poseidon(r, v, k)` via domain-separated SHA-256.
///
/// `C = SHA256(POSEIDON_DOMAIN || r_bytes || v_bytes || k_bytes)`
///
/// The domain separator enforces that deposit commitments cannot collide
/// with any other hash domain used in the contract (nullifiers, Merkle
/// nodes, etc.). When a native Poseidon host function becomes available,
/// replace this implementation — the public interface remains identical.
///
/// # Arguments
/// * `randomness`    – 32-byte blinding factor `r`
/// * `value_commit`  – 32-byte value commitment `v`
/// * `spending_key`  – 32-byte key fragment `k`
pub fn poseidon_hash_rvk(
    env: &Env,
    randomness: &BytesN<32>,
    value_commit: &BytesN<32>,
    spending_key: &BytesN<32>,
) -> BytesN<32> {
    let mut payload = Bytes::new(env);
    // Domain tag
    payload.append(&Bytes::from_slice(env, POSEIDON_DOMAIN));
    // r || v || k
    payload.append(&Bytes::from_slice(env, &randomness.to_array()));
    payload.append(&Bytes::from_slice(env, &value_commit.to_array()));
    payload.append(&Bytes::from_slice(env, &spending_key.to_array()));
    env.crypto().sha256(&payload)
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate that a deposit commitment is structurally sound and has not been
/// previously submitted (replay protection).
///
/// Checks:
/// 1. `amount > 0`
/// 2. `amount <= MAX_DEPOSIT_AMOUNT`
/// 3. `commitment != [0u8; 32]`  (zero leaf is reserved)
/// 4. The commitment does not already exist in persistent storage.
/// 5. The on-chain re-computation of `Poseidon(r, v, k)` matches `commitment`.
///
/// Returns `Ok(commitment)` on success.
pub fn validate_commitment(
    env: &Env,
    params: &DepositParams,
    commitment: &BytesN<32>,
) -> Result<(), ContractError> {
    // 1 & 2: amount range check
    if params.amount <= 0 {
        return Err(ContractError::AmountTooLow);
    }
    if params.amount > MAX_DEPOSIT_AMOUNT {
        return Err(ContractError::Overflow);
    }

    // 3: zero-leaf guard
    let zero = BytesN::from_array(env, &[0u8; 32]);
    if commitment == &zero {
        return Err(ContractError::InvalidProof);
    }

    // 4: replay protection — commitment must not already be deposited
    let key = PoseidonStorageKey::CommitmentDeposited(commitment.clone());
    if env.storage().persistent().has(&key) {
        return Err(ContractError::AlreadyRegistered);
    }

    // 5: on-chain re-derivation — the submitted commitment must equal
    //    Poseidon(r, v, k) as computed by the contract.
    let expected = poseidon_hash_rvk(
        env,
        &params.randomness,
        &params.value_commitment,
        &params.spending_key,
    );
    if &expected != commitment {
        return Err(ContractError::InvalidProof);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Merkle Proof Reconstruction
// ---------------------------------------------------------------------------

/// Reconstruct the Merkle inclusion proof path for a leaf at `leaf_index`
/// *before* that leaf was inserted (i.e. using the filled-subtree state that
/// existed just after insertion, which is what the incremental tree stores).
///
/// After `insert_deposit` completes the `PackedSubtrees` blob holds the
/// left-most un-paired hash at each level — exactly the siblings needed to
/// prove the most recently inserted leaf.
///
/// For level `l`:
/// - If `leaf_index` is a left child at level `l` (bit `l` of `leaf_index`
///   is 0) the sibling is `zero_hash(l)` — the right sub-tree is empty.
/// - If `leaf_index` is a right child (bit `l` is 1) the sibling is the
///   filled-subtree at level `l` (the left sibling that was stored when the
///   previous left child was inserted).
pub fn compute_inclusion_proof(
    env: &Env,
    leaf_index: u32,
) -> Vec<BytesN<32>> {
    let mut path = Vec::new(env);

    for level in 0..TREE_DEPTH {
        let is_right_child = ((leaf_index >> level) & 1) == 1;
        let sibling = if is_right_child {
            // Left sibling stored in packed subtree state
            get_packed_filled_subtree(env, level)
        } else {
            // Right sibling is the canonical zero node at this level
            get_zero_hash(env, level)
        };
        path.push_back(sibling);
    }

    path
}

// ---------------------------------------------------------------------------
// Main Entry Point: Private Remittance Deposit
// ---------------------------------------------------------------------------

/// Process a private remittance deposit:
///
/// 1. Re-derive and validate the Poseidon commitment `C = Poseidon(r, v, k)`.
/// 2. Mark the commitment as deposited (replay protection).
/// 3. Insert the commitment into the incremental Merkle tree.
/// 4. Reconstruct the Merkle inclusion proof path.
/// 5. Emit an anonymous `zkpc_dep` deposit event.
/// 6. Return the full `DepositResult` to the caller.
///
/// # Errors
/// - `ContractError::AmountTooLow`      — `amount <= 0`
/// - `ContractError::Overflow`          — `amount > MAX_DEPOSIT_AMOUNT`
/// - `ContractError::InvalidProof`      — commitment mismatch or zero-leaf
/// - `ContractError::AlreadyRegistered` — commitment already deposited
/// - `ContractError::CapacityExceeded`  — Merkle tree is full (2^20 leaves)
pub fn process_private_deposit(
    env: &Env,
    params: DepositParams,
) -> Result<DepositResult, ContractError> {
    // Step 1: Derive commitment C = Poseidon(r, v, k)
    let commitment = poseidon_hash_rvk(
        env,
        &params.randomness,
        &params.value_commitment,
        &params.spending_key,
    );

    // Step 2: Structural & replay validation
    validate_commitment(env, &params, &commitment)?;

    // Step 3: Mark as deposited (replay protection before insertion)
    let deposit_key = PoseidonStorageKey::CommitmentDeposited(commitment.clone());
    env.storage().persistent().set(&deposit_key, &true);
    env.storage()
        .persistent()
        .extend_ttl(&deposit_key, 5_000, 100_000);

    // Step 4: Insert into the incremental Merkle tree.
    // `insert_deposit` internally emits the `zk_dep` tree-level event and
    // records the new root in the historical root buffer.
    let (leaf_index, new_root) = insert_deposit(env, commitment.clone())?;

    // Step 5: Reconstruct the Merkle inclusion proof path for this leaf.
    // Must be called *after* insertion so the packed-subtree state reflects
    // the just-inserted leaf's left siblings.
    let proof_path = compute_inclusion_proof(env, leaf_index);

    // Step 6: Emit anonymous deposit commitment event.
    //
    // Topics: (EV_ZK_POSEIDON_DEPOSIT, commitment_hash)
    //   — commitment_hash is the only indexable field; it cannot be linked
    //     back to (r, v, k) without the depositor's secret.
    //
    // Data: (leaf_index, new_root, ledger_timestamp, amount)
    //   — amount is included so the on-chain anonymity set can be grouped
    //     by denomination (a standard privacy protocol practice), but no
    //     sender identity is revealed.
    emit_deposit_event(env, &commitment, leaf_index, &new_root, params.amount);

    Ok(DepositResult {
        commitment,
        leaf_index,
        new_root,
        proof_path,
    })
}

// ---------------------------------------------------------------------------
// Event Emission
// ---------------------------------------------------------------------------

/// Emit the anonymous private deposit commitment event.
///
/// Topics  : `(zkpc_dep, commitment)` — 2 indexed fields for RPC filtering.
/// Data    : `(leaf_index, new_root, timestamp, amount)`.
///
/// Callers outside this module can call this to re-emit the event in
/// exceptional recovery paths, but the canonical call-site is
/// `process_private_deposit`.
pub fn emit_deposit_event(
    env: &Env,
    commitment: &BytesN<32>,
    leaf_index: u32,
    new_root: &BytesN<32>,
    amount: i128,
) {
    env.events().publish(
        (EV_ZK_POSEIDON_DEPOSIT, commitment.clone()),
        (leaf_index, new_root.clone(), env.ledger().timestamp(), amount),
    );
}

// ---------------------------------------------------------------------------
// Read-Only Helpers
// ---------------------------------------------------------------------------

/// Returns `true` if the given commitment has already been deposited.
pub fn is_commitment_deposited(env: &Env, commitment: &BytesN<32>) -> bool {
    env.storage()
        .persistent()
        .has(&PoseidonStorageKey::CommitmentDeposited(commitment.clone()))
}

/// Returns the current Merkle tree root, or `None` if no deposits have
/// been made yet.
pub fn current_tree_root(env: &Env) -> Option<BytesN<32>> {
    get_current_root(env)
}

// ---------------------------------------------------------------------------
// Unit Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Ledger;

    fn make_bytes(env: &Env, byte: u8) -> BytesN<32> {
        BytesN::from_array(env, &[byte; 32])
    }

    fn default_params(env: &Env) -> DepositParams {
        let r = make_bytes(env, 0x11);
        let v = make_bytes(env, 0x22);
        let k = make_bytes(env, 0x33);
        DepositParams {
            randomness: r,
            value_commitment: v,
            spending_key: k,
            amount: 1_000_000,
        }
    }

    // ------------------------------------------------------------------
    // Poseidon hash simulation
    // ------------------------------------------------------------------

    #[test]
    fn test_poseidon_hash_determinism() {
        let env = Env::default();
        let r = make_bytes(&env, 0xaa);
        let v = make_bytes(&env, 0xbb);
        let k = make_bytes(&env, 0xcc);

        let c1 = poseidon_hash_rvk(&env, &r, &v, &k);
        let c2 = poseidon_hash_rvk(&env, &r, &v, &k);
        assert_eq!(c1, c2, "poseidon_hash_rvk must be deterministic");
    }

    #[test]
    fn test_poseidon_hash_domain_separation() {
        let env = Env::default();
        // Changing any input must produce a different hash.
        let r = make_bytes(&env, 0x01);
        let v = make_bytes(&env, 0x02);
        let k = make_bytes(&env, 0x03);

        let base = poseidon_hash_rvk(&env, &r, &v, &k);
        let diff_r = poseidon_hash_rvk(&env, &make_bytes(&env, 0xff), &v, &k);
        let diff_v = poseidon_hash_rvk(&env, &r, &make_bytes(&env, 0xff), &k);
        let diff_k = poseidon_hash_rvk(&env, &r, &v, &make_bytes(&env, 0xff));

        assert_ne!(base, diff_r, "randomness change must alter commitment");
        assert_ne!(base, diff_v, "value change must alter commitment");
        assert_ne!(base, diff_k, "key change must alter commitment");
    }

    // ------------------------------------------------------------------
    // Validation
    // ------------------------------------------------------------------

    #[test]
    fn test_validate_zero_amount_rejected() {
        let env = Env::default();
        env.ledger().set_timestamp(1_000_000);
        let mut params = default_params(&env);
        params.amount = 0;
        let commitment = poseidon_hash_rvk(
            &env,
            &params.randomness,
            &params.value_commitment,
            &params.spending_key,
        );
        assert_eq!(
            validate_commitment(&env, &params, &commitment),
            Err(ContractError::AmountTooLow)
        );
    }

    #[test]
    fn test_validate_negative_amount_rejected() {
        let env = Env::default();
        env.ledger().set_timestamp(1_000_000);
        let mut params = default_params(&env);
        params.amount = -1;
        let commitment = poseidon_hash_rvk(
            &env,
            &params.randomness,
            &params.value_commitment,
            &params.spending_key,
        );
        assert_eq!(
            validate_commitment(&env, &params, &commitment),
            Err(ContractError::AmountTooLow)
        );
    }

    #[test]
    fn test_validate_overflow_amount_rejected() {
        let env = Env::default();
        env.ledger().set_timestamp(1_000_000);
        let mut params = default_params(&env);
        params.amount = MAX_DEPOSIT_AMOUNT + 1;
        let commitment = poseidon_hash_rvk(
            &env,
            &params.randomness,
            &params.value_commitment,
            &params.spending_key,
        );
        assert_eq!(
            validate_commitment(&env, &params, &commitment),
            Err(ContractError::Overflow)
        );
    }

    #[test]
    fn test_validate_zero_commitment_rejected() {
        let env = Env::default();
        env.ledger().set_timestamp(1_000_000);
        let params = default_params(&env);
        let zero = BytesN::from_array(&env, &[0u8; 32]);
        assert_eq!(
            validate_commitment(&env, &params, &zero),
            Err(ContractError::InvalidProof)
        );
    }

    #[test]
    fn test_validate_mismatched_commitment_rejected() {
        let env = Env::default();
        env.ledger().set_timestamp(1_000_000);
        let params = default_params(&env);
        // Tampered commitment (flip one byte)
        let real_c = poseidon_hash_rvk(
            &env,
            &params.randomness,
            &params.value_commitment,
            &params.spending_key,
        );
        let mut tampered_arr = real_c.to_array();
        tampered_arr[0] ^= 0xff;
        let tampered = BytesN::from_array(&env, &tampered_arr);
        assert_eq!(
            validate_commitment(&env, &params, &tampered),
            Err(ContractError::InvalidProof)
        );
    }

    #[test]
    fn test_validate_duplicate_commitment_rejected() {
        let env = Env::default();
        env.ledger().set_timestamp(1_000_000);
        let params = default_params(&env);
        let commitment = poseidon_hash_rvk(
            &env,
            &params.randomness,
            &params.value_commitment,
            &params.spending_key,
        );
        // First submission succeeds
        assert!(validate_commitment(&env, &params, &commitment).is_ok());
        // Mark as deposited to simulate first deposit having been processed
        let key = PoseidonStorageKey::CommitmentDeposited(commitment.clone());
        env.storage().persistent().set(&key, &true);
        // Second submission must fail
        assert_eq!(
            validate_commitment(&env, &params, &commitment),
            Err(ContractError::AlreadyRegistered)
        );
    }

    // ------------------------------------------------------------------
    // Full deposit flow
    // ------------------------------------------------------------------

    #[test]
    fn test_process_private_deposit_success() {
        let env = Env::default();
        env.ledger().set_timestamp(1_000_000);
        let params = default_params(&env);

        let expected_commitment = poseidon_hash_rvk(
            &env,
            &params.randomness,
            &params.value_commitment,
            &params.spending_key,
        );

        let result = process_private_deposit(&env, params).unwrap();

        assert_eq!(result.commitment, expected_commitment);
        assert_eq!(result.leaf_index, 0);
        assert_eq!(result.proof_path.len() as u32, TREE_DEPTH);
        // Commitment is now marked as deposited
        assert!(is_commitment_deposited(&env, &result.commitment));
        // Root is stored
        assert_eq!(current_tree_root(&env), Some(result.new_root.clone()));
    }

    #[test]
    fn test_process_private_deposit_proof_path_verifies() {
        let env = Env::default();
        env.ledger().set_timestamp(1_000_000);
        let params = default_params(&env);

        let result = process_private_deposit(&env, params).unwrap();

        // The returned proof path must verify against the new root
        assert!(
            verify_merkle_proof(
                &env,
                &result.commitment,
                &result.proof_path,
                result.leaf_index,
                &result.new_root,
            ),
            "inclusion proof returned by process_private_deposit must verify"
        );
    }

    #[test]
    fn test_process_two_deposits_unique_leaf_indices() {
        let env = Env::default();
        env.ledger().set_timestamp(1_000_000);

        // First deposit
        let params1 = default_params(&env);
        let r1 = process_private_deposit(&env, params1).unwrap();
        assert_eq!(r1.leaf_index, 0);

        // Second deposit with different inputs
        let params2 = DepositParams {
            randomness: make_bytes(&env, 0xde),
            value_commitment: make_bytes(&env, 0xad),
            spending_key: make_bytes(&env, 0xbe),
            amount: 500_000,
        };
        let r2 = process_private_deposit(&env, params2).unwrap();
        assert_eq!(r2.leaf_index, 1);

        // Both proofs verify against their respective roots
        assert!(verify_merkle_proof(
            &env,
            &r2.commitment,
            &r2.proof_path,
            r2.leaf_index,
            &r2.new_root,
        ));
    }

    #[test]
    fn test_process_private_deposit_replay_rejected() {
        let env = Env::default();
        env.ledger().set_timestamp(1_000_000);
        let params = default_params(&env);

        // Clone params for second attempt (same inputs = same commitment)
        let params2 = params.clone();

        let _r1 = process_private_deposit(&env, params).unwrap();
        let err = process_private_deposit(&env, params2).unwrap_err();
        assert_eq!(err, ContractError::AlreadyRegistered);
    }

    // ------------------------------------------------------------------
    // Merkle proof reconstruction
    // ------------------------------------------------------------------

    #[test]
    fn test_compute_inclusion_proof_length() {
        let env = Env::default();
        env.ledger().set_timestamp(1_000_000);
        let params = default_params(&env);
        let result = process_private_deposit(&env, params).unwrap();
        assert_eq!(
            result.proof_path.len() as u32,
            TREE_DEPTH,
            "proof path must have exactly TREE_DEPTH siblings"
        );
    }

    #[test]
    fn test_inclusion_proof_multiple_leaves_all_verify() {
        let env = Env::default();
        env.ledger().set_timestamp(1_000_000);

        let mut results = Vec::new(&env);

        for i in 0u8..4 {
            let params = DepositParams {
                randomness: make_bytes(&env, i),
                value_commitment: make_bytes(&env, i.wrapping_add(10)),
                spending_key: make_bytes(&env, i.wrapping_add(20)),
                amount: (i as i128 + 1) * 100_000,
            };
            let r = process_private_deposit(&env, params).unwrap();
            results.push_back(r);
        }

        // Each deposit's proof should verify against the root *at the time of
        // that deposit* (the tree appends new leaves, so older proofs verify
        // against older roots which are stored in the historical root buffer).
        for i in 0..4u32 {
            let r = results.get(i).unwrap();
            // The proof was built for the root *after* this leaf was inserted.
            assert!(
                verify_merkle_proof(
                    &env,
                    &r.commitment,
                    &r.proof_path,
                    r.leaf_index,
                    &r.new_root,
                ),
                "proof for leaf {} must verify",
                i
            );
        }
    }
}
