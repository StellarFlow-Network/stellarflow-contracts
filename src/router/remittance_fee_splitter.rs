//! Cross-border remittance fee splitting router.
//!
//! Dynamically splits protocol fees between liquidity providers, anchor relayers,
//! and protocol treasury based on configurable percentage allocations stored in
//! contract storage. Emits structured events on every payout for transparency
//! and auditability.
//!
//! # Fee Distribution Flow
//!
//! 1. Cross-border transfer generates protocol fees
//! 2. Fees are routed through this splitter contract
//! 3. Configurable percentages determine allocation:
//!    - Liquidity Providers: Compensates pool liquidity contributors
//!    - Anchor Relayers: Compensates network relayers/anchors
//!    - Protocol Treasury: Protocol revenue and sustainability
//! 4. RemittanceFeesRouted event emitted with detailed breakdown
//!
//! # Storage Schema
//!
//! Fee split configurations are stored per-asset to support different
//! fee structures for different currency corridors.

use soroban_sdk::{contracttype, symbol_short, Address, Bytes, Env, Map, Vec};

use crate::events::{emit_event, EV_REMITTANCE_FEES_ROUTED};
use crate::{AssetId, ContractError, TimeLockedUpgradeContract};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum fee split percentage (100% = 10000 basis points)
pub const MAX_FEE_BPS: u32 = 10_000;

/// Default fee split if not configured
const DEFAULT_LIQUIDITY_PROVIDER_BPS: u32 = 6_000; // 60%
const DEFAULT_ANCHOR_RELAYER_BPS: u32 = 3_000;    // 30%
const DEFAULT_TREASURY_BPS: u32 = 1_000;           // 10%

// ---------------------------------------------------------------------------
// Storage Keys
// ---------------------------------------------------------------------------

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum RemittanceFeeStorageKey {
    /// Fee split configuration for a specific asset
    FeeSplitConfig(AssetId),
    /// Total fees routed for tracking and analytics
    TotalFeesRouted(AssetId),
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Fee split configuration for a specific asset/corridor.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct FeeSplitConfig {
    /// Asset this configuration applies to
    pub asset: AssetId,
    /// Percentage allocated to liquidity providers (in basis points)
    pub liquidity_provider_bps: u32,
    /// Percentage allocated to anchor relayers (in basis points)
    pub anchor_relayer_bps: u32,
    /// Percentage allocated to protocol treasury (in basis points)
    pub treasury_bps: u32,
}

/// Result of a fee distribution operation.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct FeeDistributionResult {
    /// Total fee amount that was distributed
    pub total_fee: u64,
    /// Amount allocated to liquidity providers
    pub liquidity_provider_amount: u64,
    /// Amount allocated to anchor relayers
    pub anchor_relayer_amount: u64,
    /// Amount allocated to protocol treasury
    pub treasury_amount: u64,
    /// Asset the fees were denominated in
    pub asset: AssetId,
}

/// Event data emitted when fees are routed.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct RemittanceFeesRoutedEvent {
    /// Transfer/transaction ID for correlation
    pub transfer_id: Bytes,
    /// Asset the fees were denominated in
    pub asset: AssetId,
    /// Total fee amount routed
    pub total_fee: u64,
    /// Amount allocated to liquidity providers
    pub liquidity_provider_amount: u64,
    /// Percentage allocated to liquidity providers (basis points)
    pub liquidity_provider_bps: u32,
    /// Amount allocated to anchor relayers
    pub anchor_relayer_amount: u64,
    /// Percentage allocated to anchor relayers (basis points)
    pub anchor_relayer_bps: u32,
    /// Amount allocated to protocol treasury
    pub treasury_amount: u64,
    /// Percentage allocated to protocol treasury (basis points)
    pub treasury_bps: u32,
    /// Timestamp when fees were routed
    pub timestamp: u64,
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

impl FeeSplitConfig {
    /// Create a new fee split configuration with validation.
    pub fn new(
        asset: AssetId,
        liquidity_provider_bps: u32,
        anchor_relayer_bps: u32,
        treasury_bps: u32,
    ) -> Result<Self, ContractError> {
        // Validate that percentages sum to exactly 100%
        let total_bps = liquidity_provider_bps
            .checked_add(anchor_relayer_bps)
            .ok_or(ContractError::MathOverflow)?
            .checked_add(treasury_bps)
            .ok_or(ContractError::MathOverflow)?;

        if total_bps != MAX_FEE_BPS {
            return Err(ContractError::InvalidFeeSplitConfig);
        }

        // Validate individual percentages are within bounds
        if liquidity_provider_bps > MAX_FEE_BPS
            || anchor_relayer_bps > MAX_FEE_BPS
            || treasury_bps > MAX_FEE_BPS
        {
            return Err(ContractError::InvalidFeeSplitConfig);
        }

        Ok(Self {
            asset,
            liquidity_provider_bps,
            anchor_relayer_bps,
            treasury_bps,
        })
    }

    /// Get default configuration for an asset.
    pub fn default(asset: AssetId) -> Self {
        Self {
            asset,
            liquidity_provider_bps: DEFAULT_LIQUIDITY_PROVIDER_BPS,
            anchor_relayer_bps: DEFAULT_ANCHOR_RELAYER_BPS,
            treasury_bps: DEFAULT_TREASURY_BPS,
        }
    }
}

impl FeeDistributionResult {
    /// Create a new fee distribution result.
    pub fn new(
        total_fee: u64,
        liquidity_provider_amount: u64,
        anchor_relayer_amount: u64,
        treasury_amount: u64,
        asset: AssetId,
    ) -> Self {
        Self {
            total_fee,
            liquidity_provider_amount,
            anchor_relayer_amount,
            treasury_amount,
            asset,
        }
    }

    /// Validate that the distribution sums to the total fee.
    pub fn validate(&self) -> Result<(), ContractError> {
        let distributed = self
            .liquidity_provider_amount
            .checked_add(self.anchor_relayer_amount)
            .ok_or(ContractError::MathOverflow)?
            .checked_add(self.treasury_amount)
            .ok_or(ContractError::MathOverflow)?;

        if distributed != self.total_fee {
            return Err(ContractError::FeeDistributionMismatch);
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Admin Functions
// ---------------------------------------------------------------------------

/// Set or update the fee split configuration for an asset.
///
/// Only contract administrators can modify fee split configurations.
/// This allows for dynamic adjustment of fee distribution based on
/// market conditions, corridor performance, or protocol governance decisions.
pub fn set_fee_split_config(
    env: Env,
    admin: Address,
    asset: AssetId,
    liquidity_provider_bps: u32,
    anchor_relayer_bps: u32,
    treasury_bps: u32,
) -> Result<FeeSplitConfig, ContractError> {
    admin.require_auth();
    
    let data = TimeLockedUpgradeContract::load_data(&env)?;
    if data.admin != admin {
        return Err(ContractError::NotAdmin);
    }

    let config = FeeSplitConfig::new(asset, liquidity_provider_bps, anchor_relayer_bps, treasury_bps)?;
    
    let key = RemittanceFeeStorageKey::FeeSplitConfig(asset);
    env.storage().instance().set(&key, &config);
    
    Ok(config)
}

/// Get the fee split configuration for an asset.
///
/// Returns the configured split if it exists, otherwise returns
/// the default configuration.
pub fn get_fee_split_config(env: &Env, asset: AssetId) -> FeeSplitConfig {
    let key = RemittanceFeeStorageKey::FeeSplitConfig(asset);
    env.storage()
        .instance()
        .get(&key)
        .unwrap_or(FeeSplitConfig::default(asset))
}

// ---------------------------------------------------------------------------
// Fee Distribution Functions
// ---------------------------------------------------------------------------

/// Distribute fees according to the configured split and emit event.
///
/// This is the main entry point for fee routing. It:
/// 1. Retrieves the fee split configuration for the asset
/// 2. Calculates the allocation for each recipient
/// 3. Emits a RemittanceFeesRouted event for transparency
/// 4. Updates total fees routed tracking
///
/// # Arguments
/// * `env` - Soroban environment
/// * `transfer_id` - Unique identifier for the transfer (for event correlation)
/// * `asset` - Asset the fees are denominated in
/// * `total_fee` - Total fee amount to distribute
///
/// # Returns
/// The fee distribution result with allocated amounts
pub fn distribute_fees(
    env: &Env,
    transfer_id: Bytes,
    asset: AssetId,
    total_fee: u64,
) -> Result<FeeDistributionResult, ContractError> {
    if total_fee == 0 {
        return Ok(FeeDistributionResult::new(0, 0, 0, 0, asset));
    }

    let config = get_fee_split_config(env, asset);
    
    // Calculate allocations using high-precision arithmetic to avoid rounding errors
    let liquidity_provider_amount = calculate_fee_share(total_fee, config.liquidity_provider_bps)?;
    let anchor_relayer_amount = calculate_fee_share(total_fee, config.anchor_relayer_bps)?;
    
    // Treasury gets the remainder to ensure exact total allocation
    let treasury_amount = total_fee
        .checked_sub(liquidity_provider_amount)
        .ok_or(ContractError::MathOverflow)?
        .checked_sub(anchor_relayer_amount)
        .ok_or(ContractError::MathOverflow)?;

    let result = FeeDistributionResult::new(
        total_fee,
        liquidity_provider_amount,
        anchor_relayer_amount,
        treasury_amount,
        asset,
    );

    // Validate distribution matches total
    result.validate()?;

    // Update total fees routed tracking
    update_total_fees_routed(env, asset, total_fee);

    // Emit structured event
    emit_remittance_fees_routed_event(
        env,
        transfer_id,
        asset,
        &result,
        &config,
    )?;

    Ok(result)
}

/// Calculate fee share from total amount using basis points.
///
/// Uses interior scaling to maintain precision during division.
fn calculate_fee_share(total_fee: u64, bps: u32) -> Result<u64, ContractError> {
    if bps == 0 {
        return Ok(0);
    }

    // (total_fee * bps) / 10000
    let interior_product = u128::from(total_fee)
        .checked_mul(u128::from(bps))
        .ok_or(ContractError::Overflow)?;

    let share = interior_product
        .checked_div(u128::from(MAX_FEE_BPS))
        .ok_or(ContractError::DivisionByZero)?;

    u64::try_from(share).map_err(|_| ContractError::Overflow)
}

/// Update the total fees routed counter for an asset.
fn update_total_fees_routed(env: &Env, asset: AssetId, fee_amount: u64) {
    let key = RemittanceFeeStorageKey::TotalFeesRouted(asset);
    let mut total = env
        .storage()
        .instance()
        .get::<_, u64>(&key)
        .unwrap_or(0);

    total = total.saturating_add(fee_amount);
    env.storage().instance().set(&key, &total);
}

/// Get the total fees routed for an asset.
pub fn get_total_fees_routed(env: &Env, asset: AssetId) -> u64 {
    let key = RemittanceFeeStorageKey::TotalFeesRouted(asset);
    env.storage()
        .instance()
        .get(&key)
        .unwrap_or(0)
}

/// Emit the RemittanceFeesRouted event.
fn emit_remittance_fees_routed_event(
    env: &Env,
    transfer_id: Bytes<32>,
    asset: AssetId,
    result: &FeeDistributionResult,
    config: &FeeSplitConfig,
) -> Result<(), ContractError> {
    let event_data = RemittanceFeesRoutedEvent {
        transfer_id,
        asset,
        total_fee: result.total_fee,
        liquidity_provider_amount: result.liquidity_provider_amount,
        liquidity_provider_bps: config.liquidity_provider_bps,
        anchor_relayer_amount: result.anchor_relayer_amount,
        anchor_relayer_bps: config.anchor_relayer_bps,
        treasury_amount: result.treasury_amount,
        treasury_bps: config.treasury_bps,
        timestamp: env.ledger().timestamp(),
    };

    let asset_symbol = crate::asset_id_to_symbol(asset);
    
    emit_event(env, EV_REMITTANCE_FEES_ROUTED, &[&asset_symbol], event_data)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TimeLockedUpgradeContractClient;
    use soroban_sdk::testutils::Address as _;

    fn setup() -> (Env, TimeLockedUpgradeContractClient<'static>, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
        let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        client.initialize(&admin, &treasury);
        (env, client, admin)
    }

    #[test]
    fn fee_split_config_validates_percentage_sum() {
        let asset = 3897123275; // NGN

        // Valid configuration (sums to 100%)
        let valid_config = FeeSplitConfig::new(asset, 6000, 3000, 1000);
        assert!(valid_config.is_ok());

        // Invalid configuration (doesn't sum to 100%)
        let invalid_config = FeeSplitConfig::new(asset, 5000, 3000, 1000);
        assert_eq!(invalid_config, Err(ContractError::InvalidFeeSplitConfig));

        // Invalid configuration (exceeds 100%)
        let invalid_config2 = FeeSplitConfig::new(asset, 7000, 4000, 1000);
        assert_eq!(invalid_config2, Err(ContractError::InvalidFeeSplitConfig));
    }

    #[test]
    fn fee_split_config_validates_individual_percentages() {
        let asset = 3897123275;

        // Individual percentage exceeds 100%
        let invalid_config = FeeSplitConfig::new(asset, 15000, 0, 0);
        assert_eq!(invalid_config, Err(ContractError::InvalidFeeSplitConfig));
    }

    #[test]
    fn default_fee_split_config() {
        let asset = 3897123275;
        let config = FeeSplitConfig::default(asset);

        assert_eq!(config.liquidity_provider_bps, 6000);
        assert_eq!(config.anchor_relayer_bps, 3000);
        assert_eq!(config.treasury_bps, 1000);
    }

    #[test]
    fn calculate_fee_share_with_basis_points() {
        let total_fee = 1000u64;

        // 60% should be 600
        let share_60 = calculate_fee_share(total_fee, 6000).unwrap();
        assert_eq!(share_60, 600);

        // 30% should be 300
        let share_30 = calculate_fee_share(total_fee, 3000).unwrap();
        assert_eq!(share_30, 300);

        // 10% should be 100
        let share_10 = calculate_fee_share(total_fee, 1000).unwrap();
        assert_eq!(share_10, 100);

        // 0% should be 0
        let share_0 = calculate_fee_share(total_fee, 0).unwrap();
        assert_eq!(share_0, 0);
    }

    #[test]
    fn fee_distribution_result_validates_total() {
        let asset = 3897123275;
        
        // Valid distribution (sums to total)
        let valid_result = FeeDistributionResult::new(1000, 600, 300, 100, asset);
        assert!(valid_result.validate().is_ok());

        // Invalid distribution (doesn't sum to total)
        let invalid_result = FeeDistributionResult::new(1000, 500, 300, 100, asset);
        assert_eq!(invalid_result.validate(), Err(ContractError::FeeDistributionMismatch));
    }

    #[test]
    fn distribute_fees_uses_configured_split() {
        let env = Env::default();
        let asset = 3897123275;
        let transfer_id = Bytes::from_slice(&env, &[1u8; 32]);

        // Set custom configuration
        let key = RemittanceFeeStorageKey::FeeSplitConfig(asset);
        let custom_config = FeeSplitConfig::new(asset, 5000, 4000, 1000).unwrap();
        env.storage().instance().set(&key, &custom_config);

        let total_fee = 1000u64;
        let result = distribute_fees(&env, transfer_id, asset, total_fee).unwrap();

        assert_eq!(result.total_fee, 1000);
        assert_eq!(result.liquidity_provider_amount, 500); // 50%
        assert_eq!(result.anchor_relayer_amount, 400);    // 40%
        assert_eq!(result.treasury_amount, 100);          // 10%
    }

    #[test]
    fn distribute_fees_uses_default_split_when_not_configured() {
        let env = Env::default();
        let asset = 3897123275;
        let transfer_id = Bytes::from_slice(&env, &[1u8; 32]);

        let total_fee = 1000u64;
        let result = distribute_fees(&env, transfer_id, asset, total_fee).unwrap();

        assert_eq!(result.total_fee, 1000);
        assert_eq!(result.liquidity_provider_amount, 600); // 60% default
        assert_eq!(result.anchor_relayer_amount, 300);    // 30% default
        assert_eq!(result.treasury_amount, 100);          // 10% default
    }

    #[test]
    fn distribute_fees_handles_zero_fee() {
        let env = Env::default();
        let asset = 3897123275;
        let transfer_id = Bytes::from_slice(&env, &[1u8; 32]);

        let result = distribute_fees(&env, transfer_id, asset, 0).unwrap();

        assert_eq!(result.total_fee, 0);
        assert_eq!(result.liquidity_provider_amount, 0);
        assert_eq!(result.anchor_relayer_amount, 0);
        assert_eq!(result.treasury_amount, 0);
    }

    #[test]
    fn distribute_fees_tracks_total_routed() {
        let env = Env::default();
        let asset = 3897123275;
        let transfer_id = Bytes::from_slice(&env, &[1u8; 32]);

        // First distribution
        distribute_fees(&env, transfer_id.clone(), asset, 1000).unwrap();
        assert_eq!(get_total_fees_routed(&env, asset), 1000);

        // Second distribution
        let transfer_id2 = Bytes::from_slice(&env, &[2u8; 32]);
        distribute_fees(&env, transfer_id2, asset, 500).unwrap();
        assert_eq!(get_total_fees_routed(&env, asset), 1500);
    }

    #[test]
    fn distribute_fees_emits_event() {
        let env = Env::default();
        let asset = 3897123275;
        let transfer_id = Bytes::from_slice(&env, &[1u8; 32]);

        let result = distribute_fees(&env, transfer_id.clone(), asset, 1000).unwrap();
        
        // Verify event was emitted by checking the events
        let events = env.events().all();
        assert!(!events.is_empty());
    }

    #[test]
    fn fee_distribution_handles_remainder_allocation() {
        let env = Env::default();
        let asset = 3897123275;
        let transfer_id = Bytes::from_slice(&env, &[1u8; 32]);

        // Use a configuration that might produce remainders
        let key = RemittanceFeeStorageKey::FeeSplitConfig(asset);
        let config = FeeSplitConfig::new(asset, 3333, 3333, 3334).unwrap();
        env.storage().instance().set(&key, &config);

        let total_fee = 1000u64;
        let result = distribute_fees(&env, transfer_id, asset, total_fee).unwrap();

        // Verify the distribution sums to total exactly
        let distributed = result.liquidity_provider_amount
            + result.anchor_relayer_amount
            + result.treasury_amount;
        assert_eq!(distributed, total_fee);
    }
}
