//! Auto-compounding yield strategy maximum drawdown guard (Issue #1010).
//!
//! An auto-compounding vault harvest takes accrued rewards and stakes them back
//! into the same target farm, on the bet that the reward token is worth roughly
//! what it was worth when the position was opened. That bet is wrong exactly
//! when it is most expensive: when the reward asset is depreciating.
//! Re-investing into a falling token compounds the loss instead of the yield.
//!
//! This module makes that bet measurable. It keeps a short history of the target
//! reward token's price, measures the **price trend**
//!
//! ```text
//!              P_current - P_7d
//!   ΔP  =  ─────────────────────
//!                   P_7d
//! ```
//!
//! and, when `ΔP` falls below the corridor drawdown limit (default **−20 %**,
//! [`DEFAULT_MAX_DRAWDOWN_BPS`]), **pauses auto-compounding harvest** for that
//! vault and directs the strategy to **convert accrued rewards into the base
//! stablecoin reserve** instead of re-investing them.
//!
//! # The three deliverables
//!
//! | Deliverable | Entry point |
//! |---|---|
//! | 📊 Compute `ΔP` over a 7-day window | [`record_price_sample`], [`price_trend_bps`], [`drawdown_status`] |
//! | 🚨 Pause auto-compounding past 20 % drawdown | [`sync_drawdown`], [`is_auto_compounding_paused`], [`require_compounding_allowed`] |
//! | 💵 Convert rewards to base reserves | [`convert_rewards_to_reserves`], [`strategy_action`] |
//!
//! # Fail-closed arithmetic
//!
//! The breach decision uses [`crate::math::ratio_ge`], an exact `a / b >= c / d`
//! comparison that never forms a product and therefore cannot wrap at `i128`
//! balances. [`price_trend_bps`] is a signed, saturating *reporting* view and is
//! deliberately not used to gate value, so a saturating read can never fail
//! open.
//!
//! # The pause is a pure function of state
//!
//! `compounding_paused` derives from the stored samples and the configured limit;
//! [`sync_pause`] is its only writer. An independent latch would be easier to
//! reach for, but a later tick could then silently clear it, leaving the flag
//! unexplainable from the ledger. A monitor must be able to recompute the answer
//! from state alone, so there is no hidden latch. The invariant is exact:
//!
//! ```text
//!   compounding_paused  ⟺  breached
//! ```
//!
//! # Unarmed guard
//!
//! The guard is *armed* only once it holds a sample at least `window_secs` old.
//! Before that there is no 7-day price to compare against, so there is no honest
//! trend to report and [`strategy_action`] returns
//! [`CompoundingAction::Hold`] — "do not auto-reinvest" — rather than inventing
//! one. An unarmed guard never sets `compounding_paused`; the two are
//! independent facts.
//!
//! # Sampling cadence
//!
//! The buffer holds [`MAX_PRICE_SAMPLES`] entries and the guard reads its
//! reference from the *oldest retained* sample. At the intended cadence of one
//! sample per [`SAMPLE_INTERVAL_SECONDS`] the buffer spans exactly
//! [`TREND_WINDOW_SECONDS`]. A keeper that samples **faster** than that evicts
//! the reference and disarms the guard — deliberately fail-safe, and visible
//! through `armed` in [`drawdown_status`] rather than silently corrupting a
//! trend.
//!
//! # Safety of the reserve conversion
//!
//! [`convert_rewards_to_reserves`] treats the caller-supplied router as hostile,
//! following the same discipline as
//! [`crate::vaults::harvest_compound`]:
//!
//! * **Output is measured, never reported.** The reserve amount is the vault's
//!   own `balance()` delta across the swap, not the router's return value.
//! * **Slippage is enforced locally.** The binding bound runs here against that
//!   measured delta, so a router cannot satisfy it by lying.
//! * **The route is validated.** The path must start at the configured reward
//!   token and end at the configured base reserve.
//! * **Failure is atomic.** An under-delivering swap reverts the whole
//!   transaction, so rewards are never spent without reserves arriving.
//! * **It never re-invests.** That is the entire point: once the guard trips,
//!   the proceeds are parked in the base stablecoin rather than staked back into
//!   the depreciating farm.
//!
//! # Usage
//!
//! ```
//! use stellarflow_contracts::vaults::compounding_guard::{
//!     is_drawdown_exceeded, price_trend_bps, DEFAULT_MAX_DRAWDOWN_BPS,
//! };
//!
//! // A 20 % fall is exactly at the limit and must not trip the guard.
//! assert_eq!(price_trend_bps(80, 100), Ok(-2_000));
//! assert!(!is_drawdown_exceeded(80, 100, DEFAULT_MAX_DRAWDOWN_BPS));
//!
//! // One unit further and the guard trips.
//! assert!(is_drawdown_exceeded(79, 100, DEFAULT_MAX_DRAWDOWN_BPS));
//! ```
//!
//! Downstream harvest paths should call [`require_compounding_allowed`]
//! immediately before re-investing, and branch on [`strategy_action`] to choose
//! between re-staking and [`convert_rewards_to_reserves`].

use soroban_sdk::{contracttype, symbol_short, token, Address, Env, IntoVal, Symbol, Vec};

use crate::math::ratio_ge;
use crate::{AssetId, ContractData, ContractError, DATA_KEY};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Basis-point denominator: 10 000 bp = 100 %.
pub const BPS_DENOMINATOR: i64 = 10_000;

/// Length of the price-trend lookback window: 7 days.
pub const TREND_WINDOW_SECONDS: u64 = 7 * 24 * 60 * 60; // 604_800

/// Number of price samples retained per lookback window.
///
/// Eight intervals of [`SAMPLE_INTERVAL_SECONDS`] span exactly
/// [`TREND_WINDOW_SECONDS`], so the buffer needs one extra slot to hold the
/// reference itself: nine entries covering ages `0 ..= 7d`.
pub const MAX_PRICE_SAMPLES: u32 = 9;

/// Intended cadence between price samples: one window split into 8 parts
/// (≈ 21 hours).
pub const SAMPLE_INTERVAL_SECONDS: u64 = TREND_WINDOW_SECONDS / 8; // 75_600

/// Default maximum tolerated drawdown before compounding is paused: **20 %**.
///
/// Mirrors the `$20\%$` requirement from the issue. The comparison is strict,
/// so a fall of exactly 20 % is tolerated and 20 % + 1 bp trips the guard.
pub const DEFAULT_MAX_DRAWDOWN_BPS: i64 = 2_000;

/// Floor for a configured drawdown limit: 0 bp, i.e. any depreciation at all
/// trips the guard.
pub const MIN_ALLOWED_DRAWDOWN_BPS: i64 = 0;

/// Ceiling for a configured drawdown limit: 1 000 % (10x).
///
/// Bounds the limit so a fat-fingered governance transaction cannot brick a
/// vault. Because the comparison is strict, setting this ceiling is how
/// governance effectively disables the guard without a separate kill switch.
pub const MAX_ALLOWED_DRAWDOWN_BPS: i64 = 100_000;

/// Minimum length of a reserve-conversion route, reward token to base reserve.
pub const MIN_SWAP_PATH_LEN: u32 = 2;

/// Maximum length of a reserve-conversion route.
pub const MAX_SWAP_PATH_LEN: u32 = 5;

// Event topics. Every `symbol_short!` payload must stay within 9 bytes.
/// Drawdown limit breached; auto-compounding harvest paused.
pub const EV_YIELD_DRAWDOWN_PAUSED: Symbol = symbol_short!("yld_paus");
/// Drawdown recovered inside the limit; auto-compounding harvest resumed.
pub const EV_YIELD_DRAWDOWN_RESUMED: Symbol = symbol_short!("yld_res");
/// A reward-token price sample was recorded.
pub const EV_YIELD_PRICE_SAMPLE: Symbol = symbol_short!("yld_px");
/// A vault's drawdown guard was configured or reconfigured.
pub const EV_YIELD_GUARD_CONFIGURED: Symbol = symbol_short!("yld_cfg");
/// Accrued rewards were converted into base stablecoin reserves.
pub const EV_YIELD_REWARDS_CONVERTED: Symbol = symbol_short!("yld_conv");
/// Booked base reserves were swept out of the vault.
pub const EV_YIELD_RESERVES_SWEEPED: Symbol = symbol_short!("yld_swp");

/// Second topic of [`EV_YIELD_DRAWDOWN_PAUSED`].
const STATUS_PAUSED: Symbol = symbol_short!("paused");
/// Second topic of [`EV_YIELD_DRAWDOWN_RESUMED`].
const STATUS_RESUMED: Symbol = symbol_short!("resumed");
/// Second topic of [`EV_YIELD_PRICE_SAMPLE`].
const STATUS_SAMPLED: Symbol = symbol_short!("sample");
/// Second topic of [`EV_YIELD_GUARD_CONFIGURED`].
const STATUS_CONFIGURED: Symbol = symbol_short!("config");
/// Second topic of [`EV_YIELD_REWARDS_CONVERTED`].
const STATUS_CONVERTED: Symbol = symbol_short!("convert");
/// Second topic of [`EV_YIELD_RESERVES_SWEEPED`].
const STATUS_SWEEPED: Symbol = symbol_short!("sweep");

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum YieldGuardKey {
    /// Guard configuration for a vault.
    Config(AssetId),
    /// Rolling price history for a vault's reward token, oldest first.
    Samples(AssetId),
    /// Latched auto-compounding pause flag for a vault.
    CompoundingPaused(AssetId),
    /// Ledger timestamp the current pause began.
    CompoundingPausedAt(AssetId),
    /// Base-reserve balance this guard has booked through conversions.
    ReserveAccrued(AssetId),
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// One observed reward-token price.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceSample {
    /// Oracle price, in base-asset units per reward token.
    pub price: i128,
    /// Ledger timestamp at which the price was observed.
    pub observed_at: u64,
}

/// Per-vault drawdown guard configuration.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompoundingGuardConfig {
    /// Vault this guard protects.
    pub vault: AssetId,
    /// The target farm's reward token — the asset whose trend is measured.
    pub reward_token: Address,
    /// The base stablecoin accrued rewards are converted into on a trip.
    pub base_reserve: Address,
    /// Strict drawdown limit in basis points; the guard trips past it.
    pub max_drawdown_bps: i64,
    /// Price-trend lookback window, in seconds.
    pub window_secs: u64,
}

/// Read-only monitoring snapshot for a vault's drawdown guard.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DrawdownStatus {
    /// Vault this status describes.
    pub vault: AssetId,
    /// Reward token under observation.
    pub reward_token: Address,
    /// Most recent recorded price; `0` before the first sample.
    pub current_price: i128,
    /// Price used as the `P_7d` reference; `0` when unarmed.
    pub reference_price: i128,
    /// Age of the reference sample; `0` when unarmed.
    pub reference_age_secs: u64,
    /// `true` when a sample at least `window_secs` old is available.
    pub armed: bool,
    /// Signed trend `(P_current - P_reference) / P_reference` in basis points.
    /// `0` when unarmed.
    pub trend_bps: i64,
    /// Drawdown limit in force.
    pub max_drawdown_bps: i64,
    /// `true` when the trend is below `-max_drawdown_bps`.
    pub breached: bool,
    /// Latched pause on auto-compounding harvest.
    pub compounding_paused: bool,
    /// Ledger timestamp the current pause began; `0` while live.
    pub compounding_paused_at: u64,
}

/// What the auto-compounding strategy should do with accrued rewards.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompoundingAction {
    /// Trend is healthy: re-invest rewards into the target farm.
    Reinvest,
    /// Drawdown breached: park rewards in the base stablecoin reserve.
    ConvertToReserves,
    /// No verifiable 7-day trend yet: do not auto-reinvest.
    Hold,
}

/// Result of converting accrued rewards into base stablecoin reserves.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReserveConversion {
    /// Reward tokens spent.
    pub reward_spent: i128,
    /// Base stablecoin actually delivered, measured from the vault's balance.
    pub reserve_received: i128,
}

// ---------------------------------------------------------------------------
// Trend arithmetic
// ---------------------------------------------------------------------------

/// `floor(numerator * BPS_DENOMINATOR / denominator)` for non-negative inputs.
///
/// Saturates at `i128::MAX` and never forms the intermediate product: the
/// quotient is taken first and only the remainder is scaled. For any
/// `denominator <= i64::MAX` the shrink factor is `1` and the result is exact;
/// above that the fractional part can lose well under `1e-5` bp, which is
/// immaterial for a reporting view that never gates value.
fn scaled_ratio_bps(numerator: i128, denominator: i128) -> i128 {
    let whole = numerator / denominator;
    if whole > i128::MAX / BPS_DENOMINATOR as i128 {
        return i128::MAX;
    }
    let whole_bps = whole * BPS_DENOMINATOR as i128;

    let remainder = numerator % denominator;
    let frac_bps = if remainder == 0 {
        0
    } else {
        let shrink = denominator / i64::MAX as i128 + 1;
        (remainder / shrink) * BPS_DENOMINATOR as i128 / (denominator / shrink)
    };

    whole_bps + frac_bps
}

/// Compute the signed price trend `ΔP = (P_current - P_7d) / P_7d` in basis
/// points.
///
/// Positive means the reward token appreciated across the window; negative
/// means it depreciated. `P_current == P_7d` yields `0`.
///
/// # Errors
/// * [`ContractError::YieldPriceNotPositive`] — `reference` is not strictly
///   positive, or `current` is negative. A non-positive reference makes `ΔP`
///   undefined, so it is rejected rather than silently coerced to zero.
/// * [`ContractError::MathOverflow`] — the saturated result is not
///   representable as an `i64`.
///
/// # Examples
/// ```
/// use stellarflow_contracts::vaults::compounding_guard::price_trend_bps;
///
/// assert_eq!(price_trend_bps(120, 100), Ok(2_000)); // +20 %
/// assert_eq!(price_trend_bps(80, 100), Ok(-2_000)); // -20 %
/// assert_eq!(price_trend_bps(100, 100), Ok(0)); // flat
/// ```
pub fn price_trend_bps(current: i128, reference: i128) -> Result<i64, ContractError> {
    if current < 0 {
        return Err(ContractError::YieldPriceNotPositive);
    }
    if reference <= 0 {
        return Err(ContractError::YieldPriceNotPositive);
    }

    // Both operands are non-negative, so neither the subtraction nor the
    // negation below can overflow.
    let bps = if current >= reference {
        scaled_ratio_bps(current - reference, reference)
    } else {
        -scaled_ratio_bps(reference - current, reference)
    };

    if bps > i64::MAX as i128 || bps < i64::MIN as i128 {
        return Err(ContractError::MathOverflow);
    }
    Ok(bps as i64)
}

/// `true` when the reward token has depreciated by strictly more than
/// `max_drawdown_bps` between `reference` and `current`.
///
/// This is the breach decision and is the only function that should gate
/// compounding. It is exact at every magnitude: it delegates the comparison to
/// [`crate::math::ratio_ge`], so no product is ever formed and `i128` balances
/// cannot wrap the answer into a false "safe".
///
/// A price at or above `reference` is never a drawdown. A non-positive
/// `reference` makes the trend undefined and is reported as a breach, so an
/// unmeasurable ratio can never fail open.
pub fn is_drawdown_exceeded(current: i128, reference: i128, max_drawdown_bps: i64) -> bool {
    if current < 0 || reference <= 0 {
        return true;
    }
    if current >= reference {
        return false;
    }
    if max_drawdown_bps <= 0 {
        return true;
    }
    // The limit is *strict*: a fall of exactly `max_drawdown_bps` is tolerated
    // and only past it does the guard trip. `ratio_ge` is non-strict, so
    // `loss / reference > limit / 10_000` is evaluated as the negation of
    // `limit / 10_000 >= loss / reference`, which keeps the comparison
    // overflow-free. `reference - current` is safe: both are non-negative.
    !ratio_ge(
        max_drawdown_bps as i128,
        BPS_DENOMINATOR as i128,
        reference - current,
        reference,
    )
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

/// Outcome of one trend evaluation, before it is projected onto storage.
struct Evaluation {
    armed: bool,
    current_price: i128,
    reference_price: i128,
    reference_age_secs: u64,
    trend_bps: i64,
    breached: bool,
}

impl Evaluation {
    /// An unarmed guard: nothing measurable, so nothing claimed. `breached` is
    /// `false` because an absent reference must not be presented as a breach —
    /// the strategy is steered to `Hold` by the `armed` flag instead.
    fn unarmed() -> Self {
        Self {
            armed: false,
            current_price: 0,
            reference_price: 0,
            reference_age_secs: 0,
            trend_bps: 0,
            breached: false,
        }
    }
}

/// Resolve the reference sample and evaluate the trend.
fn evaluate(env: &Env, vault: AssetId, config: &CompoundingGuardConfig) -> Evaluation {
    let samples = read_samples(env, vault);
    if samples.is_empty() {
        return Evaluation::unarmed();
    }

    let now = env.ledger().timestamp();
    // `samples` is oldest-first and `record_price_sample` refuses out-of-order
    // observations, so the last entry is the newest.
    let current = match samples.get(samples.len() - 1) {
        Some(sample) => sample,
        None => return Evaluation::unarmed(),
    };

    // The reference is the oldest retained sample that is old enough.
    let mut reference: Option<PriceSample> = None;
    for sample in samples.iter() {
        if now.saturating_sub(sample.observed_at) >= config.window_secs {
            reference = Some(sample);
            break;
        }
    }

    match reference {
        Some(reference) => Evaluation {
            armed: true,
            current_price: current.price,
            reference_price: reference.price,
            reference_age_secs: now.saturating_sub(reference.observed_at),
            trend_bps: price_trend_bps(current.price, reference.price).unwrap_or(0),
            breached: is_drawdown_exceeded(
                current.price,
                reference.price,
                config.max_drawdown_bps,
            ),
        },
        None => Evaluation {
            armed: false,
            current_price: current.price,
            ..Evaluation::unarmed()
        },
    }
}

/// Re-evaluate the trend and move the pause flag accordingly.
///
/// Returns `true` when this call flipped the flag. Emits on a transition only,
/// so a healthy vault does not emit on every sample.
fn sync_pause(env: &Env, vault: AssetId, config: &CompoundingGuardConfig) -> bool {
    let evaluation = evaluate(env, vault, config);
    let paused = is_auto_compounding_paused(env, vault);

    if evaluation.breached && !paused {
        store_pause(env, vault, true, env.ledger().timestamp());
        env.events().publish(
            (EV_YIELD_DRAWDOWN_PAUSED, STATUS_PAUSED),
            (
                vault,
                config.reward_token.clone(),
                evaluation.current_price,
                evaluation.reference_price,
                evaluation.trend_bps,
                config.max_drawdown_bps,
                env.ledger().timestamp(),
            ),
        );
        return true;
    }

    if !evaluation.breached && paused {
        store_pause(env, vault, false, 0);
        env.events().publish(
            (EV_YIELD_DRAWDOWN_RESUMED, STATUS_RESUMED),
            (
                vault,
                config.reward_token.clone(),
                evaluation.current_price,
                evaluation.reference_price,
                evaluation.trend_bps,
                config.max_drawdown_bps,
                env.ledger().timestamp(),
            ),
        );
        return true;
    }

    false
}

// ---------------------------------------------------------------------------
// State accessors
// ---------------------------------------------------------------------------

/// Read a vault's guard configuration, or `None` when unconfigured.
pub fn get_config(env: &Env, vault: AssetId) -> Option<CompoundingGuardConfig> {
    env.storage()
        .persistent()
        .get(&YieldGuardKey::Config(vault))
}

/// Read a vault's guard configuration or fail with
/// [`ContractError::VaultNotInitialized`].
pub fn require_config(env: &Env, vault: AssetId) -> Result<CompoundingGuardConfig, ContractError> {
    get_config(env, vault).ok_or(ContractError::VaultNotInitialized)
}

/// Read the retained price history for a vault, oldest first.
pub fn read_samples(env: &Env, vault: AssetId) -> Vec<PriceSample> {
    env.storage()
        .persistent()
        .get(&YieldGuardKey::Samples(vault))
        .unwrap_or_else(|| Vec::new(env))
}

/// Read the base-reserve balance booked through conversions.
pub fn reserve_accrued(env: &Env, vault: AssetId) -> i128 {
    env.storage()
        .persistent()
        .get(&YieldGuardKey::ReserveAccrued(vault))
        .unwrap_or(0i128)
}

/// `true` when auto-compounding harvest is paused for a vault.
pub fn is_auto_compounding_paused(env: &Env, vault: AssetId) -> bool {
    env.storage()
        .persistent()
        .get(&YieldGuardKey::CompoundingPaused(vault))
        .unwrap_or(false)
}

fn store_pause(env: &Env, vault: AssetId, paused: bool, at: u64) {
    env.storage()
        .persistent()
        .set(&YieldGuardKey::CompoundingPaused(vault), &paused);
    env.storage()
        .persistent()
        .set(&YieldGuardKey::CompoundingPausedAt(vault), &at);
}

/// Build the read-only monitoring snapshot for a configured vault.
///
/// # Errors
/// [`ContractError::VaultNotInitialized`] when the guard is not configured, so
/// a caller cannot mistake an unconfigured vault for a healthy one.
pub fn drawdown_status(env: &Env, vault: AssetId) -> Result<DrawdownStatus, ContractError> {
    let config = require_config(env, vault)?;
    let evaluation = evaluate(env, vault, &config);
    Ok(DrawdownStatus {
        vault,
        reward_token: config.reward_token,
        current_price: evaluation.current_price,
        reference_price: evaluation.reference_price,
        reference_age_secs: evaluation.reference_age_secs,
        armed: evaluation.armed,
        trend_bps: evaluation.trend_bps,
        max_drawdown_bps: config.max_drawdown_bps,
        breached: evaluation.breached,
        compounding_paused: is_auto_compounding_paused(env, vault),
        compounding_paused_at: env
            .storage()
            .persistent()
            .get(&YieldGuardKey::CompoundingPausedAt(vault))
            .unwrap_or(0u64),
    })
}

/// Guard for downstream harvest paths: fail unless auto-compounding is allowed.
///
/// Call immediately before re-investing accrued rewards. It reads the latched
/// flag rather than re-deriving it, so the rejection path is a single ledger
/// read and cannot disagree with the last evaluation.
pub fn require_compounding_allowed(env: &Env, vault: AssetId) -> Result<(), ContractError> {
    if is_auto_compounding_paused(env, vault) {
        return Err(ContractError::YieldCompoundingPaused);
    }
    Ok(())
}

/// What the auto-compounding strategy should do with accrued rewards.
///
/// * [`CompoundingAction::ConvertToReserves`] — the drawdown limit is breached;
///   park the rewards in the base stablecoin.
/// * [`CompoundingAction::Reinvest`] — the trend is measurable and healthy.
/// * [`CompoundingAction::Hold`] — the guard is unconfigured or holds no 7-day
///   trend, so auto-reinvestment cannot be justified either way.
pub fn strategy_action(env: &Env, vault: AssetId) -> CompoundingAction {
    match drawdown_status(env, vault) {
        Err(_) => CompoundingAction::Hold,
        Ok(status) if status.breached => CompoundingAction::ConvertToReserves,
        Ok(status) if status.armed => CompoundingAction::Reinvest,
        Ok(_) => CompoundingAction::Hold,
    }
}

// ---------------------------------------------------------------------------
// Admin
// ---------------------------------------------------------------------------

/// Configure a vault's drawdown guard.
pub fn configure_guard(
    env: &Env,
    admin: &Address,
    vault: AssetId,
    reward_token: Address,
    base_reserve: Address,
    max_drawdown_bps: i64,
    window_secs: u64,
) -> Result<CompoundingGuardConfig, ContractError> {
    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;
    if data.admin != *admin {
        return Err(ContractError::NotAdmin);
    }
    admin.require_auth();

    if reward_token == base_reserve {
        // A guard that measures the reserve token against itself can never
        // detect a drawdown in the thing it is meant to protect.
        return Err(ContractError::InvalidArgument);
    }
    if !(MIN_ALLOWED_DRAWDOWN_BPS..=MAX_ALLOWED_DRAWDOWN_BPS).contains(&max_drawdown_bps) {
        return Err(ContractError::YieldInvalidDrawdownLimit);
    }
    if window_secs == 0 {
        return Err(ContractError::InvalidArgument);
    }

    let config = CompoundingGuardConfig {
        vault,
        reward_token: reward_token.clone(),
        base_reserve: base_reserve.clone(),
        max_drawdown_bps,
        window_secs,
    };
    env.storage()
        .persistent()
        .set(&YieldGuardKey::Config(vault), &config);

    // A new or widened window can change the verdict immediately, so re-evaluate
    // rather than leaving a stale latch in place.
    sync_pause(env, vault, &config);

    env.events().publish(
        (EV_YIELD_GUARD_CONFIGURED, STATUS_CONFIGURED),
        (
            vault,
            reward_token,
            base_reserve,
            max_drawdown_bps,
            window_secs,
            env.ledger().timestamp(),
        ),
    );

    Ok(config)
}

/// Update only the drawdown limit of an existing guard.
pub fn set_max_drawdown_bps(
    env: &Env,
    admin: &Address,
    vault: AssetId,
    max_drawdown_bps: i64,
) -> Result<CompoundingGuardConfig, ContractError> {
    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;
    if data.admin != *admin {
        return Err(ContractError::NotAdmin);
    }
    admin.require_auth();

    let mut config = require_config(env, vault)?;
    if !(MIN_ALLOWED_DRAWDOWN_BPS..=MAX_ALLOWED_DRAWDOWN_BPS).contains(&max_drawdown_bps) {
        return Err(ContractError::YieldInvalidDrawdownLimit);
    }

    config.max_drawdown_bps = max_drawdown_bps;
    env.storage()
        .persistent()
        .set(&YieldGuardKey::Config(vault), &config);
    sync_pause(env, vault, &config);

    env.events().publish(
        (EV_YIELD_GUARD_CONFIGURED, STATUS_CONFIGURED),
        (vault, max_drawdown_bps, env.ledger().timestamp()),
    );

    Ok(config)
}

// ---------------------------------------------------------------------------
// Sampling
// ---------------------------------------------------------------------------

/// Record a reward-token price observation and re-evaluate the guard.
///
/// Samples are keeper-authorised and are expected to be sourced from the
/// protocol price oracle. Three invariants keep the buffer honest:
///
/// * `price` must be strictly positive — a non-positive price is not an
///   observation, and accepting one would make the trend undefined.
/// * `observed_at` is read from the ledger, never from the caller.
/// * A new sample may not precede the newest retained one, which makes
///   replaying a stale or back-dated observation impossible. That is what would
///   otherwise let a keeper rewind the reference price and fake a healthy trend.
pub fn record_price_sample(
    env: &Env,
    keeper: &Address,
    vault: AssetId,
    price: i128,
) -> Result<DrawdownStatus, ContractError> {
    keeper.require_auth();
    if price <= 0 {
        return Err(ContractError::YieldPriceNotPositive);
    }
    let config = require_config(env, vault)?;
    let now = env.ledger().timestamp();

    let mut samples = read_samples(env, vault);
    if let Some(newest) = samples.get(samples.len().saturating_sub(1)) {
        if now < newest.observed_at {
            return Err(ContractError::YieldStalePriceSample);
        }
    }

    samples.push_back(PriceSample {
        price,
        observed_at: now,
    });
    // Drop the oldest so the buffer never grows without bound. At the intended
    // cadence this keeps one sample per [`SAMPLE_INTERVAL_SECONDS`] plus the
    // `P_7d` reference.
    if samples.len() > MAX_PRICE_SAMPLES {
        samples.remove(0);
    }
    env.storage()
        .persistent()
        .set(&YieldGuardKey::Samples(vault), &samples);

    sync_pause(env, vault, &config);

    env.events().publish(
        (EV_YIELD_PRICE_SAMPLE, STATUS_SAMPLED),
        (vault, price, now, samples.len()),
    );

    Ok(drawdown_status(env, vault)?)
}

/// Permissionless monitoring tick.
///
/// Re-evaluates the trend and latches or clears the pause. Call it after a
/// sharp move so the pause reflects the current price without waiting for the
/// next scheduled sample.
pub fn sync_drawdown(env: &Env, vault: AssetId) -> Result<DrawdownStatus, ContractError> {
    let config = require_config(env, vault)?;
    sync_pause(env, vault, &config);
    drawdown_status(env, vault)
}

// ---------------------------------------------------------------------------
// Reserve conversion
// ---------------------------------------------------------------------------

/// A route is only usable if it converts exactly the configured reward token
/// into exactly the configured base reserve.
fn validate_conversion_path(
    path: &Vec<Address>,
    config: &CompoundingGuardConfig,
) -> Result<(), ContractError> {
    let len = path.len();
    if !(MIN_SWAP_PATH_LEN..=MAX_SWAP_PATH_LEN).contains(&len) {
        return Err(ContractError::YieldInvalidConversionPath);
    }
    if path.get(0) != Some(config.reward_token.clone())
        || path.get(len - 1) != Some(config.base_reserve.clone())
    {
        return Err(ContractError::YieldInvalidConversionPath);
    }
    Ok(())
}

/// Convert `reward_amount` of accrued reward tokens into the base stablecoin
/// reserve instead of re-investing them.
///
/// This is the action the guard prescribes once the drawdown limit is breached,
/// so it is callable in precisely the state where
/// [`require_compounding_allowed`] refuses. It never re-stakes: the proceeds
/// are parked in the base reserve, where they hold their value while the farm
/// token depreciates.
///
/// `keeper` authorises the execution for operational accountability, but the
/// funds come from the vault's own accrued reward balance — a keeper cannot
/// convert tokens it does not supply, and the reserves stay in the vault.
///
/// # Errors
/// * [`ContractError::VaultNotInitialized`] — the guard is not configured.
/// * [`ContractError::AmountTooLow`] — `reward_amount` or `min_reserve_out` is
///   not strictly positive.
/// * [`ContractError::InsufficientReserveBalance`] — the vault does not hold
///   `reward_amount` of the reward token.
/// * [`ContractError::YieldInvalidConversionPath`] — `path` does not run
///   `reward_token -> base_reserve`, or its length is out of bounds.
/// * [`ContractError::YieldConversionProducedNothing`] — the router delivered
///   no reserve at all.
/// * [`ContractError::SlippageExceeded`] — the reserve delivered was below
///   `min_reserve_out`.
pub fn convert_rewards_to_reserves(
    env: &Env,
    keeper: &Address,
    router: Address,
    vault: AssetId,
    path: Vec<Address>,
    reward_amount: i128,
    min_reserve_out: i128,
) -> Result<ReserveConversion, ContractError> {
    keeper.require_auth();
    if reward_amount <= 0 {
        return Err(ContractError::AmountTooLow);
    }
    if min_reserve_out <= 0 {
        return Err(ContractError::AmountTooLow);
    }

    let config = require_config(env, vault)?;
    validate_conversion_path(&path, &config)?;

    let vault_address = env.current_contract_address();
    let reward_client = token::Client::new(env, &config.reward_token);
    let reserve_client = token::Client::new(env, &config.base_reserve);

    // The rewards are already in vault custody from the harvest that produced
    // them, so the swap is funded from the vault itself.
    if reward_client.balance(&vault_address) < reward_amount {
        return Err(ContractError::InsufficientReserveBalance);
    }

    // Transfer-then-call. The reserve balance is sampled *after* funding so the
    // delta captures the swap alone and is unaffected by reserves the vault
    // already holds.
    reward_client.transfer(&vault_address, &router, &reward_amount);

    let reserve_before = reserve_client.balance(&vault_address);
    let _: soroban_sdk::Val = env.invoke_contract(
        &router,
        &soroban_sdk::Symbol::new(env, "swap_exact_tokens_for_tokens"),
        soroban_sdk::vec![
            env,
            reward_amount.into_val(env),
            // Deliberately 0: the binding slippage check is the measured one
            // below, which a hostile router cannot talk its way past.
            0i128.into_val(env),
            path.into_val(env),
            vault_address.clone().into_val(env),
        ],
    );
    let reserve_after = reserve_client.balance(&vault_address);

    // Measured, never reported: a router that claims it paid cannot make the
    // vault book reserves it did not actually receive.
    let reserve_received = reserve_after
        .checked_sub(reserve_before)
        .ok_or(ContractError::MathOverflow)?;
    if reserve_received <= 0 {
        return Err(ContractError::YieldConversionProducedNothing);
    }
    if reserve_received < min_reserve_out {
        return Err(ContractError::SlippageExceeded);
    }

    let booked = reserve_accrued(env, vault)
        .checked_add(reserve_received)
        .ok_or(ContractError::MathOverflow)?;
    env.storage()
        .persistent()
        .set(&YieldGuardKey::ReserveAccrued(vault), &booked);

    env.events().publish(
        (EV_YIELD_REWARDS_CONVERTED, STATUS_CONVERTED),
        (
            vault,
            reward_amount,
            reserve_received,
            booked,
            env.ledger().timestamp(),
        ),
    );

    Ok(ReserveConversion {
        reward_spent: reward_amount,
        reserve_received,
    })
}

/// Sweep booked base reserves out of the vault to `to`.
///
/// Administration of the reserve balance the conversions park here. Those
/// reserves sit idle for as long as the guard is tripped, so they need a way
/// out; the guard itself only ever accrues them.
pub fn sweep_reserves(
    env: &Env,
    admin: &Address,
    to: Address,
    vault: AssetId,
    amount: i128,
) -> Result<i128, ContractError> {
    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;
    if data.admin != *admin {
        return Err(ContractError::NotAdmin);
    }
    admin.require_auth();
    if amount <= 0 {
        return Err(ContractError::AmountTooLow);
    }

    let config = require_config(env, vault)?;
    let booked = reserve_accrued(env, vault);
    if amount > booked {
        return Err(ContractError::InsufficientReserveBalance);
    }

    let vault_address = env.current_contract_address();
    token::Client::new(env, &config.base_reserve).transfer(&vault_address, &to, &amount);

    env.storage()
        .persistent()
        .set(&YieldGuardKey::ReserveAccrued(vault), &(booked - amount));

    env.events().publish(
        (EV_YIELD_RESERVES_SWEEPED, STATUS_SWEEPED),
        (vault, to, amount, booked - amount, env.ledger().timestamp()),
    );

    Ok(amount)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::testutils::{Events, Ledger};
    use soroban_sdk::{contract, contractimpl};

    /// Ledger interval between successive samples, matching the intended
    /// keeper cadence.
    const STEP: u64 = SAMPLE_INTERVAL_SECONDS;

    // ── External router test double ────────────────────────────────────────

    const OUT_TOKEN: Symbol = symbol_short!("OUT");
    const RATE_NUM: Symbol = symbol_short!("NUM");
    const RATE_DEN: Symbol = symbol_short!("DEN");
    const SILENT: Symbol = symbol_short!("SILENT");

    /// Stand-in for the external DEX router. Pays out
    /// `amount_in * num / den` of `out_token` from its own float, then *lies*
    /// about the amount by reporting `i128::MAX`. Every assertion on
    /// `reserve_received` therefore proves the vault trusted its measured
    /// balance delta and not this number. A `silent` router pays nothing at all.
    #[contract]
    pub struct MockRouter;

    #[contractimpl]
    impl MockRouter {
        pub fn configure(env: Env, out_token: Address, num: i128, den: i128, silent: bool) {
            env.storage().instance().set(&OUT_TOKEN, &out_token);
            env.storage().instance().set(&RATE_NUM, &num);
            env.storage().instance().set(&RATE_DEN, &den);
            env.storage().instance().set(&SILENT, &silent);
        }

        pub fn swap_exact_tokens_for_tokens(
            env: Env,
            amount_in: i128,
            _amount_out_min: i128,
            _path: Vec<Address>,
            to: Address,
        ) -> Vec<i128> {
            let silent: bool = env.storage().instance().get(&SILENT).unwrap_or(false);
            if !silent {
                let out: Address = env.storage().instance().get(&OUT_TOKEN).unwrap();
                let num: i128 = env.storage().instance().get(&RATE_NUM).unwrap();
                let den: i128 = env.storage().instance().get(&RATE_DEN).unwrap();
                let amount_out = amount_in * num / den;
                if amount_out > 0 {
                    token::Client::new(&env, &out).transfer(
                        &env.current_contract_address(),
                        &to,
                        &amount_out,
                    );
                }
            }
            soroban_sdk::vec![&env, amount_in, i128::MAX]
        }
    }

    // ── Fixture ─────────────────────────────────────────────────────────────

    const VAULT: AssetId = 1;
    const REWARD: i128 = 1_000;
    const OTHER_REWARD: i128 = 1_100;
    const BIG_REWARD: i128 = 5_000;

    struct Fixture {
        env: Env,
        id: Address,
        admin: Address,
        keeper: Address,
        recipient: Address,
        vault_id: Address,
        reward_token: Address,
        base_reserve: Address,
        router: Address,
    }

    impl Fixture {
        fn new() -> Self {
            let env = Env::default();
            env.mock_all_auths();
            // The conversion path funds a router, invokes it and re-reads two
            // balances. That is a realistic cross-contract budget, and the test
            // budget is not what is under test here.
            env.budget().reset_unlimited();
            let id = env.register_contract(None, crate::TimeLockedUpgradeContract);
            let admin = Address::generate(&env);
            let keeper = Address::generate(&env);
            let recipient = Address::generate(&env);
            let reward_token = env.register_stellar_asset_contract(Address::generate(&env));
            let base_reserve = env.register_stellar_asset_contract(Address::generate(&env));
            let router_id = env.register_contract(None, MockRouter);

            env.as_contract(&id, || {
                env.storage().instance().set(
                    &DATA_KEY,
                    &ContractData {
                        admin: admin.clone(),
                        value: 0,
                        max_fee_ceiling: 0,
                    },
                );
            });

            Self {
                vault_id: id.clone(),
                reward_token,
                base_reserve,
                router: router_id,
                env,
                id,
                admin,
                keeper,
                recipient,
            }
        }

        fn configure(&self) -> CompoundingGuardConfig {
            self.configure_with(
                self.reward_token.clone(),
                self.base_reserve.clone(),
                DEFAULT_MAX_DRAWDOWN_BPS,
                TREND_WINDOW_SECONDS,
            )
        }

        fn configure_with(
            &self,
            reward_token: Address,
            base_reserve: Address,
            max_drawdown_bps: i64,
            window_secs: u64,
        ) -> CompoundingGuardConfig {
            self.frame(|| {
                configure_guard(
                    &self.env,
                    &self.admin,
                    VAULT,
                    reward_token,
                    base_reserve,
                    max_drawdown_bps,
                    window_secs,
                )
            })
            .expect("configure")
        }

        fn frame<T, F: FnOnce() -> T>(&self, f: F) -> T {
            self.env.as_contract(&self.id, f)
        }

        /// Advance the ledger one cadence step and record a sample.
        fn sample(&self, price: i128) -> Result<DrawdownStatus, ContractError> {
            self.env.ledger().with_mut(|li| li.timestamp += STEP);
            self.frame(|| record_price_sample(&self.env, &self.keeper, VAULT, price))
        }

        /// Record a sample without advancing the ledger.
        fn sample_burst(&self, price: i128) -> Result<DrawdownStatus, ContractError> {
            self.frame(|| record_price_sample(&self.env, &self.keeper, VAULT, price))
        }

        /// Advance the ledger to make the oldest retained sample `window_secs`
        /// old, then record `price`. This is the one call that arms the guard.
        fn sample_armed(&self, price: i128) -> DrawdownStatus {
            self.env.ledger().with_mut(|li| li.timestamp += STEP * 8);
            self.sample(price).expect("sample")
        }

        fn status(&self) -> DrawdownStatus {
            self.frame(|| drawdown_status(&self.env, VAULT)).expect("status")
        }

        fn action(&self) -> CompoundingAction {
            self.frame(|| strategy_action(&self.env, VAULT))
        }

        fn paused(&self) -> bool {
            self.frame(|| is_auto_compounding_paused(&self.env, VAULT))
        }

        fn guard(&self) -> Result<(), ContractError> {
            self.frame(|| require_compounding_allowed(&self.env, VAULT))
        }

        fn samples(&self) -> Vec<PriceSample> {
            self.frame(|| read_samples(&self.env, VAULT))
        }

        fn reserved(&self) -> i128 {
            self.frame(|| reserve_accrued(&self.env, VAULT))
        }

        fn convert(&self, amount: i128, min_out: i128) -> Result<ReserveConversion, ContractError> {
            self.frame(|| {
                let path = soroban_sdk::vec![
                    &self.env,
                    self.reward_token.clone(),
                    self.base_reserve.clone()
                ];
                convert_rewards_to_reserves(
                    &self.env,
                    &self.keeper,
                    self.router.clone(),
                    VAULT,
                    path,
                    amount,
                    min_out,
                )
            })
        }

        /// Fund the vault with accrued rewards.
        fn fund_vault(&self, amount: i128) {
            let minter = soroban_sdk::token::StellarAssetClient::new(&self.env, &self.reward_token);
            minter.mint(&self.vault_id, &amount);
        }

        /// Route the mock router to pay `num/den` of the base reserve.
        ///
        /// The router pays out of its own inventory, so it is funded with a
        /// float of the base reserve first — exactly as a real venue would be.
        /// Without that float the payout transfer fails, which is a useful
        /// reminder that the test double is exercising real token custody.
        fn route_rate(&self, num: i128, den: i128, silent: bool) {
            if !silent {
                let minter =
                    soroban_sdk::token::StellarAssetClient::new(&self.env, &self.base_reserve);
                minter.mint(&self.router, &(BIG_REWARD * 10));
            }
            self.env.as_contract(&self.router, || {
                self.env.storage().instance().set(&OUT_TOKEN, &self.base_reserve);
                self.env.storage().instance().set(&RATE_NUM, &num);
                self.env.storage().instance().set(&RATE_DEN, &den);
                self.env.storage().instance().set(&SILENT, &silent);
            });
        }

        fn emissions(&self, name: &Symbol, status: &Symbol) -> u32 {
            let expected =
                soroban_sdk::vec![&self.env, name.into_val(&self.env), status.into_val(&self.env)];
            self.env
                .events()
                .all()
                .iter()
                .filter(|(_, topics, _)| *topics == expected)
                .count() as u32
        }
    }

    // ── price_trend_bps ────────────────────────────────────────────────────

    #[test]
    fn trend_reports_signed_basis_points() {
        assert_eq!(price_trend_bps(120, 100), Ok(2_000));
        assert_eq!(price_trend_bps(80, 100), Ok(-2_000));
        assert_eq!(price_trend_bps(100, 100), Ok(0));
        assert_eq!(price_trend_bps(1, 2), Ok(-5_000));
    }

    #[test]
    fn trend_truncates_toward_zero() {
        // 1/3 of a whole is 3_333.33 bp and reports 3_333.
        assert_eq!(price_trend_bps(4, 3), Ok(3_333));
        // The mirror case: a third of a fall.
        assert_eq!(price_trend_bps(2, 3), Ok(-3_333));
    }

    #[test]
    fn trend_rejects_unusable_inputs() {
        // A non-positive reference makes ΔP undefined; a negative current price
        // is not an observation.
        assert_eq!(price_trend_bps(100, 0), Err(ContractError::YieldPriceNotPositive));
        assert_eq!(
            price_trend_bps(100, -1),
            Err(ContractError::YieldPriceNotPositive)
        );
        assert_eq!(
            price_trend_bps(-1, 100),
            Err(ContractError::YieldPriceNotPositive)
        );
    }

    #[test]
    fn trend_is_exact_at_i128_bounds() {
        // A doubling at the top of the range must not overflow the subtraction
        // or the scaling.
        assert_eq!(price_trend_bps(i128::MAX / 2, i128::MAX / 4), Ok(10_000));
        // A collapse to 1 wei is a ~100 % fall and must report, not wrap.
        assert_eq!(price_trend_bps(1, i128::MAX), Ok(-BPS_DENOMINATOR));
        // Identical maxima give exactly zero.
        assert_eq!(price_trend_bps(i128::MAX, i128::MAX), Ok(0));
    }

    // ── is_drawdown_exceeded ───────────────────────────────────────────────

    #[test]
    fn the_twenty_percent_boundary_is_strict() {
        // Exactly 20 % is tolerated; one unit further trips.
        assert!(!is_drawdown_exceeded(80, 100, DEFAULT_MAX_DRAWDOWN_BPS));
        assert!(is_drawdown_exceeded(79, 100, DEFAULT_MAX_DRAWDOWN_BPS));
    }

    #[test]
    fn a_rising_or_flat_price_is_never_a_drawdown() {
        assert!(!is_drawdown_exceeded(100, 100, DEFAULT_MAX_DRAWDOWN_BPS));
        assert!(!is_drawdown_exceeded(140, 100, DEFAULT_MAX_DRAWDOWN_BPS));
    }

    #[test]
    fn a_custom_limit_is_honoured() {
        assert!(!is_drawdown_exceeded(90, 100, 1_000));
        assert!(is_drawdown_exceeded(90, 100, 900));
        // A zero limit trips on any depreciation.
        assert!(!is_drawdown_exceeded(100, 100, 0));
        assert!(is_drawdown_exceeded(99, 100, 0));
    }

    #[test]
    fn an_unmeasurable_ratio_fails_closed() {
        assert!(is_drawdown_exceeded(100, 0, DEFAULT_MAX_DRAWDOWN_BPS));
        assert!(is_drawdown_exceeded(100, -1, DEFAULT_MAX_DRAWDOWN_BPS));
        assert!(is_drawdown_exceeded(-1, 100, DEFAULT_MAX_DRAWDOWN_BPS));
    }

    #[test]
    fn the_breach_decision_never_overflows() {
        // A 19.9 % fall measured across a near-i128 balance: the naive
        // `reference * 10_000` would wrap long before this.
        let reference = i128::MAX / 10_000;
        assert!(is_drawdown_exceeded(reference, reference * 2, DEFAULT_MAX_DRAWDOWN_BPS));
        assert!(!is_drawdown_exceeded(reference * 2, reference * 2, DEFAULT_MAX_DRAWDOWN_BPS));
    }

    // ── configuration ──────────────────────────────────────────────────────

    #[test]
    fn a_fresh_guard_is_unarmed_and_holds() {
        let f = Fixture::new();
        f.configure();
        let status = f.status();
        assert!(!status.armed);
        assert!(!status.breached);
        assert!(!status.compounding_paused);
        assert_eq!(status.trend_bps, 0);
        assert_eq!(status.max_drawdown_bps, DEFAULT_MAX_DRAWDOWN_BPS);
        assert!(f.guard().is_ok());
        // No measurable trend, so do not auto-reinvest.
        assert_eq!(f.action(), CompoundingAction::Hold);
    }

    #[test]
    fn an_unconfigured_vault_holds_and_has_no_status() {
        let f = Fixture::new();
        assert_eq!(
            f.frame(|| drawdown_status(&f.env, 999)),
            Err(ContractError::VaultNotInitialized)
        );
        assert_eq!(
            f.frame(|| strategy_action(&f.env, 999)),
            CompoundingAction::Hold
        );
    }

    #[test]
    fn configuration_rejects_a_reserve_that_is_also_the_reward() {
        let f = Fixture::new();
        let token_addr = f.reward_token.clone();
        assert_eq!(
            f.frame(|| {
                configure_guard(
                    &f.env,
                    &f.admin,
                    VAULT,
                    token_addr.clone(),
                    token_addr,
                    DEFAULT_MAX_DRAWDOWN_BPS,
                    TREND_WINDOW_SECONDS,
                )
            }),
            Err(ContractError::InvalidArgument)
        );
    }

    #[test]
    fn a_limit_outside_the_bounds_is_rejected() {
        let f = Fixture::new();
        for bad in [MIN_ALLOWED_DRAWDOWN_BPS - 1, MAX_ALLOWED_DRAWDOWN_BPS + 1] {
            let result = f.frame(|| {
                configure_guard(
                    &f.env,
                    &f.admin,
                    VAULT,
                    f.reward_token.clone(),
                    f.base_reserve.clone(),
                    bad,
                    TREND_WINDOW_SECONDS,
                )
            });
            assert_eq!(result, Err(ContractError::YieldInvalidDrawdownLimit));
        }
    }

    #[test]
    fn a_zero_window_is_rejected() {
        let f = Fixture::new();
        let result = f.frame(|| {
            configure_guard(
                &f.env,
                &f.admin,
                VAULT,
                f.reward_token.clone(),
                f.base_reserve.clone(),
                DEFAULT_MAX_DRAWDOWN_BPS,
                0,
            )
        });
        assert_eq!(result, Err(ContractError::InvalidArgument));
    }

    #[test]
    fn a_non_admin_cannot_configure_or_retune() {
        let f = Fixture::new();
        assert_eq!(
            f.frame(|| {
                configure_guard(
                    &f.env,
                    &f.keeper,
                    VAULT,
                    f.reward_token.clone(),
                    f.base_reserve.clone(),
                    DEFAULT_MAX_DRAWDOWN_BPS,
                    TREND_WINDOW_SECONDS,
                )
            }),
            Err(ContractError::NotAdmin)
        );
        f.configure();
        assert_eq!(
            f.frame(|| set_max_drawdown_bps(&f.env, &f.keeper, VAULT, 3_000)),
            Err(ContractError::NotAdmin)
        );
    }

    #[test]
    fn the_admin_can_retune_the_limit() {
        let f = Fixture::new();
        f.configure();
        let config = f
            .frame(|| set_max_drawdown_bps(&f.env, &f.admin, VAULT, 5_000))
            .expect("retune");
        assert_eq!(config.max_drawdown_bps, 5_000);
    }

    // ── sampling ───────────────────────────────────────────────────────────

    #[test]
    fn sampling_rejects_a_non_positive_price() {
        let f = Fixture::new();
        f.configure();
        assert_eq!(f.sample(0), Err(ContractError::YieldPriceNotPositive));
        assert_eq!(f.sample(-5), Err(ContractError::YieldPriceNotPositive));
        assert_eq!(f.samples().len(), 0);
    }

    #[test]
    fn the_guard_arms_once_a_full_window_has_elapsed() {
        let f = Fixture::new();
        f.configure();

        // Seven samples, none yet a full window old.
        for _ in 0..7 {
            let status = f.sample(100).expect("sample");
            assert!(!status.armed, "should stay unarmed before a full window");
        }
        // The eighth advance pushes the oldest sample past 7 days.
        let status = f.sample_armed(100);
        assert!(status.armed);
        assert_eq!(status.reference_price, 100);
        assert_eq!(status.current_price, 100);
        assert!(status.reference_age_secs >= TREND_WINDOW_SECONDS);
    }

    #[test]
    fn the_buffer_never_exceeds_its_capacity() {
        let f = Fixture::new();
        f.configure();
        for i in 0..40 {
            f.sample(100 + i).expect("sample");
        }
        assert_eq!(f.samples().len(), MAX_PRICE_SAMPLES as u32);
    }

    #[test]
    fn over_sampling_disarms_rather_than_corrupts_the_trend() {
        let f = Fixture::new();
        f.configure();
        // Nine samples inside a single ledger: the buffer is full but its
        // oldest entry is 0 seconds old, so there is no 7-day reference.
        for _ in 0..MAX_PRICE_SAMPLES {
            f.sample_burst(100).expect("sample");
        }
        assert_eq!(f.samples().len(), MAX_PRICE_SAMPLES);
        let status = f.status();
        assert!(!status.armed);
        assert_eq!(status.trend_bps, 0);
        assert!(!status.breached);
        assert_eq!(f.action(), CompoundingAction::Hold);
    }

    #[test]
    fn a_healthy_armed_trend_reinvests() {
        let f = Fixture::new();
        f.configure();
        for _ in 0..7 {
            f.sample(100).expect("sample");
        }
        let status = f.sample_armed(100);
        assert!(status.armed);
        assert!(!status.breached);
        assert_eq!(status.trend_bps, 0);
        assert_eq!(f.action(), CompoundingAction::Reinvest);
        assert!(f.guard().is_ok());
    }

    // ── the 20 % breach and resume ─────────────────────────────────────────

    #[test]
    fn a_twenty_percent_fall_trips_the_guard() {
        let f = Fixture::new();
        f.configure();
        for _ in 0..7 {
            f.sample(100).expect("sample");
        }
        // Exactly -20 %: tolerated, because the comparison is strict.
        let status = f.sample_armed(80);
        assert!(status.armed);
        assert_eq!(status.trend_bps, -2_000);
        assert!(!status.breached, "-20 % exactly must not trip the guard");
        assert!(!f.paused());
        assert_eq!(f.action(), CompoundingAction::Reinvest);
    }

    #[test]
    fn a_fall_past_twenty_percent_pauses_compounding() {
        let f = Fixture::new();
        f.configure();
        for _ in 0..7 {
            f.sample(100).expect("sample");
        }
        let status = f.sample_armed(79);
        assert_eq!(status.trend_bps, -2_100);
        assert!(status.breached);
        assert!(status.compounding_paused);
        assert!(status.compounding_paused_at > 0);
        assert!(f.paused());
        assert_eq!(f.guard(), Err(ContractError::YieldCompoundingPaused));
        assert_eq!(f.action(), CompoundingAction::ConvertToReserves);
    }

    #[test]
    fn a_recovering_trend_resumes_compounding() {
        let f = Fixture::new();
        f.configure();
        for _ in 0..7 {
            f.sample(100).expect("sample");
        }
        f.sample_armed(79);
        assert!(f.paused());

        // The reference has not rolled yet, so the next sample is compared
        // against the original 100: back to 100 is a flat trend.
        let status = f.sample(100).expect("sample");
        assert!(!status.breached);
        assert!(!status.compounding_paused);
        assert_eq!(status.compounding_paused_at, 0);
        assert!(!f.paused());
        assert!(f.guard().is_ok());
        assert_eq!(f.action(), CompoundingAction::Reinvest);
    }

    #[test]
    fn a_repeated_breach_does_not_re_emit() {
        let f = Fixture::new();
        f.configure();
        for _ in 0..7 {
            f.sample(100).expect("sample");
        }
        f.sample_armed(79);
        assert_eq!(f.emissions(&EV_YIELD_DRAWDOWN_PAUSED, &STATUS_PAUSED), 1);
        f.sample(78);
        f.sample(77);
        assert_eq!(f.emissions(&EV_YIELD_DRAWDOWN_PAUSED, &STATUS_PAUSED), 1);
    }

    #[test]
    fn the_pause_is_exactly_the_breach_flag() {
        let f = Fixture::new();
        f.configure();
        for _ in 0..7 {
            f.sample(100).expect("sample");
        }
        for price in [100i128, 90, 79, 70, 100, 79, 120] {
            let status = f.sample_armed(price);
            assert_eq!(
                status.compounding_paused, status.breached,
                "paused ⟺ breached at price {price}"
            );
        }
    }

    #[test]
    fn the_monitoring_tick_is_idempotent() {
        let f = Fixture::new();
        f.configure();
        for _ in 0..7 {
            f.sample(100).expect("sample");
        }
        f.sample_armed(79);
        let first = f.frame(|| sync_drawdown(&f.env, VAULT)).expect("sync");
        let second = f.frame(|| sync_drawdown(&f.env, VAULT)).expect("sync");
        assert_eq!(first, second);
        assert!(first.breached);
        assert!(first.compounding_paused);
    }

    #[test]
    fn raising_the_limit_can_resume_a_paused_vault() {
        let f = Fixture::new();
        f.configure();
        for _ in 0..7 {
            f.sample(100).expect("sample");
        }
        f.sample_armed(70);
        assert!(f.paused());

        // -30 % is past the 20 % default but inside a 50 % limit.
        f.frame(|| set_max_drawdown_bps(&f.env, &f.admin, VAULT, 5_000))
            .expect("retune");
        let status = f.frame(|| sync_drawdown(&f.env, VAULT)).expect("sync");
        assert_eq!(status.trend_bps, -3_000);
        assert!(!status.breached);
        assert!(!status.compounding_paused);
    }

    // ── reserve conversion ─────────────────────────────────────────────────

    #[test]
    fn conversion_requires_a_configured_guard() {
        let f = Fixture::new();
        assert_eq!(
            f.convert(REWARD, 1),
            Err(ContractError::VaultNotInitialized)
        );
    }

    #[test]
    fn conversion_books_measured_reserves() {
        let f = Fixture::new();
        f.configure();
        f.fund_vault(BIG_REWARD);
        f.route_rate(1, 1, false);

        let out = f.convert(REWARD, REWARD).expect("convert");
        // The router claims i128::MAX; the vault must book the measured 1_000.
        assert_eq!(out.reward_spent, REWARD);
        assert_eq!(out.reserve_received, REWARD);
        assert_eq!(f.reserved(), REWARD);
    }

    #[test]
    fn conversion_spends_only_the_requested_rewards() {
        let f = Fixture::new();
        f.configure();
        f.fund_vault(BIG_REWARD);
        f.route_rate(1, 1, false);

        f.convert(REWARD, REWARD).expect("convert");
        let leftover = token::Client::new(&f.env, &f.reward_token).balance(&f.vault_id);
        assert_eq!(leftover, BIG_REWARD - REWARD);
    }

    #[test]
    fn conversion_refuses_to_spend_more_than_accrued() {
        let f = Fixture::new();
        f.configure();
        f.fund_vault(REWARD);
        assert_eq!(
            f.convert(REWARD + 1, 1),
            Err(ContractError::InsufficientReserveBalance)
        );
        assert_eq!(f.reserved(), 0);
    }

    #[test]
    fn conversion_rejects_a_non_positive_amount_or_minimum() {
        let f = Fixture::new();
        f.configure();
        f.fund_vault(BIG_REWARD);
        assert_eq!(f.convert(0, 1), Err(ContractError::AmountTooLow));
        assert_eq!(
            f.convert(REWARD, 0),
            Err(ContractError::AmountTooLow)
        );
    }

    #[test]
    fn conversion_rejects_a_route_for_the_wrong_assets() {
        let f = Fixture::new();
        f.configure();
        f.fund_vault(BIG_REWARD);
        let other = f.recipient.clone();
        let result = f.frame(|| {
            let path = soroban_sdk::vec![&f.env, f.reward_token.clone(), other];
            convert_rewards_to_reserves(
                &f.env,
                &f.keeper,
                f.router.clone(),
                VAULT,
                path,
                REWARD,
                1,
            )
        });
        assert_eq!(result, Err(ContractError::YieldInvalidConversionPath));
    }

    #[test]
    fn conversion_rejects_a_short_route() {
        let f = Fixture::new();
        f.configure();
        let result = f.frame(|| {
            let path = soroban_sdk::vec![&f.env, f.reward_token.clone()];
            convert_rewards_to_reserves(
                &f.env,
                &f.keeper,
                f.router.clone(),
                VAULT,
                path,
                REWARD,
                1,
            )
        });
        assert_eq!(result, Err(ContractError::YieldInvalidConversionPath));
    }

    #[test]
    fn conversion_enforces_slippage_locally() {
        let f = Fixture::new();
        f.configure();
        f.fund_vault(BIG_REWARD);
        // Pays half of what was asked for.
        f.route_rate(1, 2, false);

        assert_eq!(
            f.convert(REWARD, REWARD),
            Err(ContractError::SlippageExceeded)
        );
        // Nothing is booked on the rejected path.
        assert_eq!(f.reserved(), 0);
    }

    #[test]
    fn conversion_fails_when_the_router_delivers_nothing() {
        let f = Fixture::new();
        f.configure();
        f.fund_vault(BIG_REWARD);
        f.route_rate(1, 1, true);

        assert_eq!(
            f.convert(REWARD, 1),
            Err(ContractError::YieldConversionProducedNothing)
        );
        assert_eq!(f.reserved(), 0);
    }

    #[test]
    fn conversion_works_exactly_while_the_guard_is_paused() {
        let f = Fixture::new();
        f.configure();
        for _ in 0..7 {
            f.sample(100).expect("sample");
        }
        f.sample_armed(50);
        assert!(f.paused());
        assert_eq!(f.action(), CompoundingAction::ConvertToReserves);

        f.fund_vault(BIG_REWARD);
        f.route_rate(1, 1, false);
        // The de-risk path is the one that must remain open.
        assert!(f.convert(REWARD, REWARD).is_ok());
        assert_eq!(f.reserved(), REWARD);
        // Re-investing is still refused.
        assert_eq!(f.guard(), Err(ContractError::YieldCompoundingPaused));
    }

    // ── reserve sweep ──────────────────────────────────────────────────────

    #[test]
    fn reserves_accumulate_across_conversions() {
        let f = Fixture::new();
        f.configure();
        f.fund_vault(BIG_REWARD);
        f.route_rate(1, 1, false);
        f.convert(REWARD, REWARD).expect("first");
        f.convert(OTHER_REWARD, OTHER_REWARD).expect("second");
        assert_eq!(f.reserved(), REWARD + OTHER_REWARD);
    }

    #[test]
    fn a_non_admin_cannot_sweep_reserves() {
        let f = Fixture::new();
        f.configure();
        assert_eq!(
            f.frame(|| sweep_reserves(&f.env, &f.keeper, f.recipient.clone(), VAULT, 1)),
            Err(ContractError::NotAdmin)
        );
    }

    #[test]
    fn a_sweep_cannot_exceed_the_booked_balance() {
        let f = Fixture::new();
        f.configure();
        f.fund_vault(BIG_REWARD);
        f.route_rate(1, 1, false);
        f.convert(REWARD, REWARD).expect("convert");

        assert_eq!(
            f.frame(|| {
                sweep_reserves(&f.env, &f.admin, f.recipient.clone(), VAULT, REWARD + 1)
            }),
            Err(ContractError::InsufficientReserveBalance)
        );
        assert_eq!(f.reserved(), REWARD);
    }

    #[test]
    fn a_sweep_moves_the_reserves_out_and_decrements_the_booking() {
        let f = Fixture::new();
        f.configure();
        f.fund_vault(BIG_REWARD);
        f.route_rate(1, 1, false);
        f.convert(REWARD, REWARD).expect("convert");

        let swept = f
            .frame(|| {
                sweep_reserves(&f.env, &f.admin, f.recipient.clone(), VAULT, REWARD / 2)
            })
            .expect("sweep");
        assert_eq!(swept, REWARD / 2);
        assert_eq!(f.reserved(), REWARD - REWARD / 2);

        let recipient_balance =
            token::Client::new(&f.env, &f.base_reserve).balance(&f.recipient);
        assert_eq!(recipient_balance, REWARD / 2);
    }

    #[test]
    fn a_sweep_rejects_a_non_positive_amount() {
        let f = Fixture::new();
        f.configure();
        assert_eq!(
            f.frame(|| sweep_reserves(&f.env, &f.admin, f.recipient.clone(), VAULT, 0)),
            Err(ContractError::AmountTooLow)
        );
    }

    // ── events ─────────────────────────────────────────────────────────────

    #[test]
    fn the_configure_event_is_emitted() {
        let f = Fixture::new();
        f.configure();
        assert!(f.emissions(&EV_YIELD_GUARD_CONFIGURED, &STATUS_CONFIGURED) >= 1);
    }

    #[test]
    fn the_sample_event_is_emitted() {
        let f = Fixture::new();
        f.configure();
        f.sample(100).expect("sample");
        assert!(f.emissions(&EV_YIELD_PRICE_SAMPLE, &STATUS_SAMPLED) >= 1);
    }

    #[test]
    fn pausing_and_resuming_emit_one_transition_each() {
        let f = Fixture::new();
        f.configure();
        for _ in 0..7 {
            f.sample(100).expect("sample");
        }
        f.sample_armed(79);
        assert_eq!(f.emissions(&EV_YIELD_DRAWDOWN_PAUSED, &STATUS_PAUSED), 1);
        assert_eq!(f.emissions(&EV_YIELD_DRAWDOWN_RESUMED, &STATUS_RESUMED), 0);

        f.sample(100);
        assert_eq!(f.emissions(&EV_YIELD_DRAWDOWN_RESUMED, &STATUS_RESUMED), 1);
    }

    #[test]
    fn conversion_and_sweep_events_are_emitted() {
        let f = Fixture::new();
        f.configure();
        f.fund_vault(BIG_REWARD);
        f.route_rate(1, 1, false);
        f.convert(REWARD, REWARD).expect("convert");
        assert!(f.emissions(&EV_YIELD_REWARDS_CONVERTED, &STATUS_CONVERTED) >= 1);

        f.frame(|| sweep_reserves(&f.env, &f.admin, f.recipient.clone(), VAULT, 1))
            .expect("sweep");
        assert!(f.emissions(&EV_YIELD_RESERVES_SWEEPED, &STATUS_SWEEPED) >= 1);
    }

    #[test]
    fn a_healthy_vault_does_not_emit_a_pause_event() {
        let f = Fixture::new();
        f.configure();
        for _ in 0..7 {
            f.sample(100).expect("sample");
        }
        f.sample_armed(100);
        assert_eq!(f.emissions(&EV_YIELD_DRAWDOWN_PAUSED, &STATUS_PAUSED), 0);
    }
}
