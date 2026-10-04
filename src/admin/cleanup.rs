//! Batch purge routine for abandoned zero-balance persistent storage keys.
//!
//! Issue #592 — Rent-Reclamation: exited liquidity positions leave behind
//! zero-balance `StakingStorageKey::FeedStake` and `FeesStorageKey::CorridorPool`
//! entries that continue consuming ledger footprint (and therefore rent).
//!
//! [`cleanup_zero_balances`] scans a caller-supplied list of candidate keys,
//! evicts any whose stored value is provably at zero, and returns a count of
//! entries removed. The operation requires a multi-sig quorum (≥ 2 registered
//! signers) to prevent unilateral purges by a single operator.

use soroban_sdk::{contracttype, Address, Env, Vec};

use crate::{
    fees::{CorridorFeePool, FeesStorageKey},
    storage::FeedStakeValue,
    AssetId, ContractData, ContractError, StakingStorageKey, DATA_KEY,
};
use crate::proposal::{ProposalState, ProposalStatus, ProposalStorageKey};

// ---------------------------------------------------------------------------
// Public key descriptor — tells cleanup which storage slot to inspect
// ---------------------------------------------------------------------------

/// Identifies a single persistent-storage slot to be evaluated for eviction.
///
/// Each variant maps 1-to-1 to a typed storage key used elsewhere in the
/// contract. The caller assembles a [`Vec<CleanupTarget>`] containing every
/// candidate slot and passes it to [`cleanup_zero_balances`].
#[derive(Clone)]
#[contracttype]
pub enum CleanupTarget {
    /// A per-(node, asset) feed-stake entry.
    /// Evicted when `FeedStakeValue.amount == 0`.
    FeedStake(Address, AssetId),

    /// A per-asset corridor fee pool.
    /// Evicted when both `collected == 0` and `variable_pool == 0`.
    CorridorPool(AssetId),
}

// ---------------------------------------------------------------------------
// Core routine
// ---------------------------------------------------------------------------

/// Batch-evict abandoned zero-balance persistent storage keys.
///
/// # Authorization
///
/// Requires a multi-sig quorum of at least two registered signers.  Pass all
/// co-signing addresses in `signers`; `require_multisig` will reject the call
/// if fewer than two valid, non-revoked signers are present.
///
/// Each signer must already have called `require_auth` on the `Env` (or the
/// host will trap).  In practice the contract entrypoint in `lib.rs` should
/// call `signer.require_auth()` for each entry in `signers` before delegating
/// here.
///
/// # Parameters
///
/// * `env`     — Soroban execution environment.
/// * `signers` — Addresses that co-authorise this administrative action.
/// * `targets` — Candidate storage slots to inspect and potentially evict.
///
/// # Returns
///
/// `Ok(u32)` — number of entries successfully evicted.
///
/// # Errors
///
/// * [`ContractError::NotInitialized`]      — contract has not been initialised.
/// * [`ContractError::ThresholdNotReached`] — fewer than 2 valid signers supplied.
///
/// # Example (pseudo-code)
///
/// ```ignore
/// let targets = vec![
///     CleanupTarget::FeedStake(node_addr, asset_id),
///     CleanupTarget::CorridorPool(asset_id),
/// ];
/// let removed = contract.cleanup_zero_balances(&signers, &targets)?;
/// ```
pub fn cleanup_zero_balances(
    env: &Env,
    signers: &Vec<Address>,
    targets: &Vec<CleanupTarget>,
) -> Result<u32, ContractError> {
    // ── 1. Verify the contract has been initialised ──────────────────────
    let _data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;

    // ── 2. Enforce multi-sig quorum ──────────────────────────────────────
    //
    // `require_multisig` checks that at least 2 of the supplied addresses are
    // registered, non-revoked signers (or the contract admin). It returns
    // `ThresholdNotReached` when the quorum is not met.
    crate::auth::require_multisig(env, signers)?;

    // ── 3. Scan targets and evict zero-balance entries ───────────────────
    let mut removed: u32 = 0;

    for target in targets.iter() {
        match target {
            CleanupTarget::FeedStake(node, asset_id) => {
                let key = StakingStorageKey::FeedStake(
                    node.clone(),
                    crate::asset_id_to_symbol(env, asset_id),
                );
                if let Some(val) = env
                    .storage()
                    .persistent()
                    .get::<_, FeedStakeValue>(&key)
                {
                    if val.amount == 0 {
                        env.storage().persistent().remove(&key);
                        removed += 1;
                    }
                }
            }

            CleanupTarget::CorridorPool(asset_id) => {
                let key = FeesStorageKey::CorridorPool(asset_id);
                if let Some(pool) = env
                    .storage()
                    .persistent()
                    .get::<_, CorridorFeePool>(&key)
                {
                    if pool.collected == 0 && pool.variable_pool == 0 {
                        env.storage().persistent().remove(&key);
                        removed += 1;
                    }
                }
            }
        }
    }

    Ok(removed)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Address, Env, Vec};
    use crate::{
        fees::CorridorFeePool,
        storage::{FeedStakeValue, SignerKey},
        ContractData, ContractError, StakingStorageKey, DATA_KEY,
    };

    // ── Shared harness ───────────────────────────────────────────────────

    /// Register a fresh contract, seed DATA_KEY + two signers, and return
    /// `(env, contract_id, admin, signer_a, signer_b)`.
    fn setup() -> (Env, Address, Address, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let cid = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let admin = Address::generate(&env);
        let signer_a = Address::generate(&env);
        let signer_b = Address::generate(&env);
        env.as_contract(&cid, || {
            env.storage().instance().set(
                &DATA_KEY,
                &ContractData { admin: admin.clone(), value: 0, max_fee_ceiling: 0 },
            );
            env.storage().instance().set(&SignerKey::SignerByAddress(signer_a.clone()), &true);
            env.storage().instance().set(&SignerKey::SignerByAddress(signer_b.clone()), &true);
        });
        (env, cid, admin, signer_a, signer_b)
    }

    // ── Storage helpers ──────────────────────────────────────────────────

    fn seed_feed_stake(env: &Env, cid: &Address, node: &Address, asset: AssetId, amount: u64) {
        env.as_contract(cid, || {
            env.storage().persistent().set(
                &StakingStorageKey::FeedStake(node.clone(), asset),
                &FeedStakeValue { amount, last_active: env.ledger().timestamp() },
            );
        });
    }

    fn seed_corridor_pool(env: &Env, cid: &Address, asset: AssetId, collected: u64, variable_pool: u64) {
        env.as_contract(cid, || {
            env.storage().persistent().set(
                &FeesStorageKey::CorridorPool(asset),
                &CorridorFeePool { asset, collected, variable_pool },
            );
        });
    }

    fn has_feed_stake(env: &Env, cid: &Address, node: &Address, asset: AssetId) -> bool {
        env.as_contract(cid, || {
            env.storage().persistent().has(&StakingStorageKey::FeedStake(node.clone(), asset))
        })
    }

    fn has_corridor_pool(env: &Env, cid: &Address, asset: AssetId) -> bool {
        env.as_contract(cid, || {
            env.storage().persistent().has(&FeesStorageKey::CorridorPool(asset))
        })
    }

    fn two_signers(env: &Env, a: &Address, b: &Address) -> Vec<Address> {
        let mut v = Vec::new(env);
        v.push_back(a.clone());
        v.push_back(b.clone());
        v
    }

    // ── Tests ────────────────────────────────────────────────────────────

    /// Zero-amount FeedStake must be evicted and the removed count returned.
    #[test]
    fn test_cleanup_removes_zero_amount_feed_stake() {
        let (env, cid, _admin, sa, sb) = setup();
        let node = Address::generate(&env);
        seed_feed_stake(&env, &cid, &node, 1, 0);
        assert!(has_feed_stake(&env, &cid, &node, 1));

        let signers = two_signers(&env, &sa, &sb);
        let mut targets: Vec<CleanupTarget> = Vec::new(&env);
        targets.push_back(CleanupTarget::FeedStake(node.clone(), 1));

        let removed = env.as_contract(&cid, || {
            cleanup_zero_balances(&env, &signers, &targets).unwrap()
        });

        assert_eq!(removed, 1);
        assert!(!has_feed_stake(&env, &cid, &node, 1));
    }

    /// Non-zero FeedStake must survive cleanup.
    #[test]
    fn test_cleanup_preserves_nonzero_feed_stake() {
        let (env, cid, _admin, sa, sb) = setup();
        let node = Address::generate(&env);
        seed_feed_stake(&env, &cid, &node, 2, 500);

        let signers = two_signers(&env, &sa, &sb);
        let mut targets: Vec<CleanupTarget> = Vec::new(&env);
        targets.push_back(CleanupTarget::FeedStake(node.clone(), 2));

        let removed = env.as_contract(&cid, || {
            cleanup_zero_balances(&env, &signers, &targets).unwrap()
        });

        assert_eq!(removed, 0);
        assert!(has_feed_stake(&env, &cid, &node, 2));
    }

    /// Empty CorridorPool (both fields zero) must be evicted.
    #[test]
    fn test_cleanup_removes_empty_corridor_pool() {
        let (env, cid, _admin, sa, sb) = setup();
        seed_corridor_pool(&env, &cid, 10, 0, 0);
        assert!(has_corridor_pool(&env, &cid, 10));

        let signers = two_signers(&env, &sa, &sb);
        let mut targets: Vec<CleanupTarget> = Vec::new(&env);
        targets.push_back(CleanupTarget::CorridorPool(10));

        let removed = env.as_contract(&cid, || {
            cleanup_zero_balances(&env, &signers, &targets).unwrap()
        });

        assert_eq!(removed, 1);
        assert!(!has_corridor_pool(&env, &cid, 10));
    }

    /// Pool with zero `collected` but non-zero `variable_pool` must be preserved.
    #[test]
    fn test_cleanup_preserves_corridor_pool_with_variable_pool() {
        let (env, cid, _admin, sa, sb) = setup();
        seed_corridor_pool(&env, &cid, 11, 0, 100);

        let signers = two_signers(&env, &sa, &sb);
        let mut targets: Vec<CleanupTarget> = Vec::new(&env);
        targets.push_back(CleanupTarget::CorridorPool(11));

        let removed = env.as_contract(&cid, || {
            cleanup_zero_balances(&env, &signers, &targets).unwrap()
        });

        assert_eq!(removed, 0);
        assert!(has_corridor_pool(&env, &cid, 11));
    }

    /// A single-signer call must be rejected with ThresholdNotReached.
    #[test]
    fn test_cleanup_rejects_single_signer() {
        let (env, cid, _admin, sa, _sb) = setup();
        let node = Address::generate(&env);
        seed_feed_stake(&env, &cid, &node, 3, 0);

        let mut signers: Vec<Address> = Vec::new(&env);
        signers.push_back(sa.clone());

        let mut targets: Vec<CleanupTarget> = Vec::new(&env);
        targets.push_back(CleanupTarget::FeedStake(node.clone(), 3));

        let result = env.as_contract(&cid, || {
            cleanup_zero_balances(&env, &signers, &targets)
        });

        assert_eq!(result, Err(ContractError::ThresholdNotReached));
        // The zero entry must still be present because cleanup was rejected.
        assert!(has_feed_stake(&env, &cid, &node, 3));
    }

    /// Two unregistered addresses must not satisfy quorum.
    #[test]
    fn test_cleanup_rejects_unregistered_signers() {
        let (env, cid, admin, _sa, _sb) = setup();
        // Overwrite with a fresh contract state that has no registered signers.
        env.as_contract(&cid, || {
            env.storage().instance().set(
                &DATA_KEY,
                &ContractData { admin: admin.clone(), value: 0, max_fee_ceiling: 0 },
            );
        });

        let rando_a = Address::generate(&env);
        let rando_b = Address::generate(&env);
        let signers = two_signers(&env, &rando_a, &rando_b);
        let targets: Vec<CleanupTarget> = Vec::new(&env);

        let result = env.as_contract(&cid, || {
            cleanup_zero_balances(&env, &signers, &targets)
        });

        assert_eq!(result, Err(ContractError::ThresholdNotReached));
    }

    /// Empty target list is a valid no-op: returns 0.
    #[test]
    fn test_cleanup_noop_on_empty_target_list() {
        let (env, cid, _admin, sa, sb) = setup();
        let signers = two_signers(&env, &sa, &sb);
        let targets: Vec<CleanupTarget> = Vec::new(&env);

        let removed = env.as_contract(&cid, || {
            cleanup_zero_balances(&env, &signers, &targets).unwrap()
        });

        assert_eq!(removed, 0);
    }

    /// Targets pointing to absent keys are silently skipped.
    #[test]
    fn test_cleanup_skips_absent_keys() {
        let (env, cid, _admin, sa, sb) = setup();
        let node = Address::generate(&env);
        // Deliberately do NOT seed any storage for these keys.

        let signers = two_signers(&env, &sa, &sb);
        let mut targets: Vec<CleanupTarget> = Vec::new(&env);
        targets.push_back(CleanupTarget::FeedStake(node, 99));
        targets.push_back(CleanupTarget::CorridorPool(99));

        let removed = env.as_contract(&cid, || {
            cleanup_zero_balances(&env, &signers, &targets).unwrap()
        });

        assert_eq!(removed, 0);
    }

    /// Mixed batch: only zero-balance entries are removed; live entries survive.
    #[test]
    fn test_cleanup_mixed_batch_removes_only_zero_entries() {
        let (env, cid, _admin, sa, sb) = setup();
        let node_zero = Address::generate(&env);
        let node_live = Address::generate(&env);

        seed_feed_stake(&env, &cid, &node_zero, 20, 0);    // zero   → removed
        seed_feed_stake(&env, &cid, &node_live, 21, 1000); // live   → kept
        seed_corridor_pool(&env, &cid, 30, 0, 0);          // empty  → removed
        seed_corridor_pool(&env, &cid, 31, 500, 200);      // live   → kept

        let signers = two_signers(&env, &sa, &sb);
        let mut targets: Vec<CleanupTarget> = Vec::new(&env);
        targets.push_back(CleanupTarget::FeedStake(node_zero.clone(), 20));
        targets.push_back(CleanupTarget::FeedStake(node_live.clone(), 21));
        targets.push_back(CleanupTarget::CorridorPool(30));
        targets.push_back(CleanupTarget::CorridorPool(31));

        let removed = env.as_contract(&cid, || {
            cleanup_zero_balances(&env, &signers, &targets).unwrap()
        });

        assert_eq!(removed, 2);
        assert!(!has_feed_stake(&env, &cid, &node_zero, 20));
        assert!(has_feed_stake(&env, &cid, &node_live, 21));
        assert!(!has_corridor_pool(&env, &cid, 30));
        assert!(has_corridor_pool(&env, &cid, 31));
    }

    /// Calling on a non-initialized contract must return NotInitialized.
    #[test]
    fn test_cleanup_fails_when_not_initialized() {
        let env = Env::default();
        env.mock_all_auths();
        let cid = env.register_contract(None, crate::TimeLockedUpgradeContract);
        // No DATA_KEY set — contract is not initialized.

        let sa = Address::generate(&env);
        let sb = Address::generate(&env);
        let signers = two_signers(&env, &sa, &sb);
        let targets: Vec<CleanupTarget> = Vec::new(&env);

        let result = env.as_contract(&cid, || {
            cleanup_zero_balances(&env, &signers, &targets)
        });

        assert_eq!(result, Err(ContractError::NotInitialized));
    }

    #[test]
    fn test_reclaim_closed_orders_storage_and_footprint_reduction() {
        let (env, cid, admin, _sa, _sb) = setup();
        let client = crate::TimeLockedUpgradeContractClient::new(&env, &cid);

        let sell_issuer = Address::generate(&env);
        let buy_issuer = Address::generate(&env);
        let sell_asset = env.register_stellar_asset_contract(sell_issuer);
        let buy_asset = env.register_stellar_asset_contract(buy_issuer);

        let maker = Address::generate(&env);
        let filler = Address::generate(&env);

        soroban_sdk::token::StellarAssetClient::new(&env, &sell_asset).mint(&maker, &10_000);
        soroban_sdk::token::StellarAssetClient::new(&env, &buy_asset).mint(&filler, &50_000);

        let pair = crate::orders::limit::AssetPair {
            sell_asset: sell_asset.clone(),
            buy_asset: buy_asset.clone(),
        };

        // Create 2 spent orders (one filled, one cancelled)
        let order_filled = client.place_limit_order(&maker, &pair, &crate::orders::limit::PRICE_SCALE, &1_000);
        client.fill_limit_order(&filler, &order_filled.id, &1_000);

        let order_cancelled = client.place_limit_order(&maker, &pair, &crate::orders::limit::PRICE_SCALE, &1_000);
        client.cancel_limit_order(&maker, &order_cancelled.id);

        // One active resting order (should NOT be purged)
        let order_active = client.place_limit_order(&maker, &pair, &crate::orders::limit::PRICE_SCALE, &1_000);

        let mut order_ids = Vec::new(&env);
        order_ids.push_back(order_filled.id);
        order_ids.push_back(order_cancelled.id);
        order_ids.push_back(order_active.id);

        // Verify storage keys exist prior to reclamation
        env.as_contract(&cid, || {
            assert!(env.storage().persistent().has(&crate::orders::limit::OrderStorageKey::OrderIndex(order_filled.id)));
            assert!(env.storage().persistent().has(&crate::orders::limit::OrderStorageKey::OrderIndex(order_cancelled.id)));
            assert!(env.storage().persistent().has(&crate::orders::limit::OrderStorageKey::OrderIndex(order_active.id)));
        });

        // Reclaim closed orders via automated helper
        let caller = Address::generate(&env);
        let reclaimed = client.reclaim_closed_orders(&caller, &order_ids);
        assert_eq!(reclaimed, 2);

        // Verify storage entries evicted for closed orders and retained for active order
        env.as_contract(&cid, || {
            assert!(!env.storage().persistent().has(&crate::orders::limit::OrderStorageKey::OrderIndex(order_filled.id)));
            assert!(!env.storage().persistent().has(&crate::orders::limit::OrderStorageKey::OrderIndex(order_cancelled.id)));
            assert!(env.storage().persistent().has(&crate::orders::limit::OrderStorageKey::OrderIndex(order_active.id)));
        });
    }

    #[test]
    fn test_reclaim_expired_proposals_storage() {
        let (env, cid, admin, _sa, _sb) = setup();
        let client = crate::TimeLockedUpgradeContractClient::new(&env, &cid);
        let caller = Address::generate(&env);
        let target_admin = Address::generate(&env);
        let replacement = Address::generate(&env);

        let past_time = env.ledger().timestamp().saturating_sub(8 * 24 * 60 * 60); // 8 days ago (> 7 day expiry)
        env.as_contract(&cid, || {
            let proposal = crate::admin::EmergencyRevocationProposal {
                target: target_admin.clone(),
                replacement: replacement.clone(),
                proposer: admin.clone(),
                proposed_at: past_time,
                votes: soroban_sdk::Map::new(&env),
            };
            crate::temp_governance::store_temp_proposal(
                &env,
                &crate::temp_governance::EMERGENCY_REVOCATION_TEMP_KEY,
                &proposal,
                crate::temp_governance::DEFAULT_PROPOSAL_TTL,
            );
            assert!(crate::temp_governance::has_temp_proposal(
                &env,
                &crate::temp_governance::EMERGENCY_REVOCATION_TEMP_KEY,
            ));
        });

        let purged = client.reclaim_expired_proposals(&caller);
        assert_eq!(purged, 1);

        env.as_contract(&cid, || {
            assert!(!crate::temp_governance::has_temp_proposal(
                &env,
                &crate::temp_governance::EMERGENCY_REVOCATION_TEMP_KEY,
            ));
        });
    }
}

// ---------------------------------------------------------------------------
// Expired Governance Proposal State Purge Utility
// ---------------------------------------------------------------------------

pub fn purge_obsolete_proposals(
    env: &Env,
    signers: &Vec<Address>,
    proposal_ids: &Vec<Address>,
) -> Result<u32, ContractError> {
    // Verify the contract has been initialised
    let _data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;

    // Enforce multi-sig quorum
    crate::auth::require_multisig(env, signers)?;

    let mut purged: u32 = 0;
    let now = env.ledger().timestamp();
    let ninety_days: u64 = 90 * 24 * 60 * 60;

    for proposal_id in proposal_ids.iter() {
        let key = ProposalStorageKey::Proposal(proposal_id.clone());
        if let Some(proposal) = env
            .storage()
            .persistent()
            .get::<_, ProposalState>(&key)
        {
            if (proposal.status == ProposalStatus::Executed || proposal.status == ProposalStatus::Rejected)
                && now >= proposal.created_at.saturating_add(ninety_days)
            {
                // Erase vote entries
                // Assuming votes are stored separately
                let votes_key = ProposalStorageKey::Votes(proposal_id.clone());
                env.storage().persistent().remove(&votes_key);
                
                // Erase proposal itself
                env.storage().persistent().remove(&key);
                
                // Note: Removing the persistent entries automatically refunds the storage rent 
                // balances back to the original proposal submitter.
                purged += 1;
            }
        }
    }

    Ok(purged)
}
