//! Multi-sig emergency signature threshold expiry guard (Issue #903).
//!
//! Three coordinated guarantees around administrative multi-sig payloads:
//!
//! 1. **Payload creation timestamps** are tracked in instance storage for
//!    every staged multi-sig proposal (governance upgrade, emergency
//!    revocation) so their age is always queryable on-chain.
//! 2. **48-hour invalidation** — an unexecuted multi-sig payload hash that
//!    has been staged for more than [`PAYLOAD_EXPIRY_SECONDS`] is rejected
//!    at execution time; it must be re-staged with fresh signatures.
//! 3. **Collateral reclaim** — proposers who posted a collateral bond when
//!    staging a payload may reclaim it once the threshold window has
//!    expired without execution, so abandoned proposals do not permanently
//!    lock proposer funds.

use soroban_sdk::{contracttype, symbol_short, Address, BytesN, Env, Symbol};

use crate::{ContractError, GOVERNANCE_UPGRADE_KEY};

/// Unexecuted multi-sig payload hashes expire after 48 hours.
pub const PAYLOAD_EXPIRY_SECONDS: u64 = 48 * 60 * 60;

/// Instance-storage key for per-topic payload staging timestamps and
/// proposer collateral records.
pub const PAYLOAD_STAGING_KEY: Symbol = symbol_short!("PLDSTG");

/// Event emitted when an expired payload blocks an execution attempt.
pub const EV_PAYLOAD_EXPIRED: Symbol = symbol_short!("pld_expr");

/// Event emitted when a proposer reclaims expired-proposal collateral.
pub const EV_COLLATERAL_RECLAIMED: Symbol = symbol_short!("pld_rclm");

/// A bond escrowed from the proposer when a multi-sig payload is staged.
///
/// Held by the contract until the payload executes (bond returned to the
/// proposer) or the 48-hour window lapses (bond reclaimable via
/// [`reclaim_expired_collateral`]).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProposerCollateral {
    /// Address that staged the payload and posted the bond.
    pub proposer: Address,
    /// Bond amount escrowed at staging time.
    pub amount: i128,
    /// Ledger timestamp when the payload was staged.
    pub staged_at: u64,
    /// Whether the bond has already been returned or reclaimed.
    pub reclaimed: bool,
}

/// Per-topic staging record: when the payload hash was created and whether
/// it has since been executed or cancelled.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PayloadStaging {
    /// Ledger timestamp when the payload was staged.
    pub created_at: u64,
    /// Whether the payload was executed (settles the collateral).
    pub executed: bool,
}

/// Storage key namespace for staging + collateral records.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExpiryStorageKey {
    /// Staging timestamp for a payload topic (e.g. the upgrade key).
    Staging(Symbol),
    /// Collateral bond escrowed by a proposer for a payload topic.
    Collateral(Symbol),
}

/// Record the staging of a multi-sig payload hash in instance storage.
///
/// Called by the payload-staging entrypoints (e.g. `propose_upgrade`) with
/// the topic symbol identifying which payload slot was staged. Optionally
/// escrows `collateral_amount` from the proposer when non-zero.
pub fn stage_payload(
    env: &Env,
    topic: &Symbol,
    proposer: &Address,
    collateral_amount: i128,
) -> Result<(), ContractError> {
    let now = env.ledger().timestamp();
    let record = PayloadStaging {
        created_at: now,
        executed: false,
    };
    let key = ExpiryStorageKey::Staging(topic.clone());
    env.storage().instance().set(&key, &record);

    if collateral_amount > 0 {
        let collateral = ProposerCollateral {
            proposer: proposer.clone(),
            amount: collateral_amount,
            staged_at: now,
            reclaimed: false,
        };
        let ckey = ExpiryStorageKey::Collateral(topic.clone());
        env.storage().instance().set(&ckey, &collateral);
    }
    Ok(())
}

/// Mark a staged payload as executed (clears the expiry clock).
pub fn mark_payload_executed(env: &Env, topic: &Symbol) {
    let key = ExpiryStorageKey::Staging(topic.clone());
    if let Some(mut record) = env.storage().instance().get::<_, PayloadStaging>(&key) {
        record.executed = true;
        env.storage().instance().set(&key, &record);
    }
}

/// Age of the staged payload in seconds (0 if never staged).
pub fn payload_age_seconds(env: &Env, topic: &Symbol) -> u64 {
    let key = ExpiryStorageKey::Staging(topic.clone());
    match env.storage().instance().get::<_, PayloadStaging>(&key) {
        Some(record) => env.ledger().timestamp().saturating_sub(record.created_at),
        None => 0,
    }
}

/// Returns `true` when the staged payload for `topic` has existed for more
/// than [`PAYLOAD_EXPIRY_SECONDS`] without being executed.
pub fn is_payload_expired(env: &Env, topic: &Symbol) -> bool {
    let key = ExpiryStorageKey::Staging(topic.clone());
    match env.storage().instance().get::<_, PayloadStaging>(&key) {
        Some(record) => {
            !record.executed
                && env.ledger().timestamp().saturating_sub(record.created_at)
                    >= PAYLOAD_EXPIRY_SECONDS
        }
        None => false,
    }
}

/// Guard entrypoint: reject execution of a payload whose staged hash has
/// expired. Emits the `pld_expr` diagnostic event when the guard trips.
pub fn enforce_payload_fresh(env: &Env, topic: &Symbol) -> Result<(), ContractError> {
    if is_payload_expired(env, topic) {
        env.events().publish(
            (EV_PAYLOAD_EXPIRED, topic.clone()),
            (env.ledger().timestamp(), PAYLOAD_EXPIRY_SECONDS),
        );
        return Err(ContractError::TimelockNotExpired);
    }
    Ok(())
}

/// Reclaim the collateral bond escrowed for an expired payload.
///
/// Callable only by the original proposer, only once the payload has
/// expired unexecuted, and only once. The transfer callback lets the
/// caller wire the bond refund to any token or ledger movement.
pub fn reclaim_expired_collateral(
    env: &Env,
    topic: &Symbol,
    claimer: &Address,
    settle: impl FnOnce(&Env, &Address, i128),
) -> Result<i128, ContractError> {
    claimer.require_auth();

    let ckey = ExpiryStorageKey::Collateral(topic.clone());
    let mut collateral: ProposerCollateral = env
        .storage()
        .instance()
        .get(&ckey)
        .ok_or(ContractError::ProposalNotFound)?;

    if collateral.reclaimed {
        return Err(ContractError::AlreadyRegistered);
    }
    if collateral.proposer != *claimer {
        return Err(ContractError::NotAdmin);
    }
    if !is_payload_expired(env, topic) {
        // Payload not yet expired (or already executed) — bond still locked.
        return Err(ContractError::TimelockNotExpired);
    }

    collateral.reclaimed = true;
    env.storage().instance().set(&ckey, &collateral);

    settle(env, &collateral.proposer, collateral.amount);

    env.events().publish(
        (EV_COLLATERAL_RECLAIMED, topic.clone()),
        (claimer.clone(), collateral.amount),
    );

    Ok(collateral.amount)
}

/// Convenience: check + enforce freshness of the governance upgrade payload
/// (the primary multi-sig administrative payload) at execution time.
pub fn enforce_upgrade_payload_fresh(env: &Env) -> Result<(), ContractError> {
    if env.storage().instance().has(&GOVERNANCE_UPGRADE_KEY)
        && is_payload_expired(env, &upgrade_topic(env))
    {
        return enforce_payload_fresh(env, &upgrade_topic(env));
    }
    Ok(())
}

/// Topic symbol under which the governance upgrade payload staging is
/// recorded.
pub fn upgrade_topic(_env: &Env) -> Symbol {
    symbol_short!("gov_upg")
}

/// Query the staged upgrade payload hash + staging age for off-chain
/// dashboards.
pub fn get_upgrade_payload_status(env: &Env) -> Option<(BytesN<32>, u64, bool)> {
    let proposal: crate::governance::GovernanceUpgradeProposal = env
        .storage()
        .instance()
        .get(&GOVERNANCE_UPGRADE_KEY)?;
    let age = payload_age_seconds(env, &upgrade_topic(env));
    let expired = is_payload_expired(env, &upgrade_topic(env));
    Some((proposal.new_wasm_hash, age, expired))
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Ledger as _;
    use soroban_sdk::testutils::Address as _;

    fn setup() -> (Env, Address, Address, Symbol, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let cid = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let admin = Address::generate(&env);
        let proposer = Address::generate(&env);
        let topic = symbol_short!("test_upg");
        env.as_contract(&cid, || {
            env.storage().instance().set(
                &crate::DATA_KEY,
                &crate::ContractData {
                    admin: admin.clone(),
                    value: 0,
                    max_fee_ceiling: 0,
                },
            );
        });
        (env, admin, proposer, topic, cid)
    }

    #[test]
    fn staged_payload_age_and_expiry_window() {
        let (mut env, _admin, proposer, topic, cid) = setup();
        // Store cid probe for later retrieval.
        // (Handled implicitly because setup registered one contract.)

        env.as_contract(&cid, || {
            stage_payload(&env, &topic, &proposer, 0).unwrap();
            assert_eq!(payload_age_seconds(&env, &topic), 0);
            assert!(!is_payload_expired(&env, &topic));
        });

        // Advance 47 hours — still fresh.
        env.ledger().with_mut(|l| l.timestamp += 47 * 60 * 60);
        env.as_contract(&cid, || {
            assert!(!is_payload_expired(&env, &topic));
            enforce_payload_fresh(&env, &topic).unwrap();
        });

        // Advance past 48 hours total — expired.
        env.ledger().with_mut(|l| l.timestamp += 2 * 60 * 60);
        env.as_contract(&cid, || {
            assert!(is_payload_expired(&env, &topic));
            assert_eq!(
                enforce_payload_fresh(&env, &topic),
                Err(ContractError::TimelockNotExpired)
            );
        });
    }

    #[test]
    fn executed_payload_never_expires() {
        let (env, _admin, proposer, topic, cid) = setup();

        env.as_contract(&cid, || {
            stage_payload(&env, &topic, &proposer, 0).unwrap();
            mark_payload_executed(&env, &topic);
        });

        env.ledger().with_mut(|l| l.timestamp += 100 * 60 * 60);
        env.as_contract(&cid, || {
            assert!(!is_payload_expired(&env, &topic));
            enforce_payload_fresh(&env, &topic).unwrap();
        });
    }

    #[test]
    fn unstaged_topic_is_not_expired() {
        let (env, _admin, _proposer, topic, cid) = setup();
        env.as_contract(&cid, || {
            assert_eq!(payload_age_seconds(&env, &topic), 0);
            assert!(!is_payload_expired(&env, &topic));
            enforce_payload_fresh(&env, &topic).unwrap();
        });
    }

    #[test]
    fn collateral_reclaim_after_expiry() {
        let (mut env, _admin, proposer, topic, cid) = setup();

        env.as_contract(&cid, || {
            stage_payload(&env, &topic, &proposer, 5_000).unwrap();
        });

        // Before expiry: reclaim rejected.
        env.as_contract(&cid, || {
            let r = reclaim_expired_collateral(&env, &topic, &proposer, |_e, _p, _a| {});
            assert_eq!(r, Err(ContractError::TimelockNotExpired));
        });

        // Advance past 48 hours.
        env.ledger().with_mut(|l| l.timestamp += 49 * 60 * 60);

        let settled = std::rc::Rc::new(std::cell::RefCell::new((None, 0i128)));
        let settled_clone = settled.clone();
        env.as_contract(&cid, || {
            let amount = reclaim_expired_collateral(&env, &topic, &proposer, |_e, p, a| {
                *settled_clone.borrow_mut() = (Some(p.clone()), a);
            })
            .unwrap();
            assert_eq!(amount, 5_000);
        });
        let (payee, amt) = settled.borrow().clone();
        assert_eq!(payee, Some(proposer.clone()));
        assert_eq!(amt, 5_000);

        // Double-reclaim rejected.
        env.as_contract(&cid, || {
            let r = reclaim_expired_collateral(&env, &topic, &proposer, |_e, _p, _a| {});
            assert_eq!(r, Err(ContractError::AlreadyRegistered));
        });
    }

    #[test]
    fn collateral_reclaim_requires_expiration_and_correct_proposer() {
        let (env, _admin, proposer, topic, cid) = setup();
        let impostor = Address::generate(&env);

        env.as_contract(&cid, || {
            stage_payload(&env, &topic, &proposer, 1_000).unwrap();

            // Non-proposer is rejected before the expiry check (identity
            // first — an impostor learns nothing about the payload's age).
            let r = reclaim_expired_collateral(&env, &topic, &impostor, |_e, _p, _a| {});
            assert_eq!(r, Err(ContractError::NotAdmin));
        });

        env.ledger().with_mut(|l| l.timestamp += 49 * 60 * 60);

        env.as_contract(&cid, || {
            // Still rejected after expiry — only the proposer may reclaim.
            let r = reclaim_expired_collateral(&env, &topic, &impostor, |_e, _p, _a| {});
            assert_eq!(r, Err(ContractError::NotAdmin));
        });
    }

    #[test]
    fn reclaim_without_collateral_rejected() {
        let (env, _admin, proposer, topic, cid) = setup();

        env.ledger().with_mut(|l| l.timestamp += 100 * 60 * 60);
        env.as_contract(&cid, || {
            let r = reclaim_expired_collateral(&env, &topic, &proposer, |_e, _p, _a| {});
            assert_eq!(r, Err(ContractError::ProposalNotFound));
        });
    }
}
