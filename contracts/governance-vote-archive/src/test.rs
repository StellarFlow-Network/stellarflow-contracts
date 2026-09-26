#![cfg(test)]

extern crate std;

use alloc::vec::Vec as RVec;
use soroban_sdk::{testutils::Address as _, Address, BytesN, Env, Vec as SVec};

use crate::bits::{self, MAX_VOTE_WEIGHT, MIN_VOTE_WEIGHT, WEIGHTS_PER_WORD};
use crate::merkle;
use crate::register::{self, ArchiveRecord, DataKey};
use crate::ArchiveError;

/// Minimal contract used only so tests can execute storage operations inside a
/// real contract context (`env.as_contract`).
#[soroban_sdk::contract]
pub struct VoteArchiveHarness;

fn setup() -> (Env, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, VoteArchiveHarness);
    let admin = Address::generate(&env);
    env.as_contract(&contract_id, || {
        register::initialize(&env, admin.clone()).unwrap();
    });
    (env, contract_id, admin)
}

/// Register `count` voters and return them in canonical (index) order.
fn seed_voters(env: &Env, contract_id: &Address, admin: &Address, count: u32) -> RVec<Address> {
    let mut voters = RVec::new();
    env.as_contract(contract_id, || {
        for _ in 0..count {
            let voter = Address::generate(env);
            register::add_voter(env, admin.clone(), voter.clone()).unwrap();
            voters.push(voter);
        }
    });
    voters
}

fn open_proposal(env: &Env, contract_id: &Address, admin: &Address, proposal_id: u64) {
    env.as_contract(contract_id, || {
        register::propose_action(env, admin.clone(), proposal_id).unwrap();
    });
}

/// Deterministic whole vote weight for slot `i`, spanning the full `1..=100`
/// range the repository's `AdminWeight` allows.
fn weight_for(i: u32) -> u32 {
    MIN_VOTE_WEIGHT + (i * 37) % MAX_VOTE_WEIGHT
}

// ─── Boolean option packing ─────────────────────────────────────────────────

#[test]
fn test_pack_vote_options_all_false() {
    let env = Env::default();
    let options = [false; 70];
    let packed = bits::pack_vote_options(&env, &options);

    assert_eq!(packed.len(), 2);
    assert_eq!(packed.get(0).unwrap(), 0);
    assert_eq!(packed.get(1).unwrap(), 0);
    for i in 0..options.len() as u32 {
        assert!(!bits::is_bit_set(&packed, i));
    }
}

#[test]
fn test_pack_vote_options_all_true() {
    let env = Env::default();
    let options = [true; 64];
    let packed = bits::pack_vote_options(&env, &options);

    assert_eq!(packed.len(), 1);
    assert_eq!(packed.get(0).unwrap(), u64::MAX);
    assert_eq!(bits::true_bit_count(&packed, 64), 64);
}

#[test]
fn test_pack_vote_options_alternating() {
    let env = Env::default();
    let mut options = [false; 64];
    for i in (0..64).step_by(2) {
        options[i] = true;
    }
    let packed = bits::pack_vote_options(&env, &options);

    assert_eq!(packed.get(0).unwrap(), 0x5555_5555_5555_5555);
    for i in 0..64u32 {
        assert_eq!(bits::is_bit_set(&packed, i), i % 2 == 0);
    }
}

#[test]
fn test_pack_vote_options_crosses_word_boundaries() {
    let env = Env::default();
    // 130 options: two full 64-bit words plus a partial third. The packed field
    // is a `Vec<u64>` with no arbitrary option cap, so this must encode cleanly.
    let mut options = alloc::vec![false; 130];
    options[0] = true;
    options[63] = true;
    options[64] = true;
    options[129] = true;

    let packed = bits::pack_vote_options(&env, &options);
    assert_eq!(packed.len(), 3);
    assert!(bits::is_bit_set(&packed, 0));
    assert!(bits::is_bit_set(&packed, 63));
    assert!(bits::is_bit_set(&packed, 64));
    assert!(bits::is_bit_set(&packed, 129));
    assert!(!bits::is_bit_set(&packed, 1));
}

#[test]
fn test_pack_vote_options_has_no_upper_bound() {
    let env = Env::default();
    // A register larger than any fixed-width cap still packs and round-trips:
    // the earlier draft rejected ballots beyond 128 options for no reason tied
    // to the governance model, silently capping registers at 128 voters.
    let mut options = alloc::vec![false; 1_000];
    options[999] = true;

    let packed = bits::pack_vote_options(&env, &options);
    assert_eq!(packed.len(), bits::option_word_count(1_000));
    assert!(bits::is_bit_set(&packed, 999));

    let decoded = bits::unpack_vote_options(&env, &packed, 1_000).unwrap();
    assert_eq!(decoded, options);
}

#[test]
fn test_pack_vote_options_round_trip() {
    let env = Env::default();
    let mut options = alloc::vec![false; 100];
    for i in 0..100usize {
        options[i] = i % 3 != 0;
    }

    let packed = bits::pack_vote_options(&env, &options);
    let decoded = bits::unpack_vote_options(&env, &packed, 100).unwrap();
    assert_eq!(decoded, options);
}

#[test]
fn test_unpack_rejects_truncated_field() {
    let env = Env::default();
    let packed = SVec::new(&env);
    assert_eq!(
        bits::unpack_vote_options(&env, &packed, 1),
        Err(ArchiveError::InvalidPackedLength)
    );
}

// ─── Vote weight packing ────────────────────────────────────────────────────

#[test]
fn test_pack_weight_rejects_zero() {
    assert_eq!(
        bits::pack_weight(0),
        Err(ArchiveError::InvalidVoteWeight),
        "a zero-weight field would be indistinguishable from an empty slot"
    );
}

#[test]
fn test_pack_weight_minimum() {
    assert_eq!(bits::pack_weight(MIN_VOTE_WEIGHT), Ok(1));
}

#[test]
fn test_pack_weight_normal_value() {
    assert_eq!(bits::pack_weight(42), Ok(42));
}

#[test]
fn test_pack_weight_maximum() {
    assert_eq!(bits::pack_weight(MAX_VOTE_WEIGHT), Ok(100));
}

#[test]
fn test_pack_weight_above_maximum_is_rejected() {
    assert_eq!(
        bits::pack_weight(MAX_VOTE_WEIGHT + 1),
        Err(ArchiveError::InvalidVoteWeight)
    );
    assert_eq!(
        bits::pack_weight(u32::MAX),
        Err(ArchiveError::InvalidVoteWeight)
    );
}

#[test]
fn test_pack_weights_round_trip_preserves_precision() {
    let env = Env::default();
    let weights: RVec<u32> = (1..=MAX_VOTE_WEIGHT).collect();

    let packed = bits::pack_weights(&env, &weights).unwrap();
    let decoded = bits::unpack_weights(&env, &packed, MAX_VOTE_WEIGHT).unwrap();

    assert_eq!(decoded, weights, "7-bit fields must be lossless for 1..=100");
}

#[test]
fn test_pack_weights_word_boundaries() {
    let env = Env::default();
    // 9 fields fill a word exactly; field 9 spills into the next word.
    let weights = [1u32, 2, 3, 4, 5, 6, 7, 8, 9, 10, 100];
    let packed = bits::pack_weights(&env, &weights).unwrap();

    assert_eq!(packed.len(), 2);
    assert_eq!(bits::packed_weight(&packed, 0).unwrap(), 1);
    assert_eq!(bits::packed_weight(&packed, 8).unwrap(), 9);
    assert_eq!(bits::packed_weight(&packed, 9).unwrap(), 10);
    assert_eq!(bits::packed_weight(&packed, 10).unwrap(), 100);
    assert_eq!(bits::weight_word_count(WEIGHTS_PER_WORD), 1);
    assert_eq!(bits::weight_word_count(WEIGHTS_PER_WORD + 1), 2);
}

#[test]
fn test_set_packed_weight_overwrites_without_bit_leakage() {
    let env = Env::default();
    let mut words = SVec::new(&env);

    bits::set_packed_weight(&mut words, 3, 100).unwrap();
    assert_eq!(bits::packed_weight(&words, 3).unwrap(), 100);

    // A smaller value must fully clear the previous 7-bit field.
    bits::set_packed_weight(&mut words, 3, 1).unwrap();
    assert_eq!(bits::packed_weight(&words, 3).unwrap(), 1);
}

#[test]
fn test_pack_weights_batch_rejects_invalid_field() {
    let env = Env::default();
    let weights = [1u32, 0, 3];
    assert_eq!(
        bits::pack_weights(&env, &weights),
        Err(ArchiveError::InvalidVoteWeight)
    );
}

#[test]
fn test_observed_vote_model_is_whole_number_and_packable() {
    // Evidence, not an assumption: this repository stores whole `u32` weights
    // in `1..=100`, which is exactly representable in a 7-bit field.
    assert!(bits::OBSERVED_VOTE_MODEL.weights_are_packable());
    assert_eq!(bits::OBSERVED_VOTE_MODEL.weight_min, MIN_VOTE_WEIGHT);
    assert_eq!(bits::OBSERVED_VOTE_MODEL.weight_max, MAX_VOTE_WEIGHT);
    assert_eq!(bits::OBSERVED_VOTE_MODEL.weight_default, 1);
    assert!(!bits::OBSERVED_VOTE_MODEL.has_fractional_weights);
}

#[test]
fn test_fractional_vote_model_disables_packing() {
    // Sub-unit precision cannot survive a 7-bit integer field. If governance
    // ever gains fractional weights the gate must fail, which sends every vote
    // to the detailed register instead of rounding it to a whole number.
    let fractional = bits::VoteModelEvidence {
        has_fractional_weights: true,
        ..bits::OBSERVED_VOTE_MODEL
    };
    assert!(!fractional.weights_are_packable());

    // A model wider than the physical field is equally unpackable.
    let widened = bits::VoteModelEvidence {
        weight_max: bits::WEIGHT_FIELD_MAX + 1,
        ..bits::OBSERVED_VOTE_MODEL
    };
    assert!(!widened.weights_are_packable());
}

#[test]
fn test_encoding_for_falls_back_instead_of_rounding() {
    // Weights the model can hold losslessly take the compact path.
    assert_eq!(bits::encoding_for(MIN_VOTE_WEIGHT), bits::Encoding::Packed);
    assert_eq!(bits::encoding_for(MAX_VOTE_WEIGHT), bits::Encoding::Packed);

    // Anything else is routed to the detailed register: never rounded,
    // clamped or dropped just to fit the packed field.
    assert_eq!(bits::encoding_for(0), bits::Encoding::DetailedFallback);
    assert_eq!(
        bits::encoding_for(MAX_VOTE_WEIGHT + 1),
        bits::Encoding::DetailedFallback
    );
    assert_eq!(bits::encoding_for(u32::MAX), bits::Encoding::DetailedFallback);
}

#[test]
fn test_packed_weight_sum_is_exact() {
    let env = Env::default();
    let weights = [7u32, 93, 1, 100];
    let packed_weights = bits::pack_weights(&env, &weights).unwrap();

    let mut options = [false; 4];
    options[0] = true;
    options[3] = true;
    let option_bits = bits::pack_vote_options(&env, &options);

    assert_eq!(bits::packed_weight_sum(&option_bits, &packed_weights, 4), 107);
}

// ─── Merkle encoding ────────────────────────────────────────────────────────

#[test]
fn test_merkle_empty_root_is_deterministic_and_distinct() {
    let env = Env::default();
    let empty = merkle::empty_root(&env);
    assert_eq!(empty, merkle::empty_root(&env));

    let leaves = SVec::new(&env);
    assert_eq!(merkle::merkle_root(&env, &leaves), empty);

    let voter = Address::generate(&env);
    let leaf = merkle::leaf_hash(&env, 1, 0, true, 1, &voter);
    assert_ne!(leaf, empty, "an empty archive must not equal a real leaf");
}

#[test]
fn test_merkle_single_leaf_root_is_the_leaf() {
    let env = Env::default();
    let voter = Address::generate(&env);
    let leaf = merkle::leaf_hash(&env, 9, 0, true, 5, &voter);

    let mut leaves = SVec::new(&env);
    leaves.push_back(leaf.clone());

    assert_eq!(merkle::merkle_root(&env, &leaves), leaf);
}

#[test]
fn test_merkle_leaf_encoding_is_domain_separated() {
    let env = Env::default();
    let voter = Address::generate(&env);

    // Different proposal ids must never collide.
    assert_ne!(
        merkle::leaf_hash(&env, 1, 0, true, 1, &voter),
        merkle::leaf_hash(&env, 2, 0, true, 1, &voter)
    );
    // Option, weight and voter index are all bound into the leaf.
    assert_ne!(
        merkle::leaf_hash(&env, 1, 0, true, 1, &voter),
        merkle::leaf_hash(&env, 1, 1, true, 1, &voter)
    );
    assert_ne!(
        merkle::leaf_hash(&env, 1, 0, true, 1, &voter),
        merkle::leaf_hash(&env, 1, 0, true, 2, &voter)
    );
    assert_ne!(
        merkle::leaf_hash(&env, 1, 0, true, 1, &voter),
        merkle::leaf_hash(&env, 1, 0, false, 1, &voter)
    );
    assert_ne!(
        merkle::leaf_hash(&env, 1, 0, true, 1, &voter),
        merkle::leaf_hash(&env, 1, 0, true, 1, &Address::generate(&env))
    );
}

fn sample_leaves(env: &Env, count: u32) -> SVec<BytesN<32>> {
    let mut voters = RVec::new();
    for _ in 0..count {
        voters.push(Address::generate(env));
    }
    let mut leaves = SVec::new(env);
    for (i, voter) in voters.iter().enumerate() {
        leaves.push_back(merkle::leaf_hash(
            env,
            1,
            i as u32,
            true,
            weight_for(i as u32),
            voter,
        ));
    }
    leaves
}

#[test]
fn test_merkle_multiple_leaves_proofs_all_verify() {
    let env = Env::default();
    let leaves = sample_leaves(&env, 7);
    let root = merkle::merkle_root(&env, &leaves);

    for index in 0..7u32 {
        let proof = merkle::merkle_proof(&env, &leaves, index);
        assert!(
            merkle::verify_proof(&env, &root, &leaves.get(index).unwrap(), index, 7, &proof),
            "proof for leaf {} must verify",
            index
        );
    }
}

#[test]
fn test_merkle_root_over_1000_votes_is_deterministic() {
    let env = Env::default();
    // ~3,000 hashes over 1,000 leaves plus proofs would exhaust the default
    // test budget, which then also fails the ledger-snapshot write on drop.
    env.budget().reset_unlimited();
    let leaves = sample_leaves(&env, 1000);

    let root_a = merkle::merkle_root(&env, &leaves);
    let root_b = merkle::merkle_root(&env, &leaves);
    assert_eq!(root_a, root_b);

    for index in [0u32, 1, 499, 998, 999] {
        let proof = merkle::merkle_proof(&env, &leaves, index);
        assert!(merkle::verify_proof(
            &env,
            &root_a,
            &leaves.get(index).unwrap(),
            index,
            1000,
            &proof
        ));
        assert!(proof.len() <= 10, "log2(1000) ~= 10 siblings");
    }
}

#[test]
fn test_merkle_proof_rejects_tampered_leaf() {
    let env = Env::default();
    let leaves = sample_leaves(&env, 5);
    let root = merkle::merkle_root(&env, &leaves);
    let proof = merkle::merkle_proof(&env, &leaves, 2);

    let tampered = merkle::leaf_hash(&env, 1, 2, true, 99, &Address::generate(&env));
    assert!(!merkle::verify_proof(&env, &root, &tampered, 2, 5, &proof));

    // Same leaf, wrong position.
    let leaf = leaves.get(2).unwrap();
    assert!(!merkle::verify_proof(&env, &root, &leaf, 3, 5, &proof));

    // Truncated proof.
    let mut short = SVec::new(&env);
    for i in 0..proof.len().saturating_sub(1) {
        short.push_back(proof.get(i).unwrap());
    }
    assert!(!merkle::verify_proof(&env, &root, &leaf, 2, 5, &short));
}

#[test]
fn test_merkle_proof_rejects_unrelated_root() {
    let env = Env::default();
    let leaves = sample_leaves(&env, 4);
    let other = sample_leaves(&env, 4);
    let other_root = merkle::merkle_root(&env, &other);
    let proof = merkle::merkle_proof(&env, &leaves, 1);

    assert!(!merkle::verify_proof(
        &env,
        &other_root,
        &leaves.get(1).unwrap(),
        1,
        4,
        &proof
    ));
}

// ─── Register life cycle ────────────────────────────────────────────────────

#[test]
fn test_cast_vote_writes_packed_register_only() {
    let (env, contract_id, admin) = setup();
    let voters = seed_voters(&env, &contract_id, &admin, 5);
    open_proposal(&env, &contract_id, &admin, 1);

    env.as_contract(&contract_id, || {
        register::set_admin_weight(&env, admin.clone(), voters[0].clone(), 7).unwrap();
        let count = register::cast_vote(&env, voters[0].clone(), 1).unwrap();
        assert_eq!(count, 1);

        // Optimized layout: one bit + one 7-bit field, no address list.
        assert_eq!(register::get_vote_bits(&env, 1).len(), 1);
        assert_eq!(register::get_packed_weights(&env, 1).len(), 1);
        assert_eq!(register::get_action_votes(&env, 1).len(), 0);
        assert!(register::has_voted(&env, 1, 0));
        assert_eq!(register::voter_index(&env, &voters[0]), Some(0));
    });
}

#[test]
fn test_cast_vote_rejects_unauthorized_and_duplicate() {
    let (env, contract_id, admin) = setup();
    let voters = seed_voters(&env, &contract_id, &admin, 3);
    let outsider = Address::generate(&env);
    open_proposal(&env, &contract_id, &admin, 1);

    env.as_contract(&contract_id, || {
        assert_eq!(
            register::cast_vote(&env, outsider, 1),
            Err(ArchiveError::NotAuthorized)
        );
        register::cast_vote(&env, voters[1].clone(), 1).unwrap();
        assert_eq!(
            register::cast_vote(&env, voters[1].clone(), 1),
            Err(ArchiveError::AlreadyVoted)
        );
        assert_eq!(
            register::cast_vote(&env, Address::generate(&env), 42),
            Err(ArchiveError::ProposalNotFound)
        );
    });
}

#[test]
fn test_packed_tally_matches_detailed_tally() {
    let (env, contract_id, admin) = setup();
    let voters = seed_voters(&env, &contract_id, &admin, 12);

    env.as_contract(&contract_id, || {
        register::propose_action(&env, admin.clone(), 1).unwrap();

        let mut expected: u64 = 0;
        for (i, voter) in voters.iter().enumerate() {
            let weight = weight_for(i as u32);
            register::set_admin_weight(&env, admin.clone(), voter.clone(), weight).unwrap();
            register::cast_vote(&env, voter.clone(), 1).unwrap();
            expected += weight as u64;
        }

        // The compact register and the legacy detailed register agree exactly.
        assert_eq!(register::tally(&env, 1).unwrap(), expected);
        assert_eq!(register::tally_detailed(&env, 1).unwrap(), 0);

        let options = register::get_vote_options(&env, 1).unwrap();
        let weights = register::get_vote_weights(&env, 1).unwrap();
        assert_eq!(options.len(), 12);
        assert_eq!(weights.len(), 12);
        for i in 0..12 {
            assert!(options.get(i).unwrap());
            assert_eq!(
                weights.get(i).unwrap(),
                weight_for(i),
                "packed weight must round-trip the configured governance weight"
            );
        }
    });
}

#[test]
fn test_legacy_detailed_register_tally_is_unchanged() {
    let (env, contract_id, admin) = setup();
    let voters = seed_voters(&env, &contract_id, &admin, 5);
    open_proposal(&env, &contract_id, &admin, 1);

    env.as_contract(&contract_id, || {
        let mut expected: u64 = 0;
        for (i, voter) in voters.iter().enumerate() {
            let weight = weight_for(i as u32);
            register::set_admin_weight(&env, admin.clone(), voter.clone(), weight).unwrap();
            register::cast_vote_detailed(&env, voter.clone(), 1).unwrap();
            expected += weight as u64;
        }

        assert_eq!(register::get_action_votes(&env, 1).len(), 5);
        assert_eq!(register::tally_detailed(&env, 1).unwrap(), expected);
        assert_eq!(register::tally(&env, 1).unwrap(), expected);
    });
}

#[test]
fn test_finalize_rejects_tally_below_threshold_without_mutation() {
    let (env, contract_id, admin) = setup();
    let voters = seed_voters(&env, &contract_id, &admin, 5);
    open_proposal(&env, &contract_id, &admin, 1);

    env.as_contract(&contract_id, || {
        register::cast_vote(&env, voters[0].clone(), 1).unwrap();

        assert_eq!(
            register::finalize(&env, 1),
            Err(ArchiveError::ThresholdNotReached)
        );
        let state = register::get_proposal(&env, 1).unwrap();
        assert!(!state.executed);
        assert_eq!(state.final_tally, 0);
    });
}

#[test]
fn test_finalize_uses_vote_count_threshold_for_small_councils() {
    let (env, contract_id, admin) = setup();
    let voters = seed_voters(&env, &contract_id, &admin, 3);
    open_proposal(&env, &contract_id, &admin, 1);

    env.as_contract(&contract_id, || {
        assert_eq!(register::required_threshold(&env), 2);
        register::cast_vote(&env, voters[0].clone(), 1).unwrap();
        register::cast_vote(&env, voters[1].clone(), 1).unwrap();

        let state = register::finalize(&env, 1).unwrap();
        assert!(state.executed);
        assert_eq!(state.final_tally, 2);
    });
}

#[test]
fn test_finalize_uses_configured_weight_threshold() {
    let (env, contract_id, admin) = setup();
    let voters = seed_voters(&env, &contract_id, &admin, 5);
    open_proposal(&env, &contract_id, &admin, 1);

    env.as_contract(&contract_id, || {
        // 50 + 31 = 81, one short of the configured 82, so the first attempt
        // must be rejected; the default-1 third voter then tips it to 82.
        register::set_weight_threshold(&env, admin.clone(), 82).unwrap();
        assert_eq!(register::required_threshold(&env), 82);

        register::set_admin_weight(&env, admin.clone(), voters[0].clone(), 50).unwrap();
        register::set_admin_weight(&env, admin.clone(), voters[1].clone(), 31).unwrap();
        register::cast_vote(&env, voters[0].clone(), 1).unwrap();
        register::cast_vote(&env, voters[1].clone(), 1).unwrap();

        assert_eq!(register::finalize(&env, 1), Err(ArchiveError::ThresholdNotReached));

        register::cast_vote(&env, voters[2].clone(), 1).unwrap();
        let state = register::finalize(&env, 1).unwrap();
        assert!(state.executed);
        assert_eq!(state.final_tally, 82);
    });
}

#[test]
fn test_set_weight_threshold_validates_range() {
    let (env, contract_id, admin) = setup();
    env.as_contract(&contract_id, || {
        assert_eq!(
            register::set_weight_threshold(&env, admin.clone(), 0),
            Err(ArchiveError::InvalidThreshold)
        );
        assert_eq!(
            register::set_weight_threshold(&env, admin.clone(), 101),
            Err(ArchiveError::InvalidThreshold)
        );
        assert!(register::set_weight_threshold(&env, admin.clone(), 100).is_ok());
    });
}

// ─── Compression ────────────────────────────────────────────────────────────

fn finalized_proposal(env: &Env, contract_id: &Address, admin: &Address, voters: &[Address]) {
    env.as_contract(contract_id, || {
        register::propose_action(env, admin.clone(), 7).unwrap();
        for (i, voter) in voters.iter().enumerate() {
            register::set_admin_weight(env, admin.clone(), voter.clone(), weight_for(i as u32))
                .unwrap();
            register::cast_vote(env, voter.clone(), 7).unwrap();
        }
        register::finalize(env, 7).unwrap();
    });
}

#[test]
fn test_compress_requires_finalization() {
    let (env, contract_id, admin) = setup();
    let voters = seed_voters(&env, &contract_id, &admin, 5);
    open_proposal(&env, &contract_id, &admin, 1);

    env.as_contract(&contract_id, || {
        register::cast_vote(&env, voters[0].clone(), 1).unwrap();
        assert_eq!(
            register::compress_after_execution(&env, 1),
            Err(ArchiveError::ProposalNotExecuted),
            "active voting state must never be compressed"
        );
        // The register is untouched by the rejected attempt.
        assert_eq!(register::get_vote_bits(&env, 1).len(), 1);
    });
}

#[test]
fn test_compress_replaces_register_with_merkle_root() {
    let (env, contract_id, admin) = setup();
    let voters = seed_voters(&env, &contract_id, &admin, 9);
    finalized_proposal(&env, &contract_id, &admin, &voters);

    env.as_contract(&contract_id, || {
        let tally_before = register::tally(&env, 7).unwrap();
        let record = register::compress_after_execution(&env, 7).unwrap();

        assert_eq!(record.leaf_count, 9);
        assert_eq!(record.final_tally, tally_before);
        assert_ne!(record.root, merkle::empty_root(&env));

        // Detailed per-voter register is gone; root and issue state remain.
        assert!(!env.storage().persistent().has(&DataKey::VoteBits(7)));
        assert!(!env.storage().persistent().has(&DataKey::PackedWeights(7)));
        assert!(!env.storage().persistent().has(&DataKey::ActionVotes(7)));
        assert!(env.storage().persistent().has(&DataKey::Archive(7)));

        // Governance semantics are unchanged by compression.
        assert_eq!(register::tally(&env, 7).unwrap(), tally_before);
        assert!(register::get_proposal(&env, 7).unwrap().executed);
    });
}

#[test]
fn test_compress_is_rejected_when_already_compressed() {
    let (env, contract_id, admin) = setup();
    let voters = seed_voters(&env, &contract_id, &admin, 5);
    finalized_proposal(&env, &contract_id, &admin, &voters);

    env.as_contract(&contract_id, || {
        let first = register::compress_after_execution(&env, 7).unwrap();
        assert_eq!(
            register::compress_after_execution(&env, 7),
            Err(ArchiveError::AlreadyCompressed),
            "repeated compression must not overwrite the committed root"
        );
        let stored = register::get_archive(&env, 7).unwrap();
        assert_eq!(stored.root, first.root);
        assert_eq!(stored.final_tally, first.final_tally);
    });
}

#[test]
fn test_compress_legacy_detailed_register() {
    let (env, contract_id, admin) = setup();
    let voters = seed_voters(&env, &contract_id, &admin, 6);

    env.as_contract(&contract_id, || {
        register::propose_action(&env, admin.clone(), 7).unwrap();
        let mut expected: u64 = 0;
        for (i, voter) in voters.iter().enumerate() {
            let weight = weight_for(i as u32);
            register::set_admin_weight(&env, admin.clone(), voter.clone(), weight).unwrap();
            register::cast_vote_detailed(&env, voter.clone(), 7).unwrap();
            expected += weight as u64;
        }
        register::finalize(&env, 7).unwrap();

        // Old representation -> new representation.
        let record = register::compress_after_execution(&env, 7).unwrap();
        assert_eq!(record.leaf_count, 6);
        assert_eq!(record.final_tally, expected);
        assert!(!env.storage().persistent().has(&DataKey::ActionVotes(7)));
    });
}

#[test]
fn test_compression_does_not_remove_shared_weights() {
    let (env, contract_id, admin) = setup();
    let voters = seed_voters(&env, &contract_id, &admin, 5);
    finalized_proposal(&env, &contract_id, &admin, &voters);

    env.as_contract(&contract_id, || {
        register::compress_after_execution(&env, 7).unwrap();

        // AdminWeight is shared governance configuration, not proposal history:
        // removing it would change the tally of future proposals.
        assert_eq!(register::admin_weight(&env, voters[0].clone()), weight_for(0));
        register::propose_action(&env, admin.clone(), 8).unwrap();
        register::cast_vote(&env, voters[0].clone(), 8).unwrap();
        assert_eq!(register::tally(&env, 8).unwrap(), weight_for(0) as u64);
    });
}

#[test]
fn test_votes_rejected_after_compression() {
    let (env, contract_id, admin) = setup();
    let voters = seed_voters(&env, &contract_id, &admin, 5);
    finalized_proposal(&env, &contract_id, &admin, &voters);

    env.as_contract(&contract_id, || {
        register::compress_after_execution(&env, 7).unwrap();
        assert_eq!(
            register::cast_vote(&env, voters[0].clone(), 7),
            Err(ArchiveError::ProposalClosed)
        );
    });
}

// ─── Historical verification ────────────────────────────────────────────────

#[test]
fn test_archived_votes_remain_verifiable() {
    let (env, contract_id, admin) = setup();
    let voters = seed_voters(&env, &contract_id, &admin, 8);
    finalized_proposal(&env, &contract_id, &admin, &voters);

    env.as_contract(&contract_id, || {
        // Proofs are produced while the detailed register still exists.
        let voter_index = 5u32;
        let proof = register::merkle_proof_for_voter(&env, 7, voter_index).unwrap();
        let leaf_position = voter_index; // every voter participated, ascending

        let record: ArchiveRecord = register::compress_after_execution(&env, 7).unwrap();

        assert!(register::verify_archived_vote(
            &env,
            7,
            leaf_position,
            voter_index,
            voters[voter_index as usize].clone(),
            true,
            weight_for(voter_index),
            &proof,
        )
        .unwrap());

        // The recomputed leaf matches the canonical encoding helper.
        let leaf = register::build_leaf(
            &env,
            7,
            voter_index,
            voters[voter_index as usize].clone(),
            true,
            weight_for(voter_index),
        );
        assert!(merkle::verify_proof(
            &env,
            &record.root,
            &leaf,
            leaf_position,
            record.leaf_count,
            &proof
        ));
    });
}

#[test]
fn test_archived_verification_rejects_tampered_and_wrong_proofs() {
    let (env, contract_id, admin) = setup();
    let voters = seed_voters(&env, &contract_id, &admin, 6);
    finalized_proposal(&env, &contract_id, &admin, &voters);

    env.as_contract(&contract_id, || {
        let proof = register::merkle_proof_for_voter(&env, 7, 2).unwrap();
        register::compress_after_execution(&env, 7).unwrap();
        let voter = voters[2].clone();

        // Correct claim verifies.
        assert!(register::verify_archived_vote(&env, 7, 2, 2, voter.clone(), true, weight_for(2), &proof).unwrap());

        // Altered weight.
        assert!(!register::verify_archived_vote(&env, 7, 2, 2, voter.clone(), true, weight_for(2) + 1, &proof).unwrap());

        // Altered option.
        assert!(!register::verify_archived_vote(&env, 7, 2, 2, voter.clone(), false, weight_for(2), &proof).unwrap());

        // Altered voter.
        assert!(!register::verify_archived_vote(&env, 7, 2, 2, voters[3].clone(), true, weight_for(2), &proof).unwrap());

        // Proof generation after compression is refused (leaves are gone).
        let wrong_proof = register::merkle_proof_for_voter(&env, 7, 3);
        assert_eq!(wrong_proof.err(), Some(ArchiveError::AlreadyCompressed));
    });
}

#[test]
fn test_proof_generation_requires_detailed_register() {
    let (env, contract_id, admin) = setup();
    let voters = seed_voters(&env, &contract_id, &admin, 5);
    finalized_proposal(&env, &contract_id, &admin, &voters);

    env.as_contract(&contract_id, || {
        assert_eq!(
            register::merkle_proof_for_voter(&env, 7, 9),
            Err(ArchiveError::UnknownVoter)
        );
        assert_eq!(
            register::verify_archived_vote(
                &env,
                7,
                0,
                0,
                voters[0].clone(),
                true,
                1,
                &SVec::new(&env)
            ),
            Err(ArchiveError::ArchiveNotFound)
        );
    });
}

#[test]
fn test_root_is_independent_of_vote_casting_order() {
    let env = Env::default();
    env.mock_all_auths();

    let first = env.register_contract(None, VoteArchiveHarness);
    let second = env.register_contract(None, VoteArchiveHarness);
    let admin = Address::generate(&env);

    let mut voters = RVec::new();
    for _ in 0..6 {
        voters.push(Address::generate(&env));
    }

    for contract_id in [&first, &second] {
        env.as_contract(contract_id, || {
            register::initialize(&env, admin.clone()).unwrap();
            for voter in voters.iter() {
                register::add_voter(&env, admin.clone(), voter.clone()).unwrap();
            }
        });
    }

    let forward: RVec<usize> = (0..voters.len()).collect();
    let reversed: RVec<usize> = (0..voters.len()).rev().collect();

    // Identical logical vote set, opposite casting order.
    for (contract_id, order) in [(&first, &forward), (&second, &reversed)] {
        env.as_contract(contract_id, || {
            register::propose_action(&env, admin.clone(), 7).unwrap();
            for index in order.iter() {
                let voter = voters.get(*index).unwrap();
                register::set_admin_weight(
                    &env,
                    admin.clone(),
                    voter.clone(),
                    weight_for(*index as u32),
                )
                .unwrap();
                register::cast_vote(&env, voter.clone(), 7).unwrap();
            }
            register::finalize(&env, 7).unwrap();
        });
    }

    let root_a = env.as_contract(&first, || {
        register::compress_after_execution(&env, 7).unwrap().root
    });
    let root_b = env.as_contract(&second, || {
        register::compress_after_execution(&env, 7).unwrap().root
    });

    assert_eq!(
        root_a, root_b,
        "the root must depend on the canonical voter order, not on cast order"
    );
}
