//! Depth-32 incremental Merkle tree for private remittance commitments.

use soroban_sdk::{contracttype, symbol_short, Bytes, BytesN, Env, Vec};

use crate::ContractError;

pub const TREE_DEPTH: u32 = 32;
const ROOT_HISTORY_SIZE: u32 = 100;
const TREE_CAPACITY: u64 = 1u64 << TREE_DEPTH;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MerkleStorageKey {
    State,
    Node(u32, u64),
    Zero(u32),
    Root(u32),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerkleTreeState {
    pub next_index: u64,
    pub current_root: BytesN<32>,
    pub root_count: u32,
    pub root_cursor: u32,
}

fn hash_pair(env: &Env, left: &BytesN<32>, right: &BytesN<32>) -> BytesN<32> {
    let mut payload = Bytes::new(env);
    payload.append(&Bytes::from_slice(env, &left.to_array()));
    payload.append(&Bytes::from_slice(env, &right.to_array()));
    env.crypto().sha256(&payload)
}

fn zero_hash(env: &Env, level: u32) -> BytesN<32> {
    if let Some(hash) = env
        .storage()
        .persistent()
        .get(&MerkleStorageKey::Zero(level))
    {
        return hash;
    }

    let hash = if level == 0 {
        BytesN::from_array(env, &[0u8; 32])
    } else {
        let child = zero_hash(env, level - 1);
        hash_pair(env, &child, &child)
    };
    let key = MerkleStorageKey::Zero(level);
    env.storage().persistent().set(&key, &hash);
    env.storage().persistent().extend_ttl(&key, 5_000, 100_000);
    hash
}

fn load_state(env: &Env) -> MerkleTreeState {
    env.storage()
        .persistent()
        .get(&MerkleStorageKey::State)
        .unwrap_or_else(|| MerkleTreeState {
            next_index: 0,
            current_root: zero_hash(env, TREE_DEPTH),
            root_count: 0,
            root_cursor: 0,
        })
}

fn store_state(env: &Env, state: &MerkleTreeState) {
    let key = MerkleStorageKey::State;
    env.storage().persistent().set(&key, state);
    env.storage().persistent().extend_ttl(&key, 5_000, 100_000);
}

fn load_node(env: &Env, level: u32, index: u64) -> BytesN<32> {
    env.storage()
        .persistent()
        .get(&MerkleStorageKey::Node(level, index))
        .unwrap_or_else(|| zero_hash(env, level))
}

fn store_node(env: &Env, level: u32, index: u64, hash: &BytesN<32>) {
    let key = MerkleStorageKey::Node(level, index);
    env.storage().persistent().set(&key, hash);
    env.storage().persistent().extend_ttl(&key, 5_000, 100_000);
}

/// Insert one commitment and atomically return the new root and leaf index.
pub fn insert(env: &Env, commitment: BytesN<32>) -> Result<(u64, BytesN<32>), ContractError> {
    let mut state = load_state(env);
    if state.next_index >= TREE_CAPACITY {
        return Err(ContractError::MerkleTreeFull);
    }

    let leaf_index = state.next_index;
    store_node(env, 0, leaf_index, &commitment);

    let mut index = leaf_index;
    let mut current = commitment;
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
    let root_key = MerkleStorageKey::Root(root_slot);
    env.storage().persistent().set(&root_key, &current);
    env.storage().persistent().extend_ttl(&root_key, 5_000, 100_000);

    state.next_index += 1;
    state.current_root = current.clone();
    state.root_count = core::cmp::min(state.root_count + 1, ROOT_HISTORY_SIZE);
    state.root_cursor = (root_slot + 1) % ROOT_HISTORY_SIZE;
    store_state(env, &state);

    env.events().publish(
        (soroban_sdk::Symbol::new(env, "merkle_add"),),
        (leaf_index, current.clone()),
    );
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
            .get::<_, BytesN<32>>(&MerkleStorageKey::Root(slot))
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
            .get::<_, BytesN<32>>(&MerkleStorageKey::Root(slot))
        {
            roots.push_back(root);
        }
    }
    roots
}
