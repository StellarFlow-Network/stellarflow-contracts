//! Voting-power snapshot module — issue #302.
//!
//! # Problem
//! Without snapshots, an attacker can:
//! 1. Wait for a proposal to go live.
//! 2. Acquire or borrow tokens to inflate their `ProviderWeight`.
//! 3. Vote with the inflated weight.
//! 4. Dump / return the tokens immediately after.
//!
//! # Solution
//! At the moment `propose_action` is called, this module iterates every
//! registered admin and writes a `VotingCheckpoint` keyed by
//! `DataKey::VotingSnapshot(action_id, voter)` into **persistent** storage.
//! The checkpoint is immutable — it is written once and never updated.
//!
//! `vote_for_action` then reads the checkpoint weight instead of the live
//! `ProviderWeight`, so any balance change made after the proposal was
//! submitted has zero effect on that vote.
//!
//! # Storage layout
//! | Key                                    | Type               | Durability  |
//! |----------------------------------------|--------------------|-------------|
//! | `VotingSnapshot(action_id, voter)`     | `VotingCheckpoint` | persistent  |
//! | `ProposalLedger(action_id)`            | `u32`              | persistent  |
//!
//! Both keys are written once at proposal time and are never mutated.

use soroban_sdk::{Address, Env, Vec};

use crate::auth::{_get_admin, _get_provider_weight};
use crate::types::{DataKey, VotingCheckpoint};

// ─────────────────────────────────────────────────────────────────────────────
// Snapshot creation
// ─────────────────────────────────────────────────────────────────────────────

/// Record a voting-power checkpoint for every registered admin at the current
/// ledger height.
///
/// Called exactly once per proposal, inside `propose_action`, immediately
/// after the `ProposedAction` is stored. The function is idempotent for a
/// given `action_id` — if a checkpoint already exists for a voter it is
/// **not** overwritten (belt-and-suspenders guard against double-calls).
///
/// # Arguments
/// * `env`       - Contract environment
/// * `action_id` - The ID of the newly created proposal
pub fn record_snapshot(env: &Env, action_id: u64) {
    let admins: Vec<Address> = _get_admin(env);
    let ledger_sequence = env.ledger().sequence();
    let timestamp = env.ledger().timestamp();

    // Record the ledger height so off-chain tooling can verify the snapshot.
    env.storage()
        .persistent()
        .set(&DataKey::ProposalLedger(action_id), &ledger_sequence);

    for voter in admins.iter() {
        let key = DataKey::VotingSnapshot(action_id, voter.clone());

        // Idempotency guard — never overwrite an existing checkpoint.
        if env.storage().persistent().has(&key) {
            continue;
        }

        let weight = _get_provider_weight(env, &voter);

        let checkpoint = VotingCheckpoint {
            action_id,
            voter: voter.clone(),
            weight,
            ledger_sequence,
            timestamp,
        };

        env.storage().persistent().set(&key, &checkpoint);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Snapshot reads
// ─────────────────────────────────────────────────────────────────────────────

/// Return the snapshotted voting power for `voter` on `action_id`.
///
/// Returns `None` if no checkpoint exists (voter was not an admin when the
/// proposal was created, or the proposal pre-dates the snapshot system).
pub fn get_snapshot_weight(env: &Env, action_id: u64, voter: &Address) -> Option<u32> {
    env.storage()
        .persistent()
        .get::<DataKey, VotingCheckpoint>(&DataKey::VotingSnapshot(action_id, voter.clone()))
        .map(|cp| cp.weight)
}

/// Return the full `VotingCheckpoint` for `voter` on `action_id`, or `None`.
pub fn get_checkpoint(env: &Env, action_id: u64, voter: &Address) -> Option<VotingCheckpoint> {
    env.storage()
        .persistent()
        .get(&DataKey::VotingSnapshot(action_id, voter.clone()))
}

/// Return the ledger sequence at which `action_id` was proposed, or `None`
/// if the proposal pre-dates the snapshot system.
pub fn get_proposal_ledger(env: &Env, action_id: u64) -> Option<u32> {
    env.storage()
        .persistent()
        .get(&DataKey::ProposalLedger(action_id))
}

/// Return `true` if the voter was an admin at proposal time (i.e. a checkpoint
/// exists for them on this action).
pub fn voter_was_eligible(env: &Env, action_id: u64, voter: &Address) -> bool {
    env.storage()
        .persistent()
        .has(&DataKey::VotingSnapshot(action_id, voter.clone()))
}

// ─────────────────────────────────────────────────────────────────────────────
// Weighted vote counting
// ─────────────────────────────────────────────────────────────────────────────

/// Compute the total snapshotted weight for all voters who have already cast
/// a vote on `action_id`.
///
/// Used by `execute_proposed_action` to enforce a weight-based quorum in
/// addition to the head-count threshold.
pub fn total_voted_weight(env: &Env, action_id: u64, voters: &Vec<Address>) -> u32 {
    let mut total: u32 = 0;
    for voter in voters.iter() {
        let w = get_snapshot_weight(env, action_id, &voter).unwrap_or(0);
        total = total.saturating_add(w);
    }
    total
}

/// Compute the total snapshotted weight across **all** admins for `action_id`.
///
/// This is the denominator when calculating what fraction of total weight has
/// voted. Returns 0 if no snapshot exists (pre-snapshot proposals).
pub fn total_eligible_weight(env: &Env, action_id: u64) -> u32 {
    let admins: Vec<Address> = _get_admin(env);
    let mut total: u32 = 0;
    for voter in admins.iter() {
        let w = get_snapshot_weight(env, action_id, &voter).unwrap_or(0);
        total = total.saturating_add(w);
    }
    total
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::{contract, contractimpl, Env};

    #[contract]
    struct TestContract;

    #[contractimpl]
    impl TestContract {}

    fn setup_with_admins(weights: &[u32]) -> (Env, soroban_sdk::Address, Vec<Address>) {
        let env = Env::default();
        let contract_id = env.register(TestContract, ());
        let mut admins = Vec::new(&env);
        for &w in weights {
            let addr = Address::generate(&env);
            admins.push_back(addr.clone());
            env.as_contract(&contract_id, || {
                crate::auth::_set_provider_weight(&env, &addr, w);
            });
        }
        env.as_contract(&contract_id, || {
            crate::auth::_set_admin(&env, &admins);
        });
        (env, contract_id, admins)
    }

    // ── record_snapshot ───────────────────────────────────────────────────────

    #[test]
    fn test_snapshot_records_all_admins() {
        let (env, contract_id, admins) = setup_with_admins(&[50, 75, 100]);
        env.as_contract(&contract_id, || {
            record_snapshot(&env, 1);
            assert_eq!(get_snapshot_weight(&env, 1, &admins.get(0).unwrap()), Some(50));
            assert_eq!(get_snapshot_weight(&env, 1, &admins.get(1).unwrap()), Some(75));
            assert_eq!(get_snapshot_weight(&env, 1, &admins.get(2).unwrap()), Some(100));
        });
    }

    #[test]
    fn test_snapshot_is_immutable_after_weight_change() {
        let (env, contract_id, admins) = setup_with_admins(&[50]);
        let voter = admins.get(0).unwrap();
        env.as_contract(&contract_id, || {
            record_snapshot(&env, 1);
            // Simulate attacker inflating weight after proposal
            crate::auth::_set_provider_weight(&env, &voter, 999);
            // Snapshot must still reflect the original weight
            assert_eq!(get_snapshot_weight(&env, 1, &voter), Some(50));
        });
    }

    #[test]
    fn test_snapshot_idempotent_on_double_call() {
        let (env, contract_id, admins) = setup_with_admins(&[60]);
        let voter = admins.get(0).unwrap();
        env.as_contract(&contract_id, || {
            record_snapshot(&env, 1);
            // Change weight then call again — must not overwrite
            crate::auth::_set_provider_weight(&env, &voter, 999);
            record_snapshot(&env, 1);
            assert_eq!(get_snapshot_weight(&env, 1, &voter), Some(60));
        });
    }

    #[test]
    fn test_different_proposals_have_independent_snapshots() {
        let (env, contract_id, admins) = setup_with_admins(&[40]);
        let voter = admins.get(0).unwrap();
        env.as_contract(&contract_id, || {
            record_snapshot(&env, 1);
            crate::auth::_set_provider_weight(&env, &voter, 80);
            record_snapshot(&env, 2);
            assert_eq!(get_snapshot_weight(&env, 1, &voter), Some(40));
            assert_eq!(get_snapshot_weight(&env, 2, &voter), Some(80));
        });
    }

    #[test]
    fn test_non_admin_has_no_snapshot() {
        let (env, contract_id, _) = setup_with_admins(&[50]);
        let outsider = Address::generate(&env);
        env.as_contract(&contract_id, || {
            record_snapshot(&env, 1);
            assert_eq!(get_snapshot_weight(&env, 1, &outsider), None);
            assert!(!voter_was_eligible(&env, 1, &outsider));
        });
    }

    #[test]
    fn test_proposal_ledger_is_recorded() {
        let (env, contract_id, _) = setup_with_admins(&[50]);
        env.as_contract(&contract_id, || {
            record_snapshot(&env, 42);
            let ledger = get_proposal_ledger(&env, 42);
            assert!(ledger.is_some());
        });
    }

    // ── total_voted_weight ────────────────────────────────────────────────────

    #[test]
    fn test_total_voted_weight_sums_correctly() {
        let (env, contract_id, admins) = setup_with_admins(&[30, 70]);
        env.as_contract(&contract_id, || {
            record_snapshot(&env, 1);
            // Both admins voted
            let mut voters = Vec::new(&env);
            voters.push_back(admins.get(0).unwrap());
            voters.push_back(admins.get(1).unwrap());
            assert_eq!(total_voted_weight(&env, 1, &voters), 100);
        });
    }

    #[test]
    fn test_total_eligible_weight_sums_all_admins() {
        let (env, contract_id, _) = setup_with_admins(&[25, 25, 50]);
        env.as_contract(&contract_id, || {
            record_snapshot(&env, 1);
            assert_eq!(total_eligible_weight(&env, 1), 100);
        });
    }

    #[test]
    fn test_zero_weight_admin_included_in_snapshot() {
        let (env, contract_id, admins) = setup_with_admins(&[0, 100]);
        env.as_contract(&contract_id, || {
            record_snapshot(&env, 1);
            assert_eq!(get_snapshot_weight(&env, 1, &admins.get(0).unwrap()), Some(0));
            assert_eq!(get_snapshot_weight(&env, 1, &admins.get(1).unwrap()), Some(100));
        });
    }
}
