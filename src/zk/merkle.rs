//! Zero-Knowledge Anonymity Set Deposit Merkle Verifier.
//!
//! Provides on-chain deposit commitment tracking, an incremental Merkle tree,
//! historical root buffer with time-based expiration constraints, spent nullifier
//! tracking to prevent double-spending, and Merkle membership verification.

use soroban_sdk::{contracttype, symbol_short, Bytes, BytesN, Env, Symbol, Vec};

use crate::ContractError;
use crate::zk::nullifier::{is_nullifier_used, register_nullifier};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Fixed depth for the deposit incremental Merkle tree.
/// Depth 20 supports 2^20 = 1,048,576 total deposit commitments.
pub const TREE_DEPTH: u32 = 20;

/// Maximum number of historical roots retained in the historical root ring buffer.
pub const ROOT_BUFFER_CAPACITY: u32 = 100;

/// Default root validity duration in seconds (7 days = 604,800 seconds).
/// Merkle roots older than this expiration window are rejected as expired.
pub const DEFAULT_ROOT_VALIDITY_DURATION: u64 = 7 * 24 * 60 * 60;

/// Event topic for deposit commitment insertions.
pub const EV_ZK_DEPOSIT: Symbol = symbol_short!("zk_dep");

/// Event topic for withdrawal / anonymity set proof verifications.
pub const EV_ZK_WITHDRAW: Symbol = symbol_short!("zk_wdraw");

// ---------------------------------------------------------------------------
// Storage Types & Keys
// ---------------------------------------------------------------------------

/// Storage keys for the Merkle tree and historical root buffer.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MerkleStorageKey {
    /// The current active Merkle root.
    CurrentRoot,
    /// Next available leaf index in the incremental tree.
    NextLeafIndex,
    /// Intermediate filled subtree hashes at each tree level: `FilledSubtree(level)`.
    FilledSubtree(u32),
    /// Zero hashes for each tree level: `ZeroHash(level)`.
    ZeroHash(u32),
    /// Detailed record of a historical root: `RootRecord(root_hash)`.
    RootRecord(BytesN<32>),
    /// Ring buffer list of historical root hashes in insertion order.
    RootBufferList,
    /// Configured validity duration of a root in seconds (0 = never expires).
    RootValidityWindow,
}

/// Metadata record associated with each recorded Merkle root.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RootRecord {
    /// The 32-byte Merkle root hash.
    pub root: BytesN<32>,
    /// Ledger timestamp when the root was added to history.
    pub inserted_at: u64,
    /// Ledger sequence number at time of insertion.
    pub sequence: u32,
    /// Total number of deposit commitments in the tree when this root was computed.
    pub leaf_count: u32,
}

/// User withdrawal verification payload.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct WithdrawalProof {
    /// Historical Merkle root against which the membership proof was generated.
    pub root: BytesN<32>,
    /// Unique nullifier preventing double-spending of the deposited note.
    pub nullifier: BytesN<32>,
    /// Commitment leaf being proven in the Merkle tree.
    pub leaf: BytesN<32>,
    /// Sibling hashes along the Merkle path from leaf to root.
    pub path: Vec<BytesN<32>>,
    /// 0-indexed position of the leaf in the Merkle tree.
    pub leaf_index: u32,
}

// ---------------------------------------------------------------------------
// Node Hashing & Zero Hashes
// ---------------------------------------------------------------------------

/// Compute SHA-256 hash of two 32-byte child nodes: `sha256(left || right)`.
pub fn hash_nodes(env: &Env, left: &BytesN<32>, right: &BytesN<32>) -> BytesN<32> {
    let mut data = Bytes::new(env);
    data.append(&Bytes::from_slice(env, &left.to_array()));
    data.append(&Bytes::from_slice(env, &right.to_array()));
    env.crypto().sha256(&data)
}

/// Get or compute the zero hash for a given tree level.
/// Level 0 is a deterministic zero leaf.
/// Level `i + 1` is `hash_nodes(zero(i), zero(i))`.
pub fn get_zero_hash(env: &Env, level: u32) -> BytesN<32> {
    let key = MerkleStorageKey::ZeroHash(level);
    if let Some(cached) = env.storage().persistent().get::<_, BytesN<32>>(&key) {
        return cached;
    }

    let zero = if level == 0 {
        let seed = Bytes::from_slice(env, b"stellarflow:anonymity:zero_leaf");
        env.crypto().sha256(&seed)
    } else {
        let prev = get_zero_hash(env, level - 1);
        hash_nodes(env, &prev, &prev)
    };

    env.storage().persistent().set(&key, &zero);
    env.storage().persistent().extend_ttl(&key, 5_000, 100_000);
    zero
}

// ---------------------------------------------------------------------------
// Historical Root Buffer Management & Expiration
// ---------------------------------------------------------------------------

/// Get the configured root validity window in seconds.
pub fn get_root_validity_window(env: &Env) -> u64 {
    env.storage()
        .persistent()
        .get(&MerkleStorageKey::RootValidityWindow)
        .unwrap_or(DEFAULT_ROOT_VALIDITY_DURATION)
}

/// Set the root validity window in seconds.
pub fn set_root_validity_window(env: &Env, validity_seconds: u64) {
    let key = MerkleStorageKey::RootValidityWindow;
    env.storage().persistent().set(&key, &validity_seconds);
    env.storage().persistent().extend_ttl(&key, 5_000, 100_000);
}

/// Register a new root into the historical root ring buffer and persistent store.
pub fn record_historical_root(env: &Env, root: BytesN<32>, leaf_count: u32) {
    let current_time = env.ledger().timestamp();
    let current_seq = env.ledger().sequence();

    let record = RootRecord {
        root: root.clone(),
        inserted_at: current_time,
        sequence: current_seq,
        leaf_count,
    };

    // Store root record
    let root_key = MerkleStorageKey::RootRecord(root.clone());
    env.storage().persistent().set(&root_key, &record);
    env.storage().persistent().extend_ttl(&root_key, 5_000, 100_000);

    // Update active current root
    let current_root_key = MerkleStorageKey::CurrentRoot;
    env.storage().persistent().set(&current_root_key, &root);
    env.storage().persistent().extend_ttl(&current_root_key, 5_000, 100_000);

    // Update ring buffer list
    let list_key = MerkleStorageKey::RootBufferList;
    let mut root_list: Vec<BytesN<32>> = env
        .storage()
        .persistent()
        .get(&list_key)
        .unwrap_or_else(|| Vec::new(env));

    root_list.push_back(root);

    // Enforce ring buffer capacity
    if root_list.len() > ROOT_BUFFER_CAPACITY {
        if let Some(oldest_root) = root_list.get(0) {
            let oldest_key = MerkleStorageKey::RootRecord(oldest_root);
            env.storage().persistent().remove(&oldest_key);
        }
        let mut trimmed = Vec::new(env);
        for i in 1..root_list.len() {
            if let Some(r) = root_list.get(i) {
                trimmed.push_back(r);
            }
        }
        root_list = trimmed;
    }

    env.storage().persistent().set(&list_key, &root_list);
    env.storage().persistent().extend_ttl(&list_key, 5_000, 100_000);
}

/// Get record of a historical root if it exists in the buffer.
pub fn get_root_record(env: &Env, root: &BytesN<32>) -> Option<RootRecord> {
    env.storage().persistent().get(&MerkleStorageKey::RootRecord(root.clone()))
}

/// Check if a root exists in history and is currently unexpired.
pub fn is_root_valid(env: &Env, root: &BytesN<32>) -> bool {
    validate_root(env, root).is_ok()
}

/// Validate that a root is known in the historical root buffer and has not expired.
///
/// Reverts with `ContractError::InvalidMerkleProof` if root is unverified or expired.
pub fn validate_root(env: &Env, root: &BytesN<32>) -> Result<RootRecord, ContractError> {
    let record: RootRecord = env
        .storage()
        .persistent()
        .get(&MerkleStorageKey::RootRecord(root.clone()))
        .ok_or(ContractError::InvalidMerkleProof)?;

    let validity_window = get_root_validity_window(env);
    if validity_window > 0 {
        let current_time = env.ledger().timestamp();
        let expires_at = record.inserted_at.saturating_add(validity_window);
        if current_time > expires_at {
            return Err(ContractError::InvalidMerkleProof);
        }
    }

    Ok(record)
}

/// Get the current active Merkle tree root.
pub fn get_current_root(env: &Env) -> Option<BytesN<32>> {
    env.storage().persistent().get(&MerkleStorageKey::CurrentRoot)
}

/// Get the total number of deposits committed to the tree.
pub fn get_total_deposits(env: &Env) -> u32 {
    env.storage()
        .persistent()
        .get(&MerkleStorageKey::NextLeafIndex)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Incremental Deposit Tree Insertion
// ---------------------------------------------------------------------------

/// Insert a deposit commitment leaf into the incremental Merkle tree.
/// Computes new root, records it in the historical buffer, and returns `(leaf_index, new_root)`.
pub fn insert_deposit(
    env: &Env,
    commitment: BytesN<32>,
) -> Result<(u32, BytesN<32>), ContractError> {
    let next_index_key = MerkleStorageKey::NextLeafIndex;
    let next_index: u32 = env
        .storage()
        .persistent()
        .get(&next_index_key)
        .unwrap_or(0);

    let max_leaves = 1u32.checked_shl(TREE_DEPTH).ok_or(ContractError::Overflow)?;
    if next_index >= max_leaves {
        return Err(ContractError::CapacityExceeded);
    }

    let mut current_hash = commitment.clone();
    let mut current_index = next_index;

    for level in 0..TREE_DEPTH {
        let is_right_child = (current_index % 2) == 1;
        let subtree_key = MerkleStorageKey::FilledSubtree(level);

        if !is_right_child {
            // Left child: save current hash and compute parent with zero hash
            env.storage().persistent().set(&subtree_key, &current_hash);
            env.storage().persistent().extend_ttl(&subtree_key, 5_000, 100_000);

            let zero = get_zero_hash(env, level);
            current_hash = hash_nodes(env, &current_hash, &zero);
        } else {
            // Right child: fetch left sibling from filled subtree and compute parent
            let left_sibling: BytesN<32> = env
                .storage()
                .persistent()
                .get(&subtree_key)
                .unwrap_or_else(|| get_zero_hash(env, level));
            current_hash = hash_nodes(env, &left_sibling, &current_hash);
        }

        current_index /= 2;
    }

    let new_leaf_count = next_index.saturating_add(1);
    env.storage().persistent().set(&next_index_key, &new_leaf_count);
    env.storage().persistent().extend_ttl(&next_index_key, 5_000, 100_000);

    // Record root in historical buffer
    record_historical_root(env, current_hash.clone(), new_leaf_count);

    // Emit deposit event
    env.events().publish(
        (EV_ZK_DEPOSIT, commitment),
        (next_index, current_hash.clone(), env.ledger().timestamp()),
    );

    Ok((next_index, current_hash))
}

// ---------------------------------------------------------------------------
// Merkle Proof Verification & Withdrawal Logic
// ---------------------------------------------------------------------------

/// Verify a Merkle proof path against a root for a given leaf and index.
pub fn verify_merkle_proof(
    env: &Env,
    leaf: &BytesN<32>,
    path: &Vec<BytesN<32>>,
    leaf_index: u32,
    root: &BytesN<32>,
) -> bool {
    if path.len() as u32 != TREE_DEPTH {
        return false;
    }

    let mut current = leaf.clone();
    let mut idx = leaf_index;

    for i in 0..TREE_DEPTH {
        let sibling = match path.get(i) {
            Some(s) => s,
            None => return false,
        };

        if (idx % 2) == 1 {
            // Leaf is right child: hash(sibling, current)
            current = hash_nodes(env, &sibling, &current);
        } else {
            // Leaf is left child: hash(current, sibling)
            current = hash_nodes(env, &current, &sibling);
        }

        idx /= 2;
    }

    &current == root
}

/// Verify a user withdrawal proof against the historical root buffer and prevent double spending.
///
/// 1. Validates that `root` is in the historical root buffer and unexpired.
///    Reverts with `ContractError::InvalidMerkleProof` if unverified or expired.
/// 2. Checks that `nullifier` has not been spent yet on-chain.
///    Reverts with `ContractError::NullifierAlreadyUsed` if already spent.
/// 3. Validates the Merkle membership proof (`leaf + path == root`).
///    Reverts with `ContractError::InvalidMerkleProof` if proof is invalid.
/// 4. Marks the `nullifier` as spent on-chain in persistent storage.
/// 5. Emits the withdrawal event.
pub fn verify_withdrawal_and_spend(
    env: &Env,
    root: &BytesN<32>,
    nullifier: &BytesN<32>,
    leaf: &BytesN<32>,
    path: &Vec<BytesN<32>>,
    leaf_index: u32,
) -> Result<(), ContractError> {
    // 1. Validate root belongs to historical root buffer and is unexpired
    validate_root(env, root)?;

    // 2. Prevent double spending
    if is_nullifier_used(env, nullifier) {
        return Err(ContractError::NullifierAlreadyUsed);
    }

    // 3. Verify Merkle proof
    if !verify_merkle_proof(env, leaf, path, leaf_index, root) {
        return Err(ContractError::InvalidMerkleProof);
    }

    // 4. Record nullifier as spent
    register_nullifier(env, nullifier.clone())?;

    // 5. Emit withdrawal event
    env.events().publish(
        (EV_ZK_WITHDRAW, nullifier.clone()),
        (root.clone(), leaf.clone(), env.ledger().timestamp()),
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Unit Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{BytesN as _, Ledger};

    fn make_leaf(env: &Env, byte: u8) -> BytesN<32> {
        BytesN::from_array(env, &[byte; 32])
    }

    #[test]
    fn test_zero_hash_determinism() {
        let env = Env::default();
        let z0 = get_zero_hash(&env, 0);
        let z1 = get_zero_hash(&env, 1);
        let expected_z1 = hash_nodes(&env, &z0, &z0);
        assert_eq!(z1, expected_z1);
    }

    #[test]
    fn test_insert_deposit_and_verify_proof() {
        let env = Env::default();
        env.ledger().set_timestamp(1_000_000);
        env.ledger().set_sequence_number(100);

        let commitment_0 = make_leaf(&env, 1);
        let (idx_0, root_0) = insert_deposit(&env, commitment_0.clone()).unwrap();
        assert_eq!(idx_0, 0);
        assert_eq!(get_current_root(&env), Some(root_0.clone()));
        assert!(is_root_valid(&env, &root_0));

        // Build Merkle proof for leaf 0
        let mut path_0 = Vec::new(&env);
        for level in 0..TREE_DEPTH {
            path_0.push_back(get_zero_hash(&env, level));
        }

        assert!(verify_merkle_proof(&env, &commitment_0, &path_0, 0, &root_0));

        // Attempt withdrawal with valid proof
        let nullifier_0 = make_leaf(&env, 99);
        assert!(!is_nullifier_used(&env, &nullifier_0));

        let res = verify_withdrawal_and_spend(
            &env,
            &root_0,
            &nullifier_0,
            &commitment_0,
            &path_0,
            0,
        );
        assert!(res.is_ok());

        // Nullifier is now spent
        assert!(is_nullifier_used(&env, &nullifier_0));

        // Second withdrawal attempt with same nullifier fails with NullifierAlreadyUsed
        let res_dup = verify_withdrawal_and_spend(
            &env,
            &root_0,
            &nullifier_0,
            &commitment_0,
            &path_0,
            0,
        );
        assert_eq!(res_dup, Err(ContractError::NullifierAlreadyUsed));
    }

    #[test]
    fn test_multiple_deposits_and_sibling_paths() {
        let env = Env::default();
        env.ledger().set_timestamp(1_000_000);

        let c0 = make_leaf(&env, 10);
        let c1 = make_leaf(&env, 20);

        let (idx_0, _) = insert_deposit(&env, c0.clone()).unwrap();
        let (idx_1, root_1) = insert_deposit(&env, c1.clone()).unwrap();
        assert_eq!(idx_0, 0);
        assert_eq!(idx_1, 1);

        // Path for c0 in tree with c0 and c1:
        // level 0 sibling is c1
        // level 1..TREE_DEPTH siblings are zero hashes
        let mut path_0 = Vec::new(&env);
        path_0.push_back(c1.clone());
        for level in 1..TREE_DEPTH {
            path_0.push_back(get_zero_hash(&env, level));
        }
        assert!(verify_merkle_proof(&env, &c0, &path_0, 0, &root_1));

        // Path for c1 in tree with c0 and c1:
        // level 0 sibling is c0
        // level 1..TREE_DEPTH siblings are zero hashes
        let mut path_1 = Vec::new(&env);
        path_1.push_back(c0.clone());
        for level in 1..TREE_DEPTH {
            path_1.push_back(get_zero_hash(&env, level));
        }
        assert!(verify_merkle_proof(&env, &c1, &path_1, 1, &root_1));

        // Verify withdrawal of c1 against historical root root_1
        let nullifier_1 = make_leaf(&env, 55);
        assert!(verify_withdrawal_and_spend(&env, &root_1, &nullifier_1, &c1, &path_1, 1).is_ok());
        assert!(is_nullifier_used(&env, &nullifier_1));
    }

    #[test]
    fn test_revert_on_unverified_root() {
        let env = Env::default();
        env.ledger().set_timestamp(1_000_000);

        let unverified_root = make_leaf(&env, 77);
        let leaf = make_leaf(&env, 1);
        let nullifier = make_leaf(&env, 2);

        let mut path = Vec::new(&env);
        for level in 0..TREE_DEPTH {
            path.push_back(get_zero_hash(&env, level));
        }

        let res = verify_withdrawal_and_spend(&env, &unverified_root, &nullifier, &leaf, &path, 0);
        assert_eq!(res, Err(ContractError::InvalidMerkleProof));
    }

    #[test]
    fn test_revert_on_expired_root() {
        let env = Env::default();
        let start_time = 1_000_000;
        env.ledger().set_timestamp(start_time);

        // Configure 1 day validity window
        let one_day = 86400;
        set_root_validity_window(&env, one_day);

        let commitment = make_leaf(&env, 1);
        let (_, root) = insert_deposit(&env, commitment.clone()).unwrap();

        let mut path = Vec::new(&env);
        for level in 0..TREE_DEPTH {
            path.push_back(get_zero_hash(&env, level));
        }

        // Within 1 day: root is valid
        env.ledger().set_timestamp(start_time + 86000);
        assert!(is_root_valid(&env, &root));

        // Advance past expiration (1 day + 1 second)
        env.ledger().set_timestamp(start_time + 86401);
        assert!(!is_root_valid(&env, &root));

        let nullifier = make_leaf(&env, 42);
        let res = verify_withdrawal_and_spend(&env, &root, &nullifier, &commitment, &path, 0);
        assert_eq!(res, Err(ContractError::InvalidMerkleProof));
    }

    #[test]
    fn test_revert_on_corrupt_proof() {
        let env = Env::default();
        env.ledger().set_timestamp(1_000_000);

        let commitment = make_leaf(&env, 1);
        let (_, root) = insert_deposit(&env, commitment.clone()).unwrap();

        // Corrupt sibling in path
        let mut corrupt_path = Vec::new(&env);
        corrupt_path.push_back(make_leaf(&env, 0xff)); // invalid sibling
        for level in 1..TREE_DEPTH {
            corrupt_path.push_back(get_zero_hash(&env, level));
        }

        let nullifier = make_leaf(&env, 42);
        let res = verify_withdrawal_and_spend(&env, &root, &nullifier, &commitment, &corrupt_path, 0);
        assert_eq!(res, Err(ContractError::InvalidMerkleProof));
        // Nullifier must not be marked spent on proof failure
        assert!(!is_nullifier_used(&env, &nullifier));
    }

    #[test]
    fn test_historical_root_ring_buffer_capacity() {
        let env = Env::default();
        env.ledger().set_timestamp(1_000_000);

        let mut roots = Vec::new(&env);
        for i in 0..(ROOT_BUFFER_CAPACITY + 5) {
            let c = make_leaf(&env, i as u8);
            let (_, root) = insert_deposit(&env, c).unwrap();
            roots.push_back(root);
        }

        // The first 5 roots should have been evicted from capacity
        for i in 0..5 {
            let old_root = roots.get(i).unwrap();
            assert_eq!(validate_root(&env, &old_root), Err(ContractError::InvalidMerkleProof));
        }

        // The latest 100 roots should remain valid
        for i in 5..(ROOT_BUFFER_CAPACITY + 5) {
            let valid_root = roots.get(i).unwrap();
            assert!(is_root_valid(&env, &valid_root));
        }
    }
}
