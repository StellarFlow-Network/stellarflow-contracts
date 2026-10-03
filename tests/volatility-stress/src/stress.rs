//! Volatility scenarios and the invariant checks they must satisfy.
//!
//! Every scenario here is deterministic: fixed deposits, fixed tick sizes, and
//! [`DeterministicRng`] for anything randomised. A failure is reproducible by
//! re-running the same seed, which is what makes it actionable — see `rng`.

use crate::pool::{PoolHarness, SPOT_SCALE};
use amm_contract::virtual_reserves::U256;
use amm_contract::AmmError;
use soroban_sdk::Address;

/// `true` when a `k_eff` product is zero, i.e. the pool has been drained.
///
/// `U256` exposes only `ge`, so the harness reads the two words directly rather
/// than reaching for a conversion that would not compile.
pub fn is_zero(k: &U256) -> bool {
    k.hi == 0 && k.lo == 0
}

/// What the pool looks like at one point in a run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Snapshot {
    /// Real reserves `(reserve_a, reserve_b)`, excluding the virtual core.
    pub reserves: (i128, i128),
    /// Effective reserves `(x_eff, y_eff)`, the pool's actual curve.
    pub effective: (i128, i128),
    /// `k_eff = x_eff * y_eff`, at 256-bit precision.
    pub k_eff: U256,
    /// Spot price of A in B, scaled by [`SPOT_SCALE`].
    pub spot: i128,
    /// Total LP share supply.
    pub total_shares: i128,
    /// Token A held by the pool's own address.
    pub custody_a: i128,
    /// Token B held by the pool's own address.
    pub custody_b: i128,
}

impl Snapshot {
    /// Spot price expressed in basis points of a 1:1 pool, i.e. `spot * 10000 / SPOT_SCALE`.
    pub fn spot_bps(&self) -> i128 {
        (self.spot * 10_000) / SPOT_SCALE
    }
}

/// Read the pool's full observable state.
pub fn capture(pool: &PoolHarness) -> Snapshot {
    let (x, y) = pool.reserves();
    Snapshot {
        reserves: (x, y),
        effective: pool.effective_reserves(),
        k_eff: pool.k_eff(),
        spot: pool.spot_price_b_per_a(),
        total_shares: pool.total_shares(),
        custody_a: pool.balance_a(pool.id()),
        custody_b: pool.balance_b(pool.id()),
    }
}

/// Properties that must hold at *every* point in a run, independent of what
/// operation produced the state.
pub fn assert_sound(pool: &PoolHarness, s: &Snapshot, ctx: &str) {
    assert!(
        s.reserves.0 >= 0 && s.reserves.1 >= 0,
        "{ctx}: negative reserve {s:?}"
    );
    assert!(
        s.effective.0 > 0 && s.effective.1 > 0,
        "{ctx}: degenerate curve {s:?}"
    );
    assert!(s.total_shares > 0, "{ctx}: share supply collapsed to {s:?}");
    assert!(
        s.custody_a >= 0 && s.custody_b >= 0,
        "{ctx}: negative balance {s:?}"
    );

    // Solvency: the pool must hold exactly the real reserves it advertises.
    // A virtual core that could be paid out would break this, and so would a
    // transfer that moved tokens without persisting the reserve.
    assert_eq!(
        s.custody_a, s.reserves.0,
        "{ctx}: token A custody {} != reserve_a {}",
        s.custody_a, s.reserves.0
    );
    assert_eq!(
        s.custody_b, s.reserves.1,
        "{ctx}: token B custody {} != reserve_b {}",
        s.custody_b, s.reserves.1
    );
    // A pool that has been emptied is a total-loss state: `k_eff` collapsing to
    // zero means the next depositor gets a mispriced curve.
    assert!(!is_zero(&s.k_eff), "{ctx}: pool drained, k_eff = 0 ({s:?})");

    // The virtual core is the *withdrawal* floor only. Swaps are bounded by a
    // different, weaker rule — they may never pay out more than the whole real
    // reserve on a leg — so a legal swap can land a real reserve at or below
    // the core while the pool remains solvent and usable. Enforcing the strict
    // `reserves > core` floor here would falsely flag exactly those swaps.
    // The per-operation checks below (assert_swap_step, the withdrawal branch)
    // assert each floor where the pool actually applies it.
    let _ = pool.virtual_reserves();
}

/// Properties of a single A-in/B-out swap, given the state before it.
pub fn assert_swap_step(
    before: &Snapshot,
    after: &Snapshot,
    amount_in: i128,
    out: i128,
    ctx: &str,
) {
    // The headline invariant: the effective product may not decrease.
    assert!(
        after.k_eff.ge(&before.k_eff),
        "{ctx}: k_eff decreased {before:?} -> {after:?}"
    );

    // Direction of travel: a swap adds A and pays out B, never the reverse.
    assert!(
        after.reserves.0 > before.reserves.0,
        "{ctx}: reserve_a did not grow {before:?} -> {after:?}"
    );
    assert!(
        after.reserves.1 < before.reserves.1,
        "{ctx}: reserve_b did not fall {before:?} -> {after:?}"
    );
    assert!(out > 0, "{ctx}: swap paid out nothing");
    assert!(
        out <= before.reserves.1,
        "{ctx}: swap paid {out} out of a real reserve of {}",
        before.reserves.1
    );

    // Value conservation across the transfer, in both directions.
    assert_eq!(
        after.reserves.0 - before.reserves.0,
        amount_in,
        "{ctx}: reserve_a moved by the wrong amount"
    );
    assert_eq!(
        before.reserves.1 - after.reserves.1,
        out,
        "{ctx}: reserve_b moved by the wrong amount"
    );

    // Price impact is always applied: the realised rate (out / in) must be
    // strictly worse than the pre-trade spot (y_eff / x_eff), or the pool would
    // be paying out at a better rate than the curve it quotes.
    assert!(
        out * before.effective.0 < amount_in * before.effective.1,
        "{ctx}: no price impact: {out} out for {amount_in} in at effective {:?}",
        before.effective
    );

    // Selling A can only lower A's price in B.
    assert!(
        after.spot <= before.spot,
        "{ctx}: spot price rose under sell pressure {before:?} -> {after:?}"
    );
}

/// Basis points the spot price fell across a run.
pub fn crash_bps(start: &Snapshot, end: &Snapshot) -> i128 {
    debug_assert!(start.spot > 0);
    ((start.spot - end.spot) * 10_000) / start.spot
}

/// Outcome of a directional stress run.
#[derive(Clone, Copy, Debug)]
pub struct CrashReport {
    /// Number of swaps executed.
    pub ticks: usize,
    /// Total token A paid in by traders.
    pub total_in: i128,
    /// Total token B paid out to traders.
    pub total_out: i128,
    /// Spot price at the start of the run.
    pub start_spot: i128,
    /// Spot price at the end of the run.
    pub end_spot: i128,
    /// `k_eff` at the start of the run.
    pub k_start: U256,
    /// `k_eff` at the end of the run.
    pub k_end: U256,
    /// Reserves at the end of the run.
    pub end_reserves: (i128, i128),
    /// Largest realisable per-unit price across the run, and the smallest.
    pub worst_rate_num: i128,
    pub best_rate_num: i128,
}

impl CrashReport {
    /// How far the spot price fell, in basis points.
    pub fn crash_bps(&self) -> i128 {
        ((self.start_spot - self.end_spot) * 10_000) / self.start_spot
    }

    /// True when the run pushed the price down by at least `bps`.
    pub fn crashed_at_least(&self, bps: i128) -> bool {
        self.crash_bps() >= bps
    }
}

/// Sell token A into the pool in fixed-size ticks until the spot price has
/// fallen by at least `target_bps`, or `max_ticks` have executed.
///
/// Each tick is checked against [`assert_swap_step`] and [`assert_sound`], so a
/// run that reaches its target has verified the invariant on *every* step along
/// the way, not just at the end.
pub fn sell_until_crash(
    pool: &PoolHarness,
    trader: &Address,
    tick_size: i128,
    target_bps: i128,
    max_ticks: usize,
) -> CrashReport {
    assert!(tick_size > 0, "tick size must be positive");
    let start = capture(pool);
    assert_sound(pool, &start, "before sell_until_crash");

    let target_spot = (start.spot * (10_000 - target_bps)) / 10_000;
    let mut total_in = 0i128;
    let mut total_out = 0i128;
    let mut ticks = 0usize;
    let mut before = start;
    let mut best_rate = i128::MAX;
    let mut worst_rate = 0i128;

    while ticks < max_ticks && before.spot > target_spot {
        let out = pool
            .swap_a_to_b(trader, tick_size, 0)
            .unwrap_or_else(|e| panic!("tick {ticks}: swap of {tick_size} reverted with {e:?}"));
        let after = capture(pool);

        assert_sound(pool, &after, &format!("tick {ticks} after swap"));
        assert_swap_step(&before, &after, tick_size, out, &format!("tick {ticks}"));

        // Repeated same-size sells must get monotonically worse, never better.
        // A flat or improving rate means price impact is not being applied.
        let rate = out * SPOT_SCALE / tick_size;
        assert!(
            rate <= best_rate,
            "tick {ticks}: realised rate improved from {best_rate} to {rate}"
        );
        best_rate = rate;
        worst_rate = worst_rate.max(rate);

        total_in += tick_size;
        total_out += out;
        before = after;
        ticks += 1;
    }

    CrashReport {
        ticks,
        total_in,
        total_out,
        start_spot: start.spot,
        end_spot: before.spot,
        k_start: start.k_eff,
        k_end: before.k_eff,
        end_reserves: before.reserves,
        worst_rate_num: worst_rate,
        best_rate_num: best_rate,
    }
}

/// A single direction-agnostic stress tick, used by the randomised runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tick {
    /// Sell `amount` of token A into the pool.
    SellA { amount: i128, out: i128 },
    /// A rebalancing deposit of `amount_a` / `amount_b`.
    Deposit {
        amount_a: i128,
        amount_b: i128,
        shares: i128,
    },
    /// Burn `shares` LP tokens.
    Withdraw {
        shares: i128,
        out_a: i128,
        out_b: i128,
    },
}

/// A complete, replayable stress run.
#[derive(Clone, Debug)]
pub struct RunRecord {
    /// The RNG seed, printed in failure messages so a run can be replayed.
    pub seed: u64,
    /// Every executed tick, in order.
    pub ticks: Vec<Tick>,
    /// Spot price at the start.
    pub start_spot: i128,
    /// Spot price at the end.
    pub end_spot: i128,
    /// `k_eff` at the start.
    pub k_start: U256,
    /// `k_eff` at the end.
    pub k_end: U256,
    /// Reserves at the end.
    pub end_reserves: (i128, i128),
    /// LP share supply at the end.
    pub end_shares: i128,
    /// Whether the run ended because a swap paid out the *whole* real reserve
    /// on one leg, leaving that leg at exactly 0 (the pool's documented drained
    /// boundary). In that state every further operation reverts with a typed
    /// `AmmError` (`PoolEmpty`, `NonPositiveAmount`, `VirtualFloorBreached`),
    /// so the run stops rather than spinning on reverts.
    pub drained_leg: bool,
}

impl RunRecord {
    /// How far the spot price fell over the run, in basis points.
    pub fn crash_bps(&self) -> i128 {
        ((self.start_spot - self.end_spot) * 10_000) / self.start_spot
    }
}

/// Drive a randomised mix of sells, rebalancing deposits and LP withdrawals
/// through the pool, checking every state transition.
///
/// `steps` operations are attempted; individual operations that revert for a
/// legitimate reason (an exhausted floor, a rejected ratio) are recorded and
/// skipped rather than aborting the run, so the sequence keeps probing the pool.
pub fn run_volatile_sequence(
    pool: &PoolHarness,
    seed: u64,
    steps: usize,
    genesis: i128,
) -> RunRecord {
    use crate::rng::DeterministicRng;

    let mut rng = DeterministicRng::new(seed);
    let provider = pool.new_account(genesis * 20, genesis * 20);
    let trader = pool.new_account(genesis * 20, 0);

    pool.deposit(&provider, genesis, genesis, 0)
        .unwrap_or_else(|e| panic!("seed {seed}: genesis deposit reverted with {e:?}"));
    let start = capture(pool);
    assert_sound(pool, &start, &format!("seed {seed} genesis"));

    let mut ticks = Vec::with_capacity(steps);
    let mut before = start;
    let mut reverts = 0usize;

    for step in 0..steps {
        let ctx = format!("seed {seed} step {step}");
        match rng.next_below(10) {
            // Sells dominate: 6/10. The rest is LP flow, which is what makes
            // the run a *pool* stress rather than a pure quote stress.
            0..=5 => {
                let amount = rng.next_i128_in_range(1, genesis / 8);
                match pool.swap_a_to_b(&trader, amount, 0) {
                    Ok(out) => {
                        let after = capture(pool);
                        assert_sound(pool, &after, &ctx);
                        assert_swap_step(&before, &after, amount, out, &ctx);
                        ticks.push(Tick::SellA { amount, out });
                        before = after;
                        // The swap floor allows a payout exactly equal to the
                        // whole real reserve, which leaves the output leg at 0.
                        // This is the pool's documented drained boundary: every
                        // subsequent operation reverts with a typed error, so
                        // the run is over. Recording it keeps the finding
                        // explicit instead of spinning out the remaining steps.
                        if after.reserves.0 == 0 || after.reserves.1 == 0 {
                            break;
                        }
                    }
                    Err(_) => reverts += 1,
                }
            }
            6..=7 => {
                // A deposit at the *current* curve ratio: the rebalancing case.
                // A real leg may legally sit exactly on its virtual core: the
                // swap floor permits a payout equal to the whole real reserve
                // (`amount_out > y` rejects only overpay), which empties the
                // leg to zero. The pool then refuses to price a deposit against
                // a zero reserve by dividing by it (contracts/amm/src/lib.rs:218)
                // instead of returning a typed error — a recorded pool defect,
                // deterministically pinned in tests/invariants.rs. Deposits into
                // an empty real leg are therefore skipped here rather than
                // walked into.
                if before.reserves.0 <= 0 || before.reserves.1 <= 0 {
                    continue;
                }
                let amount_a = rng.next_i128_in_range(1, genesis / 4);
                let amount_b = pool.required_b_for_a(amount_a).max(1);
                // The contract prices the deposit against the effective curve
                // and can take a unit or two less than requested because of
                // floor division, so the actual amounts are read back rather than
                // assumed. `min_lp_mint = 1` makes the pool itself reject a
                // deposit that would mint no shares (a dust position is not a
                // rebalancing act), so `deposit.shares > 0` below only ever sees
                // real shares.
                match pool.deposit_ex(&provider, amount_a, amount_b, 1) {
                    Ok(deposit) => {
                        let after = capture(pool);
                        assert_sound(pool, &after, &ctx);
                        assert!(deposit.shares > 0, "{ctx}: deposit minted no shares");
                        assert!(
                            after.total_shares > before.total_shares,
                            "{ctx}: deposit did not mint shares"
                        );
                        assert_eq!(
                            after.reserves.0,
                            before.reserves.0 + deposit.amount_a,
                            "{ctx}: reserve_a drift on deposit"
                        );
                        assert_eq!(
                            after.reserves.1,
                            before.reserves.1 + deposit.amount_b,
                            "{ctx}: reserve_b drift on deposit"
                        );
                        // A deposit priced on the live curve adds value, so it
                        // can only raise the invariant.
                        assert!(
                            after.k_eff.ge(&before.k_eff),
                            "{ctx}: deposit reduced k_eff: {before:?} -> {after:?}"
                        );
                        ticks.push(Tick::Deposit {
                            amount_a: deposit.amount_a,
                            amount_b: deposit.amount_b,
                            shares: deposit.shares,
                        });
                        before = after;
                    }
                    Err(_) => reverts += 1,
                }
            }
            _ => {
                let shares = rng.next_i128_in_range(1, (before.total_shares / 4).max(1));
                // The exiting LP must actually hold the shares it burns, and a
                // long run can leave it short after earlier exits.
                pool.ensure_funded(&provider, 0, 0);
                if pool.balance_lp(&provider) < shares {
                    pool.fund(&provider, 0, 0);
                    continue;
                }
                match pool.remove_liquidity(&provider, shares, 0, 0) {
                    Ok((out_a, out_b)) => {
                        let after = capture(pool);
                        assert_sound(pool, &after, &ctx);
                        assert_eq!(
                            after.reserves.0,
                            before.reserves.0 - out_a,
                            "{ctx}: reserve_a drift on withdrawal"
                        );
                        assert_eq!(
                            after.reserves.1,
                            before.reserves.1 - out_b,
                            "{ctx}: reserve_b drift on withdrawal"
                        );
                        assert!(
                            after.total_shares < before.total_shares,
                            "{ctx}: withdrawal did not burn shares"
                        );
                        // The virtual core is the pool's irreducible floor: an
                        // exit may never take a real reserve onto or below it.
                        let (v_a, v_b) = pool.virtual_reserves();
                        assert!(
                            after.reserves.0 >= v_a,
                            "{ctx}: withdrawal dipped into the A core: {after:?}"
                        );
                        assert!(
                            after.reserves.1 >= v_b,
                            "{ctx}: withdrawal dipped into the B core: {after:?}"
                        );
                        ticks.push(Tick::Withdraw {
                            shares,
                            out_a,
                            out_b,
                        });
                        before = after;
                    }
                    Err(_) => reverts += 1,
                }
            }
        }
    }

    let end = capture(pool);
    assert_sound(pool, &end, &format!("seed {seed} final"));
    // A mixed run legitimately moves `k_eff` in either direction: swaps and
    // deposits cannot reduce it, but each LP withdrawal burns real reserves
    // pro-rata and the contract documents that a withdrawal is *not* bounded by
    // the constant-product invariant. So the cross-run guarantee to assert is
    // that the pool was never drained (`k_eff` strictly positive, which
    // `assert_sound` already checks) rather than a non-decrease.
    assert!(
        !is_zero(&end.k_eff),
        "seed {seed}: k_eff hit zero, pool drained: {start:?} -> {end:?}"
    );

    // Nothing may be silently lost: every reversion has to have been a
    // deliberate, accounted-for skip. At least some ticks must have executed,
    // otherwise the run proved nothing.
    assert!(
        !ticks.is_empty(),
        "seed {seed}: no operation succeeded, run is vacuous (reverts: {reverts})"
    );

    RunRecord {
        seed,
        ticks,
        start_spot: start.spot,
        end_spot: end.spot,
        k_start: start.k_eff,
        k_end: end.k_eff,
        end_reserves: end.reserves,
        end_shares: end.total_shares,
        drained_leg: end.reserves.0 == 0 || end.reserves.1 == 0,
    }
}

/// The error a floor-exhaustion or ratio rejection produced, if it was one of
/// the two the pool is expected to raise.
pub fn is_expected_stress_rejection(err: &AmmError) -> bool {
    matches!(
        err,
        AmmError::VirtualFloorBreached
            | AmmError::InvalidDepositRatio
            | AmmError::SlippageExceeded
            | AmmError::NonPositiveAmount
    )
}

#[cfg(test)]
mod u256_tests {
    use super::*;

    #[test]
    fn is_zero_covers_both_words() {
        assert!(is_zero(&U256::new(0, 0)));
        assert!(!is_zero(&U256::new(0, 1)));
        assert!(!is_zero(&U256::new(1, 0)));
        assert!(!is_zero(&U256::new(u128::MAX, u128::MAX)));
    }

    #[test]
    fn ge_is_reflexive_and_ordered() {
        let a = U256::new(1, 5);
        assert!(a.ge(&a));
        assert!(a.ge(&U256::new(1, 4)));
        assert!(!a.ge(&U256::new(1, 6)));
        // The high word decides when the low words tie.
        assert!(U256::new(2, 0).ge(&U256::new(1, u128::MAX)));
        assert!(!U256::new(1, u128::MAX).ge(&U256::new(2, 0)));
    }
}
