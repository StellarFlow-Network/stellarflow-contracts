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

/// A nullifier is the output of the note's ZK circuit — a 32-byte field
/// element. It is unlinkable to the deposit note but unique per spend.
pub type Nullifier = BytesN<32>;

/// Sparse spent-nullifier tree depth. The path direction is derived from all
/// 256 bits of the nullifier, so every nullifier has a deterministic slot.
pub const SPENT_TREE_DEPTH: u32 = 256;

const SPENT_TREE_TTL_THRESHOLD: u32 = 5_000;
const SPENT_TREE_TTL_LEDGERS: u32 = 6_312_000;

#[contracttype]
#[derive(Clone)]
enum DataKey {
    /// Maps a spent nullifier -> unit marker. Presence = spent.
    Nullifier(Nullifier),
    /// Root of the sparse tree containing all spent nullifiers.
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
    /// The supplied sparse Merkle path does not prove the nullifier is unspent.
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
            .unwrap_or_else(|| super::spent_tree::empty_root(env))
    }

    pub fn store_spent_tree_root(env: &Env, root: &BytesN<32>) {
        let key = DataKey::SpentTreeRoot;
        env.storage().instance().set(&key, root);
        env.storage()
            .instance()
            .extend_ttl(SPENT_TREE_TTL_THRESHOLD, SPENT_TREE_TTL_LEDGERS);
    }
}

/// ---- Sparse spent-nullifier Merkle tree -------------------------------
///
/// A valid spend supplies a path proving that the nullifier's deterministic
/// leaf is empty in the current root. The leaf is then replaced with the
/// nullifier hash and the updated root is persisted in instance storage.
mod spent_tree {
    use super::*;

    fn hash_pair(env: &Env, left: &BytesN<32>, right: &BytesN<32>) -> BytesN<32> {
        let mut bytes = soroban_sdk::Bytes::new(env);
        bytes.append(&soroban_sdk::Bytes::from_slice(env, &left.to_array()));
        bytes.append(&soroban_sdk::Bytes::from_slice(env, &right.to_array()));
        env.crypto().sha256(&bytes)
    }

    fn nullifier_bit(nullifier: &Nullifier, level: u32) -> bool {
        let bytes = nullifier.to_array();
        let byte_index = 31 - (level / 8) as usize;
        let bit_index = level % 8;
        (bytes[byte_index] & (1 << bit_index)) != 0
    }

    pub fn empty_root(env: &Env) -> BytesN<32> {
        let mut root = BytesN::from_array(env, &[0u8; 32]);
        for _ in 0..SPENT_TREE_DEPTH {
            root = hash_pair(env, &root, &root);
        }
        root
    }

    /// Return the new root if `nullifier` is absent from `current_root`.
    /// An inclusion proof for the nullifier itself identifies a prior spend.
    pub fn verify_unspent_and_compute_root(
        env: &Env,
        nullifier: &Nullifier,
        path: &Vec<BytesN<32>>,
        current_root: &BytesN<32>,
    ) -> Result<BytesN<32>, NullifierError> {
        if nullifier == &BytesN::from_array(env, &[0u8; 32]) {
            return Err(NullifierError::InvalidMerkleProof);
        }

        if path.len() != SPENT_TREE_DEPTH {
            return Err(NullifierError::InvalidMerkleProof);
        }

        let mut empty_root = BytesN::from_array(env, &[0u8; 32]);
        let mut spent_root = nullifier.clone();
        for level in 0..SPENT_TREE_DEPTH {
            let sibling = path.get(level).ok_or(NullifierError::InvalidMerkleProof)?;
            if nullifier_bit(nullifier, level) {
                empty_root = hash_pair(env, &sibling, &empty_root);
                spent_root = hash_pair(env, &sibling, &spent_root);
            } else {
                empty_root = hash_pair(env, &empty_root, &sibling);
                spent_root = hash_pair(env, &spent_root, &sibling);
            }
        }

        if &spent_root == current_root {
            return Err(NullifierError::AlreadySpent);
        }
        if &empty_root != current_root {
            return Err(NullifierError::InvalidMerkleProof);
        }
        Ok(spent_root)
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
        spent_path: &Vec<BytesN<32>>,
    ) -> Result<BytesN<32>, NullifierError> {
        if storage::is_spent(env, nullifier) {
            return Err(NullifierError::AlreadySpent);
        }

        let updated_spent_root = spent_tree::verify_unspent_and_compute_root(
            env,
            nullifier,
            spent_path,
            &storage::spent_tree_root(env),
        )?;

        if !verify_zk_proof(proof, nullifier, public_inputs) {
            return Err(NullifierError::InvalidProof);
        }

        Ok(updated_spent_root)
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
        spent_path: Vec<BytesN<32>>,
        recipient: Address,
        amount: i128,
    ) -> Result<(), NullifierError> {
        let updated_spent_root =
            verifier::verify_withdrawal(&env, &nullifier, &proof, &public_inputs, &spent_path)?;

        storage::mark_spent(&env, &nullifier);
        storage::store_spent_tree_root(&env, &updated_spent_root);

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

    /// Return the current spent-nullifier Merkle root for proof construction.
    pub fn spent_tree_root(env: Env) -> BytesN<32> {
        storage::spent_tree_root(&env)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Ledger;

    fn nullifier(env: &Env, byte: u8) -> Nullifier {
        BytesN::from_array(env, &[byte; 32])
    }

    fn empty_path(env: &Env) -> Vec<BytesN<32>> {
        let mut path = Vec::new(env);
        let zero = BytesN::from_array(env, &[0u8; 32]);
        for _ in 0..SPENT_TREE_DEPTH {
            path.push_back(zero.clone());
        }
        path
    }

    #[test]
    fn verifies_absence_and_updates_root_for_next_ledger() {
        let env = Env::default();
        let nf = nullifier(&env, 7);
        let path = empty_path(&env);
        let empty_root = spent_tree::empty_root(&env);

        let updated_root =
            spent_tree::verify_unspent_and_compute_root(&env, &nf, &path, &empty_root).unwrap();
        assert_ne!(updated_root, empty_root);

        storage::store_spent_tree_root(&env, &updated_root);
        storage::mark_spent(&env, &nf);
        env.ledger().set_sequence_number(2);

        assert_eq!(storage::spent_tree_root(&env), updated_root);
        assert!(storage::is_spent(&env, &nf));
        assert_eq!(
            spent_tree::verify_unspent_and_compute_root(&env, &nf, &path, &updated_root),
            Err(NullifierError::AlreadySpent)
        );
    }

    #[test]
    fn rejects_invalid_or_wrong_length_paths() {
        let env = Env::default();
        let nf = nullifier(&env, 9);
        let root = spent_tree::empty_root(&env);
        let mut short_path = empty_path(&env);
        short_path.pop_back();

        assert_eq!(
            spent_tree::verify_unspent_and_compute_root(&env, &nf, &short_path, &root),
            Err(NullifierError::InvalidMerkleProof)
        );

        let mut invalid_path = empty_path(&env);
        invalid_path.set(0, nullifier(&env, 44));
        assert_eq!(
            spent_tree::verify_unspent_and_compute_root(&env, &nf, &invalid_path, &root),
            Err(NullifierError::InvalidMerkleProof)
        );
    }

    #[test]
    fn rejects_zero_nullifier() {
        let env = Env::default();
        let zero = BytesN::from_array(&env, &[0u8; 32]);
        assert_eq!(
            spent_tree::verify_unspent_and_compute_root(
                &env,
                &zero,
                &empty_path(&env),
                &spent_tree::empty_root(&env),
            ),
            Err(NullifierError::InvalidMerkleProof)
        );
    }
}
