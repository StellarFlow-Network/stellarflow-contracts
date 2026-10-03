//! Reproducible storage benchmark for issue #978.
//!
//! Simulates **exactly 1,000 vote submissions** with a deterministic,
//! realistic distribution (weights spanning the full `1..=100` governance
//! range the repository allows) and measures the *serialized* footprint of each
//! representation:
//!
//! | Representation | Stored keys |
//! | --- | --- |
//! | detailed (what the repository writes today) | `ActionVotes(id)` = `Vec<Address>` |
//! | compact (this module) | `VoteBits(id)` + `PackedWeights(id)` |
//! | archived (post-execution) | `Archive(id)` = root + leaf count + final tally |
//!
//! Bytes are measured with the SDK's own XDR serialization
//! ([`soroban_sdk::xdr::ToXdr`]) applied to the real stored key/value pairs, so
//! the numbers are the actual encoded representation rather than a count of
//! struct fields or an in-memory estimate. Ledger-entry framing (TTL/rent
//! metadata) is not included; it is of the same kind for every bucket.
//!
//! Run explicitly:
//!
//! ```text
//! cargo test -p governance-vote-archive storage_benchmark_1000_vote_submissions -- --nocapture
//! ```

extern crate std;

use alloc::vec::Vec as RVec;
use soroban_sdk::{
    testutils::Address as _,
    xdr::ToXdr,
    Address, Env, IntoVal, Val,
};

use crate::register::{self, DataKey};
use crate::test::VoteArchiveHarness;

/// Number of simulated vote submissions required by the issue.
pub const SIMULATED_VOTE_SUBMISSIONS: u32 = 1_000;

const DETAILED_PROPOSAL: u64 = 1;
const COMPACT_PROPOSAL: u64 = 2;

/// Deterministic weight distribution spanning the full allowed range.
fn weight_for(index: u32) -> u32 {
    1 + (index * 37) % 100
}

/// Serialized size of one stored key/value pair, in bytes.
fn entry_bytes<K, V>(env: &Env, key: K, value: V) -> u32
where
    K: IntoVal<Env, Val> + Clone,
    V: IntoVal<Env, Val> + Clone,
{
    key.to_xdr(env).len() + value.to_xdr(env).len()
}

#[test]
fn storage_benchmark_1000_vote_submissions() {
    let env = Env::default();
    env.mock_all_auths();
    // Registering 1,000 voters is intentionally O(n^2): every insertion
    // rewrites the eligible-voter list, and every vote re-reads it. That is
    // the honest cost of the register the repository ships today, not
    // something this benchmark optimizes away, so the test budget is lifted
    // rather than the workload reduced. The measured bytes are unaffected.
    env.budget().reset_unlimited();
    let contract_id = env.register_contract(None, VoteArchiveHarness);
    let admin = Address::generate(&env);

    // ── 1,000 eligible voters, registered once ──────────────────────────────
    let mut voters: RVec<Address> = RVec::new();
    for _ in 0..SIMULATED_VOTE_SUBMISSIONS {
        voters.push(Address::generate(&env));
    }

    env.as_contract(&contract_id, || {
        register::initialize(&env, admin.clone()).unwrap();
        for voter in voters.iter() {
            register::add_voter(&env, admin.clone(), voter.clone()).unwrap();
        }

        // Shared governance configuration: identical for both representations.
        for (index, voter) in voters.iter().enumerate() {
            register::set_admin_weight(&env, admin.clone(), voter.clone(), weight_for(index as u32))
                .unwrap();
        }

        // ── Before: the detailed register the repository writes today ────────
        register::propose_action(&env, admin.clone(), DETAILED_PROPOSAL).unwrap();
        for voter in voters.iter() {
            register::cast_vote_detailed(&env, voter.clone(), DETAILED_PROPOSAL).unwrap();
        }
        let detailed_tally = register::tally(&env, DETAILED_PROPOSAL).unwrap();

        // ── After: the compact packed register ──────────────────────────────
        register::propose_action(&env, admin.clone(), COMPACT_PROPOSAL).unwrap();
        for voter in voters.iter() {
            register::cast_vote(&env, voter.clone(), COMPACT_PROPOSAL).unwrap();
        }
        let compact_tally = register::tally(&env, COMPACT_PROPOSAL).unwrap();

        // ── Measurements (real serialized key + value bytes) ─────────────────
        let detailed_register = entry_bytes(
            &env,
            DataKey::ActionVotes(DETAILED_PROPOSAL),
            register::get_action_votes(&env, DETAILED_PROPOSAL),
        );

        let vote_bits = entry_bytes(
            &env,
            DataKey::VoteBits(COMPACT_PROPOSAL),
            register::get_vote_bits(&env, COMPACT_PROPOSAL),
        );
        let packed_weights = entry_bytes(
            &env,
            DataKey::PackedWeights(COMPACT_PROPOSAL),
            register::get_packed_weights(&env, COMPACT_PROPOSAL),
        );
        let compact_register = vote_bits + packed_weights;
        // Word counts are read before compression removes the compact entries.
        let vote_bit_words = register::get_vote_bits(&env, COMPACT_PROPOSAL).len();
        let packed_weight_words = register::get_packed_weights(&env, COMPACT_PROPOSAL).len();

        let mut weight_config_bytes: u32 = 0;
        for voter in voters.iter() {
            weight_config_bytes += entry_bytes(
                &env,
                DataKey::AdminWeight(voter.clone()),
                register::admin_weight(&env, voter.clone()),
            );
        }

        // ── Finalize and compress both proposals ────────────────────────────
        register::finalize(&env, DETAILED_PROPOSAL).unwrap();
        register::finalize(&env, COMPACT_PROPOSAL).unwrap();

        let archived_detailed =
            register::compress_after_execution(&env, DETAILED_PROPOSAL).unwrap();
        let archived_compact =
            register::compress_after_execution(&env, COMPACT_PROPOSAL).unwrap();

        let archived_bytes = entry_bytes(
            &env,
            DataKey::Archive(COMPACT_PROPOSAL),
            register::get_archive(&env, COMPACT_PROPOSAL).unwrap(),
        );

        // ── Invariants: the optimization must not move a single vote ────────
        assert_eq!(
            detailed_tally, compact_tally,
            "packed tally must equal the detailed weighted tally exactly"
        );
        assert_eq!(
            archived_detailed.final_tally, detailed_tally,
            "compression must not change the detailed register's final tally"
        );
        assert_eq!(
            archived_compact.final_tally, compact_tally,
            "compression must not change the compact register's final tally"
        );
        assert_eq!(
            archived_detailed.leaf_count, SIMULATED_VOTE_SUBMISSIONS,
            "every submitted vote must be represented in the archive"
        );
        assert!(
            !env.storage().persistent().has(&DataKey::VoteBits(COMPACT_PROPOSAL)),
            "compression must remove the detailed per-voter register"
        );

        assert!(compact_register < detailed_register, "packing must save bytes");
        assert!(archived_bytes < compact_register, "archival must save bytes");

        let packed_saved = detailed_register - compact_register;
        let archived_saved = detailed_register - archived_bytes;
        let packed_pct = packed_saved as f64 * 100.0 / detailed_register as f64;
        let archived_pct = archived_saved as f64 * 100.0 / detailed_register as f64;

        std::println!("");
        std::println!(
            "=== #978 governance vote register storage benchmark ({} simulated submissions) ===",
            SIMULATED_VOTE_SUBMISSIONS
        );
        std::println!("eligible voters / vote submissions            : {}", SIMULATED_VOTE_SUBMISSIONS);
        std::println!(
            "detailed register  ActionVotes(id) Vec<Address>: {:>8} bytes",
            detailed_register
        );
        std::println!(
            "compact register   VoteBits + PackedWeights    : {:>8} bytes ({} bit-word(s) + {} weight-word(s))",
            compact_register, vote_bit_words, packed_weight_words
        );
        std::println!(
            "archived           Archive(id) root+count+tally: {:>8} bytes",
            archived_bytes
        );
        std::println!("--- per-proposal historical register ---");
        std::println!("before (detailed)                              : {:>8} bytes", detailed_register);
        std::println!(
            "after  (compact)                               : {:>8} bytes   saved {} bytes ({:.2}%)",
            compact_register, packed_saved, packed_pct
        );
        std::println!(
            "after  (archived)                              : {:>8} bytes   saved {} bytes ({:.2}%)",
            archived_bytes, archived_saved, archived_pct
        );
        std::println!("--- shared governance config (unchanged, excluded above) ---");
        std::println!(
            "AdminWeight(Address) x{}                   : {:>8} bytes",
            SIMULATED_VOTE_SUBMISSIONS, weight_config_bytes
        );
        std::println!(
            "final weighted tally (identical in both)       : {}",
            compact_tally
        );
        std::println!(
            "archived Merkle root                           : 32-byte sha256 digest"
        );
    });
}
