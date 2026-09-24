//! Anti-spam dust transaction guard for StellarFlow payment corridors.
//!
//! Rejects micro-denominated deposits and swap amounts that fall below the
//! minimum economic threshold. Dust transactions waste ledger throughput and
//! are commonly used in spam attacks across payment corridors.
//!
//! # Usage
//!
//! Call [`check_min_transfer`] at the top of any deposit or swap handler:
//!
//! ```rust,ignore
//! use crate::validation::dust::check_min_transfer;
//!
//! pub fn deposit(env: Env, amount: u64) -> Result<(), ContractError> {
//!     check_min_transfer(amount)?;
//!     // … rest of deposit logic
//!     Ok(())
//! }
//! ```

use crate::ContractError;

/// Minimum transfer amount (in stroops) accepted by deposit and swap entry points.
///
/// Transactions carrying an amount below this threshold are rejected with
/// [`ContractError::AmountTooLow`] to preserve ledger throughput and block
/// spam across payment corridors.
///
/// Set to 10,000 stroops (0.001 XLM) — enough to cover base reserve costs
/// while being negligibly small for any legitimate cross-border remittance.
pub const MIN_TRANSFER_AMOUNT: u64 = 10_000;

/// Reject a transfer whose `amount` is below [`MIN_TRANSFER_AMOUNT`].
///
/// # Errors
///
/// Returns [`ContractError::AmountTooLow`] when `amount < MIN_TRANSFER_AMOUNT`.
///
/// # Examples
///
/// ```rust,ignore
/// assert!(check_min_transfer(10_000).is_ok());
/// assert_eq!(check_min_transfer(9_999), Err(ContractError::AmountTooLow));
/// assert_eq!(check_min_transfer(0),     Err(ContractError::AmountTooLow));
/// ```
pub fn check_min_transfer(amount: u64) -> Result<(), ContractError> {
    if amount < MIN_TRANSFER_AMOUNT {
        return Err(ContractError::AmountTooLow);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Rejection cases ──────────────────────────────────────────────────────

    #[test]
    fn rejects_zero_amount() {
        assert_eq!(check_min_transfer(0), Err(ContractError::AmountTooLow));
    }

    #[test]
    fn rejects_one_stroop() {
        assert_eq!(check_min_transfer(1), Err(ContractError::AmountTooLow));
    }

    #[test]
    fn rejects_one_below_threshold() {
        assert_eq!(
            check_min_transfer(MIN_TRANSFER_AMOUNT - 1),
            Err(ContractError::AmountTooLow)
        );
    }

    #[test]
    fn rejects_typical_dust_amount_100_stroops() {
        assert_eq!(check_min_transfer(100), Err(ContractError::AmountTooLow));
    }

    #[test]
    fn rejects_half_threshold() {
        assert_eq!(
            check_min_transfer(MIN_TRANSFER_AMOUNT / 2),
            Err(ContractError::AmountTooLow)
        );
    }

    // ── Acceptance cases ─────────────────────────────────────────────────────

    #[test]
    fn accepts_exactly_at_threshold() {
        assert!(check_min_transfer(MIN_TRANSFER_AMOUNT).is_ok());
    }

    #[test]
    fn accepts_one_above_threshold() {
        assert!(check_min_transfer(MIN_TRANSFER_AMOUNT + 1).is_ok());
    }

    #[test]
    fn accepts_typical_remittance_one_xlm() {
        // 1 XLM = 10_000_000 stroops — well above threshold.
        assert!(check_min_transfer(10_000_000).is_ok());
    }

    #[test]
    fn accepts_large_transfer() {
        assert!(check_min_transfer(u64::MAX).is_ok());
    }

    // ── Constant sanity ──────────────────────────────────────────────────────

    #[test]
    fn min_transfer_constant_is_ten_thousand() {
        assert_eq!(MIN_TRANSFER_AMOUNT, 10_000);
    }
}
