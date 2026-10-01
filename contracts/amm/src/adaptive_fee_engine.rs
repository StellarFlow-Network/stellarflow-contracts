//! Adaptive Swap Fee Engine Based on Volatility Oracle (Issue #930).
//!
//! Adjusts AMM dynamic swap fees automatically during market volatility.
//! Queries volatility scalar Vsigma from live dynamic oracle feeds and computes:
//!
//!     fswap = fbase + (Vsigma * fscalar) constrained to fswap <= 0.01 (100 BPS)
//!
//! Pool swap executions apply the updated fee instantly within the current ledger.
//!
//! Time Complexity:
//! - Fee computation: O(1) checked arithmetic.
//! - Oracle query: O(1) cross-contract call or cached state read.
//!
//! Space Complexity:
//! - O(1) instance storage overhead.

use soroban_sdk::{contracttype, Address, Env, Symbol, Val, Vec};
use crate::AmmError;

/// Maximum fee cap: 0.01 (1% or 100 basis points).
pub const MAX_DYNAMIC_FEE_BPS: u32 = 100;

/// Basis points denominator: 10,000 BPS = 100%.
pub const BPS_DENOMINATOR: i128 = 10_000;

/// Configuration for the adaptive swap fee engine.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdaptiveFeeConfig {
    /// Base swap fee in basis points (e.g. 20 BPS = 0.20%), where 100 BPS = 0.01.
    pub f_base: u32,
    /// Volatility sensitivity scalar factor.
    pub f_scalar: u32,
}

/// Dynamic fee status record applied within a ledger.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppliedFeeSnapshot {
    /// The applied dynamic swap fee in basis points (<= 100 BPS, i.e. <= 0.01).
    pub f_swap: u32,
    /// Volatility scalar Vsigma queried from dynamic oracle feed.
    pub v_sigma: u32,
    /// Ledger sequence at which fee was evaluated and applied.
    pub ledger_sequence: u32,
}

/// Compute dynamic fee constrained to fswap <= 0.01 (100 BPS).
///
/// Formula:
///     fswap = min(fbase + (Vsigma * fscalar), 100 BPS)
pub fn compute_dynamic_fee(f_base: u32, v_sigma: u32, f_scalar: u32) -> Result<u32, AmmError> {
    if f_base > MAX_DYNAMIC_FEE_BPS {
        return Err(AmmError::ArithmeticOverflow);
    }

    let dynamic_component = (v_sigma as u64)
        .checked_mul(f_scalar as u64)
        .ok_or(AmmError::ArithmeticOverflow)?;

    let total_fee = (f_base as u64)
        .checked_add(dynamic_component)
        .ok_or(AmmError::ArithmeticOverflow)?;

    let capped_fee = total_fee.min(MAX_DYNAMIC_FEE_BPS as u64) as u32;
    Ok(capped_fee)
}

/// Query volatility scalar Vsigma from dynamic oracle feed.
pub fn query_volatility_scalar(
    env: &Env,
    oracle: &Address,
    asset_symbol: &Symbol,
) -> u32 {
    let args: Vec<Val> = soroban_sdk::vec![env, asset_symbol.to_val()];
    env.invoke_contract::<u32>(
        oracle,
        &Symbol::new(env, "get_asset_volatility_bps"),
        args,
    )
}

/// Calculate dynamic fee deduction from input amount.
pub fn apply_fee_to_amount_in(amount_in: i128, f_swap_bps: u32) -> Result<(i128, i128), AmmError> {
    if amount_in <= 0 {
        return Err(AmmError::NonPositiveAmount);
    }

    let fee_amount = amount_in
        .checked_mul(f_swap_bps as i128)
        .ok_or(AmmError::ArithmeticOverflow)?
        .checked_div(BPS_DENOMINATOR)
        .ok_or(AmmError::ArithmeticOverflow)?;

    let net_amount_in = amount_in
        .checked_sub(fee_amount)
        .ok_or(AmmError::ArithmeticOverflow)?;

    Ok((net_amount_in, fee_amount))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_dynamic_fee_baseline() {
        // Low volatility (Vsigma = 0) -> fswap = fbase
        let fee = compute_dynamic_fee(30, 0, 1).unwrap();
        assert_eq!(fee, 30); // 0.30%
    }

    #[test]
    fn test_compute_dynamic_fee_scaling() {
        // High volatility (Vsigma = 5, fscalar = 10) -> fswap = 30 + 50 = 80 BPS (0.80%)
        let fee = compute_dynamic_fee(30, 5, 10).unwrap();
        assert_eq!(fee, 80);
    }

    #[test]
    fn test_compute_dynamic_fee_capped_at_one_percent() {
        // Extreme volatility (Vsigma = 20, fscalar = 10) -> 30 + 200 = 230 BPS
        // Constrained to fswap <= 0.01 (100 BPS)
        let fee = compute_dynamic_fee(30, 20, 10).unwrap();
        assert_eq!(fee, MAX_DYNAMIC_FEE_BPS); // 100 BPS = 0.01
    }

    #[test]
    fn test_fee_deduction() {
        let amount_in = 10_000i128;
        let fee_bps = 50u32; // 0.50%
        let (net_in, fee) = apply_fee_to_amount_in(amount_in, fee_bps).unwrap();
        assert_eq!(fee, 50);
        assert_eq!(net_in, 9_950);
    }
}
