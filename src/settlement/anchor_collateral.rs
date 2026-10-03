//! Cross-border fiat-anchor collateral ratio monitoring engine (Issue #991).
//!
//! Every cross-border remittance corridor ultimately leaves the Stellar
//! network through a *fiat anchor* — the regulated partner that holds the
//! off-chain fiat inventory and pays the beneficiary. The anchor posts token
//! collateral to the protocol, and the protocol keeps assigning it new payout
//! work for as long as that collateral is healthy.
//!
//! The engine tracks one [`AnchorCollateralState`] per
//! `(anchor, corridor)` pair and maintains the **anchor backing ratio**
//!
//! ```text
//!              Collateral_locked
//!   R_anchor = ──────────────────
//!                Volume_unsettled
//! ```
//!
//! `Collateral_locked` is the token collateral the anchor has posted.
//! `Volume_unsettled` is the fiat value the protocol has already routed to
//! that anchor but which has not yet been paid out to beneficiaries — i.e.
//! the size of the *active fiat payout queue*.
//!
//! # Monitoring rule
//!
//! When `R_anchor` falls below the corridor minimum (default **120 %**,
//! [`DEFAULT_MIN_COLLATERAL_RATIO_BPS`]), the engine **pauses new remittance
//! assignments** to that anchor. Already-queued payouts keep running: the
//! queue is never truncated, because cancelling a queued payout would strand
//! the beneficiary. When the anchor deposits additional collateral and the
//! ratio recovers to at least the minimum, routing **resumes automatically**.
//!
//! # State model
//!
//! `assignments_paused` is a **pure function** of
//! `(collateral_locked, volume_unsettled, min_ratio_bps)`: every mutating
//! entry point re-evaluates it and it is the only thing that writes it. An
//! independent latch would be simpler to reach for, but a later
//! [`sync_anchor_ratio`] could then silently clear it, leaving the flag
//! unexplainable from the ledger. A monitor must be able to recompute the
//! answer from state alone.
//!
//! The two guards and the pause therefore have distinct jobs. Assignments and
//! withdrawals are *capped* so the ratio cannot cross the floor in normal
//! operation — a refused assignment leaves the queue untouched. The pause
//! latches when a breach arrives from outside those guarded paths, such as a
//! governance threshold increase, and only new collateral (or a lowered
//! threshold) clears it.
//!
//! # Fail-closed arithmetic
//!
//! The pause decision is made with [`ratio_ge`], an exact `a / b >= c / d`
//! comparison that never multiplies and therefore never overflows, even when
//! collateral balances sit at `u128::MAX`. [`backing_ratio_bps`] is a lossy
//! basis-point *view* used for events and monitoring; it truncates and
//! saturates and is deliberately **not** used for the pause decision, so a
//! saturating read can never fail open.
//!
//! # Special case: empty payout queue
//!
//! `Volume_unsettled == 0` means the anchor has no outstanding obligation, so
//! there is nothing for the collateral to back and `R_anchor` is unbounded.
//! [`is_collateral_sufficient`] therefore returns `true` for an empty queue
//! rather than treating the undefined `0 / 0` ratio as a breach.
//!
//! # Usage
//!
//! ```
//! use stellarflow_contracts::settlement::anchor_collateral::{
//!     backing_ratio_bps, is_collateral_sufficient, DEFAULT_MIN_COLLATERAL_RATIO_BPS,
//! };
//!
//! // 1_250 posted against 1_000 outstanding → 125 % backing, healthy.
//! assert!(is_collateral_sufficient(1_250, 1_000, DEFAULT_MIN_COLLATERAL_RATIO_BPS));
//! assert_eq!(backing_ratio_bps(1_250, 1_000), 12_500);
//!
//! // 1_150 posted against 1_000 outstanding → 115 %, below the 120 % floor.
//! assert!(!is_collateral_sufficient(1_150, 1_000, DEFAULT_MIN_COLLATERAL_RATIO_BPS));
//!
//! // An empty payout queue has nothing to back, so it is always sufficient.
//! assert!(is_collateral_sufficient(0, 0, DEFAULT_MIN_COLLATERAL_RATIO_BPS));
//! ```
//!
//! Downstream routers should call [`require_anchor_assignable`] immediately
//! before routing a new remittance to an anchor.

use soroban_sdk::{contracttype, symbol_short, Address, Env, Symbol, Vec};

use crate::{AssetId, ContractData, ContractError, DATA_KEY};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Basis-point denominator: 10 000 bp = 100 %.
pub const BPS_DENOMINATOR: u32 = 10_000;

/// Default minimum anchor backing ratio required by the protocol: **120 %**.
///
/// A 20 % buffer absorbs collateral price movement and settlement slippage
/// without pausing the corridor. Mirrors the `1.20` requirement from the
/// issue.
pub const DEFAULT_MIN_COLLATERAL_RATIO_BPS: u32 = 12_000;

/// Floor for a governance-configured minimum ratio: 100 %.
///
/// The engine never permits a threshold below full backing, because a
/// sub-100 % threshold would authorise an undercollateralised payout queue by
/// construction.
pub const MIN_ALLOWED_COLLATERAL_RATIO_BPS: u32 = BPS_DENOMINATOR;

/// Ceiling for a governance-configured minimum ratio: 1 000 % (10x).
///
/// Bounds the threshold so a fat-fingered governance transaction cannot brick
/// a corridor indefinitely.
pub const MAX_ALLOWED_COLLATERAL_RATIO_BPS: u32 = 100_000;

/// Event topic: an anchor posted additional collateral.
pub const EV_ANCHOR_COLLATERAL_DEPOSITED: Symbol = symbol_short!("anc_cdep");

/// Event topic: an anchor released uncommitted collateral.
pub const EV_ANCHOR_COLLATERAL_WITHDRAWN: Symbol = symbol_short!("anc_cwdr");

/// Event topic: the backing ratio breached the corridor minimum and new
/// remittance assignments were paused.
pub const EV_ANCHOR_ROUTING_PAUSED: Symbol = symbol_short!("anc_pause");

/// Event topic: additional collateral restored the backing ratio and routing
/// resumed.
pub const EV_ANCHOR_ROUTING_RESUMED: Symbol = symbol_short!("anc_res");

/// Event topic: a payout was appended to the active queue.
pub const EV_ANCHOR_PAYOUT_QUEUED: Symbol = symbol_short!("anc_pay_q");

/// Event topic: a queued payout was settled off-ledger.
pub const EV_ANCHOR_PAYOUT_SETTLED: Symbol = symbol_short!("anc_pay_s");

/// Event topic: the per-corridor minimum ratio was reconfigured.
pub const EV_ANCHOR_MIN_RATIO_SET: Symbol = symbol_short!("anc_mins");

// Second topic on every event, naming the transition that produced it, so an
// indexer can filter a single ledger stream by `(event, status)`.
const STATUS_PAUSED: Symbol = symbol_short!("paused");
const STATUS_RESUMED: Symbol = symbol_short!("resumed");
const STATUS_DEPOSIT: Symbol = symbol_short!("deposit");
const STATUS_WITHDRAW: Symbol = symbol_short!("withdraw");
const STATUS_QUEUED: Symbol = symbol_short!("queued");
const STATUS_SETTLED: Symbol = symbol_short!("settled");
const STATUS_MINSET: Symbol = symbol_short!("minset");

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AnchorCollateralKey {
    /// Per-`(anchor, corridor)` monitoring state.
    State(Address, AssetId),
    /// Active, unsettled fiat payout queue for `(anchor, corridor)`.
    PayoutQueue(Address, AssetId),
    /// Monotonic payout-id source for `(anchor, corridor)`.
    NextPayoutId(Address, AssetId),
    /// Governance-configured minimum backing ratio for a corridor.
    CorridorMinRatio(AssetId),
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A fiat payout assigned to an anchor that has not yet completed.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FiatPayoutEntry {
    /// Monotonically increasing id within the `(anchor, corridor)` queue.
    pub id: u64,
    /// Fiat value routed to the anchor, denominated in the corridor asset.
    pub amount: u128,
    /// Ledger timestamp at which the payout was assigned.
    pub opened_at: u64,
}

/// Live monitoring state for one anchor on one corridor.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnchorCollateralState {
    /// The fiat anchor being monitored.
    pub anchor: Address,
    /// Numeric corridor asset id this state applies to.
    pub corridor: AssetId,
    /// Token collateral currently locked by the anchor.
    pub collateral_locked: u128,
    /// Fiat value assigned to the anchor but not yet paid out.
    pub volume_unsettled: u128,
    /// `true` when new remittance assignments are paused.
    pub assignments_paused: bool,
    /// Ledger timestamp the current pause began; `0` while routing is live.
    pub paused_at: u64,
    /// Backward-compatible alias of `paused_at`, retained so an indexer that
    /// only reads the snapshot can still distinguish a paused anchor.
    pub last_paused_at: u64,
    /// Backing ratio at the last evaluation, in basis points. Telemetry only.
    pub last_ratio_bps: u32,
}

/// Read-only snapshot for off-chain monitoring and indexers.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnchorCollateralStatus {
    /// The anchor being reported on.
    pub anchor: Address,
    /// Corridor asset id.
    pub corridor: AssetId,
    /// Token collateral currently locked.
    pub collateral_locked: u128,
    /// Fiat value assigned but unsettled.
    pub volume_unsettled: u128,
    /// `Collateral_locked / Volume_unsettled` in basis points, saturating at
    /// [`u32::MAX`]. `0` when the queue is empty and the ratio is unbounded.
    pub ratio_bps: u32,
    /// Minimum ratio enforced for this corridor.
    pub min_ratio_bps: u32,
    /// `true` when the collateral sufficiently backs the outstanding volume.
    pub healthy: bool,
    /// `true` when new remittance assignments are currently paused.
    pub assignments_paused: bool,
    /// Number of payouts still awaiting off-chain settlement.
    pub queue_len: u32,
}

// ---------------------------------------------------------------------------
// Ratio arithmetic
// ---------------------------------------------------------------------------

/// Compare two non-negative ratios exactly, without overflow.
///
/// Returns `true` when `a / b >= c / d`. Requires `b > 0` and `d > 0`.
///
/// The naive `a * d >= c * b` overflows for large balances, which is exactly
/// the regime this engine must survive: collateral is a `u128`, so a healthy
/// anchor can push the product past `u128::MAX`. This routine instead runs the
/// Euclidean algorithm on both ratios, comparing integer parts first and then
/// recursing on the fractional remainders.
///
/// # How the recursion terminates
///
/// Each step replaces `(a, b)` with `(b, a % b)` and `(c, d)` with
/// `(d, c % d)`, so both denominators strictly decrease and never form an
/// intermediate product. The loop runs in `O(log max(a, b, c, d))` steps.
///
/// # Why the comparison direction flips
///
/// Once the integer parts agree, the question becomes `ra / b >= rc / d`. All
/// four terms are positive, so dividing through by `ra * rc` turns that into
/// `b / ra <= d / rc` — the *reciprocal* comparison. `direction` tracks which
/// of `>=` / `<=` the current `(a, b, c, d)` tuple is asking about, and each
/// reciprocal step flips it. Equality satisfies both directions at once, so
/// the two exact-equal exits below always answer `true`.
///
/// `a = 0` and `c = 0` are handled naturally; `0 / x` compares as `0`.
pub fn ratio_ge(a: u128, b: u128, c: u128, d: u128) -> bool {
    debug_assert!(b > 0 && d > 0, "ratio_ge requires non-zero denominators");

    let (mut a, mut b, mut c, mut d) = (a, b, c, d);
    // `true` while the current tuple answers `a / b >= c / d`, `false` once a
    // reciprocal step has turned the question into `a / b <= c / d`.
    let mut direction_ge = true;

    loop {
        let qa = a / b;
        let qc = c / d;
        if qa != qc {
            // Integer parts decide the comparison outright.
            return if direction_ge { qa > qc } else { qa < qc };
        }

        // Integer parts match, so compare the fractional remainders.
        let ra = a % b;
        let rc = c % d;
        if ra == 0 && rc == 0 {
            // Exactly equal, which satisfies both `>=` and `<=`.
            return true;
        }
        if ra == 0 {
            // `a / b` is exactly `qa` while `c / d` is strictly above `qc`.
            return !direction_ge;
        }
        if rc == 0 {
            // `c / d` is exactly `qc` while `a / b` is strictly above `qa`.
            return direction_ge;
        }

        // Recurse on `b / ra` against `d / rc`, flipping the direction.
        a = b;
        b = ra;
        c = d;
        d = rc;
        direction_ge = !direction_ge;
    }
}

/// Return `Collateral_locked / Volume_unsettled` in basis points.
///
/// This is a *reporting* helper: it truncates and saturates and exists for
/// events, indexers and the [`AnchorCollateralStatus`] view. Use
/// [`is_collateral_sufficient`] for any decision that gates value movement.
///
/// A `volume_unsettled` of `0` yields `0` because the true ratio is unbounded
/// and has no basis-point representation. Callers must special-case the empty
/// queue; [`is_collateral_sufficient`] does.
///
/// # Overflow
///
/// The obvious `collateral * 10_000 / volume` overflows once collateral
/// exceeds `u128::MAX / 10_000`, which would report a wildly *understated* ratio
/// for a very well collateralised anchor. The quotient is therefore taken
/// first and only the remainder is scaled: `rem < volume`, and the remainder is
/// shrunk alongside the volume so `rem * 10_000` always fits. For any
/// `volume_unsettled <= u32::MAX` the shrink factor is `1` and the result is
/// exact; above that the fractional part can lose well under `1e-5` bp, which
/// is immaterial for a view that the decision path never consults.
pub fn backing_ratio_bps(collateral_locked: u128, volume_unsettled: u128) -> u32 {
    if volume_unsettled == 0 {
        return 0;
    }

    // Integer part, exact.
    let whole = collateral_locked / volume_unsettled;
    if whole > u32::MAX as u128 / BPS_DENOMINATOR as u128 {
        // The whole part alone already saturates the basis-point view.
        return u32::MAX;
    }
    let whole_bps = whole * BPS_DENOMINATOR as u128;

    // Fractional part, scaled without ever forming a product that overflows.
    let rem = collateral_locked % volume_unsettled;
    let frac_bps = if rem == 0 {
        0
    } else {
        let shrink = volume_unsettled / u32::MAX as u128 + 1;
        (rem / shrink) * BPS_DENOMINATOR as u128 / (volume_unsettled / shrink)
    };

    (whole_bps + frac_bps) as u32
}

/// Decide whether `collateral_locked` sufficiently backs `volume_unsettled`
/// at the `min_ratio_bps` threshold.
///
/// `volume_unsettled == 0` is always sufficient: with no outstanding payout
/// there is no obligation for the collateral to back, and the ratio is
/// unbounded rather than undefined.
pub fn is_collateral_sufficient(
    collateral_locked: u128,
    volume_unsettled: u128,
    min_ratio_bps: u32,
) -> bool {
    if volume_unsettled == 0 {
        return true;
    }
    // collateral / unsettled >= min_bps / 10_000
    ratio_ge(
        collateral_locked,
        volume_unsettled,
        min_ratio_bps as u128,
        BPS_DENOMINATOR as u128,
    )
}

// ---------------------------------------------------------------------------
// State accessors
// ---------------------------------------------------------------------------

/// Read the monitoring state for `(anchor, corridor)`, defaulting to a fresh,
/// healthy record. Never panics on a missing key.
pub fn get_anchor_state(env: &Env, anchor: &Address, corridor: AssetId) -> AnchorCollateralState {
    env.storage()
        .persistent()
        .get(&AnchorCollateralKey::State(anchor.clone(), corridor))
        .unwrap_or(AnchorCollateralState {
            anchor: anchor.clone(),
            corridor,
            collateral_locked: 0,
            volume_unsettled: 0,
            assignments_paused: false,
            paused_at: 0,
            last_paused_at: 0,
            last_ratio_bps: 0,
        })
}

fn store_anchor_state(env: &Env, state: &AnchorCollateralState) {
    env.storage().persistent().set(
        &AnchorCollateralKey::State(state.anchor.clone(), state.corridor),
        state,
    );
}

/// Read the active fiat payout queue for `(anchor, corridor)`.
pub fn read_payout_queue(env: &Env, anchor: &Address, corridor: AssetId) -> Vec<FiatPayoutEntry> {
    env.storage()
        .persistent()
        .get(&AnchorCollateralKey::PayoutQueue(anchor.clone(), corridor))
        .unwrap_or_else(|| Vec::new(env))
}

/// Number of payouts still awaiting settlement for `(anchor, corridor)`.
pub fn payout_queue_len(env: &Env, anchor: &Address, corridor: AssetId) -> u32 {
    read_payout_queue(env, anchor, corridor).len()
}

/// Sum of every queued-but-unsettled payout for `(anchor, corridor)`.
///
/// This is an independent recomputation of `volume_unsettled` from the queue
/// itself. Callers and tests use it to assert the aggregate and the queue never
/// drift apart.
pub fn queued_volume(env: &Env, anchor: &Address, corridor: AssetId) -> u128 {
    let mut total: u128 = 0;
    for entry in read_payout_queue(env, anchor, corridor).iter() {
        total = total.saturating_add(entry.amount);
    }
    total
}

/// Total token collateral locked by `anchor` on `corridor`.
pub fn locked_collateral(env: &Env, anchor: &Address, corridor: AssetId) -> u128 {
    get_anchor_state(env, anchor, corridor).collateral_locked
}

/// Total fiat volume assigned to `anchor` on `corridor` but not yet settled.
pub fn unsettled_volume(env: &Env, anchor: &Address, corridor: AssetId) -> u128 {
    get_anchor_state(env, anchor, corridor).volume_unsettled
}

/// `true` when new remittance assignments to `anchor` are paused.
pub fn is_anchor_paused(env: &Env, anchor: &Address, corridor: AssetId) -> bool {
    get_anchor_state(env, anchor, corridor).assignments_paused
}

/// The minimum backing ratio currently enforced on `corridor`.
///
/// Falls back to [`DEFAULT_MIN_COLLATERAL_RATIO_BPS`] until governance sets
/// one. Stored per corridor rather than per anchor so a threshold change is
/// authoritative for every anchor on the corridor at once.
pub fn corridor_min_ratio_bps(env: &Env, corridor: AssetId) -> u32 {
    env.storage()
        .persistent()
        .get(&AnchorCollateralKey::CorridorMinRatio(corridor))
        .unwrap_or(DEFAULT_MIN_COLLATERAL_RATIO_BPS)
}

/// Build the read-only monitoring snapshot for `(anchor, corridor)`.
pub fn get_anchor_status(env: &Env, anchor: &Address, corridor: AssetId) -> AnchorCollateralStatus {
    let state = get_anchor_state(env, anchor, corridor);
    let min_ratio_bps = corridor_min_ratio_bps(env, corridor);
    AnchorCollateralStatus {
        anchor: state.anchor,
        corridor: state.corridor,
        collateral_locked: state.collateral_locked,
        volume_unsettled: state.volume_unsettled,
        ratio_bps: backing_ratio_bps(state.collateral_locked, state.volume_unsettled),
        min_ratio_bps,
        healthy: is_collateral_sufficient(
            state.collateral_locked,
            state.volume_unsettled,
            min_ratio_bps,
        ),
        assignments_paused: state.assignments_paused,
        queue_len: payout_queue_len(env, anchor, corridor),
    }
}

// ---------------------------------------------------------------------------
// Guard
// ---------------------------------------------------------------------------

/// Guard for downstream routers: fail unless `anchor` may be assigned new
/// remittance work on `corridor`.
///
/// Call this immediately before routing a new payout. It reads the *stored*
/// pause flag rather than re-deriving it, so the rejection path is a single
/// ledger read and can never disagree with the last evaluation.
pub fn require_anchor_assignable(
    env: &Env,
    anchor: &Address,
    corridor: AssetId,
) -> Result<(), ContractError> {
    if is_anchor_paused(env, anchor, corridor) {
        return Err(ContractError::AnchorAssignmentsPaused);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Ratio evaluation
// ---------------------------------------------------------------------------

/// Re-evaluate the backing ratio from the stored balances and move the pause
/// flag accordingly.
///
/// This is the *only* writer of `assignments_paused`, which makes the flag a
/// pure function of `(collateral_locked, volume_unsettled, min_ratio_bps)`.
/// That is deliberate: an independent latch could be silently cleared by a
/// later [`sync_anchor_ratio`], leaving the flag unexplainable from the ledger.
/// A monitor must be able to recompute the answer from state alone.
///
/// Returns `true` when this call flipped the flag. Emits
/// [`EV_ANCHOR_ROUTING_PAUSED`] or [`EV_ANCHOR_ROUTING_RESUMED`] on a
/// transition only, so a healthy anchor emitting on every settlement would be
/// pure event noise.
fn sync_ratio_status(env: &Env, state: &mut AnchorCollateralState) -> bool {
    let min_ratio_bps = corridor_min_ratio_bps(env, state.corridor);
    let healthy = is_collateral_sufficient(
        state.collateral_locked,
        state.volume_unsettled,
        min_ratio_bps,
    );
    state.last_ratio_bps = backing_ratio_bps(state.collateral_locked, state.volume_unsettled);

    if healthy {
        if !state.assignments_paused {
            return false;
        }
        state.assignments_paused = false;
        state.paused_at = 0;
        env.events().publish(
            (EV_ANCHOR_ROUTING_RESUMED, STATUS_RESUMED),
            (
                state.anchor.clone(),
                state.corridor,
                state.collateral_locked,
                state.volume_unsettled,
                state.last_ratio_bps,
                min_ratio_bps,
                env.ledger().timestamp(),
            ),
        );
        return true;
    }

    if state.assignments_paused {
        return false;
    }
    state.assignments_paused = true;
    state.paused_at = env.ledger().timestamp();
    state.last_paused_at = state.paused_at;
    env.events().publish(
        (EV_ANCHOR_ROUTING_PAUSED, STATUS_PAUSED),
        (
            state.anchor.clone(),
            state.corridor,
            state.collateral_locked,
            state.volume_unsettled,
            state.last_ratio_bps,
            min_ratio_bps,
            state.paused_at,
        ),
    );
    true
}

// ---------------------------------------------------------------------------
// Admin
// ---------------------------------------------------------------------------

/// Governance-configurable per-corridor minimum backing ratio.
///
/// Applies to every anchor on the corridor and takes effect on the next
/// evaluation of each anchor, which callers can force at any time with the
/// permissionless [`sync_anchor_ratio`] tick — so raising the floor can pause
/// an undercollateralised anchor without waiting for its next state change.
///
/// The value is bounded to `[[MIN_ALLOWED_COLLATERAL_RATIO_BPS,
/// MAX_ALLOWED_COLLATERAL_RATIO_BPS]]`: never below full backing, and never so
/// high that a corridor can be bricked by a fat-fingered transaction.
pub fn set_min_collateral_ratio_bps(
    env: &Env,
    admin: &Address,
    corridor: AssetId,
    min_ratio_bps: u32,
) -> Result<u32, ContractError> {
    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;
    if data.admin != *admin {
        return Err(ContractError::NotAdmin);
    }
    admin.require_auth();

    if !(MIN_ALLOWED_COLLATERAL_RATIO_BPS..=MAX_ALLOWED_COLLATERAL_RATIO_BPS)
        .contains(&min_ratio_bps)
    {
        return Err(ContractError::InvalidArgument);
    }

    env.storage()
        .persistent()
        .set(&AnchorCollateralKey::CorridorMinRatio(corridor), &min_ratio_bps);

    env.events().publish(
        (EV_ANCHOR_MIN_RATIO_SET, STATUS_MINSET),
        (corridor, min_ratio_bps, env.ledger().timestamp()),
    );

    Ok(min_ratio_bps)
}

// ---------------------------------------------------------------------------
// Collateral lifecycle
// ---------------------------------------------------------------------------

/// Post `amount` additional token collateral for `anchor` on `corridor`.
///
/// This is the recovery path from a pause: depositing enough collateral to
/// restore the minimum ratio clears the pause and resumes routing in the same
/// transaction.
pub fn deposit_collateral(
    env: &Env,
    caller: &Address,
    anchor: &Address,
    corridor: AssetId,
    amount: u128,
) -> Result<AnchorCollateralState, ContractError> {
    caller.require_auth();
    if amount == 0 {
        return Err(ContractError::AmountTooLow);
    }

    let mut state = get_anchor_state(env, anchor, corridor);
    state.collateral_locked = state
        .collateral_locked
        .checked_add(amount)
        .ok_or(ContractError::Overflow)?;
    sync_ratio_status(env, &mut state);
    store_anchor_state(env, &state);

    env.events().publish(
        (EV_ANCHOR_COLLATERAL_DEPOSITED, STATUS_DEPOSIT),
        (
            anchor.clone(),
            corridor,
            amount,
            state.collateral_locked,
            state.volume_unsettled,
            state.last_ratio_bps,
            state.assignments_paused,
            env.ledger().timestamp(),
        ),
    );

    Ok(state)
}

/// Release `amount` of *uncommitted* collateral back to `anchor`.
///
/// Rejected outright while the anchor is paused: an undercollateralised anchor
/// must post collateral before it may move any, otherwise it could dig the hole
/// deeper. Otherwise rejected when the withdrawal would push the backing ratio
/// below the corridor minimum.
pub fn withdraw_collateral(
    env: &Env,
    caller: &Address,
    anchor: &Address,
    corridor: AssetId,
    amount: u128,
) -> Result<AnchorCollateralState, ContractError> {
    caller.require_auth();
    if amount == 0 {
        return Err(ContractError::AmountTooLow);
    }

    let mut state = get_anchor_state(env, anchor, corridor);
    if state.assignments_paused {
        return Err(ContractError::AnchorUndercollateralized);
    }
    if amount > state.collateral_locked {
        return Err(ContractError::InsufficientReserveBalance);
    }

    let remaining_collateral = state.collateral_locked - amount;
    if !is_collateral_sufficient(
        remaining_collateral,
        state.volume_unsettled,
        corridor_min_ratio_bps(env, corridor),
    ) {
        return Err(ContractError::AnchorUndercollateralized);
    }

    state.collateral_locked = remaining_collateral;
    // A compliant withdrawal can never *newly* pause a healthy anchor — the
    // prospective check above already proved sufficiency — but re-evaluating
    // keeps the invariant enforced in exactly one place.
    sync_ratio_status(env, &mut state);
    store_anchor_state(env, &state);

    env.events().publish(
        (EV_ANCHOR_COLLATERAL_WITHDRAWN, STATUS_WITHDRAW),
        (
            anchor.clone(),
            corridor,
            amount,
            state.collateral_locked,
            state.volume_unsettled,
            state.last_ratio_bps,
            state.assignments_paused,
            env.ledger().timestamp(),
        ),
    );

    Ok(state)
}

// ---------------------------------------------------------------------------
// Fiat payout queue
// ---------------------------------------------------------------------------

/// Assign a new fiat payout of `amount` to `anchor` on `corridor`.
///
/// This is the "new remittance assignment" the monitor protects. It is refused
/// with [`ContractError::AnchorAssignmentsPaused`] when the anchor's stored
/// backing ratio is already below the corridor minimum, and refused with
/// [`ContractError::AnchorUndercollateralized`] when admitting the payout
/// would push the ratio below it.
///
/// This is the cap that keeps the ratio at or above the floor in normal
/// operation, so [`assignments_paused`] itself only latches when a breach
/// arrives from outside the guarded paths — a governance threshold increase or
/// a balance adjusted by a migration. Either way the anchor stops receiving
/// work, and only new collateral reopens it.
///
/// The queue is not modified on either rejection path, so no beneficiary is
/// left without a payout and `volume_unsettled` never drifts from the queue.
pub fn assign_fiat_payout(
    env: &Env,
    caller: &Address,
    anchor: &Address,
    corridor: AssetId,
    amount: u128,
) -> Result<FiatPayoutEntry, ContractError> {
    caller.require_auth();
    if amount == 0 {
        return Err(ContractError::AmountTooLow);
    }

    require_anchor_assignable(env, anchor, corridor)?;

    let mut state = get_anchor_state(env, anchor, corridor);
    let min_ratio_bps = corridor_min_ratio_bps(env, corridor);
    let next_volume = state
        .volume_unsettled
        .checked_add(amount)
        .ok_or(ContractError::Overflow)?;

    // Fail closed without booking the obligation: rejecting the work keeps the
    // ratio at or above the floor, so the stored state stays self-consistent.
    if !is_collateral_sufficient(state.collateral_locked, next_volume, min_ratio_bps) {
        return Err(ContractError::AnchorUndercollateralized);
    }

    let id_key = AnchorCollateralKey::NextPayoutId(anchor.clone(), corridor);
    let id: u64 = env
        .storage()
        .persistent()
        .get::<_, u64>(&id_key)
        .unwrap_or(0)
        .checked_add(1)
        .ok_or(ContractError::Overflow)?;
    env.storage().persistent().set(&id_key, &id);

    let entry = FiatPayoutEntry {
        id,
        amount,
        opened_at: env.ledger().timestamp(),
    };

    let mut queue = read_payout_queue(env, anchor, corridor);
    queue.push_back(entry.clone());
    env.storage().persistent().set(
        &AnchorCollateralKey::PayoutQueue(anchor.clone(), corridor),
        &queue,
    );

    state.volume_unsettled = next_volume;
    sync_ratio_status(env, &mut state);
    store_anchor_state(env, &state);

    env.events().publish(
        (EV_ANCHOR_PAYOUT_QUEUED, STATUS_QUEUED),
        (
            anchor.clone(),
            corridor,
            id,
            amount,
            state.volume_unsettled,
            state.collateral_locked,
            state.last_ratio_bps,
            env.ledger().timestamp(),
        ),
    );

    Ok(entry)
}

/// Mark a queued payout as settled off-ledger.
///
/// Consumes the matching collateral: the anchor has paid out the fiat, so the
/// token leg that backed it is no longer locked. `volume_unsettled` and
/// `collateral_locked` fall by the same `amount`, which leaves the ratio — and
/// therefore the pause state — unchanged in the normal case.
///
/// The caller must pass the exact queued amount. A mismatch would desynchronise
/// `volume_unsettled` from the queue, so it is rejected and the entry stays
/// queued.
pub fn settle_fiat_payout(
    env: &Env,
    caller: &Address,
    anchor: &Address,
    corridor: AssetId,
    payout_id: u64,
    amount: u128,
) -> Result<AnchorCollateralState, ContractError> {
    caller.require_auth();
    if amount == 0 {
        return Err(ContractError::AmountTooLow);
    }

    let mut queue = read_payout_queue(env, anchor, corridor);
    let mut position: Option<u32> = None;
    for (idx, entry) in queue.iter().enumerate() {
        if entry.id == payout_id {
            position = Some(idx as u32);
            break;
        }
    }
    let position = position.ok_or(ContractError::InvalidEscrowState)?;

    let queued = queue.get(position).ok_or(ContractError::InvalidEscrowState)?;
    if queued.amount != amount {
        return Err(ContractError::InvalidArgument);
    }

    let mut state = get_anchor_state(env, anchor, corridor);
    // Defence in depth. A compliant engine always holds
    // `collateral_locked >= volume_unsettled` while a queue exists, so these
    // are unreachable in the happy path and only trip on a corrupted record.
    if amount > state.volume_unsettled {
        return Err(ContractError::InvalidEscrowState);
    }
    if amount > state.collateral_locked {
        return Err(ContractError::InsufficientReserveBalance);
    }

    queue.remove(position);
    env.storage().persistent().set(
        &AnchorCollateralKey::PayoutQueue(anchor.clone(), corridor),
        &queue,
    );

    state.volume_unsettled -= amount;
    state.collateral_locked -= amount;
    sync_ratio_status(env, &mut state);
    store_anchor_state(env, &state);

    env.events().publish(
        (EV_ANCHOR_PAYOUT_SETTLED, STATUS_SETTLED),
        (
            anchor.clone(),
            corridor,
            payout_id,
            amount,
            state.volume_unsettled,
            state.collateral_locked,
            state.last_ratio_bps,
            state.assignments_paused,
            env.ledger().timestamp(),
        ),
    );

    Ok(state)
}

/// Permissionless monitoring tick.
///
/// Re-reads the stored balances, re-evaluates the ratio and latches or clears
/// the pause flag. Anchors, keepers and off-chain monitors can call this so the
/// pause reflects the current state without waiting for the next collateral or
/// payout movement.
pub fn sync_anchor_ratio(
    env: &Env,
    anchor: &Address,
    corridor: AssetId,
) -> Result<AnchorCollateralStatus, ContractError> {
    let mut state = get_anchor_state(env, anchor, corridor);
    sync_ratio_status(env, &mut state);
    store_anchor_state(env, &state);
    Ok(get_anchor_status(env, anchor, corridor))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::testutils::Events as _;
    use soroban_sdk::IntoVal;

    const CORRIDOR: AssetId = 3_897_123_275; // NGN
    const OTHER_CORRIDOR: AssetId = 2_654_435_761; // KES

    /// Test harness that runs every engine call inside its own contract frame.
    ///
    /// Soroban permits one `require_auth` per address per invocation frame, so
    /// each state-changing call needs a separate frame to model the separate
    /// transactions it really is — and storage is only reachable from inside a
    /// frame. The `*_for` variants take an explicit anchor and corridor; the
    /// short forms use the fixture's own anchor on [`CORRIDOR`].
    struct Fixture {
        env: Env,
        id: Address,
        admin: Address,
        anchor: Address,
        keeper: Address,
    }

    impl Fixture {
        fn new() -> Self {
            let env = Env::default();
            env.mock_all_auths();
            let id = env.register_contract(None, crate::TimeLockedUpgradeContract);
            let admin = Address::generate(&env);
            let anchor = Address::generate(&env);
            let keeper = Address::generate(&env);

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
                anchor,
                keeper,
            }
        }

        fn frame<T, F: FnOnce() -> T>(&self, f: F) -> T {
            self.env.as_contract(&self.id, f)
        }

        // ── mutating engine entry points ────────────────────────────────

        fn deposit_for(
            &self,
            anchor: &Address,
            corridor: AssetId,
            amount: u128,
        ) -> Result<AnchorCollateralState, ContractError> {
            self.frame(|| deposit_collateral(&self.env, &self.keeper, anchor, corridor, amount))
        }

        fn deposit(&self, amount: u128) -> Result<AnchorCollateralState, ContractError> {
            self.deposit_for(&self.anchor.clone(), CORRIDOR, amount)
        }

        fn withdraw_for(
            &self,
            anchor: &Address,
            corridor: AssetId,
            amount: u128,
        ) -> Result<AnchorCollateralState, ContractError> {
            self.frame(|| withdraw_collateral(&self.env, &self.keeper, anchor, corridor, amount))
        }

        fn withdraw(&self, amount: u128) -> Result<AnchorCollateralState, ContractError> {
            self.withdraw_for(&self.anchor.clone(), CORRIDOR, amount)
        }

        fn assign_for(
            &self,
            anchor: &Address,
            corridor: AssetId,
            amount: u128,
        ) -> Result<FiatPayoutEntry, ContractError> {
            self.frame(|| assign_fiat_payout(&self.env, &self.keeper, anchor, corridor, amount))
        }

        fn assign(&self, amount: u128) -> Result<FiatPayoutEntry, ContractError> {
            self.assign_for(&self.anchor.clone(), CORRIDOR, amount)
        }

        fn settle(
            &self,
            payout_id: u64,
            amount: u128,
        ) -> Result<AnchorCollateralState, ContractError> {
            self.frame(|| {
                settle_fiat_payout(
                    &self.env,
                    &self.keeper,
                    &self.anchor,
                    CORRIDOR,
                    payout_id,
                    amount,
                )
            })
        }

        // ── reads ───────────────────────────────────────────────────────

        fn status_for(&self, anchor: &Address, corridor: AssetId) -> AnchorCollateralStatus {
            self.frame(|| get_anchor_status(&self.env, anchor, corridor))
        }

        fn status(&self) -> AnchorCollateralStatus {
            self.status_for(&self.anchor.clone(), CORRIDOR)
        }

        fn assignable_for(
            &self,
            anchor: &Address,
            corridor: AssetId,
        ) -> Result<(), ContractError> {
            self.frame(|| require_anchor_assignable(&self.env, anchor, corridor))
        }

        fn assignable(&self) -> Result<(), ContractError> {
            self.assignable_for(&self.anchor.clone(), CORRIDOR)
        }

        fn locked_for(&self, anchor: &Address, corridor: AssetId) -> u128 {
            self.frame(|| locked_collateral(&self.env, anchor, corridor))
        }

        fn unsettled_for(&self, anchor: &Address, corridor: AssetId) -> u128 {
            self.frame(|| unsettled_volume(&self.env, anchor, corridor))
        }

        fn queued_for(&self, anchor: &Address, corridor: AssetId) -> u128 {
            self.frame(|| queued_volume(&self.env, anchor, corridor))
        }

        fn queue_len_for(&self, anchor: &Address, corridor: AssetId) -> u32 {
            self.frame(|| payout_queue_len(&self.env, anchor, corridor))
        }

        fn paused_for(&self, anchor: &Address, corridor: AssetId) -> bool {
            self.frame(|| is_anchor_paused(&self.env, anchor, corridor))
        }

        fn paused(&self) -> bool {
            self.paused_for(&self.anchor.clone(), CORRIDOR)
        }

        fn min_ratio(&self, corridor: AssetId) -> u32 {
            self.frame(|| corridor_min_ratio_bps(&self.env, corridor))
        }

        fn sync(&self, anchor: &Address, corridor: AssetId) -> AnchorCollateralStatus {
            self.frame(|| sync_anchor_ratio(&self.env, anchor, corridor))
                .expect("sync_anchor_ratio is infallible")
        }

        fn set_min(&self, admin: &Address, corridor: AssetId, bps: u32) -> Result<u32, ContractError> {
            self.frame(|| set_min_collateral_ratio_bps(&self.env, admin, corridor, bps))
        }

        // ── events ──────────────────────────────────────────────────────

        /// Number of times the event `(name, status)` was published.
        fn emissions(&self, name: &Symbol, status: &Symbol) -> u32 {
            let expected = soroban_sdk::vec![
                &self.env,
                name.into_val(&self.env),
                status.into_val(&self.env)
            ];
            self.env
                .events()
                .all()
                .iter()
                .filter(|(_, topics, _)| *topics == expected)
                .count() as u32
        }

        fn emitted(&self, name: &Symbol, status: &Symbol) -> bool {
            self.emissions(name, status) > 0
        }
    }

    // ── ratio_ge ───────────────────────────────────────────────────────────

    #[test]
    fn ratio_ge_matches_exact_cross_multiplication() {
        // Exhaustive over a small grid. `a * d` and `c * b` cannot overflow
        // here, so the expected answer is directly computable and independent
        // of the Euclidean implementation.
        for b in 1u128..=24 {
            for d in 1u128..=24 {
                for a in 0u128..=24 {
                    for c in 0u128..=24 {
                        assert_eq!(
                            ratio_ge(a, b, c, d),
                            a * d >= c * b,
                            "ratio_ge({a}, {b}, {c}, {d})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn ratio_ge_never_overflows_at_u128_bounds() {
        // Naive `a * d >= c * b` would wrap on every one of these.
        assert!(ratio_ge(
            u128::MAX,
            1,
            u32::MAX as u128,
            BPS_DENOMINATOR as u128
        ));
        assert!(ratio_ge(1, u128::MAX, 1, u128::MAX));
        assert!(!ratio_ge(u128::MAX - 1, u128::MAX, 1, 1));
        assert!(ratio_ge(u128::MAX, 2, u128::MAX / 2, 1));
    }

    #[test]
    fn ratio_ge_handles_zero_numerators() {
        assert!(ratio_ge(0, 7, 0, 3));
        assert!(!ratio_ge(0, 7, 1, 3));
        assert!(ratio_ge(1, 3, 0, 3));
        assert!(ratio_ge(0, u128::MAX, 0, 1));
    }

    #[test]
    fn ratio_ge_handles_long_continued_fraction_chains() {
        // Successive Fibonacci ratios are the worst case for the Euclidean
        // recursion: the chain runs for the maximum number of steps.
        let (mut a, mut b) = (1u128, 2u128);
        let (mut c, mut d) = (1u128, 3u128);
        let mut checked = 0;
        for _ in 0..40 {
            let next = a + b;
            a = b;
            b = next;
            let next_c = c + d;
            c = d;
            d = next_c;
            if a * d != c * b {
                assert_eq!(ratio_ge(a, b, c, d), a * d > c * b);
                checked += 1;
            }
        }
        assert!(checked > 0, "the Fibonacci ladder should exercise the recursion");
    }

    // ── backing_ratio_bps ──────────────────────────────────────────────────

    #[test]
    fn backing_ratio_bps_reports_whole_percent() {
        assert_eq!(backing_ratio_bps(1_200, 1_000), 12_000);
        assert_eq!(backing_ratio_bps(1_250, 1_000), 12_500);
        assert_eq!(backing_ratio_bps(1_000, 1_000), 10_000);
        assert_eq!(backing_ratio_bps(1_199, 1_000), 11_990);
    }

    #[test]
    fn backing_ratio_bps_is_exact_at_u128_bounds() {
        // The naive `collateral * 10_000 / volume` overflows on every one of
        // these and would report a wildly understated ratio.
        assert_eq!(backing_ratio_bps(u128::MAX, u128::MAX), 10_000);
        assert_eq!(backing_ratio_bps(u128::MAX, 1), u32::MAX);
        // 1_200x collateral is 12_000_000 bp, not 12_000.
        assert_eq!(backing_ratio_bps(u128::MAX, u128::MAX / 1_200), 12_000_000);
        // A 1:1 pairing at full scale is 100 %, not the 0 bp a wrap-around
        // would have produced.
        assert_eq!(backing_ratio_bps(u128::MAX, u128::MAX - 1), 10_000);
    }

    #[test]
    fn backing_ratio_bps_agrees_with_the_exact_ratio_on_small_values() {
        for volume in 1u128..=40 {
            for collateral in 0u128..=200 {
                let expected = collateral * BPS_DENOMINATOR as u128 / volume;
                let expected = if expected > u32::MAX as u128 {
                    u32::MAX
                } else {
                    expected as u32
                };
                assert_eq!(
                    backing_ratio_bps(collateral, volume),
                    expected,
                    "backing_ratio_bps({collateral}, {volume})"
                );
            }
        }
    }

    #[test]
    fn backing_ratio_bps_truncates_and_saturates() {
        // Truncation, not rounding: 1/3 of a backing ratio is 3_333.33 bp
        // and is reported as 3_333.
        assert_eq!(backing_ratio_bps(1, 3), 3_333);
        assert_eq!(backing_ratio_bps(2, 3), 6_666);
        assert_eq!(backing_ratio_bps(11_999, 10_000), 11_999);
        // Saturation rather than wrap-around at the u32 boundary.
        assert_eq!(backing_ratio_bps(u128::MAX, 1), u32::MAX);
        assert_eq!(backing_ratio_bps(u128::MAX, u128::MAX), 10_000);
    }

    #[test]
    fn backing_ratio_bps_is_zero_for_an_empty_queue() {
        assert_eq!(backing_ratio_bps(1_000, 0), 0);
        assert_eq!(backing_ratio_bps(0, 0), 0);
    }

    // ── is_collateral_sufficient ───────────────────────────────────────────

    #[test]
    fn empty_payout_queue_is_always_sufficient() {
        assert!(is_collateral_sufficient(
            0,
            0,
            DEFAULT_MIN_COLLATERAL_RATIO_BPS
        ));
        assert!(is_collateral_sufficient(
            0,
            0,
            MAX_ALLOWED_COLLATERAL_RATIO_BPS
        ));
    }

    #[test]
    fn zero_collateral_against_a_live_queue_breaches() {
        assert!(!is_collateral_sufficient(
            0,
            1,
            DEFAULT_MIN_COLLATERAL_RATIO_BPS
        ));
    }

    #[test]
    fn the_120_percent_floor_is_inclusive() {
        // Exactly 1.20 must pass; a single stroop below must fail.
        assert!(is_collateral_sufficient(
            1_200,
            1_000,
            DEFAULT_MIN_COLLATERAL_RATIO_BPS
        ));
        assert!(!is_collateral_sufficient(
            1_199,
            1_000,
            DEFAULT_MIN_COLLATERAL_RATIO_BPS
        ));
    }

    #[test]
    fn sufficiency_respects_a_custom_threshold() {
        assert!(is_collateral_sufficient(1_500, 1_000, 15_000));
        assert!(!is_collateral_sufficient(1_500, 1_000, 15_001));
    }

    #[test]
    fn sufficiency_is_exact_at_u128_bounds() {
        let min = DEFAULT_MIN_COLLATERAL_RATIO_BPS;

        // Full backing on the largest possible balance is exactly 100 %, so it
        // clears a 100 % floor and misses the 120 % floor by a wide margin.
        assert!(is_collateral_sufficient(u128::MAX, u128::MAX, MIN_ALLOWED_COLLATERAL_RATIO_BPS));
        assert!(!is_collateral_sufficient(u128::MAX, u128::MAX, min));

        // The exact 120 % point on a 10^34-scale balance. At this magnitude
        // `collateral * 10_000` exceeds `u128::MAX`, so a naive
        // cross-multiplication would overflow — which is precisely why the
        // decision path uses `ratio_ge`.
        let volume = u128::MAX / 12_000 * 1_000; // exact multiple of 1_000
        let exact = volume / 1_000 * 1_200; // exactly 1.2 x volume
        assert!(is_collateral_sufficient(exact, volume, min));
        // One stroop short of 120 % on that same enormous balance.
        assert!(!is_collateral_sufficient(exact - 1, volume, min));
        // And the maximally collateralised anchor is trivially sufficient.
        assert!(is_collateral_sufficient(u128::MAX, 1, min));
        assert!(!is_collateral_sufficient(1, u128::MAX, min));
    }

    // ── state lifecycle ────────────────────────────────────────────────────

    #[test]
    fn a_fresh_anchor_starts_healthy_and_unpaused() {
        let f = Fixture::new();
        let status = f.status();
        assert!(status.healthy);
        assert!(!status.assignments_paused);
        assert_eq!(status.min_ratio_bps, DEFAULT_MIN_COLLATERAL_RATIO_BPS);
        assert_eq!(status.collateral_locked, 0);
        assert_eq!(status.volume_unsettled, 0);
        assert_eq!(status.queue_len, 0);
        assert_eq!(status.ratio_bps, 0);
        assert!(f.assignable().is_ok());
    }

    #[test]
    fn deposit_collateral_increases_the_locked_balance() {
        let f = Fixture::new();
        let state = f.deposit(5_000).unwrap();
        assert_eq!(state.collateral_locked, 5_000);
        assert_eq!(f.locked_for(&f.anchor, CORRIDOR), 5_000);
    }

    #[test]
    fn deposit_rejects_a_zero_amount() {
        let f = Fixture::new();
        assert_eq!(f.deposit(0), Err(ContractError::AmountTooLow));
    }

    #[test]
    fn deposit_saturates_instead_of_wrapping() {
        let f = Fixture::new();
        f.deposit(u128::MAX).unwrap();
        assert_eq!(f.deposit(1), Err(ContractError::Overflow));
        assert_eq!(f.locked_for(&f.anchor, CORRIDOR), u128::MAX);
    }

    #[test]
    fn assignment_within_capacity_queues_the_payout() {
        let f = Fixture::new();
        f.deposit(1_200).unwrap();
        let entry = f.assign(1_000).unwrap();
        assert_eq!(entry.id, 1);
        assert_eq!(entry.amount, 1_000);
        assert_eq!(f.unsettled_for(&f.anchor, CORRIDOR), 1_000);
        assert_eq!(f.queued_for(&f.anchor, CORRIDOR), 1_000);
        assert_eq!(f.queue_len_for(&f.anchor, CORRIDOR), 1);
        assert!(!f.paused());
    }

    #[test]
    fn assignment_ids_increase_monotonically() {
        let f = Fixture::new();
        f.deposit(10_000).unwrap();
        let first = f.assign(100).unwrap();
        let second = f.assign(100).unwrap();
        assert_eq!(first.id, 1);
        assert_eq!(second.id, 2);
        assert_eq!(f.queue_len_for(&f.anchor, CORRIDOR), 2);
        assert_eq!(f.queued_for(&f.anchor, CORRIDOR), 200);
    }

    #[test]
    fn assignment_rejects_a_zero_amount() {
        let f = Fixture::new();
        f.deposit(1_200).unwrap();
        assert_eq!(f.assign(0), Err(ContractError::AmountTooLow));
    }

    #[test]
    fn assignment_breaching_the_120_percent_floor_is_refused() {
        let f = Fixture::new();
        // Exactly 120 % of 1_000 → admitted.
        f.deposit(1_200).unwrap();
        f.assign(1_000).unwrap();

        // One more stroop would take the ratio to 1_200 / 1_001 ≈ 119.88 %.
        assert_eq!(
            f.assign(1),
            Err(ContractError::AnchorUndercollateralized)
        );

        // The rejected payout was neither booked nor queued, so the aggregate
        // and the queue stay in lockstep and the anchor is not paused — it is
        // at capacity, not undercollateralised.
        assert_eq!(f.unsettled_for(&f.anchor, CORRIDOR), 1_000);
        assert_eq!(f.queued_for(&f.anchor, CORRIDOR), 1_000);
        assert_eq!(f.queue_len_for(&f.anchor, CORRIDOR), 1);
        assert!(!f.paused());
        assert!(f.status().healthy);
    }

    #[test]
    fn a_paused_anchor_rejects_every_new_assignment() {
        let f = Fixture::new();
        // 1_300 / 1_000 = 130 % is fine at the 120 % default but breaches a
        // 150 % floor, which is how a stored ratio dips below the minimum.
        f.deposit(1_300).unwrap();
        f.assign(1_000).unwrap();
        f.set_min(&f.admin.clone(), CORRIDOR, 15_000).unwrap();
        let status = f.sync(&f.anchor, CORRIDOR);
        assert!(status.assignments_paused);
        assert!(!status.healthy);

        // Even a trivially small assignment is refused while paused, and the
        // guard rejects before any capacity arithmetic runs.
        assert_eq!(f.assign(1), Err(ContractError::AnchorAssignmentsPaused));
        assert_eq!(f.assign(1_000), Err(ContractError::AnchorAssignmentsPaused));
        assert_eq!(f.assignable(), Err(ContractError::AnchorAssignmentsPaused));
        assert_eq!(f.unsettled_for(&f.anchor, CORRIDOR), 1_000);
    }

    #[test]
    fn depositing_extra_collateral_resumes_routing() {
        let f = Fixture::new();
        f.deposit(1_300).unwrap();
        f.assign(1_000).unwrap();
        f.set_min(&f.admin.clone(), CORRIDOR, 15_000).unwrap();
        assert!(f.sync(&f.anchor, CORRIDOR).assignments_paused);
        assert_eq!(f.assign(10), Err(ContractError::AnchorAssignmentsPaused));

        // 1_300 + 300 = 1_600 against 1_000 outstanding is 160 % ≥ 150 %, so
        // routing resumes in the same transaction that posted the collateral.
        let state = f.deposit(300).unwrap();
        assert!(!state.assignments_paused);
        assert_eq!(state.paused_at, 0);
        assert!(!f.paused());
        assert!(f.assignable().is_ok());
        assert!(f.status().healthy);

        // And the anchor can take work again.
        assert!(f.assign(10).is_ok());
    }

    #[test]
    fn an_undersized_deposit_leaves_the_anchor_paused() {
        let f = Fixture::new();
        f.deposit(1_300).unwrap();
        f.assign(1_000).unwrap();
        f.set_min(&f.admin.clone(), CORRIDOR, 15_000).unwrap();
        f.sync(&f.anchor, CORRIDOR);

        // 1_400 / 1_000 = 140 % is still short of the 150 % floor.
        let state = f.deposit(100).unwrap();
        assert!(state.assignments_paused);
        assert_eq!(f.assign(1), Err(ContractError::AnchorAssignmentsPaused));
    }

    #[test]
    fn a_resumed_anchor_that_breaches_again_is_paused_again() {
        let f = Fixture::new();
        f.deposit(1_300).unwrap();
        f.assign(1_000).unwrap();

        // Breach at 150 %, recover with collateral, then breach again.
        f.set_min(&f.admin.clone(), CORRIDOR, 15_000).unwrap();
        f.sync(&f.anchor, CORRIDOR);
        assert!(f.paused());

        f.deposit(300).unwrap();
        assert!(!f.paused());

        // 1_600 / 1_000 = 160 % cannot satisfy a 200 % floor.
        f.set_min(&f.admin.clone(), CORRIDOR, 20_000).unwrap();
        assert!(f.sync(&f.anchor, CORRIDOR).assignments_paused);
        assert!(f.paused());
    }

    #[test]
    fn settling_a_payout_consumes_both_sides_of_the_ledger() {
        let f = Fixture::new();
        f.deposit(1_200).unwrap();
        let entry = f.assign(1_000).unwrap();

        let state = f.settle(entry.id, 1_000).unwrap();
        assert_eq!(state.collateral_locked, 200);
        assert_eq!(state.volume_unsettled, 0);
        assert_eq!(f.queue_len_for(&f.anchor, CORRIDOR), 0);
        assert_eq!(f.queued_for(&f.anchor, CORRIDOR), 0);
        assert!(!state.assignments_paused);
    }

    #[test]
    fn settling_preserves_the_backing_ratio() {
        let f = Fixture::new();
        f.deposit(2_400).unwrap();
        let entry = f.assign(1_000).unwrap();
        assert_eq!(backing_ratio_bps(2_400, 1_000), 24_000);

        f.settle(entry.id, 1_000).unwrap();
        let status = f.status();
        // Both sides fell by 1_000, leaving 1_400 against an empty queue.
        assert_eq!(status.collateral_locked, 1_400);
        assert_eq!(status.volume_unsettled, 0);
        assert!(status.healthy);
    }

    #[test]
    fn settling_drains_a_multi_entry_queue() {
        let f = Fixture::new();
        f.deposit(2_400).unwrap();
        let first = f.assign(1_000).unwrap();
        let second = f.assign(1_000).unwrap();
        assert_eq!(f.queue_len_for(&f.anchor, CORRIDOR), 2);

        f.settle(first.id, 1_000).unwrap();
        f.settle(second.id, 1_000).unwrap();

        let status = f.status();
        assert_eq!(status.queue_len, 0);
        assert_eq!(status.volume_unsettled, 0);
        assert_eq!(status.collateral_locked, 400);
        assert_eq!(f.queued_for(&f.anchor, CORRIDOR), 0);
    }

    #[test]
    fn settling_an_unknown_payout_id_fails() {
        let f = Fixture::new();
        f.deposit(1_200).unwrap();
        assert_eq!(f.settle(99, 100), Err(ContractError::InvalidEscrowState));
    }

    #[test]
    fn settling_with_a_mismatched_amount_fails() {
        let f = Fixture::new();
        f.deposit(1_200).unwrap();
        let entry = f.assign(1_000).unwrap();
        assert_eq!(f.settle(entry.id, 999), Err(ContractError::InvalidArgument));
        // The queue entry survives the rejected settlement.
        assert_eq!(f.queue_len_for(&f.anchor, CORRIDOR), 1);
        assert_eq!(f.unsettled_for(&f.anchor, CORRIDOR), 1_000);
    }

    #[test]
    fn settling_rejects_a_zero_amount() {
        let f = Fixture::new();
        f.deposit(1_200).unwrap();
        let entry = f.assign(1_000).unwrap();
        assert_eq!(f.settle(entry.id, 0), Err(ContractError::AmountTooLow));
    }

    // ── withdrawal ─────────────────────────────────────────────────────────

    #[test]
    fn withdrawing_uncommitted_collateral_succeeds() {
        let f = Fixture::new();
        f.deposit(1_500).unwrap();
        let state = f.withdraw(300).unwrap();
        assert_eq!(state.collateral_locked, 1_200);
        assert!(!state.assignments_paused);
    }

    #[test]
    fn withdrawing_more_than_is_locked_fails() {
        let f = Fixture::new();
        f.deposit(1_000).unwrap();
        assert_eq!(
            f.withdraw(1_001),
            Err(ContractError::InsufficientReserveBalance)
        );
    }

    #[test]
    fn withdrawing_rejects_a_zero_amount() {
        let f = Fixture::new();
        f.deposit(1_000).unwrap();
        assert_eq!(f.withdraw(0), Err(ContractError::AmountTooLow));
    }

    #[test]
    fn withdrawing_into_a_breach_is_rejected() {
        let f = Fixture::new();
        f.deposit(1_200).unwrap();
        f.assign(1_000).unwrap();
        // 1_200 locked backs 1_000 outstanding at 120 %; freeing even one
        // stroop would drop it to 119.99 %.
        assert_eq!(
            f.withdraw(1),
            Err(ContractError::AnchorUndercollateralized)
        );
        assert_eq!(f.locked_for(&f.anchor, CORRIDOR), 1_200);
    }

    #[test]
    fn a_paused_anchor_cannot_withdraw_at_all() {
        let f = Fixture::new();
        f.deposit(1_300).unwrap();
        f.assign(1_000).unwrap();
        f.set_min(&f.admin.clone(), CORRIDOR, 15_000).unwrap();
        f.sync(&f.anchor, CORRIDOR);
        assert!(f.paused());

        // The queue is far smaller than the lock, so only the pause freezes
        // the withdrawal — the prospective ratio check alone would allow it.
        assert_eq!(
            f.withdraw(1),
            Err(ContractError::AnchorUndercollateralized)
        );
        assert_eq!(f.locked_for(&f.anchor, CORRIDOR), 1_300);
    }

    // ── threshold configuration ────────────────────────────────────────────

    #[test]
    fn the_default_threshold_applies_until_configured() {
        let f = Fixture::new();
        assert_eq!(f.min_ratio(CORRIDOR), DEFAULT_MIN_COLLATERAL_RATIO_BPS);
        assert_eq!(f.set_min(&f.admin.clone(), CORRIDOR, 15_000), Ok(15_000));
        assert_eq!(f.min_ratio(CORRIDOR), 15_000);
    }

    #[test]
    fn raising_the_threshold_pauses_a_now_undercollateralised_anchor() {
        let f = Fixture::new();
        // 1_300 / 1_000 = 130 %: comfortable at 120 %, insufficient at 150 %.
        f.deposit(1_300).unwrap();
        f.assign(1_000).unwrap();
        assert!(!f.paused());

        assert_eq!(f.set_min(&f.admin.clone(), CORRIDOR, 15_000), Ok(15_000));
        // The stored state is untouched until it is re-evaluated ...
        assert!(!f.paused());
        // ... and the permissionless monitoring tick latches the pause.
        let status = f.sync(&f.anchor, CORRIDOR);
        assert!(!status.healthy);
        assert!(status.assignments_paused);
        assert_eq!(f.assign(1), Err(ContractError::AnchorAssignmentsPaused));
    }

    #[test]
    fn lowering_the_threshold_lets_a_paused_anchor_resume() {
        let f = Fixture::new();
        f.deposit(1_300).unwrap();
        f.assign(1_000).unwrap();
        f.set_min(&f.admin.clone(), CORRIDOR, 15_000).unwrap();
        assert!(f.sync(&f.anchor, CORRIDOR).assignments_paused);

        // 130 % clears a 100 % floor even though no collateral was added.
        assert_eq!(
            f.set_min(
                &f.admin.clone(),
                CORRIDOR,
                MIN_ALLOWED_COLLATERAL_RATIO_BPS
            ),
            Ok(MIN_ALLOWED_COLLATERAL_RATIO_BPS)
        );
        let status = f.sync(&f.anchor, CORRIDOR);
        assert!(status.healthy);
        assert!(!status.assignments_paused);
        assert!(f.assignable().is_ok());
    }

    #[test]
    fn threshold_bounds_are_enforced() {
        let f = Fixture::new();
        assert_eq!(
            f.set_min(&f.admin.clone(), CORRIDOR, MIN_ALLOWED_COLLATERAL_RATIO_BPS - 1),
            Err(ContractError::InvalidArgument)
        );
        assert_eq!(
            f.set_min(&f.admin.clone(), CORRIDOR, MAX_ALLOWED_COLLATERAL_RATIO_BPS + 1),
            Err(ContractError::InvalidArgument)
        );
        assert_eq!(
            f.set_min(
                &f.admin.clone(),
                CORRIDOR,
                MIN_ALLOWED_COLLATERAL_RATIO_BPS
            ),
            Ok(MIN_ALLOWED_COLLATERAL_RATIO_BPS)
        );
        assert_eq!(
            f.set_min(
                &f.admin.clone(),
                CORRIDOR,
                MAX_ALLOWED_COLLATERAL_RATIO_BPS
            ),
            Ok(MAX_ALLOWED_COLLATERAL_RATIO_BPS)
        );
    }

    #[test]
    fn a_non_admin_cannot_change_the_threshold() {
        let f = Fixture::new();
        assert_eq!(
            f.set_min(&f.keeper.clone(), CORRIDOR, 15_000),
            Err(ContractError::NotAdmin)
        );
    }

    // ── monitoring tick ────────────────────────────────────────────────────

    #[test]
    fn the_monitoring_tick_is_idempotent_on_a_healthy_anchor() {
        let f = Fixture::new();
        f.deposit(1_500).unwrap();
        let first = f.sync(&f.anchor, CORRIDOR);
        let second = f.sync(&f.anchor, CORRIDOR);
        assert_eq!(first, second);
        assert!(first.healthy);
        assert!(!first.assignments_paused);
    }

    #[test]
    fn the_monitoring_tick_keeps_a_paused_anchor_paused() {
        let f = Fixture::new();
        f.deposit(1_300).unwrap();
        f.assign(1_000).unwrap();
        f.set_min(&f.admin.clone(), CORRIDOR, 15_000).unwrap();
        f.sync(&f.anchor, CORRIDOR);
        assert!(f.paused());

        // Repeated ticks are idempotent: the flag is a pure function of the
        // stored balances, so it neither clears nor re-latches.
        for _ in 0..3 {
            let status = f.sync(&f.anchor, CORRIDOR);
            assert!(!status.healthy);
            assert!(status.assignments_paused);
        }
    }

    // ── status view ────────────────────────────────────────────────────────

    #[test]
    fn the_status_view_reports_the_ratio_and_depth() {
        let f = Fixture::new();
        f.deposit(1_500).unwrap();
        f.assign(1_000).unwrap();
        f.assign(100).unwrap();

        let status = f.status();
        assert_eq!(status.collateral_locked, 1_500);
        assert_eq!(status.volume_unsettled, 1_100);
        // 1_500 / 1_100 = 136.36 % → 13_636 bp after truncation.
        assert_eq!(status.ratio_bps, 13_636);
        assert_eq!(status.queue_len, 2);
        assert!(status.healthy);
    }

    #[test]
    fn the_status_view_reports_a_breach() {
        let f = Fixture::new();
        f.deposit(1_300).unwrap();
        f.assign(1_000).unwrap();
        f.set_min(&f.admin.clone(), CORRIDOR, 15_000).unwrap();
        f.sync(&f.anchor, CORRIDOR);

        let status = f.status();
        assert_eq!(status.ratio_bps, 13_000);
        assert_eq!(status.min_ratio_bps, 15_000);
        assert!(!status.healthy);
        assert!(status.assignments_paused);
    }

    // ── isolation ──────────────────────────────────────────────────────────

    #[test]
    fn corridors_are_monitored_independently() {
        let f = Fixture::new();
        f.deposit(1_300).unwrap();
        f.assign(1_000).unwrap();
        f.set_min(&f.admin.clone(), CORRIDOR, 15_000).unwrap();
        f.sync(&f.anchor, CORRIDOR);
        assert!(f.paused_for(&f.anchor, CORRIDOR));

        // A different corridor for the same anchor starts from zero and is
        // unaffected by the first corridor's threshold.
        assert_eq!(f.unsettled_for(&f.anchor, OTHER_CORRIDOR), 0);
        assert_eq!(f.locked_for(&f.anchor, OTHER_CORRIDOR), 0);
        assert!(!f.paused_for(&f.anchor, OTHER_CORRIDOR));
        assert!(f.assignable_for(&f.anchor, OTHER_CORRIDOR).is_ok());
        assert_eq!(f.min_ratio(OTHER_CORRIDOR), DEFAULT_MIN_COLLATERAL_RATIO_BPS);
    }

    #[test]
    fn thresholds_are_per_corridor() {
        let f = Fixture::new();
        f.deposit(1_300).unwrap();
        assert_eq!(
            f.set_min(&f.admin.clone(), CORRIDOR, 15_000),
            Ok(15_000)
        );
        assert_eq!(f.min_ratio(CORRIDOR), 15_000);
        assert_eq!(f.min_ratio(OTHER_CORRIDOR), DEFAULT_MIN_COLLATERAL_RATIO_BPS);
    }

    #[test]
    fn anchors_are_monitored_independently() {
        let f = Fixture::new();
        let other = Address::generate(&f.env);
        f.deposit(1_300).unwrap();
        f.assign(1_000).unwrap();
        f.set_min(&f.admin.clone(), CORRIDOR, 15_000).unwrap();
        f.sync(&f.anchor, CORRIDOR);
        assert!(f.paused());

        // The corridor threshold is shared, but an anchor with no queue has
        // an unbounded ratio, so it is not paused.
        assert_eq!(f.locked_for(&other, CORRIDOR), 0);
        assert!(!f.paused_for(&other, CORRIDOR));
        assert!(f.assignable_for(&other, CORRIDOR).is_ok());
    }

    // ── accounting invariant ───────────────────────────────────────────────

    #[test]
    fn the_queue_and_the_aggregate_never_drift() {
        let f = Fixture::new();
        f.deposit(6_000).unwrap();
        for _ in 0..4 {
            let entry = f.assign(1_000).unwrap();
            assert_eq!(
                f.unsettled_for(&f.anchor, CORRIDOR),
                f.queued_for(&f.anchor, CORRIDOR)
            );
            f.settle(entry.id, 1_000).unwrap();
            assert_eq!(
                f.unsettled_for(&f.anchor, CORRIDOR),
                f.queued_for(&f.anchor, CORRIDOR)
            );
        }
        let status = f.status();
        assert_eq!(status.queue_len, 0);
        assert_eq!(status.volume_unsettled, 0);
        assert_eq!(status.collateral_locked, 2_000);
    }

    // ── events ─────────────────────────────────────────────────────────────

    #[test]
    fn pausing_and_resuming_emit_exactly_one_transition_event() {
        let f = Fixture::new();
        f.deposit(1_300).unwrap();
        f.assign(1_000).unwrap();
        assert!(!f.emitted(&EV_ANCHOR_ROUTING_PAUSED, &STATUS_PAUSED));

        // The threshold raise alone does not move the flag; the monitor tick
        // that observes the new floor does, and emits exactly once.
        f.set_min(&f.admin.clone(), CORRIDOR, 15_000).unwrap();
        assert_eq!(f.emissions(&EV_ANCHOR_ROUTING_PAUSED, &STATUS_PAUSED), 0);
        f.sync(&f.anchor, CORRIDOR);
        f.sync(&f.anchor, CORRIDOR);
        assert_eq!(f.emissions(&EV_ANCHOR_ROUTING_PAUSED, &STATUS_PAUSED), 1);
        assert_eq!(f.emissions(&EV_ANCHOR_ROUTING_RESUMED, &STATUS_RESUMED), 0);

        f.deposit(300).unwrap();
        assert_eq!(f.emissions(&EV_ANCHOR_ROUTING_RESUMED, &STATUS_RESUMED), 1);
    }

    #[test]
    fn collateral_and_payout_events_are_emitted() {
        let f = Fixture::new();
        f.deposit(1_500).unwrap();
        f.withdraw(100).unwrap();
        let entry = f.assign(100).unwrap();
        f.settle(entry.id, 100).unwrap();

        for (name, status) in [
            (EV_ANCHOR_COLLATERAL_DEPOSITED, STATUS_DEPOSIT),
            (EV_ANCHOR_COLLATERAL_WITHDRAWN, STATUS_WITHDRAW),
            (EV_ANCHOR_PAYOUT_QUEUED, STATUS_QUEUED),
            (EV_ANCHOR_PAYOUT_SETTLED, STATUS_SETTLED),
        ] {
            let seen = f.emitted(&name, &status);
            assert!(seen, "missing event {name:?}");
        }
    }

    #[test]
    fn the_threshold_change_event_is_emitted() {
        let f = Fixture::new();
        assert!(!f.emitted(&EV_ANCHOR_MIN_RATIO_SET, &STATUS_MINSET));
        f.set_min(&f.admin.clone(), CORRIDOR, 15_000).unwrap();
        assert!(f.emitted(&EV_ANCHOR_MIN_RATIO_SET, &STATUS_MINSET));
    }

    #[test]
    fn a_rejected_operation_emits_no_state_change_event() {
        let f = Fixture::new();
        f.deposit(1_200).unwrap();
        let _ = f.assign(1_000);
        let queued_before = f.emissions(&EV_ANCHOR_PAYOUT_QUEUED, &STATUS_QUEUED);

        let _ = f.assign(1);
        assert_eq!(
            f.emissions(&EV_ANCHOR_PAYOUT_QUEUED, &STATUS_QUEUED),
            queued_before
        );
    }
}
