//! Canonical Merkle encoding used to archive historical governance vote registers.
//!
//! The repository had no hash/Merkle utility before this module (only
//! `env.crypto().sha256` inside `src/nonce.rs`), so the scheme below is defined
//! once here and every leaf/root in this crate is produced by it.
//!
//! ## Leaf encoding
//!
//! ```text
//! leaf = sha256(
//!     LEAF_DOMAIN            (1 byte,  0x00)
//!     || proposal_id         (8 bytes, big-endian u64)
//!     || voter_index         (4 bytes, big-endian u32)
//!     || option              (1 byte,  0x00 | 0x01)
//!     || weight              (4 bytes, big-endian u32)
//!     || voter_xdr_len       (4 bytes, big-endian u32)
//!     || voter_xdr           (ScVal XDR of the voter Address)
//! )
//! ```
//!
//! Every variable-length field is length-prefixed and the whole preimage is
//! domain separated, so no two distinct vote records can collide through
//! ambiguous concatenation. `voter_xdr` is produced by the SDK's
//! [`ToXdr`](soroban_sdk::xdr::ToXdr) implementation, i.e. the exact
//! serialization the ledger uses for that address.
//!
//! The weight is encoded as a full 4-byte `u32` rather than a single byte.
//! Packed weights are always in `1..=100` and would fit a byte, but the
//! detailed fallback path (see [`crate::bits::Encoding`]) can archive a weight
//! outside that range, so the leaf must not truncate it.
//!
//! ## Tree shape
//!
//! ```text
//! node  = sha256(NODE_DOMAIN || left || right)    (NODE_DOMAIN = 0x01)
//! empty = sha256(EMPTY_DOMAIN)                    (EMPTY_DOMAIN = 0x02)
//! ```
//!
//! Leaves are supplied in canonical order (ascending voter index — see
//! [`crate::register`]). A level with an odd number of nodes promotes its last
//! node to the next level unchanged; nodes are never duplicated, which keeps
//! proofs unambiguous.
//!
//! ## Ordering
//!
//! The root is a function of the *set* of leaves, not of the order in which
//! votes were cast: the caller sorts participants by canonical voter index
//! before building the leaf list. See `register::compress_after_execution`.

use alloc::vec::Vec;
use soroban_sdk::{xdr::ToXdr, Address, Bytes, BytesN, Env, Vec as SVec};

/// Domain separator for leaf preimages.
pub const LEAF_DOMAIN: u8 = 0x00;
/// Domain separator for internal node preimages.
pub const NODE_DOMAIN: u8 = 0x01;
/// Domain separator for the empty-tree root.
pub const EMPTY_DOMAIN: u8 = 0x02;

fn sha256(env: &Env, preimage: &Bytes) -> BytesN<32> {
    env.crypto().sha256(preimage)
}

/// Root of an empty register (`sha256(EMPTY_DOMAIN)`).
///
/// Deterministic and distinct from every real leaf and node, so an empty
/// archive can never be confused with a one-vote archive.
pub fn empty_root(env: &Env) -> BytesN<32> {
    let mut preimage = Bytes::new(env);
    preimage.push_back(EMPTY_DOMAIN);
    sha256(env, &preimage)
}

/// Canonical leaf for a single historical vote.
pub fn leaf_hash(
    env: &Env,
    proposal_id: u64,
    voter_index: u32,
    option: bool,
    weight: u32,
    voter: &Address,
) -> BytesN<32> {
    let voter_xdr = voter.to_xdr(env);
    let mut preimage = Bytes::new(env);
    preimage.push_back(LEAF_DOMAIN);
    preimage.extend_from_array(&proposal_id.to_be_bytes());
    preimage.extend_from_array(&voter_index.to_be_bytes());
    preimage.push_back(if option { 1 } else { 0 });
    preimage.extend_from_array(&weight.to_be_bytes());
    preimage.extend_from_array(&voter_xdr.len().to_be_bytes());
    preimage.append(&voter_xdr);
    sha256(env, &preimage)
}

/// Internal node hash binding a left and a right child.
pub fn node_hash(env: &Env, left: &BytesN<32>, right: &BytesN<32>) -> BytesN<32> {
    let left_bytes: Bytes = left.clone().into();
    let right_bytes: Bytes = right.clone().into();
    let mut preimage = Bytes::new(env);
    preimage.push_back(NODE_DOMAIN);
    preimage.append(&left_bytes);
    preimage.append(&right_bytes);
    sha256(env, &preimage)
}

/// Compute the deterministic Merkle root of an ordered leaf list.
pub fn merkle_root(env: &Env, leaves: &SVec<BytesN<32>>) -> BytesN<32> {
    if leaves.len() == 0 {
        return empty_root(env);
    }

    let mut level: Vec<BytesN<32>> = Vec::with_capacity(leaves.len() as usize);
    for leaf in leaves.iter() {
        level.push(leaf);
    }

    while level.len() > 1 {
        level = next_level(env, &level);
    }

    level[0].clone()
}

/// Build the Merkle inclusion proof for the leaf at `index`.
///
/// Returns the sibling hashes bottom-up. Levels where the node has no sibling
/// (odd level, last node) contribute nothing because that node is promoted
/// rather than hashed, which [`verify_proof`] accounts for via `tree_size`.
pub fn merkle_proof(env: &Env, leaves: &SVec<BytesN<32>>, index: u32) -> SVec<BytesN<32>> {
    let mut proof = SVec::new(env);
    if leaves.len() == 0 || index >= leaves.len() {
        return proof;
    }

    let mut level: Vec<BytesN<32>> = Vec::with_capacity(leaves.len() as usize);
    for leaf in leaves.iter() {
        level.push(leaf);
    }

    let mut idx = index as usize;
    while level.len() > 1 {
        if idx % 2 == 1 {
            proof.push_back(level[idx - 1].clone());
        } else if idx + 1 < level.len() {
            proof.push_back(level[idx + 1].clone());
        }
        idx /= 2;
        level = next_level(env, &level);
    }

    proof
}

/// Verify a leaf against a stored root.
///
/// `tree_size` is the number of leaves the root was built from; it is required
/// because odd levels promote their last node instead of duplicating it, and
/// the verifier must know when that happens.
pub fn verify_proof(
    env: &Env,
    root: &BytesN<32>,
    leaf: &BytesN<32>,
    index: u32,
    tree_size: u32,
    proof: &SVec<BytesN<32>>,
) -> bool {
    if tree_size == 0 || index >= tree_size {
        return *root == empty_root(env);
    }

    let mut hash = leaf.clone();
    let mut idx = index;
    let mut size = tree_size;
    let mut consumed: u32 = 0;

    while size > 1 {
        let level_len = (size - 1) / 2 + 1;
        if idx % 2 == 1 {
            let sibling = match proof.get(consumed) {
                Some(s) => s,
                None => return false,
            };
            hash = node_hash(env, &sibling, &hash);
            consumed += 1;
        } else if idx + 1 < size {
            let sibling = match proof.get(consumed) {
                Some(s) => s,
                None => return false,
            };
            hash = node_hash(env, &hash, &sibling);
            consumed += 1;
        }
        idx /= 2;
        size = level_len;
    }

    consumed == proof.len() && hash == *root
}

fn next_level(env: &Env, level: &[BytesN<32>]) -> Vec<BytesN<32>> {
    let mut next: Vec<BytesN<32>> = Vec::with_capacity((level.len() + 1) / 2);
    let mut i = 0;
    while i + 1 < level.len() {
        next.push(node_hash(env, &level[i], &level[i + 1]));
        i += 2;
    }
    if i < level.len() {
        next.push(level[i].clone());
    }
    next
}
