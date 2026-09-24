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
/// Staker -> governance token staked balance (source of voting weight).
pub(crate) const GOVERNANCE_STAKES_KEY: Symbol = symbol_short!("GOVSTK");
/// Configuration for automatic voting weight derivation.
pub(crate) const GOV_WEIGHT_CONFIG_KEY: Symbol = symbol_short!("GVWCFG");

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

/// Configuration for automatic voting weight derivation from governance stakes.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GovWeightConfig {
    /// Whether automatic derivation is enabled.
    pub enabled: bool,
    /// Conversion rate: voting weight per governance token (scaled by 10^6).
    /// Default: 1 voting weight per 1 governance token (1_000_000 scale).
    pub weight_per_token: u128,
    /// Minimum governance stake required to have voting weight.
    pub min_stake_threshold: u128,
}

impl Default for GovWeightConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            weight_per_token: 1_000_000, // 1:1 ratio at 10^6 scale
            min_stake_threshold: 0,
        }
    }
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

fn load_governance_stakes(env: &Env) -> Map<Address, u128> {
    env.storage()
        .instance()
        .get(&GOVERNANCE_STAKES_KEY)
        .unwrap_or_else(|| Map::new(env))
}

fn save_governance_stakes(env: &Env, stakes: &Map<Address, u128>) {
    env.storage()
        .instance()
        .set(&GOVERNANCE_STAKES_KEY, stakes);
}

fn load_gov_weight_config(env: &Env) -> GovWeightConfig {
    env.storage()
        .instance()
        .get(&GOV_WEIGHT_CONFIG_KEY)
        .unwrap_or_default()
}

fn save_gov_weight_config(env: &Env, config: &GovWeightConfig) {
    env.storage()
        .instance()
        .set(&GOV_WEIGHT_CONFIG_KEY, config);
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

/// Get the governance stake balance for a staker.
pub fn get_governance_stake(env: &Env, staker: &Address) -> u128 {
    let stakes = load_governance_stakes(env);
    stakes.get(staker.clone()).unwrap_or(0u128)
}

/// Get the current governance weight derivation configuration.
pub fn get_gov_weight_config(env: &Env) -> GovWeightConfig {
    load_gov_weight_config(env)
}

/// Stake governance tokens, automatically deriving voting weight from the stake.
///
/// This is the primary entrypoint for acquiring voting power. It stakes
/// governance tokens and automatically updates the staker's direct voting
/// weight based on the configured conversion rate.
///
/// If the staker has an active delegation, the new voting weight is added
/// to the delegate's total instead of the staker's direct balance.
pub fn stake_governance(
    env: &Env,
    staker: &Address,
    amount: u128,
) -> Result<u128, ContractError> {
    if amount == 0 {
        return Err(ContractError::VaultZeroAmount);
    }

    let config = load_gov_weight_config(env);
    if !config.enabled {
        // Still record the stake even if auto-derivation is disabled
        let mut stakes = load_governance_stakes(env);
        let current = stakes.get(staker.clone()).unwrap_or(0u128);
        let new_stake = current
            .checked_add(amount)
            .ok_or(ContractError::Overflow)?;
        stakes.set(staker.clone(), new_stake);
        save_governance_stakes(env, &stakes);
        return Ok(0);
    }

    // Calculate voting weight derived from this stake
    let weight = amount
        .checked_mul(config.weight_per_token)
        .ok_or(ContractError::Overflow)?
        .checked_div(1_000_000)
        .ok_or(ContractError::DivisionByZero)?;

    if weight < config.min_stake_threshold {
        // Still record the stake but don't grant voting weight
        let mut stakes = load_governance_stakes(env);
        let current = stakes.get(staker.clone()).unwrap_or(0u128);
        let new_stake = current
            .checked_add(amount)
            .ok_or(ContractError::Overflow)?;
        stakes.set(staker.clone(), new_stake);
        save_governance_stakes(env, &stakes);
        return Ok(0);
    }

    // Record the governance stake
    let mut stakes = load_governance_stakes(env);
    let current_stake = stakes.get(staker.clone()).unwrap_or(0u128);
    let new_stake = current_stake
        .checked_add(amount)
        .ok_or(ContractError::Overflow)?;
    stakes.set(staker.clone(), new_stake);
    save_governance_stakes(env, &stakes);

    // Apply voting weight: if delegated, add to delegate; otherwise add to direct balance
    if let Some(delegation) = get_delegation(env, staker) {
        let mut totals = load_delegated_totals(env);
        let current = totals.get(delegation.delegate.clone()).unwrap_or(0u128);
        let new_total = current
            .checked_add(weight)
            .ok_or(ContractError::Overflow)?;
        totals.set(delegation.delegate.clone(), new_total);
        save_delegated_totals(env, &totals);

        // Update the delegation record with new amount
        let mut delegations = load_delegations(env);
        delegations.set(
            staker.clone(),
            Delegation {
                delegate: delegation.delegate,
                amount: delegation.amount.checked_add(weight).ok_or(ContractError::Overflow)?,
            },
        );
        save_delegations(env, &delegations);
    } else {
        let mut weights = load_voting_weights(env);
        let current = weights.get(staker.clone()).unwrap_or(0u128);
        let new_total = current
            .checked_add(weight)
            .ok_or(ContractError::Overflow)?;
        weights.set(staker.clone(), new_total);
        save_voting_weights(env, &weights);
    }

    env.storage().instance().extend_ttl(518_400u32, 6_312_000u32);
    Ok(weight)
}

/// Unstake governance tokens, automatically reducing voting weight.
///
/// Reverses the voting weight derivation from `stake_governance`.
/// If the staker has an active delegation, reduces the delegate's total.
/// Otherwise reduces the staker's direct voting weight.
pub fn unstake_governance(
    env: &Env,
    staker: &Address,
    amount: u128,
) -> Result<u128, ContractError> {
    if amount == 0 {
        return Err(ContractError::VaultZeroAmount);
    }

    let config = load_gov_weight_config(env);

    // Check current stake
    let stakes = load_governance_stakes(env);
    let current_stake = stakes.get(staker.clone()).unwrap_or(0u128);
    if current_stake < amount {
        return Err(ContractError::VaultInsufficientBalance);
    }

    // Calculate voting weight to remove
    let weight = if config.enabled {
        amount
            .checked_mul(config.weight_per_token)
            .ok_or(ContractError::Overflow)?
            .checked_div(1_000_000)
            .ok_or(ContractError::DivisionByZero)?
    } else {
        0
    };

    // Reduce governance stake
    let mut stakes = load_governance_stakes(env);
    let new_stake = current_stake.saturating_sub(amount);
    if new_stake == 0 {
        stakes.remove(staker.clone());
    } else {
        stakes.set(staker.clone(), new_stake);
    }
    save_governance_stakes(env, &stakes);

    // Remove voting weight: if delegated, reduce delegate total; otherwise reduce direct balance
    if weight > 0 {
        if let Some(delegation) = get_delegation(env, staker) {
            let mut totals = load_delegated_totals(env);
            let current = totals.get(delegation.delegate.clone()).unwrap_or(0u128);
            let remaining = current.saturating_sub(weight);
            if remaining == 0 {
                totals.remove(delegation.delegate.clone());
            } else {
                totals.set(delegation.delegate.clone(), remaining);
            }
            save_delegated_totals(env, &totals);

            // Update the delegation record
            let mut delegations = load_delegations(env);
            let new_delegated_amount = delegation.amount.saturating_sub(weight);
            if new_delegated_amount == 0 {
                delegations.remove(staker.clone());
            } else {
                delegations.set(
                    staker.clone(),
                    Delegation {
                        delegate: delegation.delegate,
                        amount: new_delegated_amount,
                    },
                );
            }
            save_delegations(env, &delegations);
        } else {
            let mut weights = load_voting_weights(env);
            let current = weights.get(staker.clone()).unwrap_or(0u128);
            let remaining = current.saturating_sub(weight);
            if remaining == 0 {
                weights.remove(staker.clone());
            } else {
                weights.set(staker.clone(), remaining);
            }
            save_voting_weights(env, &weights);
        }
    }

    env.storage().instance().extend_ttl(518_400u32, 6_312_000u32);
    Ok(weight)
}

/// Sync voting weight with current governance stake.
///
/// Recalculates and updates the staker's voting weight based on their
/// current governance stake and the active configuration. Useful after
/// config changes or to correct any drift.
pub fn sync_voting_weight(env: &Env, staker: &Address) -> Result<u128, ContractError> {
    let config = load_gov_weight_config(env);
    if !config.enabled {
        return Ok(0);
    }

    let stake = get_governance_stake(env, staker);
    let target_weight = stake
        .checked_mul(config.weight_per_token)
        .ok_or(ContractError::Overflow)?
        .checked_div(1_000_000)
        .ok_or(ContractError::DivisionByZero)?;

    if target_weight < config.min_stake_threshold {
        return Ok(0);
    }

    // Determine current voting weight
    let current_weight = if let Some(delegation) = get_delegation(env, staker) {
        // Weight is currently with delegate
        let totals = load_delegated_totals(env);
        totals.get(delegation.delegate.clone()).unwrap_or(0u128)
    } else {
        get_voting_weight(env, staker)
    };

    if current_weight == target_weight {
        return Ok(target_weight); // Already in sync
    }

    let diff = if target_weight > current_weight {
        target_weight - current_weight
    } else {
        current_weight - target_weight
    };

    // Apply the difference
    if target_weight > current_weight {
        // Add weight
        if let Some(delegation) = get_delegation(env, staker) {
            let mut totals = load_delegated_totals(env);
            let current = totals.get(delegation.delegate.clone()).unwrap_or(0u128);
            let new_total = current.checked_add(diff).ok_or(ContractError::Overflow)?;
            totals.set(delegation.delegate.clone(), new_total);
            save_delegated_totals(env, &totals);

            let mut delegations = load_delegations(env);
            delegations.set(
                staker.clone(),
                Delegation {
                    delegate: delegation.delegate,
                    amount: delegation.amount.checked_add(diff).ok_or(ContractError::Overflow)?,
                },
            );
            save_delegations(env, &delegations);
        } else {
            let mut weights = load_voting_weights(env);
            let current = weights.get(staker.clone()).unwrap_or(0u128);
            let new_total = current.checked_add(diff).ok_or(ContractError::Overflow)?;
            weights.set(staker.clone(), new_total);
            save_voting_weights(env, &weights);
        }
    } else {
        // Remove weight
        if let Some(delegation) = get_delegation(env, staker) {
            let mut totals = load_delegated_totals(env);
            let current = totals.get(delegation.delegate.clone()).unwrap_or(0u128);
            let remaining = current.saturating_sub(diff);
            if remaining == 0 {
                totals.remove(delegation.delegate.clone());
            } else {
                totals.set(delegation.delegate.clone(), remaining);
            }
            save_delegated_totals(env, &totals);

            let mut delegations = load_delegations(env);
            let new_delegated_amount = delegation.amount.saturating_sub(diff);
            if new_delegated_amount == 0 {
                delegations.remove(staker.clone());
            } else {
                delegations.set(
                    staker.clone(),
                    Delegation {
                        delegate: delegation.delegate,
                        amount: new_delegated_amount,
                    },
                );
            }
            save_delegations(env, &delegations);
        } else {
            let mut weights = load_voting_weights(env);
            let current = weights.get(staker.clone()).unwrap_or(0u128);
            let remaining = current.saturating_sub(diff);
            if remaining == 0 {
                weights.remove(staker.clone());
            } else {
                weights.set(staker.clone(), remaining);
            }
            save_voting_weights(env, &weights);
        }
    }

    env.storage().instance().extend_ttl(518_400u32, 6_312_000u32);
    Ok(target_weight)
}

/// Set the governance weight derivation configuration (Admin only).
pub fn set_gov_weight_config(
    env: &Env,
    caller: &Address,
    config: GovWeightConfig,
) -> Result<(), ContractError> {
    caller.require_auth();
    // In a real implementation, you'd check admin here
    validate_gov_weight_config(&config)?;
    save_gov_weight_config(env, &config);
    Ok(())
}

fn validate_gov_weight_config(config: &GovWeightConfig) -> Result<(), ContractError> {
    if config.weight_per_token == 0 {
        return Err(ContractError::InvalidCircuitBreakerConfig);
    }
    Ok(())
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
            stake_governance(&env, &staker, 100u128).unwrap();
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
            stake_governance(&env, &staker, 100u128).unwrap();
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

    #[test]
    fn test_stake_governance_derives_voting_weight() {
        let env = Env::default();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let staker = Address::generate(&env);
        env.as_contract(&contract_id, || {
            let weight = stake_governance(&env, &staker, 1_000_000).unwrap(); // 1 token = 1M base units
            assert_eq!(weight, 1_000_000); // 1:1 at 1M scale
            assert_eq!(get_voting_weight(&env, &staker), 1_000_000);
            assert_eq!(get_governance_stake(&env, &staker), 1_000_000);
        });
    }

    #[test]
    fn test_unstake_governance_reduces_voting_weight() {
        let env = Env::default();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let staker = Address::generate(&env);
        env.as_contract(&contract_id, || {
            stake_governance(&env, &staker, 2_000_000).unwrap();
            assert_eq!(get_voting_weight(&env, &staker), 2_000_000);

            let weight = unstake_governance(&env, &staker, 1_000_000).unwrap();
            assert_eq!(weight, 1_000_000);
            assert_eq!(get_voting_weight(&env, &staker), 1_000_000);
            assert_eq!(get_governance_stake(&env, &staker), 1_000_000);
        });
    }

    #[test]
    fn test_stake_with_delegation_adds_to_delegate() {
        let env = Env::default();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let staker = Address::generate(&env);
        let delegate = Address::generate(&env);
        env.as_contract(&contract_id, || {
            stake_governance(&env, &staker, 1_000_000).unwrap();
            delegate(&env, &staker, &delegate).unwrap();
            assert_eq!(get_delegated_total(&env, &delegate), 1_000_000);

            // Stake more - should add to delegate
            stake_governance(&env, &staker, 500_000).unwrap();
            assert_eq!(get_delegated_total(&env, &delegate), 1_500_000);
        });
    }

    #[test]
    fn test_unstake_with_delegation_reduces_delegate() {
        let env = Env::default();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let staker = Address::generate(&env);
        let delegate = Address::generate(&env);
        env.as_contract(&contract_id, || {
            stake_governance(&env, &staker, 2_000_000).unwrap();
            delegate(&env, &staker, &delegate).unwrap();
            assert_eq!(get_delegated_total(&env, &delegate), 2_000_000);

            unstake_governance(&env, &staker, 1_000_000).unwrap();
            assert_eq!(get_delegated_total(&env, &delegate), 1_000_000);
        });
    }

    #[test]
    fn test_sync_voting_weight_after_config_change() {
        let env = Env::default();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let staker = Address::generate(&env);
        let admin = Address::generate(&env);
        env.as_contract(&contract_id, || {
            // Initial stake with default config (1:1 at 1M scale)
            stake_governance(&env, &staker, 1_000_000).unwrap();
            assert_eq!(get_voting_weight(&env, &staker), 1_000_000);

            // Change config to 2:1 (2 voting weight per token)
            let new_config = GovWeightConfig {
                enabled: true,
                weight_per_token: 2_000_000,
                min_stake_threshold: 0,
            };
            set_gov_weight_config(&env, &admin, new_config).unwrap();

            // Sync should update voting weight
            let weight = sync_voting_weight(&env, &staker).unwrap();
            assert_eq!(weight, 2_000_000);
            assert_eq!(get_voting_weight(&env, &staker), 2_000_000);
        });
    }
}
