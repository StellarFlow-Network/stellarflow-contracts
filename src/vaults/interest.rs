//! Vault debt-share index and borrow-rate controller.
//!
//! [`InterestRateController`] turns pool utilisation into an annualised borrow
//! rate in basis points and advances the debt-share index held in
//! [`PoolState`].
//!
//! # Fixed-point compound-interest precision helper
//!
//! The index is fixed point with 18 decimal places ([`INDEX_SCALE`]) and it
//! grows by *continuous compounding*:
//!
//! ```text
//! Index_t = Index_(t-1) * e^(r*dt)
//! ```
//!
//! `e^(r*dt)` is evaluated by [`exp_fixed_point`], an integer-only Taylor
//! series (`sum (r*dt)^n / n!`). Terms are rounded to nearest and the series
//! stops as soon as a term rounds to zero at 1e-18, so the result lands within
//! a few units of the 18th decimal place of the true exponential over the
//! whole admissible exponent range.
//!
//! The helper replaces the linear (simple-interest) factor the index used
//! before. Over a five-year idle period at 5% p.a. that approximation grew
//! the index by exactly 25%, where compounding grows it by 28.4025% — 272
//! basis points of interest the old math silently dropped, and the error gets
//! worse the longer the vault sits idle.
//!
//! Every step is checked arithmetic and the exponent is capped at
//! [`MAX_COMPOUND_EXPONENT`], so the helper can never overflow `i128` or
//! saturate silently: an out-of-range request is rejected instead.
//!
//! # Monotonicity
//!
//! The debt-share index is non-decreasing. Negative rates are rejected outright
//! and [`advance_debt_share_index`] re-checks `Index_t >= Index_(t-1)` after
//! every update, so rounding can never claw back interest that a previous
//! accrual already credited.

use soroban_sdk::{contracttype, Env};

use crate::ContractError;

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct InterestRateConfig {
    pub base_rate_bps: u32,
    pub multiplier_bps: u32,
    pub jump_multiplier_bps: u32,
    pub optimal_utilization_bps: u32,
    pub ledgers_per_year: u64,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct PoolState {
    pub cash: i128,
    pub borrows: i128,
    pub last_accrued_ledger: u32,
    pub accumulated_interest_index: u128,
}

/// 18-decimal fixed-point scale shared by the index and the rate helpers.
pub const INDEX_SCALE: i128 = 1_000_000_000_000_000_000;

/// Basis-point denominator: `10_000` bps == 100% per year.
pub const RATE_BPS_DENOMINATOR: i128 = 10_000;

/// Largest exponent `|r*dt|` the Taylor series accepts, in [`INDEX_SCALE`]
/// units: `4 * INDEX_SCALE` is `e^4 ~= 54.6`. Past it the largest intermediate
/// product of the series could leave `i128`, so requests above the cap are
/// rejected rather than truncated.
pub const MAX_COMPOUND_EXPONENT: i128 = 4 * INDEX_SCALE;

/// Safety cap on the number of Taylor terms.
pub const MAX_TAYLOR_TERMS: u32 = 64;

/// Evaluate `e^x` at 18 decimal places with an integer Taylor series.
///
/// `x` is fixed point in [`INDEX_SCALE`] units (`x == INDEX_SCALE` evaluates
/// `e^1`) and may be negative: `e^-|x|` is returned as the fixed-point
/// reciprocal of `e^|x|`, so the helper stays exact for negative exponents
/// without ever touching floating point.
///
/// Returns `None` when `|x|` exceeds [`MAX_COMPOUND_EXPONENT`] or when an
/// intermediate product would overflow `i128`. Callers are expected to surface
/// that as a rejection, never as a saturated result.
///
/// The series is truncated as soon as a term rounds to zero at 1e-18 (the
/// precision falloff point) or after [`MAX_TAYLOR_TERMS`] terms, whichever
/// comes first.
pub fn exp_fixed_point(x: i128) -> Option<i128> {
    if x == 0 {
        return Some(INDEX_SCALE);
    }

    let magnitude = x.checked_abs()?;
    if magnitude > MAX_COMPOUND_EXPONENT {
        return None;
    }

    let mut term = INDEX_SCALE;
    let mut sum = INDEX_SCALE;
    let mut n: i128 = 1;
    while n <= MAX_TAYLOR_TERMS as i128 {
        let denominator = INDEX_SCALE.checked_mul(n)?;
        // Round to nearest: truncating every term biases the whole series
        // downwards by several units of the least significant digit.
        let numerator = term.checked_mul(magnitude)?.checked_add(denominator / 2)?;
        term = numerator / denominator;
        if term == 0 {
            break;
        }
        sum = sum.checked_add(term)?;
        n += 1;
    }

    if x > 0 {
        Some(sum)
    } else {
        // e^-|x| = 1 / e^|x|, expressed in fixed point.
        INDEX_SCALE.checked_mul(INDEX_SCALE)?.checked_div(sum)
    }
}

/// Compound growth factor `e^(r*dt)` scaled by [`INDEX_SCALE`].
///
/// * `rate_bps` — annualised borrow rate in basis points (`10_000` bps ==
///   100% p.a.).
/// * `elapsed_ledgers` — `dt`, the ledgers since the previous accrual.
/// * `ledgers_per_year` — converts ledgers into years.
///
/// A zero rate or a zero `dt` returns the neutral factor [`INDEX_SCALE`]:
/// accruing over no time, or at no rate, changes nothing.
///
/// # Errors
/// * `ContractError::DivisionByZero` — `ledgers_per_year` is zero, so `dt`
///   cannot be expressed in years.
/// * `ContractError::InvalidInput` — `rate_bps` is negative; a shrinking index
///   would break the non-decreasing invariant.
/// * `ContractError::MathOverflow` — `r*dt` is outside the exponent range the
///   Taylor series can evaluate safely.
pub fn compound_interest_factor(
    rate_bps: i128,
    elapsed_ledgers: u64,
    ledgers_per_year: u64,
) -> Result<i128, ContractError> {
    if ledgers_per_year == 0 {
        return Err(ContractError::DivisionByZero);
    }
    if rate_bps < 0 {
        return Err(ContractError::InvalidInput);
    }
    if rate_bps == 0 || elapsed_ledgers == 0 {
        return Ok(INDEX_SCALE);
    }

    let numerator = rate_bps
        .checked_mul(i128::from(elapsed_ledgers))
        .ok_or(ContractError::MathOverflow)?
        .checked_mul(INDEX_SCALE)
        .ok_or(ContractError::MathOverflow)?;
    let denominator = RATE_BPS_DENOMINATOR
        .checked_mul(i128::from(ledgers_per_year))
        .ok_or(ContractError::MathOverflow)?;
    let exponent = numerator / denominator;

    if exponent == 0 {
        // Shorter than 1e-18 of a year: below the index's own resolution.
        return Ok(INDEX_SCALE);
    }

    exp_fixed_point(exponent).ok_or(ContractError::MathOverflow)
}

/// Apply one compounding period to a debt-share index.
///
/// `Index_t = Index_(t-1) * e^(r*dt)`, evaluated with
/// [`compound_interest_factor`] and floored back to the index's 1e-18
/// resolution.
///
/// The non-decreasing invariant `Index_t >= Index_(t-1)` is re-checked after
/// the update; it holds by construction because the factor is at least
/// [`INDEX_SCALE`] for every rate this helper accepts, and the check keeps it
/// true if the factor calculation ever changes.
///
/// # Errors
/// Propagates the rejections of [`compound_interest_factor`], plus
/// `ContractError::MathOverflow` when the scaled multiplication leaves `u128`
/// and `ContractError::InvalidInput` if the update would decrease the index.
pub fn advance_debt_share_index(
    index: u128,
    rate_bps: i128,
    elapsed_ledgers: u64,
    ledgers_per_year: u64,
) -> Result<u128, ContractError> {
    let factor = compound_interest_factor(rate_bps, elapsed_ledgers, ledgers_per_year)?;
    let factor = u128::try_from(factor).map_err(|_| ContractError::MathOverflow)?;

    let updated = index
        .checked_mul(factor)
        .ok_or(ContractError::MathOverflow)?
        / INDEX_SCALE as u128;

    if updated < index {
        return Err(ContractError::InvalidInput);
    }
    Ok(updated)
}

pub struct InterestRateController;

impl InterestRateController {
    pub fn calculate_utilization(cash: i128, borrows: i128) -> u32 {
        let total_assets = cash + borrows;
        if total_assets == 0 {
            return 0;
        }
        ((borrows * 10_000) / total_assets) as u32
    }

    pub fn calculate_interest_rate(utilization: u32, config: &InterestRateConfig) -> u32 {
        if utilization <= config.optimal_utilization_bps {
            let slope = (utilization as u64 * config.multiplier_bps as u64) / 10_000;
            config.base_rate_bps + slope as u32
        } else {
            let base_slope = (config.optimal_utilization_bps as u64 * config.multiplier_bps as u64) / 10_000;
            let excess_utilization = utilization - config.optimal_utilization_bps;
            let excess_slope = (excess_utilization as u64 * config.jump_multiplier_bps as u64) / 10_000;
            config.base_rate_bps + base_slope as u32 + excess_slope as u32
        }
    }

    /// Advance the pool's debt-share index to the current ledger and return the
    /// interest accrued on `pool.borrows` since the previous accrual.
    ///
    /// Growth is compounded through [`advance_debt_share_index`], so an
    /// extended idle period — a five-year ledger gap, say — accrues exactly the
    /// interest the same period would accrue if it were settled ledger by
    /// ledger, instead of the under-counted linear approximation.
    ///
    /// Returns `0` without touching the index when no ledger has elapsed since
    /// `pool.last_accrued_ledger`, or when no shares are outstanding
    /// (`accumulated_interest_index == 0`).
    ///
    /// # Errors
    /// Propagates the helper's rejections — a negative rate, an exponent past
    /// [`MAX_COMPOUND_EXPONENT`], a zero `ledgers_per_year` — and
    /// `ContractError::MathOverflow` if the borrow-side multiplication leaves
    /// `i128`.
    pub fn accrue_interest(
        env: &Env,
        pool: &mut PoolState,
        config: &InterestRateConfig,
    ) -> Result<i128, ContractError> {
        let current_ledger = env.ledger().sequence();
        if current_ledger <= pool.last_accrued_ledger {
            return Ok(0);
        }

        let elapsed_ledgers = (current_ledger - pool.last_accrued_ledger) as u64;
        let utilization = Self::calculate_utilization(pool.cash, pool.borrows);
        let rate_bps = Self::calculate_interest_rate(utilization, config);

        let stored_index = pool.accumulated_interest_index;
        let new_index = advance_debt_share_index(
            stored_index,
            i128::from(rate_bps),
            elapsed_ledgers,
            config.ledgers_per_year,
        )?;

        // `advance_debt_share_index` guarantees `new_index >= stored_index`.
        let index_delta = new_index - stored_index;
        let interest_accrued = if index_delta == 0 {
            0
        } else {
            let delta_i128 =
                i128::try_from(index_delta).map_err(|_| ContractError::MathOverflow)?;
            let index_i128 =
                i128::try_from(stored_index).map_err(|_| ContractError::MathOverflow)?;
            pool.borrows
                .checked_mul(delta_i128)
                .ok_or(ContractError::MathOverflow)?
                .checked_div(index_i128)
                .ok_or(ContractError::DivisionByZero)?
        };

        if interest_accrued < 0 {
            return Err(ContractError::InvalidInput);
        }

        pool.borrows = pool
            .borrows
            .checked_add(interest_accrued)
            .ok_or(ContractError::MathOverflow)?;
        pool.accumulated_interest_index = new_index;
        pool.last_accrued_ledger = current_ledger;

        Ok(interest_accrued)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Ledger;

    /// Five-second ledgers, the assumption the rest of the vaults module makes.
    const LEDGERS_PER_YEAR: u64 = 6_307_200;
    const FIVE_YEARS: u64 = 5 * LEDGERS_PER_YEAR;

    /// Move the test ledger to `sequence` using the get -> mutate -> set
    /// pattern the sibling vault modules use: rewriting a whole `LedgerInfo`
    /// would reset the entry-TTL fields of everything already written.
    fn set_ledger_sequence(env: &Env, sequence: u32) {
        let mut info = env.ledger().get();
        info.sequence_number = sequence;
        env.ledger().set(info);
    }

    fn config() -> InterestRateConfig {
        InterestRateConfig {
            base_rate_bps: 500,
            multiplier_bps: 0,
            jump_multiplier_bps: 0,
            optimal_utilization_bps: 8_000,
            ledgers_per_year: LEDGERS_PER_YEAR,
        }
    }

    fn pool() -> PoolState {
        PoolState {
            cash: 0,
            borrows: 1_000_000_000,
            last_accrued_ledger: 1_000_000,
            accumulated_interest_index: INDEX_SCALE as u128,
        }
    }

    /// Independent reference for `e^x` scaled by [`INDEX_SCALE`]: halve the
    /// argument until the series is cheap, sum it, then square the result back
    /// up. Deliberately a different algorithm from the single-shot series
    /// under test; exact to a couple of units of the 18th decimal place for
    /// `|x| <= 1`.
    fn reference_exp_fixed_point(x: i128) -> i128 {
        let mut reduced = x;
        let mut squarings = 0u32;
        while reduced.abs() > INDEX_SCALE / 8 {
            reduced /= 2;
            squarings += 1;
        }

        let mut term = INDEX_SCALE;
        let mut total = INDEX_SCALE;
        let mut n: i128 = 1;
        while n <= 32 {
            let denominator = INDEX_SCALE * n;
            term = (term * reduced + denominator / 2) / denominator;
            if term == 0 {
                break;
            }
            total += term;
            n += 1;
        }

        for _ in 0..squarings {
            total = total * total / INDEX_SCALE;
        }
        total
    }

    // -- the precision helper ------------------------------------------------

    #[test]
    fn exp_helper_is_accurate_to_eighteen_decimal_places() {
        // e = 2.718281828459045235..., e^(1/2) = 1.648721270700128146...,
        // e^(-1/4) = 0.778800783071404868...
        assert!((exp_fixed_point(INDEX_SCALE).unwrap() - 2_718_281_828_459_045_235).abs() <= 3);
        assert!((exp_fixed_point(INDEX_SCALE / 2).unwrap() - 1_648_721_270_700_128_146).abs() <= 3);
        assert!((exp_fixed_point(-INDEX_SCALE / 4).unwrap() - 778_800_783_071_404_868).abs() <= 3);
        assert_eq!(exp_fixed_point(0), Some(INDEX_SCALE));
    }

    #[test]
    fn five_year_ledger_gap_compounds_without_precision_loss() {
        // 5% p.a. over five years: r*dt = 0.25 exactly, so the factor is
        // e^(1/4) = 1.2840254166877414844... and the helper reproduces the
        // 18-decimal truncation exactly.
        let factor = compound_interest_factor(500, FIVE_YEARS, LEDGERS_PER_YEAR).unwrap();
        assert_eq!(factor, 1_284_025_416_687_741_484);

        // Recomputed in-test with integer arithmetic, via the independent
        // halve-and-square reference.
        let reference = reference_exp_fixed_point(INDEX_SCALE / 4);
        assert!(
            (factor - reference).abs() <= 4,
            "factor {factor} vs reference {reference}"
        );

        // The linear approximation this index used before would report exactly
        // 1.25: compounding adds 272 bps of index growth the old math dropped.
        assert!(factor > 1_250_000_000_000_000_000);
    }

    #[test]
    fn twenty_percent_five_year_gap_matches_e() {
        // 20% p.a. over five years: r*dt = 1, so the factor is e itself.
        let factor = compound_interest_factor(2_000, FIVE_YEARS, LEDGERS_PER_YEAR).unwrap();
        assert!(
            (factor - 2_718_281_828_459_045_235).abs() <= 3,
            "factor {factor}"
        );
    }

    // -- monotonicity --------------------------------------------------------

    #[test]
    fn index_is_monotonic_over_repeated_updates() {
        let mut index = INDEX_SCALE as u128;
        for quarter in 0..20 {
            let previous = index;
            index = advance_debt_share_index(
                index,
                500,
                LEDGERS_PER_YEAR / 4,
                LEDGERS_PER_YEAR,
            )
            .unwrap();
            assert!(index >= previous, "index regressed at step {quarter}");
            assert!(
                index > previous,
                "quarterly compounding added no growth at step {quarter}"
            );
        }

        // Twenty quarterly compounds of r*dt = 0.0125 reproduce the single-
        // shot five-year factor to within the rounding of each step.
        let one_shot =
            compound_interest_factor(500, FIVE_YEARS, LEDGERS_PER_YEAR).unwrap() as u128;
        let drift = index.abs_diff(one_shot);
        assert!(
            drift <= 1_000,
            "drift {drift} exceeds the 20 roundings at 1e-18"
        );
    }

    #[test]
    fn index_never_regresses_when_an_accrual_is_replayed() {
        let env = Env::default();
        set_ledger_sequence(&env, 1_000_000 + FIVE_YEARS as u32);

        let config = config();
        let mut pool = pool();
        let before = pool.accumulated_interest_index;

        InterestRateController::accrue_interest(&env, &mut pool, &config).unwrap();
        let after = pool.accumulated_interest_index;
        assert!(after >= before);

        // Same ledger again: sealed, so nothing accrues and nothing moves.
        assert_eq!(
            InterestRateController::accrue_interest(&env, &mut pool, &config),
            Ok(0)
        );
        assert_eq!(pool.accumulated_interest_index, after);
    }

    // -- zero-rate / zero-dt boundaries --------------------------------------

    #[test]
    fn zero_elapsed_ledgers_is_a_no_op() {
        assert_eq!(
            compound_interest_factor(500, 0, LEDGERS_PER_YEAR),
            Ok(INDEX_SCALE)
        );
        let index = 3 * INDEX_SCALE as u128;
        assert_eq!(
            advance_debt_share_index(index, 500, 0, LEDGERS_PER_YEAR),
            Ok(index)
        );
    }

    #[test]
    fn zero_rate_is_a_no_op() {
        assert_eq!(
            compound_interest_factor(0, FIVE_YEARS, LEDGERS_PER_YEAR),
            Ok(INDEX_SCALE)
        );
        let index = 7 * INDEX_SCALE as u128;
        assert_eq!(
            advance_debt_share_index(index, 0, FIVE_YEARS, LEDGERS_PER_YEAR),
            Ok(index)
        );
    }

    // -- rejections ----------------------------------------------------------

    #[test]
    fn negative_rate_is_rejected() {
        assert_eq!(
            compound_interest_factor(-1, FIVE_YEARS, LEDGERS_PER_YEAR),
            Err(ContractError::InvalidInput)
        );
        assert_eq!(
            advance_debt_share_index(INDEX_SCALE as u128, -500, FIVE_YEARS, LEDGERS_PER_YEAR),
            Err(ContractError::InvalidInput)
        );
    }

    #[test]
    fn exponent_beyond_the_cap_is_rejected() {
        // 100% p.a. for ten years: r*dt = 10, past MAX_COMPOUND_EXPONENT (4).
        assert_eq!(
            compound_interest_factor(10_000, 10 * FIVE_YEARS, LEDGERS_PER_YEAR),
            Err(ContractError::MathOverflow)
        );
        // The largest admitted exponent still evaluates.
        assert!(exp_fixed_point(MAX_COMPOUND_EXPONENT).is_some());
        assert_eq!(exp_fixed_point(MAX_COMPOUND_EXPONENT + 1), None);
    }

    #[test]
    fn zero_ledgers_per_year_is_rejected() {
        assert_eq!(
            compound_interest_factor(500, FIVE_YEARS, 0),
            Err(ContractError::DivisionByZero)
        );
    }

    // -- ledger-driven accrual ----------------------------------------------

    #[test]
    fn five_year_ledger_gap_accrues_the_compounded_amount() {
        let env = Env::default();
        set_ledger_sequence(&env, 1_000_000);
        let config = config();
        let mut pool = pool();

        // No ledger has elapsed: nothing accrues, no state moves.
        assert_eq!(
            InterestRateController::accrue_interest(&env, &mut pool, &config),
            Ok(0)
        );
        assert_eq!(pool.accumulated_interest_index, INDEX_SCALE as u128);

        set_ledger_sequence(&env, 1_000_000 + FIVE_YEARS as u32);
        let accrued = InterestRateController::accrue_interest(&env, &mut pool, &config).unwrap();

        let expected_index = 1_284_025_416_687_741_484i128;
        assert!(
            (pool.accumulated_interest_index as i128 - expected_index).abs() <= 3,
            "index {}",
            pool.accumulated_interest_index
        );
        // 1_000_000_000 * 0.284025416687741484 = 284_025_416 (floored).
        assert_eq!(accrued, 284_025_416);
        assert_eq!(pool.borrows, 1_284_025_416);
        assert_eq!(pool.last_accrued_ledger, 1_000_000 + FIVE_YEARS as u32);
    }

    #[test]
    fn empty_index_accrual_is_a_no_op() {
        let env = Env::default();
        set_ledger_sequence(&env, 1_000_000 + FIVE_YEARS as u32);
        let config = config();
        let mut pool = pool();
        pool.accumulated_interest_index = 0;

        // A pool with no shares outstanding cannot accrue anything, and the
        // borrow-side division by the index must be skipped rather than panic.
        assert_eq!(
            InterestRateController::accrue_interest(&env, &mut pool, &config),
            Ok(0)
        );
        assert_eq!(pool.accumulated_interest_index, 0);
        assert_eq!(pool.borrows, 1_000_000_000);
        assert_eq!(pool.last_accrued_ledger, 1_000_000 + FIVE_YEARS as u32);
    }
}
