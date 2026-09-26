//! ZK Deposit Commitment Merkle Tree Batch Insertion Module (Issue #956).
//!
//! Optimises gas overhead when inserting multiple ZK deposit note commitments
//! by processing an entire array `C = [c₁, c₂, … cₙ]` in a single subtree
//! update pass, recomputing the Merkle root only once after all leaves have
//! been committed.
//!
//! ## Design
//!
//! Single-leaf insertion calls `set_packed_filled_subtree` and
//! `record_historical_root` on every leaf, which means `n` inserts trigger `n`
//! root recomputations and `n` root-buffer writes.  The batch path amortises
//! that cost:
//!
//! 1. Validate the batch is non-empty and within the per-call cap.
//! 2. Check that the tree has room for all `n` leaves.
//! 3. Drive the incremental insertion loop for each commitment, updating the
//!    in-memory `filled_subtrees` array rather than writing to storage on every
//!    iteration.
//! 4. Flush the updated filled-subtree array to the packed storage blob in a
//!    single write.
//! 5. Record the final root in the historical root buffer once.
//! 6. Emit a single `ZKBatchCommitmentsAdded` event carrying the updated root.
//!
//! Storage writes per batch:
//! - `PackedSubtrees` blob             : 1 write  (O(TREE_DEPTH) bytes)
//! - `NextLeafIndex`                   : 1 write
//! - `RootRecord(new_root)`            : 1 write
//! - `CurrentRoot`                     : 1 write
//! - `RootBufferList`                  : 1 read + 1 write
//!
//! Compare with the naïve loop (5n writes for n leaves) – the batch path is
//! O(1) in terms of storage-write count regardless of `n`.

use soroban_sdk::{symbol_short, BytesN, Env, Symbol, Vec};

use crate::ContractError;
use crate::zk::merkle::{
    get_zero_hash, hash_nodes, record_historical_root, set_packed_filled_subtree,
    get_packed_filled_subtree, MerkleStorageKey, TREE_DEPTH,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of commitments accepted in a single batch call.
///
/// Chosen to keep the instruction budget well within Soroban's per-invocation
/// limit. Each additional leaf requires at most `TREE_DEPTH` (20) hash
/// operations; 64 × 20 = 1 280 SHA-256 calls — comfortably within budget.
pub const MAX_BATCH_SIZE: u32 = 64;

/// Event topic for batch ZK deposit commitment insertions.
pub const EV_ZK_BATCH_COMMIT: Symbol = symbol_short!("zk_batch");

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Return value from a successful batch insertion.
#[soroban_sdk::contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct BatchInsertResult {
    /// Leaf index of the **first** commitment inserted in this batch.
    pub first_leaf_index: u32,
    /// Leaf index of the **last** commitment inserted in this batch
    /// (= `first_leaf_index + count - 1`).
    pub last_leaf_index: u32,
    /// Number of commitments inserted.
    pub count: u32,
    /// The updated Merkle root after all insertions.
    pub new_root: BytesN<32>,
}

// ---------------------------------------------------------------------------
// Core batch insertion logic
// ---------------------------------------------------------------------------

/// Insert an array of ZK deposit commitments into the incremental Merkle tree
/// in a single batched subtree update.
///
/// ## Arguments
/// * `env`         – Soroban contract environment.
/// * `commitments` – Non-empty slice of 32-byte commitment hashes,
///                   `|commitments| ≤ MAX_BATCH_SIZE`.
///
/// ## Returns
/// `Ok(BatchInsertResult)` on success, or one of:
/// * `ContractError::AmountTooLow`      – `commitments` is empty.
/// * `ContractError::Overflow`          – `|commitments| > MAX_BATCH_SIZE`.
/// * `ContractError::MerkleTreeFull`    – Insufficient remaining leaf capacity.
///
/// ## Events
/// Emits a single `(EV_ZK_BATCH_COMMIT, new_root)` event upon success.
pub fn batch_insert_deposits(
    env: &Env,
    commitments: &Vec<BytesN<32>>,
) -> Result<BatchInsertResult, ContractError> {
    let count = commitments.len() as u32;

    // ── 1. Validate batch size ────────────────────────────────────────────
    if count == 0 {
        return Err(ContractError::AmountTooLow);
    }
    if count > MAX_BATCH_SIZE {
        return Err(ContractError::Overflow);
    }

    // ── 2. Capacity check ─────────────────────────────────────────────────
    let next_index_key = MerkleStorageKey::NextLeafIndex;
    let start_index: u32 = env
        .storage()
        .persistent()
        .get(&next_index_key)
        .unwrap_or(0);

    let max_leaves: u32 = 1u32
        .checked_shl(TREE_DEPTH)
        .ok_or(ContractError::Overflow)?;

    let end_index = start_index
        .checked_add(count)
        .ok_or(ContractError::Overflow)?;

    if end_index > max_leaves {
        return Err(ContractError::MerkleTreeFull);
    }

    // ── 3. Load filled-subtree cache into a local in-memory array ─────────
    // We read all TREE_DEPTH subtree hashes upfront so the main loop only
    // needs in-memory reads/writes, and we flush once at the end.
    let mut filled: [Option<BytesN<32>>; 20] = core::array::from_fn(|_| None);
    for level in 0..TREE_DEPTH {
        filled[level as usize] = Some(get_packed_filled_subtree(env, level));
    }

    // ── 4. Incremental insertion loop ─────────────────────────────────────
    // For each commitment we run the standard incremental path update but
    // read/write `filled[]` in memory rather than touching storage.
    let mut current_root = BytesN::from_array(env, &[0u8; 32]); // placeholder

    for i in 0..count {
        let commitment = commitments.get(i).ok_or(ContractError::Overflow)?;
        let leaf_index = start_index + i;

        let new_root = insert_one_in_memory(env, commitment, leaf_index, &mut filled)?;
        current_root = new_root;
    }

    // ── 5. Flush filled-subtree array to packed storage (single write) ────
    for level in 0..TREE_DEPTH {
        if let Some(ref hash) = filled[level as usize] {
            set_packed_filled_subtree(env, level, hash);
        }
    }

    // ── 6. Persist the updated leaf counter ───────────────────────────────
    env.storage()
        .persistent()
        .set(&next_index_key, &end_index);
    env.storage()
        .persistent()
        .extend_ttl(&next_index_key, 5_000, 100_000);

    // ── 7. Record the final root in the historical buffer (single write) ──
    record_historical_root(env, current_root.clone(), end_index);

    // ── 8. Emit the batch event ───────────────────────────────────────────
    env.events().publish(
        (EV_ZK_BATCH_COMMIT, current_root.clone()),
        (start_index, end_index - 1, count, env.ledger().timestamp()),
    );

    Ok(BatchInsertResult {
        first_leaf_index: start_index,
        last_leaf_index: end_index - 1,
        count,
        new_root: current_root,
    })
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Run one incremental insertion step entirely against the in-memory
/// `filled` array (no storage I/O except for zero-hash reads, which are
/// cached on first access by `get_zero_hash`).
///
/// Returns the new root hash after this leaf has been absorbed.
fn insert_one_in_memory(
    env: &Env,
    commitment: BytesN<32>,
    leaf_index: u32,
    filled: &mut [Option<BytesN<32>>; 20],
) -> Result<BytesN<32>, ContractError> {
    let mut current_hash = commitment;
    let mut current_index = leaf_index;

    for level in 0..TREE_DEPTH {
        let is_right_child = (current_index % 2) == 1;

        if !is_right_child {
            // This commitment becomes the new left sibling at this level.
            filled[level as usize] = Some(current_hash.clone());
            // Pair it with the zero hash to the right and propagate upward.
            let zero = get_zero_hash(env, level);
            current_hash = hash_nodes(env, &current_hash, &zero);
        } else {
            // Retrieve the left sibling stored at this level.
            let left_sibling = filled[level as usize]
                .clone()
                .unwrap_or_else(|| get_zero_hash(env, level));
            current_hash = hash_nodes(env, &left_sibling, &current_hash);
            // The filled slot is consumed (odd position always has a pair);
            // reset it so future insertions at this level start fresh.
            filled[level as usize] = None;
        }

        current_index /= 2;
    }

    Ok(current_hash)
}

// ---------------------------------------------------------------------------
// Unit Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zk::merkle::{
        get_current_root, get_total_deposits, insert_deposit, is_root_valid,
    };
    use soroban_sdk::testutils::Ledger;

    fn make_commitment(env: &Env, byte: u8) -> BytesN<32> {
        BytesN::from_array(env, &[byte; 32])
    }

    fn make_commitments(env: &Env, values: &[u8]) -> Vec<BytesN<32>> {
        let mut v = Vec::new(env);
        for &b in values {
            v.push_back(make_commitment(env, b));
        }
        v
    }

    // ── Validation tests ──────────────────────────────────────────────────

    #[test]
    fn rejects_empty_batch() {
        let env = Env::default();
        env.ledger().set_timestamp(1_000_000);

        let empty: Vec<BytesN<32>> = Vec::new(&env);
        let result = batch_insert_deposits(&env, &empty);
        assert_eq!(result, Err(ContractError::AmountTooLow));
    }

    #[test]
    fn rejects_batch_over_max_size() {
        let env = Env::default();
        env.ledger().set_timestamp(1_000_000);

        let mut over_max: Vec<BytesN<32>> = Vec::new(&env);
        for i in 0..=(MAX_BATCH_SIZE) {
            over_max.push_back(make_commitment(&env, (i % 250) as u8));
        }
        let result = batch_insert_deposits(&env, &over_max);
        assert_eq!(result, Err(ContractError::Overflow));
    }

    // ── Single-element batch consistency ─────────────────────────────────

    #[test]
    fn single_element_batch_matches_single_insert() {
        // Inserting a single commitment via batch must produce the identical
        // root as inserting via the scalar `insert_deposit` path.
        let env_single = Env::default();
        env_single.ledger().set_timestamp(1_000_000);
        env_single.ledger().set_sequence_number(100);

        let env_batch = Env::default();
        env_batch.ledger().set_timestamp(1_000_000);
        env_batch.ledger().set_sequence_number(100);

        let commitment = make_commitment(&env_single, 0xAB);

        let (_, root_single) = insert_deposit(&env_single, commitment.clone()).unwrap();

        let batch = make_commitments(&env_batch, &[0xAB]);
        let result = batch_insert_deposits(&env_batch, &batch).unwrap();

        assert_eq!(result.first_leaf_index, 0);
        assert_eq!(result.last_leaf_index, 0);
        assert_eq!(result.count, 1);
        assert_eq!(result.new_root, root_single,
            "batch(1) root must equal scalar insert root");
    }

    // ── Multi-leaf batch correctness ──────────────────────────────────────

    #[test]
    fn batch_root_matches_sequential_scalar_inserts() {
        // The root produced by batch-inserting [c0, c1, c2, c3] must equal
        // the root produced by four consecutive scalar inserts of the same
        // commitments, because the incremental algorithm is deterministic.
        let env_seq = Env::default();
        env_seq.ledger().set_timestamp(2_000_000);
        env_seq.ledger().set_sequence_number(200);

        let env_batch = Env::default();
        env_batch.ledger().set_timestamp(2_000_000);
        env_batch.ledger().set_sequence_number(200);

        let values = [0x01u8, 0x02, 0x03, 0x04];

        // Scalar reference
        let mut root_seq = BytesN::from_array(&env_seq, &[0u8; 32]);
        for &b in &values {
            let c = make_commitment(&env_seq, b);
            let (_, r) = insert_deposit(&env_seq, c).unwrap();
            root_seq = r;
        }

        // Batch
        let batch = make_commitments(&env_batch, &values);
        let result = batch_insert_deposits(&env_batch, &batch).unwrap();

        assert_eq!(result.count, 4);
        assert_eq!(result.first_leaf_index, 0);
        assert_eq!(result.last_leaf_index, 3);
        assert_eq!(result.new_root, root_seq,
            "batch(4) root must equal 4 sequential scalar insert roots");
    }

    // ── Leaf counter & root registration ─────────────────────────────────

    #[test]
    fn batch_advances_leaf_counter_correctly() {
        let env = Env::default();
        env.ledger().set_timestamp(1_000_000);
        env.ledger().set_sequence_number(100);

        assert_eq!(get_total_deposits(&env), 0);

        let batch1 = make_commitments(&env, &[0x10, 0x11, 0x12]);
        let r1 = batch_insert_deposits(&env, &batch1).unwrap();

        assert_eq!(get_total_deposits(&env), 3);
        assert_eq!(r1.first_leaf_index, 0);
        assert_eq!(r1.last_leaf_index, 2);

        let batch2 = make_commitments(&env, &[0x20, 0x21]);
        let r2 = batch_insert_deposits(&env, &batch2).unwrap();

        assert_eq!(get_total_deposits(&env), 5);
        assert_eq!(r2.first_leaf_index, 3);
        assert_eq!(r2.last_leaf_index, 4);
    }

    #[test]
    fn batch_root_is_recorded_and_valid() {
        let env = Env::default();
        env.ledger().set_timestamp(1_000_000);
        env.ledger().set_sequence_number(100);

        let batch = make_commitments(&env, &[0xAA, 0xBB, 0xCC]);
        let result = batch_insert_deposits(&env, &batch).unwrap();

        // Current root must be the batch result root
        assert_eq!(get_current_root(&env), Some(result.new_root.clone()));

        // Root must be in the historical buffer and valid (within window)
        assert!(
            is_root_valid(&env, &result.new_root),
            "batch root should be valid immediately after insertion"
        );
    }

    // ── Incremental batch chaining ────────────────────────────────────────

    #[test]
    fn multiple_batches_produce_same_root_as_scalar_reference() {
        // Insert 6 leaves via two batches of 3; compare against 6 scalar inserts.
        let env_ref = Env::default();
        env_ref.ledger().set_timestamp(3_000_000);
        env_ref.ledger().set_sequence_number(300);

        let env_batched = Env::default();
        env_batched.ledger().set_timestamp(3_000_000);
        env_batched.ledger().set_sequence_number(300);

        let all_values = [0x01u8, 0x02, 0x03, 0x04, 0x05, 0x06];

        // Reference: 6 scalar inserts
        let mut root_ref = BytesN::from_array(&env_ref, &[0u8; 32]);
        for &b in &all_values {
            let (_, r) = insert_deposit(&env_ref, make_commitment(&env_ref, b)).unwrap();
            root_ref = r;
        }

        // Two consecutive batches of 3
        let b1 = make_commitments(&env_batched, &all_values[..3]);
        batch_insert_deposits(&env_batched, &b1).unwrap();

        let b2 = make_commitments(&env_batched, &all_values[3..]);
        let r2 = batch_insert_deposits(&env_batched, &b2).unwrap();

        assert_eq!(
            r2.new_root, root_ref,
            "two batches of 3 must equal 6 sequential scalar inserts"
        );
    }

    // ── Full-capacity boundary ────────────────────────────────────────────

    #[test]
    fn batch_at_exact_max_size_is_accepted() {
        let env = Env::default();
        env.ledger().set_timestamp(1_000_000);

        // MAX_BATCH_SIZE = 64 leaves — should succeed
        let mut commitments: Vec<BytesN<32>> = Vec::new(&env);
        for i in 0..MAX_BATCH_SIZE {
            commitments.push_back(make_commitment(&env, (i % 250) as u8));
        }
        let result = batch_insert_deposits(&env, &commitments);
        assert!(result.is_ok(), "exact MAX_BATCH_SIZE should be accepted");
        let r = result.unwrap();
        assert_eq!(r.count, MAX_BATCH_SIZE);
        assert_eq!(r.first_leaf_index, 0);
        assert_eq!(r.last_leaf_index, MAX_BATCH_SIZE - 1);
    }

    #[test]
    fn rejects_batch_exceeding_tree_capacity() {
        let env = Env::default();
        env.ledger().set_timestamp(1_000_000);
        env.ledger().set_sequence_number(100);

        // Fill the tree to (max_leaves - 1) using scalar inserts so we can
        // test overflow with a 2-element batch (only 1 slot remains).
        // max_leaves = 2^20 = 1_048_576; we simulate this by manually
        // writing a near-full leaf counter.
        let max_leaves: u32 = 1u32 << TREE_DEPTH; // 1_048_576
        let near_full = max_leaves - 1;

        env.storage()
            .persistent()
            .set(&MerkleStorageKey::NextLeafIndex, &near_full);

        // A 2-leaf batch should be rejected (only 1 slot remains)
        let batch = make_commitments(&env, &[0x01, 0x02]);
        let result = batch_insert_deposits(&env, &batch);
        assert_eq!(result, Err(ContractError::MerkleTreeFull));
    }

    // ── Event emission ────────────────────────────────────────────────────

    #[test]
    fn batch_emits_zk_batch_commit_event() {
        let env = Env::default();
        env.ledger().set_timestamp(1_000_000);
        env.ledger().set_sequence_number(100);

        let batch = make_commitments(&env, &[0xDE, 0xAD]);
        let result = batch_insert_deposits(&env, &batch).unwrap();

        // Verify the event was published by checking the symbol constant is
        // correct (9-char limit for symbol_short).
        // The soroban test environment captures events; confirm the topic
        // matches EV_ZK_BATCH_COMMIT.
        let events = env.events().all();
        let found = events.iter().any(|(_contract_id, topics, _data)| {
            topics.get(0).map(|t| t == EV_ZK_BATCH_COMMIT.to_val()).unwrap_or(false)
        });
        assert!(found, "EV_ZK_BATCH_COMMIT event must be emitted");
        // Sanity: root was captured in event payload
        assert_ne!(result.new_root, BytesN::from_array(&env, &[0u8; 32]));
    }
}
