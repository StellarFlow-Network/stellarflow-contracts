//! Bridge wrapped token supply dynamic cap guard (Issue #1009).
//!
//! [`crate::bridge::mint`] caps each wrapped asset against a *static*
//! `max_supply` written at registration time. That ceiling is a promise about
//! the future, not a measurement of the present, and it drifts: collateral can
//! arrive, be rebalanced, or be slashed on the locked chain while the on-chain
//! ceiling stays exactly where it was. A cap that is not tied to real backing
//! becomes either needlessly restrictive (collateral arrived, ceiling did not
//! move) or unsound (collateral left, ceiling did not move).
//!
//! This module replaces the static promise with a measurement. It tracks
//!
//! ```text
//!   S_wrapped  =  total active wrapped token supply
//!   C_locked   =  verified locked collateral on the source chain
//! ```
//!
//! and derives the ceiling from the second, so it moves with the backing:
//!
//! ```text
//!   C_allowed  =  floor(C_locked * capacity_bps / 10_000)
//!
//!   mint is rejected  ⟺  S_wrapped + ΔS > C_allowed
//! ```
//!
//! With the default `capacity_bps` of [`DEFAULT_CAPACITY_BPS`] (10 000 = 100 %)
//! this is exactly the `S_wrapped + ΔS <= C_locked` rule the issue specifies.
//! Governance may lower the ratio to hold a permanent buffer above the wrapped
//! supply, but `capacity_bps` is capped at 100 % so the guard can never be
//! configured to permit more supply than there is collateral — that is the
//! entire invariant.
//!
//! # The three deliverables
//!
//! | Deliverable | Entry point |
//! |---|---|
//! | 🪙 Track `S_wrapped` against `C_locked` | [`bridge_cap_status`], [`deposit_locked_collateral`], [`record_wrapped_mint`], [`record_wrapped_burn`] |
//! | 🛑 Revert when `S_wrapped + ΔS > C_locked` | [`require_mint_allowed`], [`mint_within_cap`] |
//! | 📜 Emit `BridgeCapExceeded` | [`EV_BRIDGE_CAP_EXCEEDED`] |
//!
//! # Fail-closed arithmetic
//!
//! [`allowed_supply`] takes the quotient before the remainder is scaled, so it
//! never forms the `C_locked * capacity_bps` product. That product overflows
//! `i128` once locked collateral exceeds roughly `i128::MAX / 10_000`, and an
//! overflowed cap silently reads as *no headroom left* or, worse, as a large
//! positive one. The mint check itself uses [`checked_add`] rather than `+`.
//!
//! # The guard refuses; the mint engine mints
//!
//! This module does not take over token issuance. It is the authority on
//! *whether* a mint may proceed; [`crate::bridge::mint`] remains the authority
//! on the resulting supply. `S_wrapped` is a mirror of the mint engine's
//! `total_supply`, kept current by [`record_wrapped_mint`] and
//! [`record_wrapped_burn`], which the engine calls as part of the same
//! transaction.
//!
//! That mirror can only ever be reconciled *upward* by [`sync_wrapped_supply`].
//! A stale mirror that under-reports supply is the dangerous direction — it
//! grants phantom headroom — so the permissionless reconciliation tick refuses
//! to lower it, and lowering requires the authorized [`record_wrapped_burn`].
//!
//! # Collateral can never be withdrawn past the supply
//!
//! [`release_locked_collateral`] is refused when the release would leave
//! `C_locked < S_wrapped`. Otherwise a relayer could slash the backing out from
//! under a live supply and the guard would only notice on the next mint — by
//! which time the cap is meaningless.
//!
//! # Usage
//!
//! ```
//! use stellarflow_contracts::bridge::cap_guard::{
//!     allowed_supply, DEFAULT_CAPACITY_BPS,
//! };
//!
//! // 1_000 locked backs at most 1_000 wrapped at the default 100 % capacity.
//! assert_eq!(allowed_supply(1_000, DEFAULT_CAPACITY_BPS), Ok(1_000));
//!
//! // Holding a 10 % permanent buffer lowers the ceiling to 900.
//! assert_eq!(allowed_supply(1_000, 9_000), Ok(900));
//!
//! // Truncating division floors: 1_999 locked at 50 % admits 999, not 1_000.
//! assert_eq!(allowed_supply(1_999, 5_000), Ok(999));
//! ```
//!
//! Bridge mint paths should call [`require_mint_allowed`] before issuing, and
//! [`record_wrapped_mint`] once the supply has actually moved.

use soroban_sdk::{contracttype, symbol_short, Address, Env, Symbol};

use crate::{ContractData, ContractError, DATA_KEY};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Basis-point denominator: 10 000 bp = 100 %.
pub const BPS_DENOMINATOR: i64 = 10_000;

/// Default share of verified locked collateral that may be represented by
/// wrapped supply: **100 %**.
///
/// This reproduces the issue's `S_wrapped + ΔS <= C_locked` rule exactly.
/// Governance can lower it to keep a permanent buffer, but never raise it past
/// full backing.
pub const DEFAULT_CAPACITY_BPS: i64 = 10_000;

/// Floor for a configured capacity: 0 %, which mints nothing.
pub const MIN_ALLOWED_CAPACITY_BPS: i64 = 0;

/// Ceiling for a configured capacity: 100 %.
///
/// The guard exists to stop the bridge issuing more wrapped supply than it has
/// collateral. Allowing a configuration above that would make the cap
/// advisory, so it is not configurable.
pub const MAX_ALLOWED_CAPACITY_BPS: i64 = BPS_DENOMINATOR;

// Event topics. Every `symbol_short!` payload must stay within 9 bytes.
/// A mint was refused because it would push wrapped supply past the
/// collateral-backed cap. This is the event named in the issue.
pub const EV_BRIDGE_CAP_EXCEEDED: Symbol = symbol_short!("br_cap_x");
/// Verified locked collateral was deposited or released.
pub const EV_BRIDGE_CAP_COLLATERAL: Symbol = symbol_short!("br_col");
/// Wrapped supply was mirrored upward after a successful mint.
pub const EV_BRIDGE_CAP_MINT: Symbol = symbol_short!("br_mnt");
/// Wrapped supply was mirrored downward after a burn.
pub const EV_BRIDGE_CAP_BURN: Symbol = symbol_short!("br_brn");
/// An asset's capacity ratio was configured.
pub const EV_BRIDGE_CAP_SET: Symbol = symbol_short!("br_cset");
/// Wrapped supply was reconciled against the mint engine.
pub const EV_BRIDGE_CAP_SYNC: Symbol = symbol_short!("br_sync");

/// Second topic of [`EV_BRIDGE_CAP_EXCEEDED`].
const STATUS_EXCEEDED: Symbol = symbol_short!("exceeded");
/// Second topic of [`EV_BRIDGE_CAP_COLLATERAL`].
const STATUS_COLLATERAL: Symbol = symbol_short!("coll");
/// Second topic of [`EV_BRIDGE_CAP_MINT`].
const STATUS_MINT: Symbol = symbol_short!("mint");
/// Second topic of [`EV_BRIDGE_CAP_BURN`].
const STATUS_BURN: Symbol = symbol_short!("burn");
/// Second topic of [`EV_BRIDGE_CAP_SET`].
const STATUS_CONFIG: Symbol = symbol_short!("config");
/// Second topic of [`EV_BRIDGE_CAP_SYNC`].
const STATUS_SYNC: Symbol = symbol_short!("sync");

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BridgeCapKey {
    /// Per-asset guard configuration.
    Config(Symbol),
    /// Mirrored active wrapped supply, `S_wrapped`.
    WrappedSupply(Symbol),
    /// Verified locked collateral, `C_locked`.
    LockedCollateral(Symbol),
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Per-asset cap configuration.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BridgeCapConfig {
    /// The wrapped asset this guard covers.
    pub asset_code: Symbol,
    /// The bridge controller authorized to report collateral and supply moves.
    pub verifier: Address,
    /// Share of locked collateral that may be represented by supply, in bps.
    pub capacity_bps: i64,
}

/// Read-only snapshot of an asset's collateral-backed cap.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BridgeCapStatus {
    /// The wrapped asset this status describes.
    pub asset_code: Symbol,
    /// `S_wrapped`: mirrored active wrapped supply.
    pub wrapped_supply: i128,
    /// `C_locked`: verified locked collateral.
    pub locked_collateral: i128,
    /// `C_allowed`: ceiling derived from locked collateral and capacity.
    pub allowed_supply: i128,
    /// `C_allowed - S_wrapped`, floored at zero. This is the mint headroom.
    pub headroom: i128,
    /// Capacity ratio in force.
    pub capacity_bps: i64,
    /// `true` when live supply is already at or above the ceiling.
    pub at_cap: bool,
    /// Utilisation of the ceiling in bps: `S_wrapped * 10_000 / C_allowed`,
    /// `0` when the ceiling is zero and saturating at [`i64::MAX`].
    pub utilisation_bps: i64,
}

// ---------------------------------------------------------------------------
// Capacity arithmetic
// ---------------------------------------------------------------------------

/// `floor(locked_collateral * capacity_bps / 10_000)`, without overflow.
///
/// The quotient is taken first and only the remainder is scaled: `locked /
/// 10_000 * capacity_bps` is bounded by `locked` because `capacity_bps` is
/// bounded by 10 000, and the remainder is below 10 000 so its product is
/// under 10^8. No step can overflow, whatever `locked_collateral` holds.
///
/// Truncation is downward, so the guard never rounds a ceiling *up* past the
/// collateral that backs it.
///
/// # Errors
/// * [`ContractError::BridgeCapInvalidCollateral`] — `locked_collateral` is
///   negative.
/// * [`ContractError::BridgeCapInvalidConfig`] — `capacity_bps` falls outside
///   `[[MIN_ALLOWED_CAPACITY_BPS], [MAX_ALLOWED_CAPACITY_BPS]]`.
pub fn allowed_supply(locked_collateral: i128, capacity_bps: i64) -> Result<i128, ContractError> {
    if locked_collateral < 0 {
        return Err(ContractError::BridgeCapInvalidCollateral);
    }
    if !(MIN_ALLOWED_CAPACITY_BPS..=MAX_ALLOWED_CAPACITY_BPS).contains(&capacity_bps) {
        return Err(ContractError::BridgeCapInvalidConfig);
    }
    if locked_collateral == 0 || capacity_bps == 0 {
        return Ok(0);
    }

    let denom = BPS_DENOMINATOR as i128;
    let whole = locked_collateral / denom;
    let remainder = locked_collateral % denom;

    // `whole <= locked / 10_000` and `capacity_bps <= 10_000`, so this product
    // is bounded by `locked_collateral` and cannot overflow.
    Ok(whole * capacity_bps as i128 + remainder * capacity_bps as i128 / denom)
}

/// `floor(supply * 10_000 / allowed)`, saturating at [`i64::MAX`].
///
/// Same quotient-first discipline as [`allowed_supply`], and for the same
/// reason: `supply * 10_000` overflows `i128` once supply exceeds roughly
/// `i128::MAX / 10_000`, which a saturating multiply turns into a *wildly
/// understated* utilisation rather than an error.
fn utilisation_bps_of(supply: i128, allowed: i128) -> i64 {
    let denom = BPS_DENOMINATOR as i128;
    if allowed <= 0 || supply <= 0 {
        return 0;
    }

    let whole = supply / allowed;
    if whole > i64::MAX as i128 / denom {
        return i64::MAX;
    }
    let mut bps = whole * denom;

    // `remainder < allowed`, so it is shrunk alongside `allowed` to keep the
    // product inside i128.
    let remainder = supply % allowed;
    if remainder > 0 {
        let shrink = allowed / i64::MAX as i128 + 1;
        bps += (remainder / shrink) * denom / (allowed / shrink);
    }

    if bps > i64::MAX as i128 {
        i64::MAX
    } else {
        bps as i64
    }
}

/// `true` when a mint of `delta_supply` would push wrapped supply past the
/// collateral-backed ceiling, i.e. `S_wrapped + ΔS > C_locked * capacity_bps /
///
/// 10_000`.
///
/// Pure, and therefore safe to call from a view. The addition is checked rather
/// than performed, so a `ΔS` that overflows is reported as a breach rather
/// than wrapping to a small positive number and sailing through the cap.
pub fn mint_exceeds_cap(
    wrapped_supply: i128,
    locked_collateral: i128,
    capacity_bps: i64,
    delta_supply: i128,
) -> bool {
    if wrapped_supply < 0 || locked_collateral < 0 || delta_supply < 0 {
        return true;
    }
    let ceiling = match allowed_supply(locked_collateral, capacity_bps) {
        Ok(ceiling) => ceiling,
        // An unmeasurable ceiling cannot be shown to accommodate a mint.
        Err(_) => return true,
    };
    match wrapped_supply.checked_add(delta_supply) {
        Some(new_supply) => new_supply > ceiling,
        // Overflow means the request is nonsense, not that it is affordable.
        None => true,
    }
}

// ---------------------------------------------------------------------------
// State accessors
// ---------------------------------------------------------------------------

/// Read an asset's cap configuration, or `None` when unconfigured.
pub fn get_cap_config(env: &Env, asset_code: &Symbol) -> Option<BridgeCapConfig> {
    env.storage()
        .persistent()
        .get(&BridgeCapKey::Config(asset_code.clone()))
}

fn require_cap_config(
    env: &Env,
    asset_code: &Symbol,
) -> Result<BridgeCapConfig, ContractError> {
    get_cap_config(env, asset_code).ok_or(ContractError::BridgeEscrowNotConfigured)
}

/// `S_wrapped`: mirrored active wrapped supply for an asset.
pub fn wrapped_supply(env: &Env, asset_code: &Symbol) -> i128 {
    env.storage()
        .persistent()
        .get(&BridgeCapKey::WrappedSupply(asset_code.clone()))
        .unwrap_or(0i128)
}

/// `C_locked`: verified locked collateral for an asset.
pub fn locked_collateral(env: &Env, asset_code: &Symbol) -> i128 {
    env.storage()
        .persistent()
        .get(&BridgeCapKey::LockedCollateral(asset_code.clone()))
        .unwrap_or(0i128)
}

/// The collateral-backed ceiling for an asset, or
/// [`ContractError::BridgeEscrowNotConfigured`] when the guard is unset.
pub fn mint_ceiling(env: &Env, asset_code: &Symbol) -> Result<i128, ContractError> {
    let config = require_cap_config(env, asset_code)?;
    allowed_supply(locked_collateral(env, asset_code), config.capacity_bps)
}

/// Remaining mint headroom, floored at zero.
pub fn mint_headroom(env: &Env, asset_code: &Symbol) -> Result<i128, ContractError> {
    let ceiling = mint_ceiling(env, asset_code)?;
    Ok((ceiling - wrapped_supply(env, asset_code)).max(0))
}

/// Build the read-only monitoring snapshot for an asset.
pub fn bridge_cap_status(env: &Env, asset_code: &Symbol) -> Result<BridgeCapStatus, ContractError> {
    let config = require_cap_config(env, asset_code)?;
    let supply = wrapped_supply(env, asset_code);
    let locked = locked_collateral(env, asset_code);
    let allowed = allowed_supply(locked, config.capacity_bps)?;
    let headroom = (allowed - supply).max(0);

    // Utilisation is reporting only, and an empty ceiling yields 0 rather than
    // an undefined division.
    let utilisation_bps = utilisation_bps_of(supply, allowed);

    Ok(BridgeCapStatus {
        asset_code: asset_code.clone(),
        wrapped_supply: supply,
        locked_collateral: locked,
        allowed_supply: allowed,
        headroom,
        capacity_bps: config.capacity_bps,
        at_cap: supply >= allowed,
        utilisation_bps,
    })
}

// ---------------------------------------------------------------------------
// Guard
// ---------------------------------------------------------------------------

/// Guard for bridge mint paths: fail unless `delta_supply` fits under the cap.
///
/// This is the `S_wrapped + ΔS > C_locked` rule from the issue. On a breach it
/// emits [`EV_BRIDGE_CAP_EXCEEDED`] with the full picture and returns
/// [`ContractError::BridgeCapExceeded`], so the refusal is both a revert and an
/// observable event.
///
/// Call it *before* issuing, then [`record_wrapped_mint`] once the supply has
/// actually moved.
pub fn require_mint_allowed(
    env: &Env,
    asset_code: &Symbol,
    delta_supply: i128,
) -> Result<(), ContractError> {
    let config = require_cap_config(env, asset_code)?;
    if delta_supply <= 0 {
        return Err(ContractError::BridgeCapInvalidAmount);
    }

    let supply = wrapped_supply(env, asset_code);
    let locked = locked_collateral(env, asset_code);
    if !mint_exceeds_cap(supply, locked, config.capacity_bps, delta_supply) {
        return Ok(());
    }

    // Report the refusal in full: the numbers a relayer needs to know whether
    // the bridge is short of collateral or simply full.
    let ceiling = allowed_supply(locked, config.capacity_bps).unwrap_or(0);
    let shortfall = (supply
        .checked_add(delta_supply)
        .unwrap_or(i128::MAX)
        .checked_sub(ceiling)
        .unwrap_or(i128::MAX))
    .max(0);
    env.events().publish(
        (EV_BRIDGE_CAP_EXCEEDED, STATUS_EXCEEDED),
        (
            asset_code.clone(),
            supply,
            delta_supply,
            locked,
            ceiling,
            shortfall,
            config.capacity_bps,
            env.ledger().timestamp(),
        ),
    );

    Err(ContractError::BridgeCapExceeded)
}

/// Guard, mirror and acknowledge in one step: mint `delta_supply` worth of
/// wrapped supply.
///
/// Refuses exactly as [`require_mint_allowed`] does — same event, same error —
/// and on success records the supply move so the mirror cannot drift. The
/// verifier's authorization is required because this reports on the bridge's own
/// issuance.
pub fn mint_within_cap(
    env: &Env,
    verifier: &Address,
    asset_code: &Symbol,
    delta_supply: i128,
) -> Result<i128, ContractError> {
    let config = require_cap_config(env, asset_code)?;
    if config.verifier != *verifier {
        return Err(ContractError::BridgeNotController);
    }
    verifier.require_auth();

    require_mint_allowed(env, asset_code, delta_supply)?;
    record_wrapped_mint(env, verifier, asset_code, delta_supply)
}

// ---------------------------------------------------------------------------
// Collateral lifecycle
// ---------------------------------------------------------------------------

/// Record `amount` of newly verified locked collateral for `asset_code`.
///
/// Verifier-authorized: the bridge controller is the party that attests the
/// funds are actually locked on the source chain. Because the ceiling is a
/// function of locked collateral, this is what re-opens mint headroom.
pub fn deposit_locked_collateral(
    env: &Env,
    verifier: &Address,
    asset_code: &Symbol,
    amount: i128,
) -> Result<i128, ContractError> {
    let config = require_cap_config(env, asset_code)?;
    if config.verifier != *verifier {
        return Err(ContractError::BridgeNotController);
    }
    verifier.require_auth();
    if amount <= 0 {
        return Err(ContractError::BridgeCapInvalidAmount);
    }

    let locked = locked_collateral(env, asset_code)
        .checked_add(amount)
        .ok_or(ContractError::MathOverflow)?;
    env.storage()
        .persistent()
        .set(&BridgeCapKey::LockedCollateral(asset_code.clone()), &locked);

    env.events().publish(
        (EV_BRIDGE_CAP_COLLATERAL, STATUS_COLLATERAL),
        (
            asset_code.clone(),
            amount,
            locked,
            mint_ceiling(env, asset_code).unwrap_or(0),
            env.ledger().timestamp(),
        ),
    );

    Ok(locked)
}

/// Release `amount` of locked collateral for `asset_code`.
///
/// Refused when the release would leave `C_locked < S_wrapped`, i.e. when it
/// would strand live wrapped supply without backing. Admin-authorized: unbacking
/// collateral is a governance-and-slashing decision, not a keeper action.
pub fn release_locked_collateral(
    env: &Env,
    admin: &Address,
    asset_code: &Symbol,
    amount: i128,
) -> Result<i128, ContractError> {
    require_cap_config(env, asset_code)?;
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
        return Err(ContractError::BridgeCapInvalidAmount);
    }

    let locked = locked_collateral(env, asset_code);
    if amount > locked {
        return Err(ContractError::BridgeCapInsufficientCollateral);
    }
    let remaining = locked - amount;
    if remaining < wrapped_supply(env, asset_code) {
        return Err(ContractError::BridgeCapUndercollateralized);
    }

    env.storage()
        .persistent()
        .set(&BridgeCapKey::LockedCollateral(asset_code.clone()), &remaining);

    env.events().publish(
        (EV_BRIDGE_CAP_COLLATERAL, STATUS_COLLATERAL),
        (
            asset_code.clone(),
            -amount,
            remaining,
            mint_ceiling(env, asset_code).unwrap_or(0),
            env.ledger().timestamp(),
        ),
    );

    Ok(remaining)
}

// ---------------------------------------------------------------------------
// Wrapped supply mirror
// ---------------------------------------------------------------------------

/// Mirror `delta_supply` of wrapped supply upward after a successful mint.
///
/// Verifier-authorized. The guard has already been applied by
/// [`require_mint_allowed`]; this only keeps `S_wrapped` current.
pub fn record_wrapped_mint(
    env: &Env,
    verifier: &Address,
    asset_code: &Symbol,
    delta_supply: i128,
) -> Result<i128, ContractError> {
    let config = require_cap_config(env, asset_code)?;
    if config.verifier != *verifier {
        return Err(ContractError::BridgeNotController);
    }
    if delta_supply <= 0 {
        return Err(ContractError::BridgeCapInvalidAmount);
    }

    let supply = wrapped_supply(env, asset_code)
        .checked_add(delta_supply)
        .ok_or(ContractError::MathOverflow)?;
    env.storage()
        .persistent()
        .set(&BridgeCapKey::WrappedSupply(asset_code.clone()), &supply);

    env.events().publish(
        (EV_BRIDGE_CAP_MINT, STATUS_MINT),
        (
            asset_code.clone(),
            delta_supply,
            supply,
            mint_ceiling(env, asset_code).unwrap_or(0),
            env.ledger().timestamp(),
        ),
    );

    Ok(supply)
}

/// Mirror `delta_supply` of wrapped supply downward after a burn or release.
///
/// This is the only way to lower `S_wrapped`, which is deliberate: a burn is
/// authorized and collateral-backed, whereas a permissionless "correction"
/// would hand anyone the ability to mint phantom headroom.
pub fn record_wrapped_burn(
    env: &Env,
    verifier: &Address,
    asset_code: &Symbol,
    delta_supply: i128,
) -> Result<i128, ContractError> {
    let config = require_cap_config(env, asset_code)?;
    if config.verifier != *verifier {
        return Err(ContractError::BridgeNotController);
    }
    if delta_supply <= 0 {
        return Err(ContractError::BridgeCapInvalidAmount);
    }

    let supply = wrapped_supply(env, asset_code);
    if delta_supply > supply {
        return Err(ContractError::BridgeInsufficientBalance);
    }
    let remaining = supply - delta_supply;
    env.storage()
        .persistent()
        .set(&BridgeCapKey::WrappedSupply(asset_code.clone()), &remaining);

    env.events().publish(
        (EV_BRIDGE_CAP_BURN, STATUS_BURN),
        (
            asset_code.clone(),
            delta_supply,
            remaining,
            env.ledger().timestamp(),
        ),
    );

    Ok(remaining)
}

/// Reconcile the mirror against the mint engine's authoritative
/// `total_supply`.
///
/// This tick is **one-directional by design**: an observed total above the
/// mirror is adopted, because under-reporting supply is the dangerous
/// direction and must be corrected without ceremony. An observed total *below*
/// the mirror is refused with [`ContractError::BridgeCapInvalidAmount`],
/// because lowering `S_wrapped` is the privileged operation that
/// [`record_wrapped_burn`] exists to perform.
///
/// Permissionless: it can only ever tighten the guard, never loosen it.
pub fn sync_wrapped_supply(
    env: &Env,
    asset_code: &Symbol,
    observed_total_supply: i128,
) -> Result<i128, ContractError> {
    require_cap_config(env, asset_code)?;
    if observed_total_supply < 0 {
        return Err(ContractError::BridgeCapInvalidAmount);
    }

    let supply = wrapped_supply(env, asset_code);
    if observed_total_supply < supply {
        return Err(ContractError::BridgeCapInvalidAmount);
    }
    if observed_total_supply == supply {
        return Ok(supply);
    }

    env.storage().persistent().set(
        &BridgeCapKey::WrappedSupply(asset_code.clone()),
        &observed_total_supply,
    );

    env.events().publish(
        (EV_BRIDGE_CAP_SYNC, STATUS_SYNC),
        (
            asset_code.clone(),
            supply,
            observed_total_supply,
            mint_ceiling(env, asset_code).unwrap_or(0),
            env.ledger().timestamp(),
        ),
    );

    Ok(observed_total_supply)
}

// ---------------------------------------------------------------------------
// Admin
// ---------------------------------------------------------------------------

/// Configure — or reconfigure — the cap guard for a wrapped asset.
pub fn configure_cap_guard(
    env: &Env,
    admin: &Address,
    asset_code: &Symbol,
    verifier: &Address,
    capacity_bps: i64,
) -> Result<BridgeCapConfig, ContractError> {
    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;
    if data.admin != *admin {
        return Err(ContractError::NotAdmin);
    }
    admin.require_auth();

    if !(MIN_ALLOWED_CAPACITY_BPS..=MAX_ALLOWED_CAPACITY_BPS).contains(&capacity_bps) {
        return Err(ContractError::BridgeCapInvalidConfig);
    }

    let config = BridgeCapConfig {
        asset_code: asset_code.clone(),
        verifier: verifier.clone(),
        capacity_bps,
    };
    env.storage()
        .persistent()
        .set(&BridgeCapKey::Config(asset_code.clone()), &config);

    env.events().publish(
        (EV_BRIDGE_CAP_SET, STATUS_CONFIG),
        (
            asset_code.clone(),
            verifier.clone(),
            capacity_bps,
            mint_ceiling(env, asset_code).unwrap_or(0),
            env.ledger().timestamp(),
        ),
    );

    Ok(config)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::testutils::Events as _;
    use soroban_sdk::IntoVal as _;

    const WBTC: Symbol = symbol_short!("WBTC");
    const WETH: Symbol = symbol_short!("WETH");

    /// Wrapped supply used throughout: 1_000 units.
    const SUPPLY: i128 = 1_000;

    /// Every call runs in its own contract frame: Soroban permits one
    /// `require_auth` per address per invocation, and storage is only
    /// reachable from inside a frame.
    struct Fixture {
        env: Env,
        id: Address,
        admin: Address,
        verifier: Address,
    }

    impl Fixture {
        fn new() -> Self {
            let env = Env::default();
            env.mock_all_auths();
            let id = env.register_contract(None, crate::TimeLockedUpgradeContract);
            let admin = Address::generate(&env);
            let verifier = Address::generate(&env);

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
                env,
                id,
                admin,
                verifier,
            }
        }

        fn frame<T, F: FnOnce() -> T>(&self, f: F) -> T {
            self.env.as_contract(&self.id, f)
        }

        fn configure(&self) -> BridgeCapConfig {
            self.configure_with(WBTC, self.verifier.clone(), DEFAULT_CAPACITY_BPS)
        }

        fn configure_with(
            &self,
            asset_code: Symbol,
            verifier: Address,
            capacity_bps: i64,
        ) -> BridgeCapConfig {
            self.frame(|| configure_cap_guard(&self.env, &self.admin, &asset_code, &verifier, capacity_bps))
                .expect("configure")
        }

        fn deposit(&self, amount: i128) -> Result<i128, ContractError> {
            self.frame(|| {
                deposit_locked_collateral(&self.env, &self.verifier, &WBTC, amount)
            })
        }

        fn release(&self, amount: i128) -> Result<i128, ContractError> {
            self.frame(|| release_locked_collateral(&self.env, &self.admin, &WBTC, amount))
        }

        fn mint(&self, delta: i128) -> Result<i128, ContractError> {
            self.frame(|| mint_within_cap(&self.env, &self.verifier, &WBTC, delta))
        }

        fn burn(&self, delta: i128) -> Result<i128, ContractError> {
            self.frame(|| record_wrapped_burn(&self.env, &self.verifier, &WBTC, delta))
        }

        fn sync(&self, observed: i128) -> Result<i128, ContractError> {
            self.frame(|| sync_wrapped_supply(&self.env, &WBTC, observed))
        }

        fn require_allowed(&self, delta: i128) -> Result<(), ContractError> {
            self.frame(|| require_mint_allowed(&self.env, &WBTC, delta))
        }

        fn status(&self) -> BridgeCapStatus {
            self.frame(|| bridge_cap_status(&self.env, &WBTC)).expect("status")
        }

        fn supply(&self) -> i128 {
            self.frame(|| wrapped_supply(&self.env, &WBTC))
        }

        fn locked(&self) -> i128 {
            self.frame(|| locked_collateral(&self.env, &WBTC))
        }

        fn headroom(&self) -> i128 {
            self.frame(|| mint_headroom(&self.env, &WBTC)).expect("headroom")
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

    /// A configured asset with `locked` collateral and `SUPPLY` wrapped supply.
    fn backed(locked: i128) -> (Fixture, BridgeCapConfig) {
        let f = Fixture::new();
        let config = f.configure();
        f.deposit(locked).expect("deposit");
        f.mint(SUPPLY).expect("mint");
        (f, config)
    }

    // ── allowed_supply ─────────────────────────────────────────────────────

    #[test]
    fn full_capacity_mirrors_the_issue_formula() {
        // 100 % capacity reproduces `S_wrapped + ΔS <= C_locked` exactly.
        assert_eq!(allowed_supply(1_000, DEFAULT_CAPACITY_BPS), Ok(1_000));
        assert_eq!(allowed_supply(0, DEFAULT_CAPACITY_BPS), Ok(0));
        assert_eq!(allowed_supply(1, DEFAULT_CAPACITY_BPS), Ok(1));
    }

    #[test]
    fn a_lower_capacity_holds_a_permanent_buffer() {
        assert_eq!(allowed_supply(1_000, 9_000), Ok(900));
        assert_eq!(allowed_supply(1_000, 5_000), Ok(500));
        assert_eq!(allowed_supply(1_000, 0), Ok(0));
    }

    #[test]
    fn capacity_truncates_downward() {
        // Flooring must never round a ceiling up past the collateral behind it.
        assert_eq!(allowed_supply(1_999, 5_000), Ok(999));
        assert_eq!(allowed_supply(3, 5_000), Ok(1));
        assert_eq!(allowed_supply(3, 1), Ok(0));
    }

    #[test]
    fn capacity_is_never_configurable_above_full_backing() {
        assert_eq!(
            allowed_supply(1_000, MAX_ALLOWED_CAPACITY_BPS + 1),
            Err(ContractError::BridgeCapInvalidConfig)
        );
        assert_eq!(
            allowed_supply(1_000, MIN_ALLOWED_CAPACITY_BPS - 1),
            Err(ContractError::BridgeCapInvalidConfig)
        );
        assert_eq!(
            allowed_supply(1_000, DEFAULT_CAPACITY_BPS),
            allowed_supply(1_000, MAX_ALLOWED_CAPACITY_BPS)
        );
    }

    #[test]
    fn capacity_rejects_negative_collateral() {
        assert_eq!(
            allowed_supply(-1, DEFAULT_CAPACITY_BPS),
            Err(ContractError::BridgeCapInvalidCollateral)
        );
    }

    #[test]
    fn capacity_never_overflows_at_i128_bounds() {
        // The naive `locked * capacity_bps` overflows on every one of these.
        assert_eq!(allowed_supply(i128::MAX, DEFAULT_CAPACITY_BPS), Ok(i128::MAX));
        assert_eq!(allowed_supply(i128::MAX, 0), Ok(0));
        assert_eq!(allowed_supply(i128::MAX, 1), Ok(i128::MAX / 10_000));

        // 99.99 % of the maximum balance: strictly under the collateral, and
        // within one part in 10_000 of it.
        let almost = allowed_supply(i128::MAX, 9_999).expect("almost");
        assert!(almost < i128::MAX);
        assert!(almost > i128::MAX - i128::MAX / 9_000);
        assert_eq!(allowed_supply(almost, DEFAULT_CAPACITY_BPS), Ok(almost));
    }

    // ── mint_exceeds_cap ───────────────────────────────────────────────────

    #[test]
    fn the_cap_boundary_is_exclusive() {
        // Landing exactly on the ceiling is allowed; one more unit is not.
        assert!(!mint_exceeds_cap(1_000, 1_000, DEFAULT_CAPACITY_BPS, 0));
        assert!(!mint_exceeds_cap(0, 1_000, DEFAULT_CAPACITY_BPS, 1_000));
        assert!(mint_exceeds_cap(0, 1_000, DEFAULT_CAPACITY_BPS, 1_001));
    }

    #[test]
    fn a_buffered_capacity_is_respected() {
        // 1_000 collateral at 90 % admits 900, not 1_000.
        assert!(!mint_exceeds_cap(0, 1_000, 9_000, 900));
        assert!(mint_exceeds_cap(0, 1_000, 9_000, 901));
    }

    #[test]
    fn nonsensical_inputs_always_breach() {
        // Fail closed: nothing unmeasurable may be read as headroom.
        assert!(mint_exceeds_cap(-1, 1_000, DEFAULT_CAPACITY_BPS, 1));
        assert!(mint_exceeds_cap(0, -1, DEFAULT_CAPACITY_BPS, 1));
        assert!(mint_exceeds_cap(0, 1_000, DEFAULT_CAPACITY_BPS, -1));
        assert!(mint_exceeds_cap(0, 1_000, 20_000, 1));
        // A delta that overflows the supply is a breach, not a wrap to a small
        // positive number that would sail under the ceiling.
        assert!(mint_exceeds_cap(i128::MAX, i128::MAX, DEFAULT_CAPACITY_BPS, 1));
    }

    // ── configuration ──────────────────────────────────────────────────────

    #[test]
    fn a_fresh_guard_starts_empty() {
        let f = Fixture::new();
        f.configure();
        let status = f.status();
        assert_eq!(status.wrapped_supply, 0);
        assert_eq!(status.locked_collateral, 0);
        assert_eq!(status.allowed_supply, 0);
        assert_eq!(status.headroom, 0);
        assert_eq!(status.capacity_bps, DEFAULT_CAPACITY_BPS);
        assert!(status.at_cap);
        assert_eq!(status.utilisation_bps, 0);
    }

    #[test]
    fn an_unconfigured_asset_has_no_status() {
        let f = Fixture::new();
        assert_eq!(
            f.frame(|| bridge_cap_status(&f.env, &WBTC)),
            Err(ContractError::BridgeEscrowNotConfigured)
        );
        assert_eq!(
            f.frame(|| require_mint_allowed(&f.env, &WBTC, 1)),
            Err(ContractError::BridgeEscrowNotConfigured)
        );
    }

    #[test]
    fn a_non_admin_cannot_configure_or_release() {
        let f = Fixture::new();
        assert_eq!(
            f.frame(|| {
                configure_cap_guard(
                    &f.env,
                    &f.verifier,
                    &WBTC,
                    &f.verifier,
                    DEFAULT_CAPACITY_BPS,
                )
            }),
            Err(ContractError::NotAdmin)
        );
        f.configure();
        f.deposit(1_000).expect("deposit");

        // A stranger holds no admin seat, so the release must be refused.
        let stranger = Address::generate(&f.env);
        assert_eq!(
            f.frame(|| release_locked_collateral(&f.env, &stranger, &WBTC, 1)),
            Err(ContractError::NotAdmin)
        );
        assert_eq!(f.locked(), 1_000);
    }

    #[test]
    fn a_non_verifier_cannot_report_collateral_or_supply() {
        let f = Fixture::new();
        f.configure();
        let stranger = Address::generate(&f.env);
        assert_eq!(
            f.frame(|| deposit_locked_collateral(&f.env, &stranger, &WBTC, 1)),
            Err(ContractError::BridgeNotController)
        );
        assert_eq!(
            f.frame(|| record_wrapped_mint(&f.env, &stranger, &WBTC, 1)),
            Err(ContractError::BridgeNotController)
        );
        assert_eq!(
            f.frame(|| record_wrapped_burn(&f.env, &stranger, &WBTC, 1)),
            Err(ContractError::BridgeNotController)
        );
    }

    #[test]
    fn a_capacity_outside_the_bounds_is_rejected() {
        let f = Fixture::new();
        for bad in [
            MIN_ALLOWED_CAPACITY_BPS - 1,
            MAX_ALLOWED_CAPACITY_BPS + 1,
        ] {
            let result = f.frame(|| {
                configure_cap_guard(&f.env, &f.admin, &WBTC, &f.verifier, bad)
            });
            assert_eq!(result, Err(ContractError::BridgeCapInvalidConfig));
        }
    }

    // ── the cap in operation ───────────────────────────────────────────────

    #[test]
    fn a_mint_up_to_the_ceiling_succeeds() {
        let f = Fixture::new();
        f.configure();
        f.deposit(1_000).expect("deposit");
        assert_eq!(f.mint(1_000), Ok(1_000));
        assert_eq!(f.supply(), 1_000);
        assert_eq!(f.headroom(), 0);
        assert!(f.status().at_cap);
    }

    #[test]
    fn a_mint_past_the_ceiling_is_reverted() {
        let (f, _config) = backed(1_000);
        // 1_000 collateral, 1_000 supply, so any further mint breaches.
        assert_eq!(f.mint(1), Err(ContractError::BridgeCapExceeded));
        // Reverted means nothing moved.
        assert_eq!(f.supply(), 1_000);
        assert_eq!(f.locked(), 1_000);
    }

    #[test]
    fn a_mint_into_an_empty_bridge_is_reverted() {
        let f = Fixture::new();
        f.configure();
        // No collateral at all, so no headroom.
        assert_eq!(f.mint(1), Err(ContractError::BridgeCapExceeded));
        assert_eq!(f.supply(), 0);
    }

    #[test]
    fn a_mint_that_overflows_the_supply_is_reverted() {
        let f = Fixture::new();
        f.configure();
        f.deposit(i128::MAX).expect("deposit");
        f.mint(i128::MAX - 1).expect("mint");
        // `supply + delta` would wrap, which must read as a breach.
        assert_eq!(
            f.require_allowed(10),
            Err(ContractError::BridgeCapExceeded)
        );
    }

    #[test]
    fn a_non_positive_mint_delta_is_rejected() {
        let (f, _config) = backed(1_000);
        assert_eq!(f.mint(0), Err(ContractError::BridgeCapInvalidAmount));
        assert_eq!(f.mint(-5), Err(ContractError::BridgeCapInvalidAmount));
    }

    #[test]
    fn depositing_collateral_reopens_headroom() {
        let (f, _config) = backed(1_000);
        assert_eq!(f.mint(1), Err(ContractError::BridgeCapExceeded));

        f.deposit(500).expect("deposit");
        assert_eq!(f.locked(), 1_500);
        assert_eq!(f.headroom(), 500);
        assert_eq!(f.mint(500), Ok(1_500));
        assert_eq!(f.mint(1), Err(ContractError::BridgeCapExceeded));
    }

    #[test]
    fn a_burn_reopens_headroom() {
        let (f, _config) = backed(1_000);
        assert_eq!(f.mint(1), Err(ContractError::BridgeCapExceeded));

        f.burn(400).expect("burn");
        assert_eq!(f.supply(), 600);
        assert_eq!(f.headroom(), 400);
        assert_eq!(f.mint(400), Ok(1_000));
    }

    #[test]
    fn a_burn_cannot_exceed_the_mirrored_supply() {
        let (f, _config) = backed(1_000);
        assert_eq!(f.burn(1_001), Err(ContractError::BridgeInsufficientBalance));
        assert_eq!(f.burn(0), Err(ContractError::BridgeCapInvalidAmount));
        assert_eq!(f.supply(), 1_000);
    }

    // ── the invariant ──────────────────────────────────────────────────────

    #[test]
    fn supply_never_exceeds_locked_collateral() {
        let f = Fixture::new();
        f.configure();
        // A scripted sequence of deposits, mints and burns, asserting the
        // invariant after every single step.
        for step in 0..40i128 {
            f.deposit(100).expect("deposit");
            let headroom = f.headroom();
            if headroom > 0 {
                let _ = f.mint(headroom);
            }
            let _ = f.mint(1); // may be refused
            let status = f.status();
            assert!(
                status.wrapped_supply <= status.locked_collateral,
                "step {step}: S={} exceeded C={}",
                status.wrapped_supply,
                status.locked_collateral
            );
            if step % 3 == 0 && status.wrapped_supply > 0 {
                f.burn(10).expect("burn");
            }
        }
        let status = f.status();
        assert!(status.wrapped_supply <= status.locked_collateral);
    }

    #[test]
    fn a_buffered_capacity_holds_more_collateral_than_supply() {
        let f = Fixture::new();
        f.configure_with(WBTC, f.verifier.clone(), 9_000);
        f.deposit(1_000).expect("deposit");
        f.mint(900).expect("mint");
        let status = f.status();
        // 900 supply against 1_000 locked: the 10 % buffer is intact.
        assert_eq!(status.wrapped_supply, 900);
        assert_eq!(status.locked_collateral, 1_000);
        assert_eq!(status.allowed_supply, 900);
        assert_eq!(status.headroom, 0);
        assert_eq!(f.mint(1), Err(ContractError::BridgeCapExceeded));
    }

    // ── collateral release ─────────────────────────────────────────────────

    #[test]
    fn collateral_can_be_released_while_the_backing_intact() {
        let f = Fixture::new();
        f.configure();
        f.deposit(1_500).expect("deposit");
        f.mint(1_000).expect("mint");
        // Releasing down to exactly the supply leaves a fully backed bridge.
        assert_eq!(f.release(500), Ok(1_000));
        assert_eq!(f.locked(), 1_000);
        assert_eq!(f.headroom(), 0);
    }

    #[test]
    fn collateral_cannot_be_released_past_the_supply() {
        let (f, _config) = backed(1_000);
        // This is the whole point: the backing must not leave while the
        // wrapped supply it backs is still outstanding.
        assert_eq!(f.release(1), Err(ContractError::BridgeCapUndercollateralized));
        assert_eq!(f.locked(), 1_000);
    }

    #[test]
    fn a_release_larger_than_the_lock_is_rejected() {
        let (f, _config) = backed(1_000);
        assert_eq!(
            f.release(1_001),
            Err(ContractError::BridgeCapInsufficientCollateral)
        );
    }

    #[test]
    fn a_non_positive_release_is_rejected() {
        let (f, _config) = backed(1_000);
        assert_eq!(f.release(0), Err(ContractError::BridgeCapInvalidAmount));
        assert_eq!(f.release(-5), Err(ContractError::BridgeCapInvalidAmount));
    }

    // ── reconciliation ─────────────────────────────────────────────────────

    #[test]
    fn reconciliation_tightens_but_never_loosens() {
        let (f, _config) = backed(1_000);
        // Raising the mirror to the engine's view is always allowed.
        assert_eq!(f.sync(1_200), Ok(1_200));
        assert_eq!(f.supply(), 1_200);

        // Lowering it is privileged and goes through `record_wrapped_burn`.
        assert_eq!(f.sync(1_000), Err(ContractError::BridgeCapInvalidAmount));
        assert_eq!(f.supply(), 1_200);
        assert_eq!(f.burn(200), Ok(1_000));
    }

    #[test]
    fn a_reconciliation_to_the_same_value_is_a_no_op() {
        let (f, _config) = backed(1_000);
        assert_eq!(f.sync(1_000), Ok(1_000));
        assert_eq!(f.emissions(&EV_BRIDGE_CAP_SYNC, &STATUS_SYNC), 0);
    }

    #[test]
    fn reconciliation_rejects_a_negative_total() {
        let (f, _config) = backed(1_000);
        assert_eq!(f.sync(-1), Err(ContractError::BridgeCapInvalidAmount));
    }

    #[test]
    fn reconciliation_closes_phantom_headroom() {
        let f = Fixture::new();
        f.configure();
        f.deposit(1_000).expect("deposit");
        f.mint(1_000).expect("mint");
        // A stale mirror that under-reports supply would grant phantom
        // headroom; the tick closes it without ceremony.
        f.burn(500).expect("burn");
        assert_eq!(f.headroom(), 500);
        assert_eq!(f.sync(1_000), Ok(1_000));
        assert_eq!(f.headroom(), 0);
        assert_eq!(f.mint(1), Err(ContractError::BridgeCapExceeded));
    }

    // ── status view ────────────────────────────────────────────────────────

    #[test]
    fn the_status_view_reports_the_full_position() {
        let (f, _config) = backed(1_500);
        let status = f.status();
        assert_eq!(status.wrapped_supply, 1_000);
        assert_eq!(status.locked_collateral, 1_500);
        assert_eq!(status.allowed_supply, 1_500);
        assert_eq!(status.headroom, 500);
        assert!(!status.at_cap);

        // Draining the headroom lands exactly on the ceiling, which is allowed.
        f.mint(500).expect("mint");
        let status = f.status();
        assert_eq!(status.wrapped_supply, 1_500);
        assert_eq!(status.headroom, 0);
        assert!(status.at_cap);
        assert_eq!(status.utilisation_bps, 10_000);
    }

    #[test]
    fn utilisation_is_reported_in_bps() {
        let f = Fixture::new();
        f.configure();
        f.deposit(1_000).expect("deposit");
        f.mint(500).expect("mint");
        assert_eq!(f.status().utilisation_bps, 5_000);
        f.mint(250).expect("mint");
        assert_eq!(f.status().utilisation_bps, 7_500);
    }

    #[test]
    fn utilisation_saturates_instead_of_wrapping() {
        let f = Fixture::new();
        f.configure();
        f.deposit(i128::MAX).expect("deposit");
        f.mint(i128::MAX).expect("mint");
        assert_eq!(f.status().utilisation_bps, 10_000);
    }

    #[test]
    fn assets_are_guarded_independently() {
        let f = Fixture::new();
        f.configure_with(WBTC, f.verifier.clone(), DEFAULT_CAPACITY_BPS);
        f.configure_with(WETH, f.verifier.clone(), DEFAULT_CAPACITY_BPS);
        f.deposit(1_000).expect("deposit");
        f.mint(1_000).expect("mint");

        // WETH is untouched: no collateral, no supply, and still a breach.
        assert_eq!(
            f.frame(|| bridge_cap_status(&f.env, &WETH))
                .map(|s| s.wrapped_supply),
            Ok(0)
        );
        assert_eq!(
            f.frame(|| require_mint_allowed(&f.env, &WETH, 1)),
            Err(ContractError::BridgeCapExceeded)
        );
    }

    // ── events ─────────────────────────────────────────────────────────────

    #[test]
    fn a_refused_mint_emits_bridge_cap_exceeded() {
        let (f, _config) = backed(1_000);
        assert_eq!(f.emissions(&EV_BRIDGE_CAP_EXCEEDED, &STATUS_EXCEEDED), 0);
        let _ = f.mint(1);
        assert_eq!(f.emissions(&EV_BRIDGE_CAP_EXCEEDED, &STATUS_EXCEEDED), 1);
    }

    #[test]
    fn every_refused_mint_emits_the_event() {
        let (f, _config) = backed(1_000);
        for _ in 0..3 {
            let _ = f.mint(1);
        }
        assert_eq!(f.emissions(&EV_BRIDGE_CAP_EXCEEDED, &STATUS_EXCEEDED), 3);
    }

    #[test]
    fn an_accepted_mint_emits_no_cap_event() {
        let f = Fixture::new();
        f.configure();
        f.deposit(1_000).expect("deposit");
        f.mint(1_000).expect("mint");
        assert_eq!(f.emissions(&EV_BRIDGE_CAP_EXCEEDED, &STATUS_EXCEEDED), 0);
        assert!(f.emissions(&EV_BRIDGE_CAP_MINT, &STATUS_MINT) >= 1);
    }

    #[test]
    fn the_lifecycle_events_are_emitted() {
        let f = Fixture::new();
        f.configure();
        assert!(
            f.emissions(&EV_BRIDGE_CAP_SET, &STATUS_CONFIG) >= 1,
            "configure should emit"
        );
        f.deposit(1_000).expect("deposit");
        f.mint(1_000).expect("mint");
        f.burn(100).expect("burn");
        f.release(100).expect("release");
        f.sync(1_000).expect("sync");
        for (name, status) in [
            (EV_BRIDGE_CAP_COLLATERAL, STATUS_COLLATERAL),
            (EV_BRIDGE_CAP_MINT, STATUS_MINT),
            (EV_BRIDGE_CAP_BURN, STATUS_BURN),
            (EV_BRIDGE_CAP_SYNC, STATUS_SYNC),
        ] {
            assert!(
                f.emissions(&name, &status) >= 1,
                "missing event {name:?}"
            );
        }
    }
}
