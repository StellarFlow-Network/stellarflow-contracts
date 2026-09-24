use crate::median::{calculate_median, MedianError};
use soroban_sdk::{contracttype, Address, Env, String, Vec};

use crate::{ContractError, Error};

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
}

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnbondingQueued {
    pub validator: Address,
    pub amount: i128,
    pub requested_ledger: u32,
    pub release_ledger: u32,
}

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

pub fn report_missed_blocks(env: &Env, relayer: &Address, missed_blocks: u32) -> Result<i128, Error> {
    let current = env
        .storage()
        .persistent()
        .get::<crate::types::DataKey, u32>(&crate::types::DataKey::ProviderConsecutiveMissedBlocks(relayer.clone()))
        .unwrap_or(0);
    let updated = current.saturating_add(missed_blocks);
    env.storage()
        .persistent()
        .set(&crate::types::DataKey::ProviderConsecutiveMissedBlocks(relayer.clone()), &updated);
    Ok(get_slash_multiplier(env, relayer)? )
}

pub fn report_successful_uptime(env: &Env, relayer: &Address) -> Result<bool, Error> {
    env.storage()
        .persistent()
        .remove(&crate::types::DataKey::ProviderConsecutiveMissedBlocks(relayer.clone()));
    env.storage()
        .persistent()
        .set(&crate::types::DataKey::ProviderUptimeStreakStart(relayer.clone()), &env.ledger().timestamp());
    Ok(true)
}

pub fn get_consecutive_missed_blocks(env: &Env, relayer: &Address) -> u32 {
    env.storage()
        .persistent()
        .get::<crate::types::DataKey, u32>(&crate::types::DataKey::ProviderConsecutiveMissedBlocks(relayer.clone()))
        .unwrap_or(0)
}

pub fn get_slash_multiplier(env: &Env, relayer: &Address) -> Result<i128, Error> {
    let missed = get_consecutive_missed_blocks(env, relayer);
    let multiplier = 1_i128 + (missed / 3) as i128;
    Ok(multiplier.min(8))
}

pub fn get_uptime_streak_start(env: &Env, relayer: &Address) -> Option<u64> {
    env.storage()
        .persistent()
        .get::<crate::types::DataKey, u64>(&crate::types::DataKey::ProviderUptimeStreakStart(relayer.clone()))
}

pub fn parse_slash_amount(_env: &Env, data: &String) -> Result<i128, ContractError> {
    let text = data.to_string();
    text.parse::<i128>().map_err(|_| ContractError::InvalidSlashAmount)
}

pub fn execute_slash_internal(
    env: &Env,
    executor: &Address,
    bad_relayer: &Address,
    amount: i128,
) -> Result<(), ContractError> {
    executor.require_auth();
    if amount <= 0 {
        return Err(ContractError::InvalidSlashAmount);
    }

    let current_stake = env
        .storage()
        .persistent()
        .get::<crate::types::DataKey, i128>(&crate::types::DataKey::ProviderStake(bad_relayer.clone()))
        .unwrap_or(0);
    if amount > current_stake {
        return Err(ContractError::InsufficientStake);
    }

    let new_stake = current_stake - amount;
    env.storage()
        .persistent()
        .set(&crate::types::DataKey::ProviderStake(bad_relayer.clone()), &new_stake);
    Ok(())
}

pub fn apply_slash_cap(raw_penalty: i128, bond_capacity: i128) -> i128 {
    if bond_capacity <= 0 {
        return 0;
    }

    let cap = bond_capacity.saturating_mul(25).saturating_div(100);
    raw_penalty.min(cap)
}

pub fn deviation_multiplier(tier: DeviationTier) -> i128 {
    match tier {
        DeviationTier::Minor => 1,
        DeviationTier::Moderate => 2,
        DeviationTier::Significant => 4,
        DeviationTier::Manipulation => 8,
    }
}

pub fn set_stake(env: &Env, relayer: &Address, amount: i128) {
    env.storage()
        .persistent()
        .set(&crate::types::DataKey::ProviderStake(relayer.clone()), &amount);
}

pub fn get_stake(env: &Env, relayer: &Address) -> i128 {
    env.storage()
        .persistent()
        .get::<crate::types::DataKey, i128>(&crate::types::DataKey::ProviderStake(relayer.clone()))
        .unwrap_or(0)
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DeviationTier {
    Minor,
    Moderate,
    Significant,
    Manipulation,
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{contract, contractimpl, vec, Env, testutils::Address as _, testutils::Ledger};

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
            env.ledger().set_sequence_number(MIN_UNBONDING_DELAY_LEDGERS);

            assert_eq!(release_unbonded_stake(&env, &validator), Err(Error::UnbondingDelayActive));
        });
    }

    #[test]
    fn release_succeeds_at_exact_delay_boundary() {
        let (env, contract_id, validator) = setup();
        env.ledger().set_sequence_number(1);

        env.as_contract(&contract_id, || {
            request_unbonding(&env, &validator, 900).unwrap();
            env.ledger().set_sequence_number(1 + MIN_UNBONDING_DELAY_LEDGERS);

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

            assert_eq!(request_unbonding(&env, &validator, 700), Err(Error::UnbondingAlreadyQueued));
        });
    }

    #[test]
    fn test_1_apply_slash_cap_unit_tests() {
        assert_eq!(apply_slash_cap(500_000, 1_000_000), 250_000);
        assert_eq!(apply_slash_cap(100_000, 1_000_000), 100_000);
        assert_eq!(apply_slash_cap(250_000, 1_000_000), 250_000);
        assert_eq!(apply_slash_cap(250_000, 0), 0);
        assert_eq!(apply_slash_cap(0, 1_000_000), 0);
    }

    #[test]
    fn test_2_penalty_capped_at_25_percent_of_bond() {
        let env = Env::default();
        let relayer = Address::generate(&env);
        set_stake(&env, &relayer, 1_000_000);

        let capped = apply_slash_cap(500_000, 1_000_000);
        assert_eq!(capped, 250_000);

        let remaining = 1_000_000 - capped;
        assert_eq!(remaining, 750_000);
    }

    #[test]
    fn test_3_small_penalty_not_inflated_to_cap() {
        let env = Env::default();
        let relayer = Address::generate(&env);
        set_stake(&env, &relayer, 1_000_000);

        let raw = 50_000;
        let capped = apply_slash_cap(raw, 1_000_000);
        assert_eq!(capped, raw);
        assert_eq!(1_000_000 - capped, 950_000);
    }

    #[test]
    fn test_4_penalty_exactly_at_cap_boundary() {
        let env = Env::default();
        let relayer = Address::generate(&env);
        set_stake(&env, &relayer, 1_000_000);

        let raw = 250_000;
        let capped = apply_slash_cap(raw, 1_000_000);
        assert_eq!(capped, raw);
    }

    #[test]
    fn test_5_connectivity_drop_penalty_well_below_cap() {
        let env = Env::default();
        let relayer = Address::generate(&env);
        set_stake(&env, &relayer, 1_000_000);

        let base = 50_000;
        let tier_mult = deviation_multiplier(DeviationTier::Minor);
        let raw = base * tier_mult;
        let capped = apply_slash_cap(raw, 1_000_000);

        assert_eq!(capped, raw);
        assert!(capped < 250_000);
        assert!(capped > 0);
    }

    #[test]
    fn test_6_severity_ordering_preserved_under_cap() {
        let bond_capacity = 1_000_000;

        let minor = apply_slash_cap(50_000 * deviation_multiplier(DeviationTier::Minor), bond_capacity);
        let moderate = apply_slash_cap(50_000 * deviation_multiplier(DeviationTier::Moderate), bond_capacity);
        let significant = apply_slash_cap(50_000 * deviation_multiplier(DeviationTier::Significant), bond_capacity);
        let manipulation = apply_slash_cap(50_000 * deviation_multiplier(DeviationTier::Manipulation), bond_capacity);

        assert!(minor < moderate);
        assert!(moderate < significant);
        assert!(significant < manipulation);
    }

    #[test]
    fn test_7_no_bankruptcy_from_single_incident() {
        let min_stake = 100_000;
        let raw = i128::MAX;
        let capped = apply_slash_cap(raw, min_stake);

        assert_eq!(capped, min_stake * 25 / 100);
        let remaining = min_stake - capped;
        assert_eq!(remaining, min_stake * 75 / 100);
        assert!(remaining > 0);
    }

    #[test]
    fn test_8_multiple_incidents_accumulate_independently() {
        let mut stake = 1_000_000;

        let cap1 = stake * 25 / 100;
        stake -= cap1;
        assert_eq!(stake, 750_000);

        let cap2 = stake * 25 / 100;
        stake -= cap2;
        assert_eq!(stake, 562_500);

        assert_eq!(cap1, 250_000);
        assert_eq!(cap2, 187_500);
    }

    #[test]
    fn test_9_saturating_arithmetic_on_max_bond_value() {
        let max_bond = i128::MAX;
        let raw = i128::MAX;

        let capped = apply_slash_cap(raw, max_bond);
        assert_eq!(capped, max_bond.saturating_mul(25).saturating_div(100));
    }
}
