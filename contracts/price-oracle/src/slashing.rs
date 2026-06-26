use crate::median::{calculate_median, MedianError};
use soroban_sdk::{contracttype, Vec};

/// Discrete slashing tiers used to differentiate small communication noise from deliberate manipulation.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SlashingTier {
    NoPenalty,
    Low,
    Medium,
    High,
    Critical,
}

/// The result of comparing a faulty provider's submitted price against the consensus median.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviationAnalysis {
    pub submitted_price: i128,
    pub finalized_median_price: i128,
    pub deviation_bps: u128,
    pub slashing_bps: u32,
    pub tier: SlashingTier,
}

impl SlashingTier {
    pub fn from_deviation_bps(deviation_bps: u128) -> Self {
        match deviation_bps {
            0..=100 => SlashingTier::NoPenalty,
            101..=250 => SlashingTier::Low,
            251..=500 => SlashingTier::Medium,
            501..=1_000 => SlashingTier::High,
            _ => SlashingTier::Critical,
        }
    }

    pub fn burn_rate_bps(self) -> u32 {
        match self {
            SlashingTier::NoPenalty => 0,
            SlashingTier::Low => 50,
            SlashingTier::Medium => 150,
            SlashingTier::High => 400,
            SlashingTier::Critical => 1_000,
        }
    }
}

/// Calculate the absolute price deviation from the finalized consensus median in basis points.
/// Returns `None` when the consensus median is zero or when the result cannot be computed safely.
pub fn calculate_price_deviation_bps(submitted_price: i128, finalized_median_price: i128) -> Option<u128> {
    if finalized_median_price <= 0 {
        return None;
    }

    let deviation = if submitted_price >= finalized_median_price {
        submitted_price - finalized_median_price
    } else {
        finalized_median_price - submitted_price
    };

    let numerator = (deviation as u128).checked_mul(10_000)?;
    let denominator = finalized_median_price as u128;
    Some(numerator / denominator)
}

/// Convert a deviation into a slashing burn rate in basis points using a tiered scale.
pub fn calculate_slashing_bps(deviation_bps: u128) -> u32 {
    SlashingTier::from_deviation_bps(deviation_bps).burn_rate_bps()
}

/// Analyze a faulty node price submission against a finalized median consensus price set.
///
/// This returns the computed median, the absolute deviation in basis points, and
/// a burn rate that grows with the magnitude of the deviation.
pub fn analyze_deviation_against_finalized_median(
    submitted_price: i128,
    consensus_prices: Vec<i128>,
) -> Result<DeviationAnalysis, MedianError> {
    let finalized_median_price = calculate_median(consensus_prices)?;
    let deviation_bps = calculate_price_deviation_bps(submitted_price, finalized_median_price)
        .unwrap_or(0);
    let tier = SlashingTier::from_deviation_bps(deviation_bps);
    let slashing_bps = tier.burn_rate_bps();

    Ok(DeviationAnalysis {
        submitted_price,
        finalized_median_price,
        deviation_bps,
        slashing_bps,
        tier,
    })
use soroban_sdk::{contractevent, contracttype, Address, Env};

use crate::Error;

pub const MIN_UNBONDING_DELAY_LEDGERS: u32 = 10_000;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnbondingRequest {
    pub validator: Address,
    pub amount: i128,
    pub requested_ledger: u32,
    pub release_ledger: u32,
    pub released: bool,
}

#[contracttype]
#[derive(Clone)]
enum DataKey {
    Unbonding(Address),
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnbondingQueued {
    pub validator: Address,
    pub amount: i128,
    pub requested_ledger: u32,
    pub release_ledger: u32,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnbondingReleased {
    pub validator: Address,
    pub amount: i128,
    pub release_ledger: u32,
}

pub fn request_unbonding(
    env: &Env,
    validator: &Address,
    amount: i128,
) -> Result<UnbondingRequest, Error> {
    if amount <= 0 {
        return Err(Error::InvalidStakeAmount);
    }

    validator.require_auth();

    if let Some(existing) = get_unbonding_request(env, validator) {
        if !existing.released {
            return Err(Error::UnbondingAlreadyQueued);
        }
    }

    let requested_ledger = env.ledger().sequence();
    let release_ledger = requested_ledger
        .checked_add(MIN_UNBONDING_DELAY_LEDGERS)
        .ok_or(Error::LedgerSequenceOverflow)?;
    let request = UnbondingRequest {
        validator: validator.clone(),
        amount,
        requested_ledger,
        release_ledger,
        released: false,
    };

    env.storage()
        .persistent()
        .set(&DataKey::Unbonding(validator.clone()), &request);

    UnbondingQueued {
        validator: validator.clone(),
        amount,
        requested_ledger,
        release_ledger,
    }
    .publish(env);

    Ok(request)
}

pub fn release_unbonded_stake(env: &Env, validator: &Address) -> Result<i128, Error> {
    validator.require_auth();

    let key = DataKey::Unbonding(validator.clone());
    let mut request = env
        .storage()
        .persistent()
        .get::<DataKey, UnbondingRequest>(&key)
        .ok_or(Error::UnbondingRequestNotFound)?;

    if request.released {
        return Err(Error::UnbondingAlreadyReleased);
    }

    let current_ledger = env.ledger().sequence();
    if current_ledger < request.release_ledger {
        return Err(Error::UnbondingDelayActive);
    }

    request.released = true;
    env.storage().persistent().set(&key, &request);

    UnbondingReleased {
        validator: validator.clone(),
        amount: request.amount,
        release_ledger: current_ledger,
    }
    .publish(env);

    Ok(request.amount)
}

pub fn get_unbonding_request(env: &Env, validator: &Address) -> Option<UnbondingRequest> {
    env.storage()
        .persistent()
        .get(&DataKey::Unbonding(validator.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{vec, Env};

    #[test]
    fn test_calculate_price_deviation_bps_returns_none_for_zero_median() {
        assert_eq!(calculate_price_deviation_bps(1_000_000, 0), None);
    }

    #[test]
    fn test_calculate_price_deviation_bps_small_deviation() {
        assert_eq!(calculate_price_deviation_bps(1_001_000, 1_000_000), Some(100));
        assert_eq!(calculate_price_deviation_bps(999_000, 1_000_000), Some(100));
    }

    #[test]
    fn test_calculate_slashing_bps_tiers() {
        assert_eq!(calculate_slashing_bps(0), 0);
        assert_eq!(calculate_slashing_bps(150), 50);
        assert_eq!(calculate_slashing_bps(300), 150);
        assert_eq!(calculate_slashing_bps(750), 400);
        assert_eq!(calculate_slashing_bps(2_500), 1_000);
    }

    #[test]
    fn test_analyze_deviation_against_finalized_median() {
        let env = Env::default();
        let prices = vec![&env, 10_000_i128, 10_100_i128, 9_900_i128, 11_000_i128];

        let analysis = analyze_deviation_against_finalized_median(11_500, prices).unwrap();

        assert_eq!(analysis.finalized_median_price, 10_050);
        assert_eq!(analysis.deviation_bps, 1_447);
        assert_eq!(analysis.tier, SlashingTier::Critical);
        assert_eq!(analysis.slashing_bps, 1_000);
    }

    #[test]
    fn test_slashing_tier_for_minor_node_hiccup() {
        assert_eq!(SlashingTier::from_deviation_bps(100), SlashingTier::NoPenalty);
        assert_eq!(SlashingTier::from_deviation_bps(180), SlashingTier::Low);
    use soroban_sdk::{contract, contractimpl, testutils::Address as _, testutils::Ledger};

    #[contract]
    struct TestContract;

    #[contractimpl]
    impl TestContract {}

    fn setup() -> (Env, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(TestContract, ());
        let validator = Address::generate(&env);
        (env, contract_id, validator)
    }

    #[test]
    fn request_queues_unbonding_for_minimum_delay() {
        let (env, contract_id, validator) = setup();
        env.ledger().set_sequence_number(250);

        env.as_contract(&contract_id, || {
            let request = request_unbonding(&env, &validator, 1_500).unwrap();

            assert_eq!(request.amount, 1_500);
            assert_eq!(request.requested_ledger, 250);
            assert_eq!(request.release_ledger, 10_250);
            assert!(!request.released);
            assert_eq!(get_unbonding_request(&env, &validator), Some(request));
        });
    }

    #[test]
    fn release_fails_before_delay_expires() {
        let (env, contract_id, validator) = setup();
        env.ledger().set_sequence_number(1);

        env.as_contract(&contract_id, || {
            request_unbonding(&env, &validator, 900).unwrap();
            env.ledger()
                .set_sequence_number(MIN_UNBONDING_DELAY_LEDGERS);

            assert_eq!(
                release_unbonded_stake(&env, &validator),
                Err(Error::UnbondingDelayActive)
            );
        });
    }

    #[test]
    fn release_succeeds_at_exact_delay_boundary() {
        let (env, contract_id, validator) = setup();
        env.ledger().set_sequence_number(1);

        env.as_contract(&contract_id, || {
            request_unbonding(&env, &validator, 900).unwrap();
            env.ledger()
                .set_sequence_number(1 + MIN_UNBONDING_DELAY_LEDGERS);

            assert_eq!(release_unbonded_stake(&env, &validator), Ok(900));
            let released = get_unbonding_request(&env, &validator).unwrap();
            assert!(released.released);
        });
    }

    #[test]
    fn duplicate_pending_unbonding_is_rejected() {
        let (env, contract_id, validator) = setup();

        env.as_contract(&contract_id, || {
            request_unbonding(&env, &validator, 900).unwrap();

            assert_eq!(
                request_unbonding(&env, &validator, 700),
                Err(Error::UnbondingAlreadyQueued)
            );
        });
    }
}
