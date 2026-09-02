//! ── Governance Voting-Power Delegation ──
//!
//! Allows governance stakers to delegate their voting weight to a representative
//! (a "delegate") and to instantly revoke that delegation.
//!
//! When a staker delegates, their voting weight is removed from their own
//! direct balance map and added to the delegate's aggregated delegated power.
//! [`undelegate`] reverses this: it clears the staker's delegate association,
//! recomputes the former delegate's total delegated power, and restores the
//! voting weight directly into the staker's own balance map.

use soroban_sdk::{symbol_short, Address, Env, IntoVal, Map, Symbol, Val, Vec};

use crate::ContractError;

// ── Storage keys ────────────────────────────────────────────────────────────

/// Staker -> active delegation (delegate + amount delegated).
pub(crate) const DELEGATIONS_KEY: Symbol = symbol_short!("DELS");
/// Delegate -> sum of all voting weight delegated to them.
pub(crate) const DELEGATED_TOTALS_KEY: Symbol = symbol_short!("DELTOT");
/// Staker -> direct voting weight held in their own balance map.
pub(crate) const VOTING_WEIGHTS_KEY: Symbol = symbol_short!("VTWGT");

// ── Data types ──────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone)]
pub struct Delegation {
    /// The address the staker delegated their voting power to.
    pub delegate: Address,
    /// The amount of voting weight moved to the delegate.
    pub amount: u128,
}

#[contracttype]
#[derive(Clone)]
pub struct UndelegateEvent {
    pub staker: Address,
    pub former_delegate: Address,
    pub restored_weight: u128,
    pub delegate_remaining_power: u128,
}

// ── Storage accessors ───────────────────────────────────────────────────────

fn load_delegations(env: &Env) -> Map<Address, Delegation> {
    env.storage()
        .instance()
        .get(&DELEGATIONS_KEY)
        .unwrap_or_else(|| Map::new(env))
}

fn save_delegations(env: &Env, delegations: &Map<Address, Delegation>) {
    env.storage().instance().set(&DELEGATIONS_KEY, delegations);
}

fn load_delegated_totals(env: &Env) -> Map<Address, u128> {
    env.storage()
        .instance()
        .get(&DELEGATED_TOTALS_KEY)
        .unwrap_or_else(|| Map::new(env))
}

fn save_delegated_totals(env: &Env, totals: &Map<Address, u128>) {
    env.storage()
        .instance()
        .set(&DELEGATED_TOTALS_KEY, totals);
}

fn load_voting_weights(env: &Env) -> Map<Address, u128> {
    env.storage()
        .instance()
        .get(&VOTING_WEIGHTS_KEY)
        .unwrap_or_else(|| Map::new(env))
}

fn save_voting_weights(env: &Env, weights: &Map<Address, u128>) {
    env.storage()
        .instance()
        .set(&VOTING_WEIGHTS_KEY, weights);
}

// ── Public read helpers ─────────────────────────────────────────────────────

/// Direct voting weight currently held by `staker` (their own balance map).
pub fn get_voting_weight(env: &Env, staker: &Address) -> u128 {
    let weights = load_voting_weights(env);
    weights.get(staker.clone()).unwrap_or(0u128)
}

/// Active delegation for `staker`, if any.
pub fn get_delegation(env: &Env, staker: &Address) -> Option<Delegation> {
    let delegations = load_delegations(env);
    delegations.get(staker.clone())
}

/// Total voting power delegated to `delegate` across all stakers.
pub fn get_delegated_total(env: &Env, delegate: &Address) -> u128 {
    let totals = load_delegated_totals(env);
    totals.get(delegate.clone()).unwrap_or(0u128)
}

/// Initialize (or top up) a staker's direct voting weight in their balance map.
///
/// This is the source balance that a staker moves when they delegate. It would
/// normally be seeded from a staker's locked governance stake; this entrypoint
/// exists to make the delegation lifecycle testable and to allow governance to
/// credit voting weight directly.
pub fn set_voting_weight(env: &Env, staker: &Address, amount: u128) {
    let mut weights = load_voting_weights(env);
    let current = weights.get(staker.clone()).unwrap_or(0u128);
    let new_total = current
        .checked_add(amount)
        .ok_or(ContractError::Overflow)
        .unwrap();
    weights.set(staker.clone(), new_total);
    save_voting_weights(env, &weights);
    env.storage().instance().extend_ttl(518_400u32, 6_312_000u32);
}

// ── Core logic ──────────────────────────────────────────────────────────────

/// Delegate the staker's entire direct voting weight to `delegate`.
///
/// The staker's direct balance map is cleared to zero and the same amount is
/// added to the delegate's aggregated delegated power metric.
pub fn delegate(env: &Env, staker: &Address, delegate: &Address) -> Result<(), ContractError> {
    if staker == delegate {
        return Err(ContractError::InvalidDelegate);
    }

    // If the staker already had an active delegation, the movable amount is the
    // amount previously delegated (their direct balance is already zeroed).
    // Otherwise it is the direct voting weight currently in their balance map.
    let amount = if let Some(existing) = get_delegation(env, staker) {
        // Reclaim the former delegate's totals first so metrics stay consistent.
        let mut totals = load_delegated_totals(env);
        let prev = totals.get(existing.delegate.clone()).unwrap_or(0u128);
        totals.set(existing.delegate.clone(), prev.saturating_sub(existing.amount));
        save_delegated_totals(env, &totals);
        existing.amount
    } else {
        let direct = get_voting_weight(env, staker);
        if direct == 0 {
            return Err(ContractError::NoVotingWeight);
        }
        direct
    };

    // Move the staker's weight out of their direct balance map.
    let mut weights = load_voting_weights(env);
    weights.set(staker.clone(), 0u128);
    save_voting_weights(env, &weights);

    // Record the delegation and credit the new delegate.
    let mut delegations = load_delegations(env);
    delegations.set(
        staker.clone(),
        Delegation {
            delegate: delegate.clone(),
            amount,
        },
    );
    save_delegations(env, &delegations);

    let mut totals = load_delegated_totals(env);
    let current = totals.get(delegate.clone()).unwrap_or(0u128);
    let new_total = current.checked_add(amount).ok_or(ContractError::Overflow)?;
    totals.set(delegate.clone(), new_total);
    save_delegated_totals(env, &totals);

    env.storage().instance().extend_ttl(518_400u32, 6_312_000u32);
    Ok(())
}

/// Instantly revoke delegated voting power and reclaim direct voting rights.
///
/// Deliverables implemented here:
/// 1. Clear the staker's target delegate association.
/// 2. Recompute the former delegate's total delegated power metric.
/// 3. Restore the voting weight directly into the staker's balance map.
pub fn undelegate(env: &Env, staker: &Address) -> Result<UndelegateEvent, ContractError> {
    let delegation = get_delegation(env, staker)
        .ok_or(ContractError::NoActiveDelegation)?;

    let former_delegate = delegation.delegate.clone();
    let amount = delegation.amount;

    // (1) Clear the staker's delegate association.
    let mut delegations = load_delegations(env);
    delegations.remove(staker.clone());
    save_delegations(env, &delegations);

    // (2) Recompute the former delegate's total delegated power metric.
    let mut totals = load_delegated_totals(env);
    let current = totals.get(former_delegate.clone()).unwrap_or(0u128);
    let remaining = current.saturating_sub(amount);
    if remaining == 0 {
        totals.remove(former_delegate.clone());
    } else {
        totals.set(former_delegate.clone(), remaining);
    }
    save_delegated_totals(env, &totals);

    // (3) Restore the voting weight directly into the staker's balance map.
    let mut weights = load_voting_weights(env);
    let own = weights.get(staker.clone()).unwrap_or(0u128);
    let restored = own
        .checked_add(amount)
        .ok_or(ContractError::Overflow)?;
    weights.set(staker.clone(), restored);
    save_voting_weights(env, &weights);

    env.storage().instance().extend_ttl(518_400u32, 6_312_000u32);

    let event = UndelegateEvent {
        staker: staker.clone(),
        former_delegate: former_delegate.clone(),
        restored_weight: amount,
        delegate_remaining_power: remaining,
    };

    let mut topics: Vec<Val> = Vec::new(env);
    topics.push_back(symbol_short!("UNDELEG").into_val(env));
    topics.push_back(staker.clone().into_val(env));
    topics.push_back(former_delegate.clone().into_val(env));
    env.events().publish(topics, event.clone());

    Ok(event)
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    fn setup() -> (Env, Address, Address, Address) {
        let env = Env::default();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let staker = Address::generate(&env);
        let delegate = Address::generate(&env);
        let other = Address::generate(&env);
        env.as_contract(&contract_id, || {
            set_voting_weight(&env, &staker, 100u128);
        });
        (env, contract_id, staker, delegate)
    }

    #[test]
    fn test_delegate_then_undelegate() {
        let env = Env::default();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let staker = Address::generate(&env);
        let delegate = Address::generate(&env);
        env.as_contract(&contract_id, || {
            set_voting_weight(&env, &staker, 100u128);
            delegate(&env, &staker, &delegate).unwrap();
            assert_eq!(get_voting_weight(&env, &staker), 0u128);
            assert_eq!(get_delegated_total(&env, &delegate), 100u128);

            let ev = undelegate(&env, &staker).unwrap();
            assert_eq!(ev.restored_weight, 100u128);
            assert_eq!(ev.delegate_remaining_power, 0u128);
            // Association cleared
            assert!(get_delegation(&env, &staker).is_none());
            // Weight restored to staker balance map
            assert_eq!(get_voting_weight(&env, &staker), 100u128);
            // Former delegate power recomputed to zero
            assert_eq!(get_delegated_total(&env, &delegate), 0u128);
        });
    }

    #[test]
    fn test_undelegate_without_delegation_errors() {
        let env = Env::default();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let staker = Address::generate(&env);
        env.as_contract(&contract_id, || {
            assert_eq!(
                undelegate(&env, &staker),
                Err(ContractError::NoActiveDelegation)
            );
        });
    }
}
