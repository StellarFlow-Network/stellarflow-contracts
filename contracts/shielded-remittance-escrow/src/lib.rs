//! Shielded Remittance Escrow — Deposit Note Nullifier Verifier
//!
//! Implements double-spend protection for private cross-border remittance
//! withdrawals via zero-knowledge nullifiers, following the standard
//! shielded-pool pattern (nullifier = deterministic hash derived from the
//! spent note + spender's secret, revealed only at withdrawal time so it
//! can never be linked back to the original deposit).
//!
//! Responsibilities are split into three layers:
//!   - `storage`   : persistent nullifier-set access (single responsibility)
//!   - `verifier`  : pure verification logic (no I/O side effects)
//!   - `contract`  : public entrypoint orchestrating verify -> record -> emit
//!
//! Event emission follows this repo's existing convention (see
//! stop-loss-trigger's `trig_reg` event) of `env.events().publish((topics),
//! data)` rather than the newer `#[contractevent]` derive macro, since this
//! workspace pins `soroban-sdk = "=20.0.0"`.

#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, BytesN, Env, Vec,
};

const SPENT_TREE_DEPTH: u32 = 20;
const INSTANCE_TTL_THRESHOLD: u32 = 1_000_000;
const INSTANCE_TTL_LEDGERS: u32 = 6_312_000;

fn hash_nodes(env: &Env, left: &BytesN<32>, right: &BytesN<32>) -> BytesN<32> {
    let mut data = soroban_sdk::Bytes::from_slice(env, &left.to_array());
    data.append(&soroban_sdk::Bytes::from_slice(env, &right.to_array()));
    env.crypto().sha256(&data)
}

/// A nullifier is the output of the note's ZK circuit — a 32-byte field
/// element. It is unlinkable to the deposit note but unique per spend.
pub type Nullifier = BytesN<32>;

#[contracttype]
#[derive(Clone)]
enum DataKey {
    /// Maps a spent nullifier -> unit marker. Presence = spent.
    Nullifier(Nullifier),
    /// Latest root of the spent-nullifier Merkle tree.
    SpentTreeRoot,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum NullifierError {
    /// The nullifier has already been recorded — this note was already withdrawn.
    AlreadySpent = 1,
    /// The supplied ZK proof did not verify against the nullifier/public inputs.
    InvalidProof = 2,
    /// The nullifier's Merkle path is invalid or does not prove an empty leaf.
    InvalidMerkleProof = 3,
}

/// ---- Storage layer ---------------------------------------------------
/// Isolated so the persistence mechanism (instance vs persistent storage,
/// TTL policy, key layout) can change without touching verification logic.
mod storage {
    use super::*;

    const NULLIFIER_TTL_LEDGERS: u32 = 6_312_000; // ~1 year at 5s/ledger
    const NULLIFIER_TTL_THRESHOLD: u32 = 1_000_000;

    pub fn is_spent(env: &Env, nullifier: &Nullifier) -> bool {
        env.storage()
            .persistent()
            .has(&DataKey::Nullifier(nullifier.clone()))
    }

    pub fn mark_spent(env: &Env, nullifier: &Nullifier) {
        let key = DataKey::Nullifier(nullifier.clone());
        env.storage().persistent().set(&key, &true);
        env.storage()
            .persistent()
            .extend_ttl(&key, NULLIFIER_TTL_THRESHOLD, NULLIFIER_TTL_LEDGERS);
    }

    pub fn spent_tree_root(env: &Env) -> BytesN<32> {
        env.storage()
            .instance()
            .get(&DataKey::SpentTreeRoot)
            .unwrap_or_else(|| empty_hash(env, SPENT_TREE_DEPTH))
    }

    pub fn mark_spent_in_tree(env: &Env, root: &BytesN<32>) {
        env.storage().instance().set(&DataKey::SpentTreeRoot, root);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
    }

    fn empty_hash(env: &Env, level: u32) -> BytesN<32> {
        let mut hash = BytesN::from_array(env, &[0; 32]);
        for _ in 0..level {
            hash = hash_nodes(env, &hash, &hash);
        }
        hash
    }
}

/// ---- Verification layer -----------------------------------------------
/// Pure(ish) checks — no storage writes happen here, only reads plus proof
/// verification, so this can be unit tested independently of contract state
/// transitions.
mod verifier {
    use super::*;

    /// Checks the nullifier hasn't been spent before AND that the caller's
    /// ZK proof is valid for the given public inputs. Order matters: fail
    /// fast on the cheap check (storage read) before the expensive one
    /// (proof verification).
    pub fn verify_withdrawal(
        env: &Env,
        nullifier: &Nullifier,
        proof: &BytesN<256>,
        public_inputs: &BytesN<32>,
        path: &Vec<BytesN<32>>,
        leaf_index: u32,
    ) -> Result<BytesN<32>, NullifierError> {
        if storage::is_spent(env, nullifier) {
            return Err(NullifierError::AlreadySpent);
        }

        let updated_root = verify_and_update_spent_tree(env, nullifier, path, leaf_index)?;

        if !verify_zk_proof(proof, nullifier, public_inputs) {
            return Err(NullifierError::InvalidProof);
        }

        Ok(updated_root)
    }

    /// Verify an empty leaf against the latest spent root, then compute the
    /// root that results from inserting this nullifier at that leaf.
    fn verify_and_update_spent_tree(
        env: &Env,
        nullifier: &Nullifier,
        path: &Vec<BytesN<32>>,
        leaf_index: u32,
    ) -> Result<BytesN<32>, NullifierError> {
        if path.len() != SPENT_TREE_DEPTH {
            return Err(NullifierError::InvalidMerkleProof);
        }

        let current_root = storage::spent_tree_root(env);
        let empty_leaf = BytesN::from_array(env, &[0; 32]);
        let computed_empty_root = compute_root(env, &empty_leaf, path, leaf_index)?;
        if computed_empty_root != current_root {
            return Err(NullifierError::InvalidMerkleProof);
        }

        compute_root(env, nullifier, path, leaf_index)
    }

    fn compute_root(
        env: &Env,
        leaf: &BytesN<32>,
        path: &Vec<BytesN<32>>,
        leaf_index: u32,
    ) -> Result<BytesN<32>, NullifierError> {
        if path.len() != SPENT_TREE_DEPTH || leaf_index >= (1u32 << SPENT_TREE_DEPTH) {
            return Err(NullifierError::InvalidMerkleProof);
        }

        let mut current = leaf.clone();
        let mut index = leaf_index;
        for level in 0..SPENT_TREE_DEPTH {
            let sibling = path.get(level).ok_or(NullifierError::InvalidMerkleProof)?;
            current = if index & 1 == 0 {
                hash_nodes(env, &current, &sibling)
            } else {
                hash_nodes(env, &sibling, &current)
            };
            index >>= 1;
        }
        Ok(current)
    }

    /// Placeholder for the actual proof system integration (e.g. Groth16 /
    /// PLONK verifier over BLS12-381). Wire this up to your circuit's
    /// verifying key before deploying — this stub always rejects so the
    /// contract fails closed rather than silently accepting unverified
    /// withdrawals.
    fn verify_zk_proof(
        _proof: &BytesN<256>,
        _nullifier: &Nullifier,
        _public_inputs: &BytesN<32>,
    ) -> bool {
        // TODO: integrate real verifying key + pairing check.
        false
    }
}

#[contract]
pub struct NullifierVerifier;

#[contractimpl]
impl NullifierVerifier {
    /// Executes a shielded withdrawal:
    ///   1. Verify the nullifier is unspent and the proof is valid.
    ///   2. Record the nullifier so it can never be replayed.
    ///   3. Emit an anonymous payout event for indexers.
    ///
    /// `proof` and `public_inputs` are opaque to this module — they're
    /// handed to the ZK verifying key. `recipient`/`amount` are the only
    /// non-anonymous data in the whole flow, by design (someone has to
    /// receive the funds).
    pub fn withdraw(
        env: Env,
        nullifier: Nullifier,
        proof: BytesN<256>,
        public_inputs: BytesN<32>,
        nullifier_path: Vec<BytesN<32>>,
        leaf_index: u32,
        recipient: Address,
        amount: i128,
    ) -> Result<(), NullifierError> {
        let updated_spent_root = verifier::verify_withdrawal(
            &env,
            &nullifier,
            &proof,
            &public_inputs,
            &nullifier_path,
            leaf_index,
        )?;

        storage::mark_spent(&env, &nullifier);
        storage::mark_spent_in_tree(&env, &updated_spent_root);

        // Anonymous payout event: topics carry only the event tag and the
        // nullifier (spend-uniqueness marker, unlinkable to the deposit).
        // Data carries recipient + amount — the only fields that must be
        // public for the payout to be indexable at all.
        env.events()
            .publish((symbol_short!("payout"), nullifier), (recipient, amount));

        Ok(())
    }

    /// Read-only check exposed for off-chain callers / indexers who want to
    /// pre-flight a nullifier before submitting a withdrawal tx.
    pub fn is_nullifier_spent(env: Env, nullifier: Nullifier) -> bool {
        storage::is_spent(&env, &nullifier)
    }

    /// Return the current spent-tree root, or the canonical empty-tree root
    /// before the first nullifier is recorded.
    pub fn spent_tree_root(env: Env) -> BytesN<32> {
        storage::spent_tree_root(&env)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Ledger;

    fn empty_sibling(env: &Env, level: u32) -> BytesN<32> {
        let mut hash = BytesN::from_array(env, &[0; 32]);
        for _ in 0..level {
            hash = hash_nodes(env, &hash, &hash);
        }
        hash
    }

    #[test]
    fn verifies_empty_leaf_and_persists_updated_spent_root() {
        let env = Env::default();
        let nullifier = BytesN::from_array(&env, &[7; 32]);
        let mut path = Vec::new(&env);
        for level in 0..SPENT_TREE_DEPTH {
            path.push_back(empty_sibling(&env, level));
        }

        let old_root = storage::spent_tree_root(&env);
        let updated_root = verifier::verify_and_update_spent_tree(&env, &nullifier, &path, 0)
            .expect("valid empty-leaf proof");
        assert_ne!(updated_root, old_root);

        storage::mark_spent(&env, &nullifier);
        storage::mark_spent_in_tree(&env, &updated_root);
        assert_eq!(storage::spent_tree_root(&env), updated_root);
        assert!(storage::is_spent(&env, &nullifier));

        // The previous empty-tree path is stale once the root changes.
        let other_nullifier = BytesN::from_array(&env, &[8; 32]);
        assert_eq!(
            verifier::verify_and_update_spent_tree(&env, &other_nullifier, &path, 0),
            Err(NullifierError::InvalidMerkleProof)
        );
    }

    #[test]
    fn rejects_spent_nullifier_even_in_a_later_ledger() {
        let env = Env::default();
        let nullifier = BytesN::from_array(&env, &[9; 32]);
        storage::mark_spent(&env, &nullifier);
        env.ledger().set_sequence_number(2);

        assert_eq!(
            verifier::verify_and_update_spent_tree(&env, &nullifier, &Vec::new(&env), 0,),
            Err(NullifierError::AlreadySpent)
        );
    }

    #[test]
    fn rejects_invalid_or_wrong_depth_merkle_paths() {
        let env = Env::default();
        let nullifier = BytesN::from_array(&env, &[11; 32]);
        assert_eq!(
            verifier::verify_and_update_spent_tree(&env, &nullifier, &Vec::new(&env), 0),
            Err(NullifierError::InvalidMerkleProof)
        );

        let mut path = Vec::new(&env);
        for level in 0..SPENT_TREE_DEPTH {
            path.push_back(empty_sibling(&env, level));
        }
        path.set(0, BytesN::from_array(&env, &[0xff; 32]));
        assert_eq!(
            verifier::verify_and_update_spent_tree(&env, &nullifier, &path, 0),
            Err(NullifierError::InvalidMerkleProof)
        );
    }

    #[test]
    fn invalid_zk_proof_does_not_advance_spent_tree() {
        let env = Env::default();
        let nullifier = BytesN::from_array(&env, &[13; 32]);
        let proof = BytesN::from_array(&env, &[0; 256]);
        let public_inputs = BytesN::from_array(&env, &[0; 32]);
        let mut path = Vec::new(&env);
        for level in 0..SPENT_TREE_DEPTH {
            path.push_back(empty_sibling(&env, level));
        }
        let original_root = storage::spent_tree_root(&env);

        assert_eq!(
            verifier::verify_withdrawal(&env, &nullifier, &proof, &public_inputs, &path, 0,),
            Err(NullifierError::InvalidProof)
        );
        assert_eq!(storage::spent_tree_root(&env), original_root);
        assert!(!storage::is_spent(&env, &nullifier));
    }
}
