//! Standardized event topic construction for all StellarFlow contracts.
//!
//! Provides a unified event publishing API that enforces:
//! - A maximum of 4 indexed Symbol topics per event (for RPC filterability).
//! - Consistent snake_case naming across all contract modules.
//! - Deterministic topic ordering: event name first, then entity keys.
//!
//! # Why 4 topics?
//!
//! Soroban RPC `getEvents` filters on topic vectors. Keeping topics to ≤ 4
//! symbols ensures efficient B-tree lookups on the indexer and avoids
//! exceeding the ledger event topic budget. Most cross-border settlement
//! events need at most: event_name + entity_type + entity_id + status.
//!
//! # Usage
//!
//! ```ignore
//! use crate::events::{emit_event, EventName};
//!
//! // Emit a 2-topic event (name + asset)
//! emit_event(env, EventName::PriceUpdate, &[&asset_sym], &(price, timestamp));
//! ```

use soroban_sdk::{symbol_short, Env, Symbol, Vec};

use crate::ContractError;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of indexed Symbol topics allowed per event.
/// RPC `getEvents` queries filter on topic vectors; keeping this bounded
/// at 4 ensures efficient filtering and avoids ledger budget overflows.
pub const MAX_EVENT_TOPICS: u32 = 4;

// ---------------------------------------------------------------------------
// Standardized event names
// ---------------------------------------------------------------------------

// Each constant is a `Symbol` ≤ 9 bytes (the `symbol_short!` limit).
// Longer names use `Symbol::new` at call sites.

/// Price oracle: a price feed was updated for an asset.
pub const EV_PRICE_UPDATE: Symbol = symbol_short!("price_up");

/// Price oracle: a price floor was set for an asset.
pub const EV_PRICE_FLOOR_SET: Symbol = symbol_short!("floor_set");

/// Price oracle: a price floor was rolled back.
pub const EV_PRICE_FLOOR_ROLL: Symbol = symbol_short!("floor_roll");

/// Price oracle: price bounds were configured.
pub const EV_PRICE_BOUNDS_SET: Symbol = symbol_short!("bounds_set");

/// Price oracle: price bounds were rolled back.
pub const EV_PRICE_BOUNDS_ROLL: Symbol = symbol_short!("bounds_roll");

/// Price oracle: max deviation percentage was updated.
pub const EV_MAX_DEV_SET: Symbol = symbol_short!("dev_set");

/// Price oracle: max deviation percentage was rolled back.
pub const EV_MAX_DEV_ROLL: Symbol = symbol_short!("dev_roll");

/// Price oracle: asset metadata was configured.
pub const EV_ASSET_META_SET: Symbol = symbol_short!("meta_set");

/// Price oracle: asset info was configured.
pub const EV_ASSET_INFO_SET: Symbol = symbol_short!("info_set");

/// Price oracle: asset description was stored.
pub const EV_ASSET_DESC_SET: Symbol = symbol_short!("desc_set");

/// Price oracle: emergency halt was toggled.
pub const EV_EMERGENCY_HALT: Symbol = symbol_short!("emrg_halt");

/// Price oracle: reward was claimed by a validator.
pub const EV_REWARD_CLAIMED: Symbol = symbol_short!("rwd_claim");

/// Telemetry: a telemetry submission was accepted.
pub const EV_TELEMETRY_OK: Symbol = symbol_short!("telem_ok");

/// Consensus: fallback oracle rate was used.
pub const EV_FALLBACK_WARN: Symbol = symbol_short!("FallbackW");

/// HTLC: a new time-locked contract was created.
pub const EV_HTLC_NEW: Symbol = symbol_short!("htlc_new");

/// HTLC: funds were claimed with valid pre-image.
pub const EV_HTLC_CLAIM: Symbol = symbol_short!("htlc_clm");

/// HTLC: funds were refunded after deadline.
pub const EV_HTLC_REFUND: Symbol = symbol_short!("htlc_ref");

/// Router: a multi-hop route executed successfully.
pub const EV_ROUTE_OK: Symbol = symbol_short!("route_ok");

/// Admin: a coordinator was added.
pub const EV_COORD_ADDED: Symbol = symbol_short!("coord_add");

/// Admin: a coordinator was removed.
pub const EV_COORD_REMOVED: Symbol = symbol_short!("coord_rem");

/// Admin: admin ownership was transferred.
pub const EV_ADMIN_TRANSFER: Symbol = symbol_short!("adm_xfer");

/// Admin: emergency revocation vote was cast.
pub const EV_REVOCATION_VOTE: Symbol = symbol_short!("revk_vote");

/// Admin: emergency revocation was executed.
pub const EV_REVOCATION_EXEC: Symbol = symbol_short!("revk_exec");

/// Staking: a validator was registered.
pub const EV_STAKE_REG: Symbol = symbol_short!("stake_reg");

/// Staking: a validator unstaked.
pub const EV_STAKE_UNREG: Symbol = symbol_short!("stake_unr");

/// Staking: a feed stake was registered.
pub const EV_FEED_STAKE_REG: Symbol = symbol_short!("feed_reg");

/// Staking: a feed stake was withdrawn.
pub const EV_FEED_STAKE_UNREG: Symbol = symbol_short!("feed_unr");

/// Slashing: an ingestion penalty was applied.
pub const EV_PENALTY_APPLIED: Symbol = symbol_short!("pnlty_ap");

/// Governance: a staged upgrade was proposed.
pub const EV_UPGRADE_PROPOSED: Symbol = symbol_short!("upg_prop");

/// Governance: an upgrade was executed.
pub const EV_UPGRADE_EXECUTED: Symbol = symbol_short!("upg_exec");

/// Governance: an upgrade was cancelled.
pub const EV_UPGRADE_CANCELLED: Symbol = symbol_short!("upg_canc");

/// Governance: a revocation ballot was opened.
pub const EV_BALLOT_OPENED: Symbol = symbol_short!("ball_open");

/// Governance: a revocation ballot was closed.
pub const EV_BALLOT_CLOSED: Symbol = symbol_short!("ball_clos");

// ---------------------------------------------------------------------------
// Core publishing function
// ---------------------------------------------------------------------------

/// Publish a standardized event with topic count validation.
///
/// # Arguments
/// * `env` - Soroban environment.
/// * `event_name` - The primary event topic (determines RPC filter key).
/// * `extra_topics` - Additional indexed topics (up to 3 more, for a total
///   of 4 including `event_name`).
/// * `data` - The non-indexed event payload.
///
/// # Errors
/// Returns [`ContractError::EventTopicLimitExceeded`] if the total topic
/// count (1 + extra_topics length) exceeds [`MAX_EVENT_TOPICS`].
pub fn emit_event<D: soroban_sdk::IntoVal<Env, soroban_sdk::Val>>(
    env: &Env,
    event_name: Symbol,
    extra_topics: &[&Symbol],
    data: D,
) -> Result<(), ContractError> {
    let total = 1 + extra_topics.len() as u32;
    if total > MAX_EVENT_TOPICS {
        return Err(ContractError::EventTopicLimitExceeded);
    }

    // Build the topic tuple in a fixed-size array, then slice it.
    //
    // Soroban `publish` accepts any `IntoVal` for the topics argument.
    // A 4-element array of `Symbol` values covers the maximum case.
    let t0 = event_name;
    let t1 = extra_topics.get(0).copied();
    let t2 = extra_topics.get(1).copied();
    let t3 = extra_topics.get(2).copied();

    // Construct a Vec<Symbol> for the topics.
    let mut topics: Vec<Symbol> = Vec::new(env);
    topics.push_back(t0);
    if let Some(s) = t1 {
        topics.push_back(s);
    }
    if let Some(s) = t2 {
        topics.push_back(s);
    }
    if let Some(s) = t3 {
        topics.push_back(s);
    }

    env.events().publish(topics, data);
    Ok(())
}

/// Validate that a topic vector does not exceed the limit.
/// Returns `Ok(())` if valid, or `Err(EventTopicLimitExceeded)` if too long.
pub fn validate_topics(topic_count: u32) -> Result<(), ContractError> {
    if topic_count > MAX_EVENT_TOPICS {
        Err(ContractError::EventTopicLimitExceeded)
    } else {
        Ok(())
    }
}

/// Return the number of topics a well-formed event would have given
/// the number of extra topics provided (does not publish).
pub fn topic_count(extra_topics: u32) -> u32 {
    let total = 1 + extra_topics;
    if total > MAX_EVENT_TOPICS {
        MAX_EVENT_TOPICS
    } else {
        total
    }
}

// ---------------------------------------------------------------------------
// Convenience publishers for common event shapes
// ---------------------------------------------------------------------------

/// Publish a simple 2-topic event: (event_name, entity_id).
pub fn emit_simple2<D: soroban_sdk::IntoVal<Env, soroban_sdk::Val>>(
    env: &Env,
    event_name: Symbol,
    entity_id: Symbol,
    data: D,
) -> Result<(), ContractError> {
    emit_event(env, event_name, &[&entity_id], data)
}

/// Publish a 3-topic event: (event_name, entity_type, entity_id).
pub fn emit_simple3<D: soroban_sdk::IntoVal<Env, soroban_sdk::Val>>(
    env: &Env,
    event_name: Symbol,
    entity_type: Symbol,
    entity_id: Symbol,
    data: D,
) -> Result<(), ContractError> {
    emit_event(env, event_name, &[&entity_type, &entity_id], data)
}

/// Publish a 4-topic event: (event_name, entity_type, entity_id, status).
pub fn emit_simple4<D: soroban_sdk::IntoVal<Env, soroban_sdk::Val>>(
    env: &Env,
    event_name: Symbol,
    entity_type: Symbol,
    entity_id: Symbol,
    status: Symbol,
    data: D,
) -> Result<(), ContractError> {
    emit_event(
        env,
        event_name,
        &[&entity_type, &entity_id, &status],
        data,
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    #[test]
    fn validate_topics_within_limit() {
        assert!(validate_topics(0).is_ok());
        assert!(validate_topics(1).is_ok());
        assert!(validate_topics(2).is_ok());
        assert!(validate_topics(3).is_ok());
        assert!(validate_topics(4).is_ok());
    }

    #[test]
    fn validate_topics_exceeds_limit() {
        assert_eq!(
            validate_topics(5),
            Err(ContractError::EventTopicLimitExceeded)
        );
        assert_eq!(
            validate_topics(10),
            Err(ContractError::EventTopicLimitExceeded)
        );
    }

    #[test]
    fn topic_count_returns_correct_totals() {
        assert_eq!(topic_count(0), 1); // just event_name
        assert_eq!(topic_count(1), 2);
        assert_eq!(topic_count(2), 3);
        assert_eq!(topic_count(3), 4);
        assert_eq!(topic_count(4), 4); // capped at MAX
        assert_eq!(topic_count(100), 4); // capped at MAX
    }

    #[test]
    fn emit_event_success_within_limit() {
        let env = Env::default();
        let result = emit_event(&env, EV_PRICE_UPDATE, &[], (100u32,));
        assert!(result.is_ok());
    }

    #[test]
    fn emit_event_success_at_limit() {
        let env = Env::default();
        let result = emit_event(
            &env,
            EV_ROUTE_OK,
            &[&EV_PRICE_UPDATE, &EV_HTLC_NEW, &EV_TELEMETRY_OK],
            (42u32,),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn emit_event_fails_over_limit() {
        let env = Env::default();
        let result = emit_event(
            &env,
            EV_ROUTE_OK,
            &[
                &EV_PRICE_UPDATE,
                &EV_HTLC_NEW,
                &EV_TELEMETRY_OK,
                &EV_COORD_ADDED,
            ],
            (42u32,),
        );
        assert_eq!(
            result,
            Err(ContractError::EventTopicLimitExceeded)
        );
    }

    #[test]
    fn emit_simple2_success() {
        let env = Env::default();
        let result = emit_simple2(&env, EV_HTLC_CLAIM, EV_PRICE_UPDATE, (100u32,));
        assert!(result.is_ok());
    }

    #[test]
    fn emit_simple3_success() {
        let env = Env::default();
        let result = emit_simple3(
            &env,
            EV_STAKE_REG,
            EV_PRICE_UPDATE,
            EV_HTLC_NEW,
            (200u32,),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn emit_simple4_success() {
        let env = Env::default();
        let result = emit_simple4(
            &env,
            EV_REVOCATION_EXEC,
            EV_COORD_ADDED,
            EV_COORD_REMOVED,
            EV_ADMIN_TRANSFER,
            (999u32,),
        );
        assert!(result.is_ok());
    }

    // ── Symbol constant sanity checks ─────────────────────────────────

    #[test]
    fn event_names_are_distinct() {
        let mut seen = soroban_sdk::Map::<Symbol, ()>::new(&Env::default());
        let names = [
            EV_PRICE_UPDATE,
            EV_PRICE_FLOOR_SET,
            EV_PRICE_FLOOR_ROLL,
            EV_PRICE_BOUNDS_SET,
            EV_PRICE_BOUNDS_ROLL,
            EV_MAX_DEV_SET,
            EV_MAX_DEV_ROLL,
            EV_ASSET_META_SET,
            EV_ASSET_INFO_SET,
            EV_ASSET_DESC_SET,
            EV_EMERGENCY_HALT,
            EV_REWARD_CLAIMED,
            EV_TELEMETRY_OK,
            EV_FALLBACK_WARN,
            EV_HTLC_NEW,
            EV_HTLC_CLAIM,
            EV_HTLC_REFUND,
            EV_ROUTE_OK,
            EV_COORD_ADDED,
            EV_COORD_REMOVED,
            EV_ADMIN_TRANSFER,
            EV_REVOCATION_VOTE,
            EV_REVOCATION_EXEC,
            EV_STAKE_REG,
            EV_STAKE_UNREG,
            EV_FEED_STAKE_REG,
            EV_FEED_STAKE_UNREG,
            EV_PENALTY_APPLIED,
            EV_UPGRADE_PROPOSED,
            EV_UPGRADE_EXECUTED,
            EV_UPGRADE_CANCELLED,
            EV_BALLOT_OPENED,
            EV_BALLOT_CLOSED,
        ];
        for name in names.iter() {
            assert!(
                seen.try_insert(*name, ()).is_ok(),
                "duplicate event name: {:?}",
                name
            );
        }
    }

    #[test]
    fn max_event_topics_is_four() {
        assert_eq!(MAX_EVENT_TOPICS, 4);
    }
}
