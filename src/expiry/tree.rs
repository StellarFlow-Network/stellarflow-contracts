//! Incremental Merkle tree for deposit commitments, with the deposit
//! timestamp (`t_deposit`) attached per-leaf, as issue #965 asks.
//!
//! Structurally this mirrors `crate::escrow::merkle` (depth 32, 100-slot
//! root history, sibling path recompute on insert) but it is a copy, not an
//! import — the original lives in a file that participates in the crate-wide
//! parse failure tracked in issue #1114, and this module must not depend on it.
//!
//! The one deliberate deviation from `escrow::merkle`: every leaf is committed
//! as `H(commitment || t_deposit)`, so the timestamp is cryptographically bound
//! to the leaf, not just recorded alongside it. Anyone verifying a Merkle
//! proof against a published root can confirm *when* a commitment was
//! deposited without the contract storing any preimage data.

use soroban_sdk::{Bytes, BytesN, Env, Vec};

use super::{DepositRecord, ExpiryError, ExpiryStorageKey};

/// Tree depth. `2^32` leaves, same capacity as `crate::escrow::merkle`.
pub const TREE_DEPTH: u32 = 32;
/// Number of historical roots retained for proof verification.
const ROOT_HISTORY_SIZE: u32 = 100;
/// Total leaf capacity (`2^32`).
const TREE_CAPACITY: u64 = 1u64 << TREE_DEPTH;

/// Per-leaf tree state.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpiryMerkleState {
    pub next_index: u64,
    pub current_root: BytesN<32>,
    pub root_count: u32,
    pub root_cursor: u32,
}

/// Key family for the expiry-guard commitment tree. Distinct from
/// [`ExpiryStorageKey`] so tree internals and deposit records cannot collide.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExpiryMerkleKey {
    State,
    Node(u32, u64),
    Zero(u32),
    Root(u32),
    /// Maps `leaf_index -> deposit timestamp`, so a verifier holding only the
    /// index can recover the `t_deposit` half of the leaf preimage.
    DepositTime(u64),
}

fn hash_pair(env: &Env, left: &BytesN<32>, right: &BytesN<32>) -> BytesN<32> {
    let mut payload = Bytes::new(env);
    payload.append(&Bytes::from_slice(env, &left.to_array()));
    payload.append(&Bytes::from_slice(env, &right.to_array()));
    env.crypto().sha256(&payload)
}

/// `H(commitment_bytes || t_deposit_be_bytes)` — the actual tree leaf.
///
/// This is the "attach `t_deposit` to commitment entries" requirement, made
/// cryptographic rather than bookkeeping-only: the timestamp participates in
/// the hash, so a published root is a commitment to the set of
/// `(commitment, deposited_at)` pairs, not just the commitments.
fn leaf_hash(env: &Env, commitment: &BytesN<32>, deposited_at: u64) -> BytesN<32> {
    let mut payload = Bytes::new(env);
    payload.append(&Bytes::from_slice(env, &commitment.to_array()));
    payload.extend_from_array(&deposited_at.to_be_bytes());
    env.crypto().sha256(&payload)
}

fn zero_hash(env: &Env, level: u32) -> BytesN<32> {
    if let Some(hash) = env
        .storage()
        .persistent()
        .get(&ExpiryMerkleKey::Zero(level))
    {
        return hash;
    }
    let hash = if level == 0 {
        // An unused leaf is `H([0u8;32] || 0u64)` rather than a bare zero
        // hash, since every real leaf has a timestamp appended.
        let empty_commitment = BytesN::from_array(env, &[0u8; 32]);
        leaf_hash(env, &empty_commitment, 0)
    } else {
        let child = zero_hash(env, level - 1);
        hash_pair(env, &child, &child)
    };
    let key = ExpiryMerkleKey::Zero(level);
    env.storage().persistent().set(&key, &hash);
    env.storage().persistent().extend_ttl(&key, 5_000, 100_000);
    hash
}

pub fn load_state(env: &Env) -> ExpiryMerkleState {
    env.storage()
        .persistent()
        .get(&ExpiryMerkleKey::State)
        .unwrap_or_else(|| ExpiryMerkleState {
            next_index: 0,
            current_root: zero_hash(env, TREE_DEPTH),
            root_count: 0,
            root_cursor: 0,
        })
}

fn store_state(env: &Env, state: &ExpiryMerkleState) {
    let key = ExpiryMerkleKey::State;
    env.storage().persistent().set(&key, state);
    env.storage().persistent().extend_ttl(&key, 5_000, 100_000);
}

fn load_node(env: &Env, level: u32, index: u64) -> BytesN<32> {
    env.storage()
        .persistent()
        .get(&ExpiryMerkleKey::Node(level, index))
        .unwrap_or_else(|| zero_hash(env, level))
}

fn store_node(env: &Env, level: u32, index: u64, hash: &BytesN<32>) {
    let key = ExpiryMerkleKey::Node(level, index);
    env.storage().persistent().set(&key, hash);
    env.storage().persistent().extend_ttl(&key, 5_000, 100_000);
}

/// Insert one deposit's commitment and atomically return the new root and
/// leaf index. Also stamps the deposit's expiry into `record` based on
/// `t_deposit`.
pub fn insert(
    env: &Env,
    record: &mut DepositRecord,
    commitment: BytesN<32>,
    t_deposit: u64,
    expiry_secs: u64,
) -> Result<(u64, BytesN<32>), ExpiryError> {
    let mut state = load_state(env);
    if state.next_index >= TREE_CAPACITY {
        return Err(ExpiryError::DepositNotFound); // tree-full is unreachable at 2^32 leaves
    }

    let leaf_index = state.next_index;
    let leaf = leaf_hash(env, &commitment, t_deposit);
    store_node(env, 0, leaf_index, &leaf);

    let mut index = leaf_index;
    let mut current = leaf;
    for level in 0..TREE_DEPTH {
        let sibling_index = index ^ 1;
        let sibling = load_node(env, level, sibling_index);
        current = if index & 1 == 0 {
            hash_pair(env, &current, &sibling)
        } else {
            hash_pair(env, &sibling, &current)
        };
        index /= 2;
        store_node(env, level + 1, index, &current);
    }

    let root_slot = state.root_cursor;
    let root_key = ExpiryMerkleKey::Root(root_slot);
    env.storage().persistent().set(&root_key, &current);
    env.storage()
        .persistent()
        .extend_ttl(&root_key, 5_000, 100_000);

    // Record `t_deposit` under the leaf's own key so it survives independent
    // of the deposit record's lifecycle.
    let time_key = ExpiryMerkleKey::DepositTime(leaf_index);
    env.storage().persistent().set(&time_key, &t_deposit);
    env.storage()
        .persistent()
        .extend_ttl(&time_key, 5_000, 100_000);

    state.next_index += 1;
    state.current_root = current.clone();
    state.root_count = core::cmp::min(state.root_count + 1, ROOT_HISTORY_SIZE);
    state.root_cursor = (root_slot + 1) % ROOT_HISTORY_SIZE;
    store_state(env, &state);

    record.index = leaf_index;
    record.commitment = commitment;
    record.deposited_at = t_deposit;
    record.expires_at = t_deposit.saturating_add(expiry_secs);

    Ok((leaf_index, current))
}

pub fn current_root(env: &Env) -> BytesN<32> {
    load_state(env).current_root
}

pub fn next_index(env: &Env) -> u64 {
    load_state(env).next_index
}

pub fn is_known_root(env: &Env, root: BytesN<32>) -> bool {
    let state = load_state(env);
    for offset in 0..state.root_count {
        let slot = (state.root_cursor + ROOT_HISTORY_SIZE - 1 - offset) % ROOT_HISTORY_SIZE;
        if env
            .storage()
            .persistent()
            .get::<_, BytesN<32>>(&ExpiryMerkleKey::Root(slot))
            == Some(root.clone())
        {
            return true;
        }
    }
    false
}

/// Return historical roots from newest to oldest.
pub fn root_history(env: &Env) -> Vec<BytesN<32>> {
    let state = load_state(env);
    let mut roots = Vec::new(env);
    for offset in 0..state.root_count {
        let slot = (state.root_cursor + ROOT_HISTORY_SIZE - 1 - offset) % ROOT_HISTORY_SIZE;
        if let Some(root) = env
            .storage()
            .persistent()
            .get::<_, BytesN<32>>(&ExpiryMerkleKey::Root(slot))
        {
            roots.push_back(root);
        }
    }
    roots
}

/// Look up the `t_deposit` recorded for a leaf index.
pub fn deposit_time(env: &Env, leaf_index: u64) -> Option<u64> {
    env.storage()
        .persistent()
        .get(&ExpiryMerkleKey::DepositTime(leaf_index))
}
