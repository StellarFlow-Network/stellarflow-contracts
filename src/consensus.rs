use soroban_sdk::{contracttype, symbol_short, Address, Env, Map, Symbol, Vec};
use crate::ContractError;

/// Basis-point denominator used when converting a BPS fraction to a multiplier.
pub const BPS_DENOMINATOR: u64 = 10_000;

/// A single provider's submission paired with its consensus weight (stake amount).
#[contracttype]
#[derive(Clone)]
pub struct WeightedEntry {
    /// Raw submitted value (e.g. price in smallest denomination).
    pub value: u64,
    /// Weight assigned to this entry, typically the provider's staked amount.
    pub weight: u64,
}

/// Multiply a raw value by a weight, returning `Overflow` on saturation.
///
/// This is the inner kernel called for each entry in `compute_weighted_sum`.
pub fn apply_weight(value: u64, weight: u64) -> Result<u64, ContractError> {
    value.checked_mul(weight).ok_or(ContractError::Overflow)
}

/// Accumulate the sum of `entry.value * entry.weight` across every entry in the
/// dataset.  Each individual product and every running-total addition is checked
/// so no intermediate result can wrap silently.
pub fn compact_duplicate_price_rows(env: &Env, entries: &Vec<WeightedEntry>) -> Result<Vec<WeightedEntry>, ContractError> {
    let mut compacted: Vec<WeightedEntry> = Vec::new(env);
    let mut index_by_value: Map<u64, u64> = Map::new(env);

    for i in 0..entries.len() {
        let entry = entries.get(i).unwrap();

        if let Some(existing_index) = index_by_value.get(entry.value) {
            let idx = existing_index as u32;
            let existing = compacted.get(idx).unwrap();
            let merged_weight = existing
                .weight
                .checked_add(entry.weight)
                .ok_or(ContractError::Overflow)?;

            compacted.set(
                idx,
                WeightedEntry {
                    value: existing.value,
                    weight: merged_weight,
                },
            );
        } else {
            let index = compacted.len() as u64;
            compacted.push_back(entry.clone());
            index_by_value.set(entry.value, index);
        }
    }

    Ok(compacted)
}

pub fn compute_weighted_sum(env: &Env, entries: &Vec<WeightedEntry>) -> Result<(u64, u64), ContractError> {
    let compacted = compact_duplicate_price_rows(env, entries)?;
    let mut weighted_sum: u64 = 0;
    let mut total_weight: u64 = 0;

    for i in 0..compacted.len() {
        let entry = compacted.get(i).unwrap();

        let weighted_value = apply_weight(entry.value, entry.weight)?;

        weighted_sum = weighted_sum
            .checked_add(weighted_value)
            .ok_or(ContractError::Overflow)?;

        total_weight = total_weight
            .checked_add(entry.weight)
            .ok_or(ContractError::Overflow)?;
    }

    Ok((weighted_sum, total_weight))
}

/// Compute the stake-weighted average across all entries.
///
/// Returns `(weighted_average, total_weight)`.  Division is always safe once
/// the checked accumulation above has succeeded, but we guard the zero-weight
/// edge case to avoid a panic.
pub fn compute_weighted_average(env: &Env, entries: &Vec<WeightedEntry>) -> Result<u64, ContractError> {
    let (weighted_sum, total_weight) = compute_weighted_sum(env, entries)?;

    if total_weight == 0 {
        return Ok(0);
    }

    Ok(weighted_sum / total_weight)
}

/// Compute the minimum weight required for quorum.
///
/// `quorum_bps` is expressed in basis points (e.g. 6700 = 67 %).
/// The multiplication `total_weight * quorum_bps` is checked before the
/// denominator division so large stake totals cannot overflow silently.
pub fn compute_quorum_threshold(total_weight: u64, quorum_bps: u64) -> Result<u64, ContractError> {
    let numerator = total_weight
        .checked_mul(quorum_bps)
        .ok_or(ContractError::Overflow)?;

    Ok(numerator / BPS_DENOMINATOR)
}

/// Scale a raw consensus score by a fixed precision multiplier.
///
/// Used when promoting an integer score to a higher-precision representation
/// before further computation.  Both the score itself and the scale factor are
/// checked to prevent rollover.
pub fn normalize_weight_score(raw_score: u64, precision: u64) -> Result<u64, ContractError> {
    raw_score
        .checked_mul(precision)
        .ok_or(ContractError::Overflow)
}

/// Compute how much of the accumulated weighted score a single entry
/// contributes, expressed in basis points of the total.
///
/// Returns a value in [0, 10 000].  The intermediate `entry_weight * BPS_DENOMINATOR`
/// product is checked before the final division.
pub fn entry_weight_share_bps(entry_weight: u64, total_weight: u64) -> Result<u64, ContractError> {
    if total_weight == 0 {
        return Ok(0);
    }

    let numerator = entry_weight
        .checked_mul(BPS_DENOMINATOR)
        .ok_or(ContractError::Overflow)?;

    Ok(numerator / total_weight)
}

/// Result type for price retrieval.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PriceResult {
    Live(i64),            // Live price from oracle
    Fallback(i64, u32),   // Historical backup price and safety warning code
}

/// Safety warning code returned when the live oracle feed is offline.
pub const WARNING_ORACLE_OFFLINE: u32 = 1001;

/// Retrieves the price for a given asset symbol with a graceful fallback.
///
/// Returns `PriceResult::Live` if the oracle call succeeds, otherwise `PriceResult::Fallback`
/// and emits a warning event.
pub fn get_price_with_fallback(env: &Env, asset: Symbol, fallback_rate: i64) -> PriceResult {
    let oracle_result = mock_oracle_price(env, asset.clone());
    match oracle_result {
        Ok(price) => PriceResult::Live(price),
        Err(_) => {
            // Emit a warning event for observability.
            env.events().publish(
                (symbol_short!("FallbackW"), asset),
                (fallback_rate, WARNING_ORACLE_OFFLINE),
            );
            PriceResult::Fallback(fallback_rate, WARNING_ORACLE_OFFLINE)
        }
    }
}

/// Mock function representing the external oracle price lookup.
/// Uses temporary storage to allow tests to configure success/failure paths.
pub fn mock_oracle_price(env: &Env, _asset: Symbol) -> Result<i64, ContractError> {
    let key = symbol_short!("mock_prc");
    if env.storage().temporary().has(&key) {
        let val: i64 = env.storage().temporary().get(&key).unwrap();
        if val >= 0 {
            Ok(val)
        } else {
            Err(ContractError::NotRegistered)
        }
    } else {
        Err(ContractError::NotRegistered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::Env;
    use soroban_sdk::testutils::Address as _;

    fn make_entries(env: &Env, pairs: &[(u64, u64)]) -> Vec<WeightedEntry> {
        let mut v = Vec::new(env);
        for &(value, weight) in pairs {
            v.push_back(WeightedEntry { value, weight });
        }
        v
    }

    // --- apply_weight ---

    #[test]
    fn test_apply_weight_normal() {
        assert_eq!(apply_weight(100, 50).unwrap(), 5_000);
    }

    #[test]
    fn test_apply_weight_zero_value() {
        assert_eq!(apply_weight(0, u64::MAX).unwrap(), 0);
    }

    #[test]
    fn test_apply_weight_zero_weight() {
        assert_eq!(apply_weight(u64::MAX, 0).unwrap(), 0);
    }

    #[test]
    fn test_apply_weight_overflow() {
        let result = apply_weight(u64::MAX, 2);
        assert_eq!(result, Err(ContractError::Overflow));
    }

    // --- compute_weighted_sum ---

    #[test]
    fn test_weighted_sum_single_entry() {
        let env = Env::default();
        let entries = make_entries(&env, &[(200, 3)]);
        let (ws, tw) = compute_weighted_sum(&env, &entries).unwrap();
        assert_eq!(ws, 600);
        assert_eq!(tw, 3);
    }

    #[test]
    fn test_weighted_sum_multiple_entries() {
        let env = Env::default();
        // (100 * 10) + (200 * 5) = 1000 + 1000 = 2000, total_weight = 15
        let entries = make_entries(&env, &[(100, 10), (200, 5)]);
        let (ws, tw) = compute_weighted_sum(&env, &entries).unwrap();
        assert_eq!(ws, 2_000);
        assert_eq!(tw, 15);
    }

    #[test]
    fn test_weighted_sum_duplicate_price_rows_compact() {
        let env = Env::default();
        // Same price value appears twice; weights should merge before weighted sum.
        // 100*(10+5) + 200*5 = 1500 + 1000 = 2500
        let entries = make_entries(&env, &[(100, 10), (100, 5), (200, 5)]);
        let (ws, tw) = compute_weighted_sum(&env, &entries).unwrap();
        assert_eq!(ws, 2_500);
        assert_eq!(tw, 20);
    }

    #[test]
    fn test_weighted_sum_empty_dataset() {
        let env = Env::default();
        let entries = make_entries(&env, &[]);
        let (ws, tw) = compute_weighted_sum(&env, &entries).unwrap();
        assert_eq!(ws, 0);
        assert_eq!(tw, 0);
    }

    #[test]
    fn test_weighted_sum_overflow_on_product() {
        let env = Env::default();
        let entries = make_entries(&env, &[(u64::MAX, 2)]);
        let result = compute_weighted_sum(&env, &entries);
        assert_eq!(result, Err(ContractError::Overflow));
    }

    #[test]
    fn test_weighted_sum_overflow_on_accumulation() {
        let env = Env::default();
        // Two entries that are individually fine but their sum overflows u64.
        let half = u64::MAX / 2;
        let entries = make_entries(&env, &[(half, 2), (half, 2)]);
        // half*2 = u64::MAX-1, second half*2 would overflow the running sum
        // u64::MAX - 1 + (u64::MAX - 1) overflows
        let result = compute_weighted_sum(&env, &entries);
        assert_eq!(result, Err(ContractError::Overflow));
    }

    // --- compute_weighted_average ---

    #[test]
    fn test_weighted_average_normal() {
        let env = Env::default();
        // (1000 * 3 + 2000 * 1) / (3 + 1) = 5000 / 4 = 1250
        let entries = make_entries(&env, &[(1_000, 3), (2_000, 1)]);
        assert_eq!(compute_weighted_average(&env, &entries).unwrap(), 1_250);
    }

    #[test]
    fn test_weighted_average_zero_total_weight() {
        let env = Env::default();
        let entries = make_entries(&env, &[(500, 0), (300, 0)]);
        assert_eq!(compute_weighted_average(&env, &entries).unwrap(), 0);
    }

    // --- compute_quorum_threshold ---

    #[test]
    fn test_quorum_threshold_two_thirds() {
        // 6700 BPS of 1_000_000 = 670_000
        assert_eq!(compute_quorum_threshold(1_000_000, 6_700).unwrap(), 670_000);
    }

    #[test]
    fn test_quorum_threshold_fifty_percent() {
        assert_eq!(compute_quorum_threshold(200, 5_000).unwrap(), 100);
    }

    #[test]
    fn test_quorum_threshold_overflow() {
        // u64::MAX * 2 overflows even before dividing
        let result = compute_quorum_threshold(u64::MAX, 2);
        assert_eq!(result, Err(ContractError::Overflow));
    }

    #[test]
    fn test_quorum_threshold_zero_weight() {
        assert_eq!(compute_quorum_threshold(0, 6_700).unwrap(), 0);
    }

    // --- normalize_weight_score ---

    #[test]
    fn test_normalize_score_normal() {
        assert_eq!(normalize_weight_score(42, 1_000).unwrap(), 42_000);
    }

    #[test]
    fn test_normalize_score_overflow() {
        let result = normalize_weight_score(u64::MAX, 2);
        assert_eq!(result, Err(ContractError::Overflow));
    }

    #[test]
    fn test_normalize_score_zero() {
        assert_eq!(normalize_weight_score(0, u64::MAX).unwrap(), 0);
    }

    // --- entry_weight_share_bps ---

    #[test]
    fn test_share_bps_full_weight() {
        // Entry holds all the weight → 10 000 BPS
        assert_eq!(entry_weight_share_bps(500, 500).unwrap(), 10_000);
    }

    #[test]
    fn test_share_bps_half_weight() {
        assert_eq!(entry_weight_share_bps(250, 500).unwrap(), 5_000);
    }

    #[test]
    fn test_share_bps_zero_total() {
        assert_eq!(entry_weight_share_bps(100, 0).unwrap(), 0);
    }

    #[test]
    fn test_share_bps_overflow_on_numerator() {
        let result = entry_weight_share_bps(u64::MAX, 1);
        assert_eq!(result, Err(ContractError::Overflow));
    }

    // --- get_price_with_fallback tests ---

    #[test]
    fn test_get_price_with_fallback_success() {
        let env = Env::default();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);

        env.as_contract(&contract_id, || {
            let asset = symbol_short!("BTC");
            // Configure the mock price to return 50000
            env.storage().temporary().set(&symbol_short!("mock_prc"), &50000i64);

            let result = get_price_with_fallback(&env, asset, 45000);
            assert_eq!(result, PriceResult::Live(50000));
        });
    }

    #[test]
    fn test_get_price_with_fallback_failure() {
        let env = Env::default();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);

        env.as_contract(&contract_id, || {
            let asset = symbol_short!("BTC");
            // No mock price configured (or set to negative to trigger failure)
            env.storage().temporary().set(&symbol_short!("mock_prc"), &-1i64);

            let result = get_price_with_fallback(&env, asset, 45000);
            assert_eq!(result, PriceResult::Fallback(45000, WARNING_ORACLE_OFFLINE));
        });
    }

    #[test]
    fn test_get_price_with_fallback_failure_emits_event() {
        use soroban_sdk::testutils::Events;
        let env = Env::default();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);

        env.as_contract(&contract_id, || {
            let asset = symbol_short!("BTC");

            let result = get_price_with_fallback(&env, asset.clone(), 45000);
            assert_eq!(result, PriceResult::Fallback(45000, WARNING_ORACLE_OFFLINE));

            let events = env.events().all();
            assert!(events.len() > 0);
        });
    }
}
