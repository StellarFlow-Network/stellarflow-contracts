//! Flash Loan Arbitrage Fee Multiplier Engine (Issue #902).
//!
//! Dynamically scales the protocol fee charged on flash loans based on the
//! total borrowed liquidity volume relative to the pool:
//!
//! ```text
//! f_fee = f_base + (L_borrowed / L_pool) × f_premium
//! ```
//!
//! All arithmetic is integer-only (basis points), so the fee scalar is fully
//! deterministic on-chain. The engine also exposes the repayment verification
//! gate required by the acceptance criteria — a flash loan is only considered
//! repaid when the returned balance covers the principal plus the scaled fee:
//!
//! ```text
//! B_return >= B_borrowed × (1 + f_fee)
//! ```
//!
//! When that threshold is unmet, [`assert_flash_repayment`] reverts execution
//! with [`ContractError::InsufficientFlashLoanRepayment`].
//!
//! ## Usage
//!
//! ```ignore
//! use crate::flash_fee_engine::{compute_flash_loan_fee, assert_flash_repayment};
//!
//! let quote = compute_flash_loan_fee(5, 20, 1_000, 100_000)?;  // fee_bps = 5
//! let _     = quote.fee_bps;
//! assert_flash_repayment(1_000, 1_005, quote.fee_bps)?;         // passes
//! ```
//!
//! Closes #902

use crate::ContractError;

// ─── Constants ───────────────────────────────────────────────────────────────

/// Basis-point denominator (10_000 bps == 100 %).
pub const BPS_DENOMINATOR: u128 = 10_000;

/// Default base flash-loan protocol fee: 0.05 % = 5 bps.
pub const FLASH_FEE_BASE_BPS: u32 = 5;

/// Default premium multiplier applied to the borrowed-liquidity ratio:
/// 0.20 % = 20 bps when the loan equals the entire pool.
pub const FLASH_FEE_PREMIUM_BPS: u32 = 20;

// ─── Types ───────────────────────────────────────────────────────────────────

/// A fully-quantified dynamic fee quote for a flash loan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlashLoanFeeQuote {
    /// Final protocol fee in basis points: `f_base + ratio × f_premium`.
    pub fee_bps: u32,
    /// Configured base fee in basis points.
    pub base_bps: u32,
    /// Configured premium fee in basis points.
    pub premium_bps: u32,
    /// Borrowed-liquidity ratio `L_borrowed / L_pool` expressed in basis
    /// points (10_000 bps == the loan is the entire pool; larger loans
    /// saturate there).
    pub borrowed_ratio_bps: u32,
}

// ─── Public API ──────────────────────────────────────────────────────────────

/// Compute the dynamic flash-loan fee scalar for a borrowed amount drawn
/// against a pool holding `pool` total liquidity.
///
/// The borrowed-liquidity ratio saturates at 100 % so a loan larger than the
/// whole pool never charges more than `f_base + f_premium`.
///
/// # Errors
///
/// - [`ContractError::InvalidArgument`] – `base_bps`/`premium_bps` above
///   10_000 bps, a negative `borrowed`, or a non-positive `pool`.
/// - [`ContractError::MathOverflow`]    – ratio scaling overflow.
pub fn compute_flash_loan_fee(
    base_bps: u32,
    premium_bps: u32,
    borrowed: i128,
    pool: i128,
) -> Result<FlashLoanFeeQuote, ContractError> {
    if base_bps > BPS_DENOMINATOR as u32 || premium_bps > BPS_DENOMINATOR as u32 {
        return Err(ContractError::InvalidArgument);
    }
    if borrowed < 0 || pool <= 0 {
        return Err(ContractError::InvalidArgument);
    }

    // ratio = min(L_borrowed / L_pool, 1), expressed in basis points.
    let borrowed_ratio_bps: u32 = if borrowed == 0 {
        0
    } else if borrowed >= pool {
        BPS_DENOMINATOR as u32
    } else {
        // Scale the ratio to basis points before dividing so a partial loan
        // keeps ~1 bp resolution (largest intermediate ~ 1e4 × L_borrowed).
        let scaled = (borrowed as u128)
            .checked_mul(BPS_DENOMINATOR)
            .ok_or(ContractError::MathOverflow)?;
        (scaled / (pool as u128)) as u32
    };

    // premium_term = (ratio_bps × f_premium) / 10_000
    let premium_term = (borrowed_ratio_bps as u128)
        .checked_mul(premium_bps as u128)
        .ok_or(ContractError::MathOverflow)?
        .checked_div(BPS_DENOMINATOR)
        .ok_or(ContractError::DivisionByZero)?;

    let fee_bps = (base_bps as u128)
        .checked_add(premium_term)
        .ok_or(ContractError::MathOverflow)?;

    Ok(FlashLoanFeeQuote {
        fee_bps: fee_bps as u32,
        base_bps,
        premium_bps,
        borrowed_ratio_bps,
    })
}

/// Minimum balance a borrower must return to clear a flash loan: the
/// principal plus the dynamic protocol fee, `B_borrowed × (1 + f_fee)`.
pub fn required_repayment(borrowed: i128, fee_bps: u32) -> Result<i128, ContractError> {
    if borrowed < 0 {
        return Err(ContractError::InvalidArgument);
    }
    let fee = (borrowed as u128)
        .checked_mul(fee_bps as u128)
        .ok_or(ContractError::MathOverflow)?
        .checked_div(BPS_DENOMINATOR)
        .ok_or(ContractError::DivisionByZero)?;
    Ok(borrowed + fee as i128)
}

/// Repayment verification: returns `true` when the returned balance covers
/// `B_borrowed × (1 + f_fee)`.
pub fn verify_flash_repayment(
    borrowed: i128,
    returned: i128,
    fee_bps: u32,
) -> Result<bool, ContractError> {
    let required = required_repayment(borrowed, fee_bps)?;
    Ok(returned >= required)
}

/// Assertion gate for flash-loan repayment verification.
///
/// Reverts with [`ContractError::InsufficientFlashLoanRepayment`] when the
/// returned balance does not clear the fee-adjusted threshold.
pub fn assert_flash_repayment(
    borrowed: i128,
    returned: i128,
    fee_bps: u32,
) -> Result<(), ContractError> {
    if verify_flash_repayment(borrowed, returned, fee_bps)? {
        Ok(())
    } else {
        Err(ContractError::InsufficientFlashLoanRepayment)
    }
}
// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContractError;

    #[test]
    fn zero_borrow_charges_only_base_fee() {
        let quote = compute_flash_loan_fee(FLASH_FEE_BASE_BPS, FLASH_FEE_PREMIUM_BPS, 0, 1_000)
            .expect("zero loan should quote");
        assert_eq!(quote.fee_bps, FLASH_FEE_BASE_BPS);
        assert_eq!(quote.borrowed_ratio_bps, 0);
    }

    #[test]
    fn full_pool_loan_charges_base_plus_full_premium() {
        let quote = compute_flash_loan_fee(FLASH_FEE_BASE_BPS, FLASH_FEE_PREMIUM_BPS, 100_000, 100_000)
            .expect("full-pool loan should quote");
        assert_eq!(quote.borrowed_ratio_bps, 10_000);
        assert_eq!(quote.fee_bps, FLASH_FEE_BASE_BPS + FLASH_FEE_PREMIUM_BPS);
    }

    #[test]
    fn oversized_loan_saturates_at_full_premium() {
        // A loan 10× the pool must not quote more than base + premium.
        let quote = compute_flash_loan_fee(FLASH_FEE_BASE_BPS, FLASH_FEE_PREMIUM_BPS, 1_000_000, 100_000)
            .expect("oversized loan should saturate");
        assert_eq!(quote.borrowed_ratio_bps, 10_000);
        assert_eq!(quote.fee_bps, 25);
    }

    #[test]
    fn half_pool_loan_charges_half_premium() {
        // ratio = 50% = 5_000 bps → premium_term = (5_000 × 20) / 10_000 = 10 bps.
        let quote = compute_flash_loan_fee(FLASH_FEE_BASE_BPS, FLASH_FEE_PREMIUM_BPS, 50_000, 100_000)
            .expect("half-pool loan should quote");
        assert_eq!(quote.borrowed_ratio_bps, 5_000);
        assert_eq!(quote.fee_bps, 15);
    }

    #[test]
    fn fee_bps_rounds_down_to_integer() {
        // ratio = 1% = 100 bps → premium_term = (100 × 20) / 10_000 = 0.2 → 0 bps.
        let quote = compute_flash_loan_fee(FLASH_FEE_BASE_BPS, FLASH_FEE_PREMIUM_BPS, 1_000, 100_000)
            .expect("small loan should quote");
        assert_eq!(quote.borrowed_ratio_bps, 100);
        assert_eq!(quote.fee_bps, FLASH_FEE_BASE_BPS);
    }

    #[test]
    fn invalid_arguments_rejected() {
        assert_eq!(
            compute_flash_loan_fee(10_001, 20, 1_000, 100_000),
            Err(ContractError::InvalidArgument)
        );
        assert_eq!(
            compute_flash_loan_fee(5, 20, -1, 100_000),
            Err(ContractError::InvalidArgument)
        );
        assert_eq!(
            compute_flash_loan_fee(5, 20, 1_000, 0),
            Err(ContractError::InvalidArgument)
        );
    }

    #[test]
    fn repayment_verification_passes_at_exact_threshold() {
        // 1_000 × (1 + 5/10_000) = 1_000 + 0.5 → floor = 1_000.
        assert_eq!(verify_flash_repayment(1_000, 1_000, 5), Ok(true));
        // 10_000 × (1 + 25/10_000) = 10_025.
        assert_eq!(verify_flash_repayment(10_000, 10_025, 25), Ok(true));
    }

    #[test]
    fn repayment_verification_rejects_shortfall() {
        assert_eq!(verify_flash_repayment(10_000, 10_024, 25), Ok(false));
        assert_eq!(verify_flash_repayment(10_000, 9_000, 25), Ok(false));
    }

    #[test]
    fn assert_repayment_errors_on_insufficient_return() {
        assert_eq!(
            assert_flash_repayment(10_000, 10_024, 25),
            Err(ContractError::InsufficientFlashLoanRepayment)
        );
        assert!(assert_flash_repayment(10_000, 10_025, 25).is_ok());
    }
}