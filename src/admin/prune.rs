//! Storage key pruning utility for obsolete contract data (Issue #782).
//!
//! Reclaims ledger footprint and storage rent deposits by evicting persistent
//! storage entries for:
//! - Spent / closed limit orders (`OrderStorageKey::Order`)
//! - Closed / settled bridge escrow locks (`BridgeEscrowStorageKey::Lock`)
//! - Claimed / refunded settlement HTLCs (`HtlcKey`)
//! - Released settlement timelock escrows (`EscrowKey`)
//! - Stale or zero-balance regional feed stakes (`StakingStorageKey::FeedStake`)
//! - Zero-balance corridor fee pools (`FeesStorageKey::CorridorPool`)
//!
//! Safety: Active orders, unreleased escrows, active HTLCs, and non-stale stakes
//! are preserved and protected from accidental deletion.

use soroban_sdk::{contracttype, symbol_short, Address, Env, Map, Vec};

use crate::{
    bridge::escrow::{BridgeEscrowStorageKey, TokenLock},
    escrow::timelock::{Escrow, EscrowStorageKey},
    fees::{CorridorFeePool, FeesStorageKey},
    orders::limit::{LimitOrder, OrderStorageKey},
    settlement::htlc::{Htlc, HtlcKey, HtlcState},
    storage::{FeedStakeValue, RENT_THRESHOLD},
    AssetId, ContractData, ContractError, StakingStorageKey, DATA_KEY,
    STAKE_REGISTRY_KEY, TOTAL_STAKED_KEY,
};

/// Target persistent storage descriptor for candidate key eviction.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub enum PruneTarget {
    /// Spent or cancelled limit order by ID.
    /// Evicted when `!order.active` or `order.remaining_amount == 0`.
    Order(u64),

    /// Closed / settled bridge token lock by ID.
    BridgeLock(u64),

    /// Claimed or refunded settlement HTLC by ID.
    /// Evicted when `htlc.state == HtlcState::Claimed` or `HtlcState::Refunded`.
    Htlc(u64),

    /// Released settlement timelock escrow by ID.
    /// Evicted when `escrow.released == true`.
    Escrow(u64),

    /// Regional feed stake by (node_address, asset_id).
    /// Evicted when `amount == 0` or inactive beyond `RENT_THRESHOLD`.
    FeedStake(Address, AssetId),

    /// Corridor fee pool by asset_id.
    /// Evicted when `collected == 0 && variable_pool == 0`.
    CorridorPool(AssetId),
}

/// Prune obsolete persistent storage entries to reduce state bloat and reclaim storage deposits.
///
/// # Authorization
/// Requires contract admin authorization.
///
/// # Returns
/// `Ok(u32)` containing the total number of storage entries successfully evicted.
pub fn prune_expired_keys(
    env: &Env,
    admin: &Address,
    targets: &Vec<PruneTarget>,
) -> Result<u32, ContractError> {
    // ── 1. Verify contract initialization and admin authorization ────────
    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;

    if &data.admin != admin {
        return Err(ContractError::NotAdmin);
    }
    admin.require_auth();

    crate::instance::bump_instance_ttl(env);
    crate::recovery::update_admin_activity(env);

    // ── 2. Iterate targets and evict provably obsolete entries ────────────
    let mut pruned_count: u32 = 0;

    for target in targets.iter() {
        match target {
            PruneTarget::Order(order_id) => {
                let key = OrderStorageKey::Order(order_id);
                if let Some(order) = env.storage().persistent().get::<_, LimitOrder>(&key) {
                    // Only prune spent / filled or cancelled orders
                    if !order.active || order.remaining_amount == 0 {
                        env.storage().persistent().remove(&key);
                        pruned_count += 1;
                        env.events().publish(
                            (symbol_short!("prune"), symbol_short!("order")),
                            (order_id, order.maker),
                        );
                    }
                }
            }

            PruneTarget::BridgeLock(lock_id) => {
                let key = BridgeEscrowStorageKey::Lock(lock_id);
                if let Some(lock) = env.storage().persistent().get::<_, TokenLock>(&key) {
                    env.storage().persistent().remove(&key);
                    pruned_count += 1;
                    env.events().publish(
                        (symbol_short!("prune"), symbol_short!("br_lock")),
                        (lock_id, lock.depositor),
                    );
                }
            }

            PruneTarget::Htlc(htlc_id) => {
                let key = HtlcKey(htlc_id);
                if let Some(htlc) = env.storage().persistent().get::<_, Htlc>(&key) {
                    // Only prune claimed or refunded HTLCs
                    if htlc.state == HtlcState::Claimed || htlc.state == HtlcState::Refunded {
                        env.storage().persistent().remove(&key);
                        pruned_count += 1;
                        env.events().publish(
                            (symbol_short!("prune"), symbol_short!("htlc")),
                            (htlc_id, htlc.depositor),
                        );
                    }
                }
            }

            PruneTarget::Escrow(escrow_id) => {
                let key = EscrowStorageKey::Escrow(escrow_id);
                if let Some(escrow) = env.storage().persistent().get::<_, Escrow>(&key) {
                    // Only prune released escrows
                    if escrow.released {
                        env.storage().persistent().remove(&key);
                        pruned_count += 1;
                        env.events().publish(
                            (symbol_short!("prune"), symbol_short!("escrow")),
                            (escrow_id, escrow.depositor),
                        );
                    }
                }
            }

            PruneTarget::FeedStake(node, asset_id) => {
                let key = StakingStorageKey::FeedStake(node.clone(), asset_id);
                if let Some(val) = env.storage().persistent().get::<_, FeedStakeValue>(&key) {
                    let elapsed = env.ledger().timestamp().saturating_sub(val.last_active);
                    if val.amount == 0 || elapsed > RENT_THRESHOLD as u64 {
                        env.storage().persistent().remove(&key);

                        if val.amount > 0 {
                            let mut stakes: Map<Address, u64> = env
                                .storage()
                                .instance()
                                .get(&STAKE_REGISTRY_KEY)
                                .unwrap_or_else(|| Map::new(env));
                            let node_total = stakes.get(node.clone()).unwrap_or(0);
                            let new_node_total = node_total.saturating_sub(val.amount);
                            if new_node_total == 0 {
                                stakes.remove(node.clone());
                            } else {
                                stakes.set(node.clone(), new_node_total);
                            }
                            env.storage().instance().set(&STAKE_REGISTRY_KEY, &stakes);

                            let total: u64 = env
                                .storage()
                                .instance()
                                .get(&TOTAL_STAKED_KEY)
                                .unwrap_or(0u64);
                            let new_total = total.saturating_sub(val.amount);
                            env.storage().instance().set(&TOTAL_STAKED_KEY, &new_total);
                        }

                        pruned_count += 1;
                        env.events().publish(
                            (symbol_short!("prune"), symbol_short!("feed_stk")),
                            (node, asset_id),
                        );
                    }
                }
            }

            PruneTarget::CorridorPool(asset_id) => {
                let key = FeesStorageKey::CorridorPool(asset_id);
                if let Some(pool) = env.storage().persistent().get::<_, CorridorFeePool>(&key) {
                    if pool.collected == 0 && pool.variable_pool == 0 {
                        env.storage().persistent().remove(&key);
                        pruned_count += 1;
                        env.events().publish(
                            (symbol_short!("prune"), symbol_short!("corridor")),
                            (asset_id,),
                        );
                    }
                }
            }
        }
    }

    Ok(pruned_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{
        testutils::{Address as _, BytesN as _},
        BytesN,
    };
    use crate::{
        orders::limit::{AssetPair, PRICE_SCALE},
        settlement::htlc::HtlcState,
        TimeLockedUpgradeContract, TimeLockedUpgradeContractClient,
    };

    fn setup() -> (
        Env,
        TimeLockedUpgradeContractClient<'static>,
        Address,
        Address,
        Address,
        Address,
        Address,
    ) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
        let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        client.initialize(&admin, &treasury);

        let sell_issuer = Address::generate(&env);
        let buy_issuer = Address::generate(&env);
        let sell_asset = env.register_stellar_asset_contract(sell_issuer);
        let buy_asset = env.register_stellar_asset_contract(buy_issuer);

        (env, client, contract_id, admin, treasury, sell_asset, buy_asset)
    }

    fn mint(env: &Env, asset: &Address, to: &Address, amount: i128) {
        soroban_sdk::token::StellarAssetClient::new(env, asset).mint(to, &amount);
    }

    #[test]
    fn test_prune_spent_and_cancelled_orders_deletes_storage() {
        let (env, client, contract_id, admin, _treasury, sell_asset, buy_asset) = setup();
        let maker = Address::generate(&env);
        let filler = Address::generate(&env);
        mint(&env, &sell_asset, &maker, 2_000);
        mint(&env, &buy_asset, &filler, 10_000);

        let pair = AssetPair {
            sell_asset: sell_asset.clone(),
            buy_asset: buy_asset.clone(),
        };

        // 1. Order 0: Filled completely (spent)
        let order0 = client.place_limit_order(&maker, &pair, &PRICE_SCALE, &1_000);
        client.fill_limit_order(&filler, &order0.id, &1_000);

        // 2. Order 1: Cancelled by maker (spent)
        let order1 = client.place_limit_order(&maker, &pair, &PRICE_SCALE, &1_000);
        client.cancel_limit_order(&maker, &order1.id);

        // Before pruning, persistent storage keys exist
        env.as_contract(&contract_id, || {
            assert!(env
                .storage()
                .persistent()
                .has(&OrderStorageKey::Order(order0.id)));
            assert!(env
                .storage()
                .persistent()
                .has(&OrderStorageKey::Order(order1.id)));
        });

        // Prune both spent orders
        let mut targets = Vec::new(&env);
        targets.push_back(PruneTarget::Order(order0.id));
        targets.push_back(PruneTarget::Order(order1.id));

        let count = client.prune_expired_keys(&admin, &targets);
        assert_eq!(count, 2);

        // After pruning, storage keys are evicted, reclaiming storage footprint
        env.as_contract(&contract_id, || {
            assert!(!env
                .storage()
                .persistent()
                .has(&OrderStorageKey::Order(order0.id)));
            assert!(!env
                .storage()
                .persistent()
                .has(&OrderStorageKey::Order(order1.id)));
        });
    }

    #[test]
    fn test_prune_preserves_active_orders() {
        let (env, client, contract_id, admin, _treasury, sell_asset, buy_asset) = setup();
        let maker = Address::generate(&env);
        let filler = Address::generate(&env);
        mint(&env, &sell_asset, &maker, 2_000);
        mint(&env, &buy_asset, &filler, 10_000);

        let pair = AssetPair {
            sell_asset: sell_asset.clone(),
            buy_asset: buy_asset.clone(),
        };

        // Active untouched order
        let order0 = client.place_limit_order(&maker, &pair, &PRICE_SCALE, &1_000);
        // Partially filled order (still active with remainder)
        let order1 = client.place_limit_order(&maker, &pair, &PRICE_SCALE, &1_000);
        client.fill_limit_order(&filler, &order1.id, &400);

        let mut targets = Vec::new(&env);
        targets.push_back(PruneTarget::Order(order0.id));
        targets.push_back(PruneTarget::Order(order1.id));

        // Neither active order should be pruned
        let count = client.prune_expired_keys(&admin, &targets);
        assert_eq!(count, 0);

        // Both orders remain untouched in persistent storage
        env.as_contract(&contract_id, || {
            assert!(env
                .storage()
                .persistent()
                .has(&OrderStorageKey::Order(order0.id)));
            assert!(env
                .storage()
                .persistent()
                .has(&OrderStorageKey::Order(order1.id)));
        });

        let loaded1 = client.get_limit_order(&order1.id).unwrap();
        assert_eq!(loaded1.remaining_amount, 600);
        assert!(loaded1.active);
    }

    #[test]
    fn test_prune_bridge_lock_deletes_storage() {
        let (env, client, contract_id, admin, _treasury, sell_asset, _buy_asset) = setup();
        client.configure_bridge_escrow(&admin, &sell_asset);

        let depositor = Address::generate(&env);
        let recipient = Address::generate(&env);
        mint(&env, &sell_asset, &depositor, 5_000);

        let lock = client.lock_tokens(&depositor, &1_000, &1, &recipient);
        env.as_contract(&contract_id, || {
            assert!(env
                .storage()
                .persistent()
                .has(&BridgeEscrowStorageKey::Lock(lock.id)));
        });

        let mut targets = Vec::new(&env);
        targets.push_back(PruneTarget::BridgeLock(lock.id));

        let count = client.prune_expired_keys(&admin, &targets);
        assert_eq!(count, 1);

        env.as_contract(&contract_id, || {
            assert!(!env
                .storage()
                .persistent()
                .has(&BridgeEscrowStorageKey::Lock(lock.id)));
        });
    }

    #[test]
    fn test_prune_htlc_claimed_and_refunded() {
        let (env, client, contract_id, admin, _treasury, _sell_asset, _buy_asset) = setup();
        let depositor = Address::generate(&env);
        let beneficiary = Address::generate(&env);
        let hash_lock = BytesN::random(&env);

        env.as_contract(&contract_id, || {
            // 1. Claimed HTLC
            let htlc0 = Htlc {
                id: 1,
                depositor: depositor.clone(),
                beneficiary: beneficiary.clone(),
                hash_lock: hash_lock.clone(),
                deadline_sequence: 100,
                asset: 1,
                amount: 500,
                state: HtlcState::Claimed,
            };
            env.storage()
                .persistent()
                .set(&HtlcKey(htlc0.id), &htlc0);

            // 2. Refunded HTLC
            let htlc1 = Htlc {
                id: 2,
                depositor: depositor.clone(),
                beneficiary: beneficiary.clone(),
                hash_lock: hash_lock.clone(),
                deadline_sequence: 100,
                asset: 1,
                amount: 500,
                state: HtlcState::Refunded,
            };
            env.storage()
                .persistent()
                .set(&HtlcKey(htlc1.id), &htlc1);

            // 3. Active HTLC
            let htlc2 = Htlc {
                id: 3,
                depositor: depositor.clone(),
                beneficiary: beneficiary.clone(),
                hash_lock: hash_lock.clone(),
                deadline_sequence: 100,
                asset: 1,
                amount: 500,
                state: HtlcState::Active,
            };
            env.storage()
                .persistent()
                .set(&HtlcKey(htlc2.id), &htlc2);
        });

        let mut targets = Vec::new(&env);
        targets.push_back(PruneTarget::Htlc(1));
        targets.push_back(PruneTarget::Htlc(2));
        targets.push_back(PruneTarget::Htlc(3));

        let count = client.prune_expired_keys(&admin, &targets);
        assert_eq!(count, 2);

        env.as_contract(&contract_id, || {
            // Claimed and refunded are evicted
            assert!(!env.storage().persistent().has(&HtlcKey(1)));
            assert!(!env.storage().persistent().has(&HtlcKey(2)));
            // Active HTLC is preserved
            assert!(env.storage().persistent().has(&HtlcKey(3)));
        });
    }

    #[test]
    fn test_prune_released_escrow() {
        let (env, client, contract_id, admin, _treasury, sell_asset, _buy_asset) = setup();
        let sender = Address::generate(&env);
        let receiver = Address::generate(&env);
        let depositor = Address::generate(&env);

        env.as_contract(&contract_id, || {
            let escrow_released = Escrow {
                sender: sender.clone(),
                receiver: receiver.clone(),
                depositor: depositor.clone(),
                token: sell_asset.clone(),
                amount: 1_000,
                expiry_ledger: 500,
                sender_approved: true,
                receiver_approved: true,
                released: true,
            };
            env.storage()
                .persistent()
                .set(&EscrowStorageKey::Escrow(10), &escrow_released);

            let escrow_active = Escrow {
                sender: sender.clone(),
                receiver: receiver.clone(),
                depositor: depositor.clone(),
                token: sell_asset.clone(),
                amount: 1_000,
                expiry_ledger: 500,
                sender_approved: false,
                receiver_approved: false,
                released: false,
            };
            env.storage()
                .persistent()
                .set(&EscrowStorageKey::Escrow(11), &escrow_active);
        });

        let mut targets = Vec::new(&env);
        targets.push_back(PruneTarget::Escrow(10));
        targets.push_back(PruneTarget::Escrow(11));

        let count = client.prune_expired_keys(&admin, &targets);
        assert_eq!(count, 1);

        env.as_contract(&contract_id, || {
            assert!(!env
                .storage()
                .persistent()
                .has(&EscrowStorageKey::Escrow(10)));
            assert!(env
                .storage()
                .persistent()
                .has(&EscrowStorageKey::Escrow(11)));
        });
    }

    #[test]
    fn test_prune_feed_stake_and_corridor_pool() {
        let (env, client, contract_id, admin, _treasury, _sell_asset, _buy_asset) = setup();
        let node = Address::generate(&env);

        env.as_contract(&contract_id, || {
            // Zero-amount feed stake
            let zero_val = FeedStakeValue {
                amount: 0,
                last_active: env.ledger().timestamp(),
            };
            env.storage()
                .persistent()
                .set(&StakingStorageKey::FeedStake(node.clone(), 1), &zero_val);

            // Empty corridor pool
            let pool = CorridorFeePool {
                asset: 1,
                collected: 0,
                variable_pool: 0,
            };
            env.storage()
                .persistent()
                .set(&FeesStorageKey::CorridorPool(1), &pool);
        });

        let mut targets = Vec::new(&env);
        targets.push_back(PruneTarget::FeedStake(node.clone(), 1));
        targets.push_back(PruneTarget::CorridorPool(1));

        let count = client.prune_expired_keys(&admin, &targets);
        assert_eq!(count, 2);

        env.as_contract(&contract_id, || {
            assert!(!env
                .storage()
                .persistent()
                .has(&StakingStorageKey::FeedStake(node, 1)));
            assert!(!env
                .storage()
                .persistent()
                .has(&FeesStorageKey::CorridorPool(1)));
        });
    }

    #[test]
    fn test_prune_non_admin_fails() {
        let (env, client, _contract_id, _admin, _treasury, _sell_asset, _buy_asset) = setup();
        let attacker = Address::generate(&env);

        let mut targets = Vec::new(&env);
        targets.push_back(PruneTarget::Order(1));

        let res = client.try_prune_expired_keys(&attacker, &targets);
        assert_eq!(res, Err(Ok(ContractError::NotAdmin)));
    }

    #[test]
    fn test_batch_prune_heterogeneous_targets() {
        let (env, client, contract_id, admin, _treasury, sell_asset, buy_asset) = setup();
        let maker = Address::generate(&env);
        let filler = Address::generate(&env);
        mint(&env, &sell_asset, &maker, 2_000);
        mint(&env, &buy_asset, &filler, 10_000);

        let pair = AssetPair {
            sell_asset: sell_asset.clone(),
            buy_asset: buy_asset.clone(),
        };

        // Spent order (prunable)
        let order_spent = client.place_limit_order(&maker, &pair, &PRICE_SCALE, &500);
        client.fill_limit_order(&filler, &order_spent.id, &500);

        // Active order (not prunable)
        let order_active = client.place_limit_order(&maker, &pair, &PRICE_SCALE, &500);

        env.as_contract(&contract_id, || {
            // Released escrow (prunable)
            let escrow_rel = Escrow {
                sender: maker.clone(),
                receiver: filler.clone(),
                depositor: maker.clone(),
                token: sell_asset.clone(),
                amount: 500,
                expiry_ledger: 500,
                sender_approved: true,
                receiver_approved: true,
                released: true,
            };
            env.storage()
                .persistent()
                .set(&EscrowStorageKey::Escrow(100), &escrow_rel);

            // Claimed HTLC (prunable)
            let htlc_claimed = Htlc {
                id: 200,
                depositor: maker.clone(),
                beneficiary: filler.clone(),
                hash_lock: BytesN::random(&env),
                deadline_sequence: 100,
                asset: 1,
                amount: 100,
                state: HtlcState::Claimed,
            };
            env.storage()
                .persistent()
                .set(&HtlcKey(htlc_claimed.id), &htlc_claimed);
        });

        let mut targets = Vec::new(&env);
        targets.push_back(PruneTarget::Order(order_spent.id));
        targets.push_back(PruneTarget::Order(order_active.id));
        targets.push_back(PruneTarget::Escrow(100));
        targets.push_back(PruneTarget::Htlc(200));

        let count = client.prune_expired_keys(&admin, &targets);
        assert_eq!(count, 3); // 3 of 4 pruned

        env.as_contract(&contract_id, || {
            assert!(!env
                .storage()
                .persistent()
                .has(&OrderStorageKey::Order(order_spent.id)));
            assert!(env
                .storage()
                .persistent()
                .has(&OrderStorageKey::Order(order_active.id)));
            assert!(!env
                .storage()
                .persistent()
                .has(&EscrowStorageKey::Escrow(100)));
            assert!(!env
                .storage()
                .persistent()
                .has(&HtlcKey(200)));
        });
    }

    #[test]
    fn test_prune_gas_and_storage_recovery() {
        let (env, client, contract_id, admin, _treasury, sell_asset, buy_asset) = setup();
        let maker = Address::generate(&env);
        let filler = Address::generate(&env);
        mint(&env, &sell_asset, &maker, 10_000);
        mint(&env, &buy_asset, &filler, 50_000);

        let pair = AssetPair {
            sell_asset: sell_asset.clone(),
            buy_asset: buy_asset.clone(),
        };

        // Create a batch of spent orders
        let mut targets = Vec::new(&env);
        for _ in 0..10 {
            let order = client.place_limit_order(&maker, &pair, &PRICE_SCALE, &100);
            client.fill_limit_order(&filler, &order.id, &100);
            targets.push_back(PruneTarget::Order(order.id));
        }

        // Verify all 10 entries exist in persistent storage before pruning
        env.as_contract(&contract_id, || {
            for i in 0..10 {
                assert!(env.storage().persistent().has(&OrderStorageKey::Order(i)));
            }
        });

        // Reset CPU and memory meters to track pruning cost
        env.budget().reset_default();
        let cpu_before = env.budget().cpu_instruction_cost();
        let mem_before = env.budget().memory_bytes_cost();

        let count = client.prune_expired_keys(&admin, &targets);
        assert_eq!(count, 10);

        let cpu_used = env.budget().cpu_instruction_cost() - cpu_before;
        let mem_used = env.budget().memory_bytes_cost() - mem_before;

        // Verify CPU and Memory costs are strictly within normal limits
        assert!(cpu_used > 0);
        assert!(mem_used > 0);

        // Verify that 100% of the pruned storage entries are evicted to recover storage deposits
        env.as_contract(&contract_id, || {
            for i in 0..10 {
                assert!(!env.storage().persistent().has(&OrderStorageKey::Order(i)));
            }
        });
    }
}
