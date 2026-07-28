use crate::ContractError;

/// Enforce maximum slippage tolerance on a swap output.
///
/// Compares the final calculated output amount (`amount_out`) against the
/// caller-specified minimum (`min_amount_out`). If the output falls below the
/// threshold the transaction is aborted with
/// [`ContractError::SlippageExceeded`], which reverts all state changes and
/// protects the user from sandwich attacks and adverse price movement.
///
/// # Arguments
/// * `amount_out` - The final output amount after fee deduction and pool math.
/// * `min_amount_out` - The caller's hard minimum acceptable payout.
///
/// # Returns
/// `Ok(amount_out)` if the check passes, allowing callers to chain the
/// validated value directly.
///
/// # Errors
/// * [`ContractError::SlippageExceeded`] — when `amount_out < min_amount_out`.
pub fn enforce_slippage(
    amount_out: u128,
    min_amount_out: u128,
) -> Result<u128, ContractError> {
    if amount_out < min_amount_out {
        return Err(ContractError::SlippageExceeded);
    }
    Ok(amount_out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_when_output_meets_minimum() {
        assert_eq!(enforce_slippage(100, 100), Ok(100));
    }

    #[test]
    fn passes_when_output_exceeds_minimum() {
        assert_eq!(enforce_slippage(200, 100), Ok(200));
    }

    #[test]
    fn fails_when_output_below_minimum() {
        assert_eq!(enforce_slippage(99, 100), Err(ContractError::SlippageExceeded));
    }

    #[test]
    fn fails_on_zero_output_with_nonzero_minimum() {
        assert_eq!(enforce_slippage(0, 1), Err(ContractError::SlippageExceeded));
    }

    #[test]
    fn passes_when_both_are_zero() {
        assert_eq!(enforce_slippage(0, 0), Ok(0));
    }

    #[test]
    fn large_values_enforced() {
        let large_out = u128::MAX;
        let large_min = u128::MAX;
        assert_eq!(enforce_slippage(large_out, large_min), Ok(large_out));
    }

    #[test]
    fn large_values_fail_when_below() {
        assert_eq!(
            enforce_slippage(u128::MAX - 1, u128::MAX),
            Err(ContractError::SlippageExceeded)
        );
    }
}
