//! Market-path scenarios that combine stress with recovery.
//!
//! Each scenario is a full story: build a pool, push it hard, then act as an LP
//! arriving afterwards. That is the case the pool's invariants actually have to
//! survive, because a crash is only interesting if the pool is still usable
//! afterwards.

use crate::pool::PoolHarness;
use crate::stress::{
    assert_sound, assert_swap_step, capture, is_zero, sell_until_crash, CrashReport, Snapshot, Tick,
};
use soroban_sdk::Address;

/// Genesis deposit for every scenario: large enough that the 1,000-unit virtual
/// core is a rounding error, small enough that all arithmetic stays far from
/// `i128` limits.
pub const GENESIS: i128 = 1_000_000;

/// Every step of one scenario, with the state at the extremes.
#[derive(Clone, Debug)]
pub struct Scenario {
    /// Deterministic seed the scenario ran under.
    pub seed: u64,
    /// The sell-off that drove the price down.
    pub crash: CrashReport,
    /// Operations executed after the crash: LP flow and any further trades.
    pub recovery: Vec<Tick>,
    /// State after the crash, before recovery.
    pub post_crash: Snapshot,
    /// State at the end of the scenario.
    pub end: Snapshot,
    /// State at genesis.
    pub genesis: Snapshot,
    /// State after the rebalancing deposit, before the LP exit.
    pub post_deposit: Snapshot,
    /// State after the LP exit, before the final trade. Equal to
    /// `post_deposit` when the scenario's LP had nothing to exit.
    pub post_exit: Snapshot,
    /// The LP that funded the pool and stayed through the crash.
    pub lp: Address,
}

impl Scenario {
    /// Price movement across the whole scenario, in basis points.
    pub fn crash_bps(&self) -> i128 {
        self.crash.crash_bps()
    }

    /// The invariant held across the trade sequences: genesis → crash, and the
    /// final trade after recovery.
    ///
    /// It is deliberately *not* a genesis-to-end comparison. The scenario also
    /// contains a deposit and a withdrawal, and a withdrawal legitimately removes
    /// capital and therefore shrinks `k_eff`; the contract's non-decreasing
    /// invariant is a swap invariant. The deposit direction is checked where it
    /// happens, and the withdrawal is checked for pro-rata fairness and floor
    /// compliance instead.
    pub fn k_held_across_trades(&self) -> bool {
        self.post_crash.k_eff.ge(&self.genesis.k_eff) && self.end.k_eff.ge(&self.post_exit.k_eff)
    }

    /// The realised worst price across the sell-off, in scaled units per unit in.
    pub fn worst_rate(&self) -> i128 {
        self.crash.worst_rate_num
    }
}

/// Sell the pool down by `target_bps`, then rebalance and let an LP exit.
///
/// Steps, in order:
/// 1. Genesis deposit of `GENESIS` on both legs, creating the first LP position.
/// 2. A trader sells token A in `tick_size` increments until the spot price has
///    fallen by `target_bps` or `max_ticks` trades have executed. Every tick is
///    checked for `k_after >= k_before`, applied price impact, and solvency.
/// 3. A second LP arrives and deposits at the *new* curve ratio, which is the
///    rebalancing case: the deposit must be priced against the post-crash curve,
///    not the stale pre-crash one.
/// 4. The first LP burns a slice of its position, which must respect the virtual
///    core floor.
///
/// `seed` is recorded for reproducibility; the path itself is deterministic, so
/// the seed only appears in failure output.
pub fn scenario_selloff_then_rebalance(
    tick_size: i128,
    target_bps: i128,
    max_ticks: usize,
) -> Scenario {
    scenario_selloff_then_rebalance_seeded(0x5EED, tick_size, target_bps, max_ticks)
}

/// [`scenario_selloff_then_rebalance`] with an explicit seed, for callers that
/// run the same path under several seeds.
pub fn scenario_selloff_then_rebalance_seeded(
    seed: u64,
    tick_size: i128,
    target_bps: i128,
    max_ticks: usize,
) -> Scenario {
    let ctx = format!("seed {seed} tick {tick_size} target {target_bps}bps");
    let pool = PoolHarness::new();

    // 1. Genesis. The LP is funded well beyond what it deposits so it can also
    //    exit later without a shortfall.
    let lp = pool.new_account(GENESIS * 8, GENESIS * 8);
    pool.deposit(&lp, GENESIS, GENESIS, 0)
        .unwrap_or_else(|e| panic!("{ctx}: genesis deposit reverted with {e:?}"));
    let genesis = capture(&pool);
    assert_sound(&pool, &genesis, &format!("{ctx} genesis"));

    // 2. Crash.
    let trader = pool.new_account(GENESIS * 40, 0);
    let crash = sell_until_crash(&pool, &trader, tick_size, target_bps, max_ticks);
    let post_crash = capture(&pool);
    assert_sound(&pool, &post_crash, &format!("{ctx} post-crash"));

    let mut recovery = Vec::new();

    // 3. Rebalance: a new LP deposits into the *post-crash* curve.
    //
    // This is the assertion that matters. A pool that priced deposits against a
    // stale ratio would let this depositor buy shares at the pre-crash price
    // and immediately drain value from the incumbent LP, so the post-deposit
    // spot price must barely move and `k_eff` must not fall.
    let rebalancer = pool.new_account(GENESIS * 8, GENESIS * 8);
    let rebalancer_start_a = pool.balance_a(&rebalancer);
    let rebalancer_start_b = pool.balance_b(&rebalancer);
    let deposit_a = GENESIS / 10;
    let deposit_b = pool.required_b_for_a(deposit_a).max(1);
    let deposit = pool
        .deposit_ex(&rebalancer, deposit_a, deposit_b, 0)
        .unwrap_or_else(|e| panic!("{ctx}: rebalancing deposit reverted with {e:?}"));

    let after_deposit = capture(&pool);
    assert_sound(
        &pool,
        &after_deposit,
        &format!("{ctx} after rebalancing deposit"),
    );
    assert!(
        after_deposit.k_eff.ge(&post_crash.k_eff),
        "{ctx}: rebalancing deposit reduced k_eff: {post_crash:?} -> {after_deposit:?}"
    );
    // A correctly priced deposit is close to price-neutral. Allow a small drift
    // for integer division of the effective ratio, but nothing like the size of
    // the crash: if this number approaches the crash size, deposits are being
    // priced off a stale curve.
    let drift_bps = (((after_deposit.spot - post_crash.spot).abs()) * 10_000) / post_crash.spot;
    assert!(
        drift_bps < crash.crash_bps().max(1) / 4 + 1,
        "{ctx}: a correctly priced deposit moved the spot price by {drift_bps} bps, \
         which suggests deposits are quoted off a stale curve"
    );
    assert_eq!(
        after_deposit.reserves.0,
        post_crash.reserves.0 + deposit.amount_a,
        "{ctx}: reserve_a drift on rebalance"
    );
    assert_eq!(
        after_deposit.reserves.1,
        post_crash.reserves.1 + deposit.amount_b,
        "{ctx}: reserve_b drift on rebalance"
    );
    // The pool took the amounts it persisted, and the depositor was charged
    // exactly those: no unaccounted tokens moved in either direction.
    assert_eq!(
        pool.balance_a(&rebalancer),
        rebalancer_start_a - deposit.amount_a,
        "{ctx}: rebalancer's token A balance does not match what it paid in"
    );
    assert_eq!(
        pool.balance_b(&rebalancer),
        rebalancer_start_b - deposit.amount_b,
        "{ctx}: rebalancer's token B balance does not match what it paid in"
    );
    // Floor division may shave a unit off the requested deposit, but only a
    // negligible amount: a materially smaller take would mean the pool priced
    // the deposit off a different curve than the one it quotes.
    assert!(
        deposit.amount_a > deposit_a - 10 && deposit.amount_b + 1 >= deposit_b,
        "{ctx}: deposit took {} / {}, far from the requested {deposit_a} / {deposit_b}",
        deposit.amount_a,
        deposit.amount_b
    );
    recovery.push(Tick::Deposit {
        amount_a: deposit.amount_a,
        amount_b: deposit.amount_b,
        shares: deposit.shares,
    });

    // 4. Partial exit by the incumbent LP, respecting the virtual core.
    let mut post_exit = after_deposit;
    let lp_shares = pool.balance_lp(&lp);
    let exit_shares = lp_shares / 4;
    if exit_shares > 0 {
        let before = capture(&pool);
        let (out_a, out_b) = pool
            .remove_liquidity(&lp, exit_shares, 0, 0)
            .unwrap_or_else(|e| panic!("{ctx}: LP exit reverted with {e:?}"));
        let after = capture(&pool);
        assert_sound(&pool, &after, &format!("{ctx} after LP exit"));

        assert_eq!(
            after.reserves.0,
            before.reserves.0 - out_a,
            "{ctx}: reserve_a drift on exit"
        );
        assert_eq!(
            after.reserves.1,
            before.reserves.1 - out_b,
            "{ctx}: reserve_b drift on exit"
        );
        assert_eq!(
            after.total_shares,
            before.total_shares - exit_shares,
            "{ctx}: share supply drift on exit"
        );

        // A withdrawal removes capital, so `k_eff` is *expected* to fall: the
        // contract's non-decreasing invariant is a swap invariant, not a
        // withdrawal invariant. What must hold is that the exit was pro-rata and
        // that the remaining holders were not diluted.
        assert_eq!(
            out_a,
            (exit_shares * before.reserves.0) / before.total_shares,
            "{ctx}: exit was not pro-rata on token A"
        );
        assert_eq!(
            out_b,
            (exit_shares * before.reserves.1) / before.total_shares,
            "{ctx}: exit was not pro-rata on token B"
        );

        // Pro-rata removal leaves the curve essentially where it was, so the
        // remaining LPs keep their position. A large price move here would mean
        // the exit was priced off a stale curve. The tolerance covers the
        // virtual core becoming a larger share of a smaller pool.
        let drift_bps = (after.spot - before.spot).abs() * 10_000 / before.spot;
        assert!(
            drift_bps < 100,
            "{ctx}: a pro-rata exit moved the spot price by {drift_bps} bps, \
             which suggests remaining LPs were diluted"
        );

        // The core is the pool's irreducible floor: an exit may never take a real
        // reserve onto or below it, which is what prevents a total-loss drain.
        let (v_a, v_b) = pool.virtual_reserves();
        assert!(
            after.reserves.0 > v_a,
            "{ctx}: exit took reserve_a into the core: {after:?}"
        );
        assert!(
            after.reserves.1 > v_b,
            "{ctx}: exit took reserve_b into the core: {after:?}"
        );

        // The exiting LP got a strictly positive payout on both legs.
        assert!(out_a > 0 && out_b > 0, "{ctx}: exit paid out nothing");
        recovery.push(Tick::Withdraw {
            shares: exit_shares,
            out_a,
            out_b,
        });
        post_exit = after;
    }

    // 5. The post-crash curve is still live: one more trade succeeds and is
    //    still governed by the invariant.
    let final_trader = pool.new_account(GENESIS * 4, 0);
    let before_final = capture(&pool);
    let final_out = pool
        .swap_a_to_b(&final_trader, tick_size, 0)
        .unwrap_or_else(|e| panic!("{ctx}: post-recovery trade reverted with {e:?}"));
    let end = capture(&pool);
    assert_sound(&pool, &end, &format!("{ctx} final"));
    assert_swap_step(
        &before_final,
        &end,
        tick_size,
        final_out,
        &format!("{ctx} final trade"),
    );
    recovery.push(Tick::SellA {
        amount: tick_size,
        out: final_out,
    });

    Scenario {
        seed,
        crash,
        recovery,
        post_crash,
        post_deposit: after_deposit,
        post_exit,
        end,
        genesis,
        lp,
    }
}

/// Assertions every scenario must satisfy, given the crash depth it targeted.
///
/// Kept in one place so each scenario test states only its own parameters and
/// the depth it demands, rather than restating the property set.
pub fn assert_record_holds(scenario: &Scenario, min_crash_bps: i128) {
    assert!(
        scenario.crash_bps() >= min_crash_bps,
        "scenario (seed {}) only reached {} bps, wanted at least {min_crash_bps}",
        scenario.seed,
        scenario.crash_bps()
    );
    // The headline invariant, checked step-by-step inside the scenario and
    // again across the two trade sequences here.
    assert!(
        scenario.k_held_across_trades(),
        "k_eff fell across a trade sequence in scenario (seed {}): genesis {:?}, post-crash {:?}, post-exit {:?}, end {:?}",
        scenario.seed,
        scenario.genesis,
        scenario.post_crash,
        scenario.post_exit,
        scenario.end
    );
    // The deposit leg adds capital, so it can only raise the invariant.
    assert!(
        scenario.post_deposit.k_eff.ge(&scenario.post_crash.k_eff),
        "the rebalancing deposit reduced k_eff in scenario (seed {})",
        scenario.seed
    );
    // A withdrawal removes capital, so it is expected to lower k_eff; what must
    // not happen is it lowering the *spot price*, which would mean the remaining
    // LPs were diluted by the exit.
    let withdrawal = scenario.recovery.iter().find_map(|t| match t {
        Tick::Withdraw { out_a, out_b, .. } => Some((*out_a, *out_b)),
        _ => None,
    });
    if let Some((out_a, out_b)) = withdrawal {
        assert!(
            out_a > 0 && out_b > 0,
            "scenario (seed {}) withdrew a zero amount",
            scenario.seed
        );
    }
    // Never a total-loss state.
    assert!(
        !is_zero(&scenario.end.k_eff),
        "scenario (seed {}) drained the pool",
        scenario.seed
    );
    assert!(
        scenario.end.reserves.0 > 0 && scenario.end.reserves.1 > 0,
        "scenario (seed {}) left a dead reserve",
        scenario.seed
    );
    // And the recovery leg actually ran, otherwise the scenario proves nothing
    // about post-crash usability.
    assert!(
        !scenario.recovery.is_empty(),
        "scenario (seed {}) executed no recovery operations",
        scenario.seed
    );
}
