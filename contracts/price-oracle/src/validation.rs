/// Deviation threshold: 15% expressed in basis points (1500 bps).
const DEVIATION_THRESHOLD_BPS: i128 = 1500;

/// Compute the rolling baseline average from a slice of (timestamp, price) TWAP entries.
///
/// Returns `None` when the slice is empty.
fn baseline_average(entries: &soroban_sdk::Vec<(u64, i128)>) -> Option<i128> {
    let len = entries.len();
    if len == 0 {
        return None;
    }
    let mut sum: i128 = 0;
    for i in 0..len {
        let (_, price) = entries.get(i).unwrap();
        sum = sum.checked_add(price)?;
    }
    sum.checked_div(len as i128)
}

/// Returns `true` when `price` falls within ±15% of `baseline`.
///
/// Uses basis-point arithmetic to avoid floating-point:
///   deviation_bps = |price - baseline| * 10_000 / baseline
fn within_threshold(price: i128, baseline: i128) -> bool {
    if baseline <= 0 {
        return true; // no baseline yet — allow all
    }
    let delta = (price - baseline).unsigned_abs() as i128;
    // deviation_bps = delta * 10_000 / baseline
    match delta.checked_mul(10_000).and_then(|n| n.checked_div(baseline)) {
        Some(deviation_bps) => deviation_bps <= DEVIATION_THRESHOLD_BPS,
        None => false, // overflow means wildly out of range — reject
    }
}

/// Filter a set of validator feed submissions against the rolling baseline average.
///
/// Returns only the entries whose reported price falls within the ±15% variance
/// threshold. Feeds outside the threshold are silently dropped, preventing a
/// single compromised validator from skewing the consensus median.
///
/// If the TWAP buffer is empty (no baseline established yet), all entries are
/// kept so the oracle can bootstrap normally.
pub fn filter_feeds_by_deviation(
    twap_entries: &soroban_sdk::Vec<(u64, i128)>,
    feeds: soroban_sdk::Vec<crate::types::PriceBufferEntry>,
    env: &soroban_sdk::Env,
) -> soroban_sdk::Vec<crate::types::PriceBufferEntry> {
    let baseline = match baseline_average(twap_entries) {
        Some(b) => b,
        None => return feeds, // no history — pass all through
    };

    let mut accepted = soroban_sdk::Vec::new(env);
    for i in 0..feeds.len() {
        let entry = feeds.get(i).unwrap();
        if within_threshold(entry.price, baseline) {
            accepted.push_back(entry);
        }
    }
    accepted
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Address, Env};
    use crate::types::PriceBufferEntry;

    fn make_twap(env: &Env, prices: &[i128]) -> soroban_sdk::Vec<(u64, i128)> {
        let mut v = soroban_sdk::Vec::new(env);
        for (i, &p) in prices.iter().enumerate() {
            v.push_back((i as u64, p));
        }
        v
    }

    fn make_entry(env: &Env, price: i128) -> PriceBufferEntry {
        PriceBufferEntry {
            price,
            provider: Address::generate(env),
            timestamp: 0,
        }
    }

    #[test]
    fn passes_all_when_no_baseline() {
        let env = Env::default();
        let twap = soroban_sdk::Vec::new(&env);
        let mut feeds = soroban_sdk::Vec::new(&env);
        feeds.push_back(make_entry(&env, 999_999_999));
        feeds.push_back(make_entry(&env, 1));
        let result = filter_feeds_by_deviation(&twap, feeds.clone(), &env);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn keeps_feed_within_15_percent() {
        let env = Env::default();
        // baseline = 1_000_000_000
        let twap = make_twap(&env, &[1_000_000_000]);
        let mut feeds = soroban_sdk::Vec::new(&env);
        // exactly +15% => 1_150_000_000 — should be accepted
        feeds.push_back(make_entry(&env, 1_150_000_000));
        // exactly -15% => 850_000_000 — should be accepted
        feeds.push_back(make_entry(&env, 850_000_000));
        let result = filter_feeds_by_deviation(&twap, feeds, &env);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn drops_feed_outside_15_percent() {
        let env = Env::default();
        // baseline = 1_000_000_000
        let twap = make_twap(&env, &[1_000_000_000]);
        let mut feeds = soroban_sdk::Vec::new(&env);
        // +16% — should be dropped
        feeds.push_back(make_entry(&env, 1_160_000_000));
        // -16% — should be dropped
        feeds.push_back(make_entry(&env, 840_000_000));
        let result = filter_feeds_by_deviation(&twap, feeds, &env);
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn mixed_feeds_only_valid_kept() {
        let env = Env::default();
        let twap = make_twap(&env, &[1_000_000_000, 1_010_000_000, 990_000_000]);
        // baseline avg = (1_000_000_000 + 1_010_000_000 + 990_000_000) / 3 = 1_000_000_000
        let mut feeds = soroban_sdk::Vec::new(&env);
        feeds.push_back(make_entry(&env, 1_050_000_000)); // +5% — keep
        feeds.push_back(make_entry(&env, 1_200_000_000)); // +20% — drop
        feeds.push_back(make_entry(&env, 950_000_000));   // -5% — keep
        let result = filter_feeds_by_deviation(&twap, feeds, &env);
        assert_eq!(result.len(), 2);
    }
//! Liquidity volume validation module — flash loan manipulation prevention.
//!
//! Aggregating market prices from thinly backed liquidity channels can expose
//! downstream financial engines to flash loan price manipulations. This module
//! implements explicit liquidity volume validation checks that terminate
//! transaction paths early if a validator node's reported pool liquidity falls
//! below the configured minimum security threshold.
//!
//! # Security Model
//! 
//! Flash loan attacks exploit temporary price dislocations in low-liquidity pools.
//! By requiring minimum liquidity thresholds, we ensure that price submissions
//! come from markets with sufficient depth to resist manipulation.
//!
//! # Flow
//! 1. Admin sets liquidity threshold per asset via `set_liquidity_threshold`.
//! 2. Provider submits price + liquidity data via `update_price`.
//! 3. Contract validates liquidity meets threshold before accepting submission.
//! 4. Submissions below threshold are rejected with `LiquidityBelowThreshold` error.
//!
//! # Storage layout
//! | Key                                  | Type      | Description                                    |
//! |--------------------------------------|-----------|------------------------------------------------|
//! | `DataKey::LiquidityThreshold(Symbol)` | `i128`    | Minimum liquidity required per asset (stroops) |
//! | `DataKey::ProviderReportedLiquidity(Address, Symbol)` | `i128` | Last reported liquidity by provider for asset |
//! | `DataKey::LastLiquidityValidation(Symbol)` | `u64` | Timestamp of last successful validation |

use soroban_sdk::{Address, Env, Symbol, Vec};

use crate::types::{AssetWeight, DataKey};
use crate::ContractError;

/// Re-exported from lib.rs — maximum assets allowed in a single `clear_assets` batch.
const MAX_CLEAR_ASSETS: u32 = 20;

/// Minimum allowed liquidity threshold (1 XLM equivalent = 10_000_000 stroops).
/// Prevents admins from setting unreasonably low thresholds that defeat the purpose.
pub const MIN_LIQUIDITY_THRESHOLD: i128 = 10_000_000;

/// Maximum reasonable liquidity threshold (1 billion XLM equivalent).
/// Prevents accidental misconfiguration that would reject all submissions.
pub const MAX_LIQUIDITY_THRESHOLD: i128 = 1_000_000_000_0000000;

/// Multiplier for low-liquidity slash penalty (basis points).
/// Applied when provider submits prices from pools below the threshold.
pub const LOW_LIQUIDITY_SLASH_MULTIPLIER: i128 = 5;

// ─────────────────────────────────────────────────────────────────────────────
// Storage Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Read the minimum liquidity threshold for an asset.
/// Returns None if no threshold has been configured.
pub fn get_liquidity_threshold(env: &Env, asset: &Symbol) -> Option<i128> {
    env.storage()
        .persistent()
        .get(&DataKey::LiquidityThreshold(asset.clone()))
}

/// Set the minimum liquidity threshold for an asset.
/// Must be within MIN_LIQUIDITY_THRESHOLD..MAX_LIQUIDITY_THRESHOLD range.
fn set_liquidity_threshold(env: &Env, asset: &Symbol, threshold: i128) {
    env.storage()
        .persistent()
        .set(&DataKey::LiquidityThreshold(asset.clone()), &threshold);
}

/// Public admin-facing wrapper for setting the liquidity threshold.
/// Validates that the threshold is within the allowed range and persists it.
/// Returns `Err(ContractError::InvalidLiquidityThreshold)` if out of range.
pub fn set_liquidity_threshold_internal(
    env: &Env,
    asset: &Symbol,
    threshold: i128,
) -> Result<(), ContractError> {
    if threshold < MIN_LIQUIDITY_THRESHOLD || threshold > MAX_LIQUIDITY_THRESHOLD {
        return Err(ContractError::InvalidLiquidityThreshold);
    }
    set_liquidity_threshold(env, asset, threshold);
    Ok(())
}

/// Remove the liquidity threshold for an asset.
/// After removal, liquidity validation is disabled for this asset.
pub fn remove_liquidity_threshold_internal(env: &Env, asset: &Symbol) {
    env.storage()
        .persistent()
        .remove(&DataKey::LiquidityThreshold(asset.clone()));
}

/// Read the last reported liquidity from a specific provider for an asset.
/// Returns None if the provider has never reported liquidity for this asset.
pub fn get_provider_liquidity(env: &Env, provider: &Address, asset: &Symbol) -> Option<i128> {
    env.storage()
        .persistent()
        .get(&DataKey::ProviderReportedLiquidity(
            provider.clone(),
            asset.clone(),
        ))
}

/// Store the liquidity value reported by a provider for an asset.
fn set_provider_liquidity(env: &Env, provider: &Address, asset: &Symbol, liquidity: i128) {
    env.storage().persistent().set(
        &DataKey::ProviderReportedLiquidity(provider.clone(), asset.clone()),
        &liquidity,
    );
}

/// Record the timestamp of the last successful liquidity validation for an asset.
fn set_last_validation_timestamp(env: &Env, asset: &Symbol) {
    let timestamp = env.ledger().timestamp();
    env.storage()
        .persistent()
        .set(&DataKey::LastLiquidityValidation(asset.clone()), &timestamp);
}

/// Read the timestamp of the last successful liquidity validation for an asset.
pub fn get_last_validation_timestamp(env: &Env, asset: &Symbol) -> Option<u64> {
    env.storage()
        .persistent()
        .get(&DataKey::LastLiquidityValidation(asset.clone()))
}

/// Ensure the current consensus buffer contains at least three independent
/// provider sources before a price can be finalized.
///
/// This prevents accepting a finalized consensus price when the active input
/// pool has collapsed below the minimum safe participation threshold.
///
/// Returns `Err(ContractError::MinimumQuorumNotMet)` if fewer than 3 unique
/// node operators have submitted data points during the current cycle window.
pub fn validate_consensus_quorum(env: &Env, buffer: &crate::types::PriceBuffer) -> Result<(), ContractError> {
    let mut unique_sources = soroban_sdk::Map::new(env);

    for entry in buffer.entries.iter() {
        unique_sources.set(entry.provider.clone(), ());
    }

    if unique_sources.len() < 3 {
        return Err(ContractError::MinimumQuorumNotMet);
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Core Validation Logic
// ─────────────────────────────────────────────────────────────────────────────

/// Validate that reported pool liquidity meets the configured minimum threshold.
///
/// This function is called during `update_price` to ensure price submissions
/// come from sufficiently liquid markets that cannot be easily manipulated via
/// flash loans or other short-term capital injection attacks.
///
/// # Parameters
/// - `env`: Soroban environment
/// - `asset`: The asset pair being priced (e.g. "XLM/USD")
/// - `provider`: Address of the relayer submitting the price
/// - `reported_liquidity`: Total pool liquidity value reported by the provider (in stroops)
///
/// # Returns
/// - `Ok(())` if liquidity meets or exceeds the threshold, or no threshold is set
/// - `Err(ContractError::LiquidityBelowThreshold)` if liquidity is insufficient
/// - `Err(ContractError::InvalidLiquidity)` if reported liquidity is negative or zero
///
/// # Security Properties
/// 1. **Early termination**: Transaction is rejected before price enters buffer
/// 2. **Per-asset thresholds**: Different assets can have different liquidity requirements
/// 3. **Provider tracking**: Historical liquidity data enables reputation scoring
/// 4. **Audit trail**: Timestamps allow reconstruction of liquidity history
///
/// # Example
/// ```rust
/// // Admin sets 100M stroops minimum liquidity for XLM/USD
/// set_liquidity_threshold_internal(&env, &Symbol::new(&env, "XLM_USD"), 100_000_000);
///
/// // Provider attempts to submit price with 50M liquidity
/// let result = validate_liquidity(
///     &env,
///     &Symbol::new(&env, "XLM_USD"),
///     &provider_addr,
///     50_000_000
/// );
/// // Result: Err(ContractError::LiquidityBelowThreshold)
/// ```
pub fn validate_liquidity(
    env: &Env,
    asset: &Symbol,
    provider: &Address,
    reported_liquidity: i128,
) -> Result<(), ContractError> {
    // Reject negative or zero liquidity values
    if reported_liquidity <= 0 {
        return Err(ContractError::InvalidLiquidity);
    }

    // Check if a liquidity threshold has been configured for this asset
    let threshold = match get_liquidity_threshold(env, asset) {
        Some(t) => t,
        None => {
            // No threshold configured — validation passes by default.
            // This allows gradual rollout: assets without explicit thresholds
            // continue to accept all submissions until governance configures them.
            return Ok(());
        }
    };

    // Compare reported liquidity against the configured threshold
    if reported_liquidity < threshold {
        // Emit event for monitoring and alerting
        env.events().publish(
            (Symbol::new(env, "liquidity_violation"),),
            (
                asset.clone(),
                provider.clone(),
                reported_liquidity,
                threshold,
            ),
        );

        // Store the insufficient liquidity value for reputation tracking
        set_provider_liquidity(env, provider, asset, reported_liquidity);

        return Err(ContractError::LiquidityBelowThreshold);
    }

    // Validation passed — record the successful submission
    set_provider_liquidity(env, provider, asset, reported_liquidity);
    set_last_validation_timestamp(env, asset);

    // Emit success event for monitoring
    env.events().publish(
        (Symbol::new(env, "liquidity_validated"),),
        (
            asset.clone(),
            provider.clone(),
            reported_liquidity,
            threshold,
        ),
    );

    Ok(())
}

/// Compute the weighted index price from a borrowed basket of assets.
pub fn calculate_index_price(
    env: &Env,
    components: &Vec<AssetWeight>,
) -> Result<i128, ContractError> {
    if components.is_empty() {
        return Err(ContractError::AssetNotFound);
    }

    let mut total_weighted_price: i128 = 0;
    let mut total_weight: u32 = 0;

    for component in components.iter() {
        if !env
            .storage()
            .persistent()
            .has(&DataKey::TrackedAsset(component.asset.clone()))
        {
            return Err(ContractError::AssetNotFound);
        }

        if component.weight == 0 {
            return Err(ContractError::InvalidWeight);
        }

        let price_data = crate::PriceOracle::get_price(env.clone(), component.asset.clone(), true)?;
        let weight_i128: i128 = component.weight.into();
        let weighted_val = price_data
            .price
            .checked_mul(weight_i128)
            .ok_or(ContractError::InvalidPrice)?;

        total_weighted_price = total_weighted_price
            .checked_add(weighted_val)
            .ok_or(ContractError::InvalidPrice)?;

        total_weight = total_weight
            .checked_add(component.weight)
            .unwrap_or(total_weight);
    }

    if total_weight == 0 {
        return Err(ContractError::InvalidWeight);
    }

    total_weighted_price
        .checked_div(total_weight as i128)
        .ok_or(ContractError::PriceMathOverflow)
}

/// Remove a batch of price entries without copying the input vector.
pub fn clear_assets(env: &Env, assets: &Vec<Symbol>) -> Result<(), ContractError> {
    if assets.len() > MAX_CLEAR_ASSETS {
        return Err(ContractError::TooManyAssets);
    }

    let storage = env.storage().persistent();
    for asset in assets.iter() {
        storage.remove(&DataKey::Price(asset));
    }

    Ok(())
}
