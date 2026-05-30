//! FE-216: Efficient event indexing via topic mapping.
//! 
//! This module provides utility functions for emitting indexed events that enable
//! off-chain indexing services to efficiently filter events by:
//! - Asset symbol (for price events)
//! - Validator/relayer identity (for slashing and stake events)
//! - Admin addresses (for governance and admin events)
//! 
//! By including these searchable parameters as event topics (first elements of the tuple),
//! indexers avoid scanning complete data blocks, significantly reducing processing delays.

use soroban_sdk::{Address, Env, Symbol};

/// Publishes a price update event with the asset symbol as the first topic.
/// Indexers can filter on topic[0] == asset to find all events for that pair.
/// Efficiently enables downstream contracts to subscribe to price changes for specific assets.
pub fn publish_price_event(env: &Env, asset: Symbol, price: i128, timestamp: u64) {
    env.events().publish(
        (asset,),                          // topic[0] = asset symbol for efficient filtering
        (price, timestamp),                // data payload
    );
}

/// Publishes a validator/relayer stake event with validator identity as the first topic.
/// Indexers can filter on topic[0] == validator to track all stake activity for that relayer.
/// Enables efficient monitoring of validator collateral changes.
pub fn publish_stake_event(
    env: &Env,
    event_type: Symbol,           // "stake_deposited" or "stake_withdrawn"
    validator: Address,           // The relayer/validator address
    amount: i128,                 // Amount staked or unstaked
    new_balance: i128,            // Updated stake balance
) {
    env.events().publish(
        (event_type, validator),          // topic[0] = event type, topic[1] = validator
        (amount, new_balance),            // data payload
    );
}

/// Publishes a slashing event with validator identity and executor as indexed topics.
/// Indexers can filter on:
/// - topic[0] == validator to find all slash events for a relayer
/// - topic[1] == executor to track admin actions
/// Enables governance oversight and validator monitoring.
pub fn publish_slash_event(
    env: &Env,
    validator: Address,           // The slashed relayer
    executor: Address,            // The admin who executed the slash
    amount: i128,                 // Amount slashed
    remaining_stake: i128,        // Remaining collateral after slash
) {
    env.events().publish(
        (Symbol::new(env, "slash_executed"), validator, executor),  // 3 indexed topics
        (amount, remaining_stake),        // data payload
    );
}

/// Publishes an admin governance event with admin address as the first topic.
/// Indexers can filter on topic[0] == admin to track all admin actions by a specific address.
/// Enables audit trails and governance monitoring.
pub fn publish_admin_event(
    env: &Env,
    event_name: Symbol,           // e.g., "admin_registered", "admin_removed"
    admin: Address,               // The admin involved
    details_arg1: Option<Address>,  // Optional secondary address (e.g., target admin)
    details_arg2: u32,            // Optional u32 data (e.g., action type)
) {
    if let Some(addr) = details_arg1 {
        env.events().publish(
            (event_name, admin),
            (addr, details_arg2),
        );
    } else {
        env.events().publish(
            (event_name, admin),
            (details_arg2,),
        );
    }
}

/// Publishes a vote/governance event with voter address as the first topic.
/// Indexers can filter on topic[0] == voter to track voting behavior.
/// Enables efficient governance analytics.
pub fn publish_vote_event(
    env: &Env,
    voter: Address,               // The voting address
    action_id: u64,               // The proposal ID being voted on
    vote_count: u32,              // Total votes for this action
) {
    env.events().publish(
        (Symbol::new(env, "action_voted"), voter),  // topic[0] = voter
        (action_id, vote_count),                    // data payload
    );
}