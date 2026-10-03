#![no_std]
//! # Dynamic Variable Interest Rate Model for Borrow Vaults (Issue #942)
//!
//! A two-slope ("kink") variable borrow rate curve driven by pool utilization.
//!
//! ## The model
//!
//! Given total outstanding debt `D` and total supplied collateral `C`, the
//! pool utilization is
//!
//! ```text
//! U = D / C
//! ```
//!
//! and the variable borrow rate follows the classic kinked curve, with all
//! rates and utilization expressed in basis points (`BPS = 10_000`):
//!
//! ```text
//! U <= U_optimal:  r = r_base + (U / U_optimal) * slope1
//! U >  U_optimal:  r = r_base + slope1 + ((U - U_optimal) / (1 - U_optimal)) * slope2
//! ```
//!
//! The two branches meet exactly at the kink: at `U = U_optimal` the first
//! branch yields `r_base + slope1`, which is also the intercept of the second,
//! so the curve is continuous. Below the kink (`slope1`) borrowing gets
//! gently more expensive as liquidity thins; above the kink (`slope2`,
//! typically much steeper) the rate spikes to ration the remaining liquidity
//! and pull suppliers back in.
//!
//! ## Edge cases
//!
//! * An empty pool (`D = C = 0`) reports `U = 0` and therefore the base rate.
//! * Debt with zero backing collateral (`C = 0, D > 0`) reports full (`100%`)
//!   utilization — economically the pool has nothing left to lend.
//! * Utilization above 100% (`D > C`, an undercollateralized pool) is
//!   representable and keeps pricing on the steep branch, punitively so.
//! * `U_optimal` is constrained to `(0, BPS)` at configuration time so both
//!   divisors in the formulas are provably non-zero.
//!
//! All intermediate products run in `u128`; anything that still does not fit
//! in a `u32` basis-point value is reported as [`ModelError::Overflow`]
//! rather than wrapping.

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, Address, Env, Symbol,
};

/// Basis-point denominator: `10_000 bps == 100%`.
pub const BPS: u32 = 10_000;

/// Topic emitted when the rate model parameters are installed or updated.
pub const MODEL_UPDATED: &str = "RateModelUpdated";

/// Errors returned by the interest rate module.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ModelError {
    /// `initialize` has already been called.
    AlreadyInitialized = 1,
    /// The contract has not been initialised yet.
    NotInitialized = 2,
    /// The caller is not the registered admin.
    NotAdmin = 3,
    /// `optimal_utilization_bps` is not strictly inside `(0, 10_000)`.
    InvalidOptimalUtilization = 4,
    /// A debt or collateral amount was negative.
    NegativeAmount = 5,
    /// An intermediate or final value exceeded the representable range.
    Overflow = 6,
}

/// Tunable parameters of the kinked rate curve, all in basis points.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RateModel {
    /// Rate charged at zero utilization (`r_base`).
    pub base_rate_bps: u32,
    /// Utilization kink (`U_optimal`): gentle slope below, steep slope above.
    pub optimal_utilization_bps: u32,
    /// Gentle slope applied below the kink (`r_slope1`).
    pub slope1_bps: u32,
    /// Steep slope applied above the kink (`r_slope2`).
    pub slope2_bps: u32,
}

/// Storage keys.
#[contracttype]
pub enum DataKey {
    /// Address allowed to install and update the rate model.
    Admin,
    /// Active [`RateModel`] parameters.
    Model,
}

#[contract]
pub struct InterestRateModel;

/// Pool utilization in basis points: `U = D * BPS / C`.
///
/// * `D = C = 0` (empty pool) → `0`.
/// * `C = 0, D > 0` (nothing backing the debt) → `BPS` (fully utilized).
/// * `D > C` is representable and yields `U > BPS`.
pub fn utilization_rate(total_debt: i128, total_collateral: i128) -> Result<u32, ModelError> {
    if total_debt < 0 || total_collateral < 0 {
        return Err(ModelError::NegativeAmount);
    }
    if total_collateral == 0 {
        return Ok(if total_debt == 0 { 0 } else { BPS });
    }
    let utilization = (total_debt as u128)
        .checked_mul(BPS as u128)
        .ok_or(ModelError::Overflow)?
        .checked_div(total_collateral as u128)
        .ok_or(ModelError::Overflow)?;
    u32::try_from(utilization).map_err(|_| ModelError::Overflow)
}

/// Variable borrow rate in basis points for a given utilization.
///
/// Implements the two issue formulas exactly, with `u128` intermediates and
/// floor division (rounding favours the protocol, as borrowers pay the rate).
pub fn borrow_rate(utilization_bps: u32, model: &RateModel) -> Result<u32, ModelError> {
    validate_model(model)?;
    let base = model.base_rate_bps as u128;
    let rate = if utilization_bps <= model.optimal_utilization_bps {
        let scaled = (utilization_bps as u128)
            .checked_mul(model.slope1_bps as u128)
            .ok_or(ModelError::Overflow)?
            .checked_div(model.optimal_utilization_bps as u128)
            .ok_or(ModelError::Overflow)?;
        base.checked_add(scaled).ok_or(ModelError::Overflow)?
    } else {
        let excess = (utilization_bps - model.optimal_utilization_bps) as u128;
        let steep = excess
            .checked_mul(model.slope2_bps as u128)
            .ok_or(ModelError::Overflow)?
            .checked_div((BPS - model.optimal_utilization_bps) as u128)
            .ok_or(ModelError::Overflow)?;
        base.checked_add(model.slope1_bps as u128)
            .ok_or(ModelError::Overflow)?
            .checked_add(steep)
            .ok_or(ModelError::Overflow)?
    };
    u32::try_from(rate).map_err(|_| ModelError::Overflow)
}

/// Validate model parameters: the kink must sit strictly inside `(0, BPS)`
/// so both formula divisors are non-zero.
pub fn validate_model(model: &RateModel) -> Result<(), ModelError> {
    if model.optimal_utilization_bps == 0 || model.optimal_utilization_bps >= BPS {
        return Err(ModelError::InvalidOptimalUtilization);
    }
    Ok(())
}

fn load_admin(env: &Env) -> Result<Address, ModelError> {
    env.storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(ModelError::NotInitialized)
}

fn load_model(env: &Env) -> Result<RateModel, ModelError> {
    env.storage()
        .instance()
        .get(&DataKey::Model)
        .ok_or(ModelError::NotInitialized)
}

#[contractimpl]
impl InterestRateModel {
    /// Deploy-time setup. Installs the initial kinked curve parameters.
    pub fn initialize(
        env: Env,
        admin: Address,
        base_rate_bps: u32,
        optimal_utilization_bps: u32,
        slope1_bps: u32,
        slope2_bps: u32,
    ) -> Result<(), ModelError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(ModelError::AlreadyInitialized);
        }
        let model = RateModel {
            base_rate_bps,
            optimal_utilization_bps,
            slope1_bps,
            slope2_bps,
        };
        validate_model(&model)?;
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Model, &model);
        env.events().publish(
            (Symbol::new(&env, MODEL_UPDATED),),
            (admin, base_rate_bps, optimal_utilization_bps, slope1_bps, slope2_bps),
        );
        Ok(())
    }

    /// Replace the curve parameters. Admin-only.
    pub fn set_model(
        env: Env,
        caller: Address,
        base_rate_bps: u32,
        optimal_utilization_bps: u32,
        slope1_bps: u32,
        slope2_bps: u32,
    ) -> Result<RateModel, ModelError> {
        let admin = load_admin(&env)?;
        if caller != admin {
            return Err(ModelError::NotAdmin);
        }
        caller.require_auth();
        let model = RateModel {
            base_rate_bps,
            optimal_utilization_bps,
            slope1_bps,
            slope2_bps,
        };
        validate_model(&model)?;
        env.storage().instance().set(&DataKey::Model, &model);
        env.events().publish(
            (Symbol::new(&env, MODEL_UPDATED),),
            (caller, base_rate_bps, optimal_utilization_bps, slope1_bps, slope2_bps),
        );
        Ok(model)
    }

    /// Current pool utilization in basis points for `D` debt against `C`
    /// collateral.
    pub fn utilization(env: Env, total_debt: i128, total_collateral: i128) -> Result<u32, ModelError> {
        let _ = env;
        utilization_rate(total_debt, total_collateral)
    }

    /// Variable borrow rate in basis points for an explicit utilization value.
    pub fn rate_for_utilization(env: Env, utilization_bps: u32) -> Result<u32, ModelError> {
        let model = load_model(&env)?;
        borrow_rate(utilization_bps, &model)
    }

    /// Variable borrow rate in basis points for `D` debt against `C`
    /// collateral under the active model: utilization first, then the kinked
    /// curve.
    pub fn borrow_rate_for_pool(
        env: Env,
        total_debt: i128,
        total_collateral: i128,
    ) -> Result<u32, ModelError> {
        let model = load_model(&env)?;
        let utilization = utilization_rate(total_debt, total_collateral)?;
        borrow_rate(utilization, &model)
    }

    /// Active curve parameters.
    pub fn get_model(env: Env) -> Result<RateModel, ModelError> {
        load_model(&env)
    }
}

#[cfg(test)]
mod test {
    use super::{borrow_rate, utilization_rate, InterestRateModel, InterestRateModelClient, ModelError, RateModel, BPS};
    use soroban_sdk::{
        testutils::{Address as _, Ledger as _},
        Address, Env, Symbol, TryFromVal,
    };

    /// base 2%, kink at 80%, gentle slope 10%, steep slope 50%.
    const BASE: u32 = 200;
    const OPTIMAL: u32 = 8_000;
    const SLOPE1: u32 = 1_000;
    const SLOPE2: u32 = 5_000;

    fn model() -> RateModel {
        RateModel {
            base_rate_bps: BASE,
            optimal_utilization_bps: OPTIMAL,
            slope1_bps: SLOPE1,
            slope2_bps: SLOPE2,
        }
    }

    fn setup() -> (Env, InterestRateModelClient<'static>, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let id = env.register_contract(None, InterestRateModel);
        let client = InterestRateModelClient::new(&env, &id);
        let admin = Address::generate(&env);
        client.initialize(&admin, &BASE, &OPTIMAL, &SLOPE1, &SLOPE2);
        (env, client, admin)
    }

    // ── utilization ─────────────────────────────────────────────────────────

    #[test]
    fn empty_pool_has_zero_utilization() {
        assert_eq!(utilization_rate(0, 0), Ok(0));
    }

    #[test]
    fn utilization_scales_with_debt_over_collateral() {
        assert_eq!(utilization_rate(0, 1_000), Ok(0));
        assert_eq!(utilization_rate(500, 1_000), Ok(5_000));
        assert_eq!(utilization_rate(1_000, 1_000), Ok(10_000));
        // Undercollateralized pools report above 100%.
        assert_eq!(utilization_rate(1_500, 1_000), Ok(15_000));
    }

    #[test]
    fn debt_without_collateral_is_fully_utilized() {
        assert_eq!(utilization_rate(100, 0), Ok(BPS));
    }

    #[test]
    fn utilization_rejects_negative_amounts() {
        assert_eq!(utilization_rate(-1, 100), Err(ModelError::NegativeAmount));
        assert_eq!(utilization_rate(100, -1), Err(ModelError::NegativeAmount));
    }

    #[test]
    fn utilization_floors_fractional_basis_points() {
        // 1/3 of the pool: 3_333.33… floors to 3_333, favouring borrowers.
        assert_eq!(utilization_rate(1, 3), Ok(3_333));
    }

    // ── borrow rate ─────────────────────────────────────────────────────────

    #[test]
    fn idle_pool_pays_the_base_rate() {
        assert_eq!(borrow_rate(0, &model()), Ok(BASE));
    }

    #[test]
    fn gentle_slope_below_the_kink() {
        // r = 200 + (4_000 / 8_000) * 1_000 = 700.
        assert_eq!(borrow_rate(4_000, &model()), Ok(700));
    }

    #[test]
    fn curve_is_continuous_at_the_kink() {
        // At U = U_optimal the gentle branch yields exactly base + slope1,
        // which is also the steep branch's intercept.
        assert_eq!(borrow_rate(OPTIMAL, &model()), Ok(BASE + SLOPE1));
        assert_eq!(borrow_rate(OPTIMAL, &model()), Ok(1_200));
    }

    #[test]
    fn steep_slope_above_the_kink() {
        // r = 200 + 1_000 + ((9_000 - 8_000) / (10_000 - 8_000)) * 5_000
        //   = 1_200 + 2_500 = 3_700.
        assert_eq!(borrow_rate(9_000, &model()), Ok(3_700));
    }

    #[test]
    fn fully_utilized_pool_pays_base_plus_both_slopes() {
        assert_eq!(borrow_rate(BPS, &model()), Ok(BASE + SLOPE1 + SLOPE2));
    }

    #[test]
    fn rate_is_monotone_across_the_kink() {
        let (mut prev, _) = (borrow_rate(0, &model()).unwrap(), 0);
        for u in (500..=10_000).step_by(500) {
            let rate = borrow_rate(u, &model()).unwrap();
            assert!(rate >= prev, "rate must not decrease: U={u}");
            prev = rate;
        }
        // And strictly steeper above the kink: equal 5pp utilization steps
        // move the rate more past 80% than before it.
        let gentle_step = borrow_rate(4_000, &model()).unwrap() - borrow_rate(3_500, &model()).unwrap();
        let steep_step = borrow_rate(9_000, &model()).unwrap() - borrow_rate(8_500, &model()).unwrap();
        assert!(steep_step > gentle_step);
    }

    #[test]
    fn invalid_kink_is_rejected() {
        let zero = RateModel { optimal_utilization_bps: 0, ..model() };
        assert_eq!(borrow_rate(100, &zero), Err(ModelError::InvalidOptimalUtilization));
        let full = RateModel { optimal_utilization_bps: BPS, ..model() };
        assert_eq!(borrow_rate(100, &full), Err(ModelError::InvalidOptimalUtilization));
    }

    // ── contract surface ────────────────────────────────────────────────────

    #[test]
    fn initialize_installs_the_model() {
        let (_, client, _) = setup();
        assert_eq!(client.get_model(), model());
    }

    #[test]
    fn initialize_validates_the_kink() {
        let env = Env::default();
        env.mock_all_auths();
        let id = env.register_contract(None, InterestRateModel);
        let client = InterestRateModelClient::new(&env, &id);
        let res = client.try_initialize(&Address::generate(&env), &BASE, &0, &SLOPE1, &SLOPE2);
        assert_eq!(res, Err(Ok(ModelError::InvalidOptimalUtilization)));
        let res = client.try_initialize(&Address::generate(&env), &BASE, &BPS, &SLOPE1, &SLOPE2);
        assert_eq!(res, Err(Ok(ModelError::InvalidOptimalUtilization)));
    }

    #[test]
    fn initialize_cannot_run_twice() {
        let (_, client, admin) = setup();
        let res = client.try_initialize(&admin, &BASE, &OPTIMAL, &SLOPE1, &SLOPE2);
        assert_eq!(res, Err(Ok(ModelError::AlreadyInitialized)));
    }

    #[test]
    fn only_admin_can_replace_the_model() {
        let (env, client, _) = setup();
        let intruder = Address::generate(&env);
        assert_eq!(
            client.try_set_model(&intruder, &BASE, &OPTIMAL, &SLOPE1, &SLOPE2),
            Err(Ok(ModelError::NotAdmin))
        );
    }

    #[test]
    fn pool_rate_composes_utilization_and_curve() {
        let (_, client, _) = setup();
        // 4_000/5_000 = 80% = kink → base + slope1.
        assert_eq!(client.borrow_rate_for_pool(&4_000, &5_000), 1_200);
        // Empty pool → base rate.
        assert_eq!(client.borrow_rate_for_pool(&0, &5_000), 200);
        // 4_500/5_000 = 90% → 200 + 1_000 + (1_000/2_000)*5_000 = 3_700.
        assert_eq!(client.borrow_rate_for_pool(&4_500, &5_000), 3_700);
    }

    #[test]
    fn model_update_event_carries_new_parameters() {
        use soroban_sdk::testutils::Events as _;

        let (env, client, admin) = setup();
        client.set_model(&admin, &300, &7_500, &900, &4_000);

        let events = env.events().all();
        let (_, topics, data) = events.get(events.len() - 1).unwrap();
        let topic: Symbol = Symbol::try_from_val(&env, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic, Symbol::new(&env, "RateModelUpdated"));
        let (actor, b, o, s1, s2): (Address, u32, u32, u32, u32) =
            TryFromVal::try_from_val(&env, &data).unwrap();
        assert_eq!((actor, b, o, s1, s2), (admin, 300, 7_500, 900, 4_000));
        assert_eq!(
            client.get_model(),
            RateModel {
                base_rate_bps: 300,
                optimal_utilization_bps: 7_500,
                slope1_bps: 900,
                slope2_bps: 4_000,
            }
        );
    }

    #[test]
    fn ledger_time_is_not_an_input() {
        // The curve is a pure function of (D, C): advancing the ledger must
        // not move the quoted rate.
        use soroban_sdk::testutils::Ledger as _;
        let (env, client, _) = setup();
        let before = client.borrow_rate_for_pool(&4_000, &5_000);
        env.ledger().with_mut(|li| li.timestamp += 30 * 24 * 60 * 60);
        assert_eq!(client.borrow_rate_for_pool(&4_000, &5_000), before);
        let _ = env;
    }
}
