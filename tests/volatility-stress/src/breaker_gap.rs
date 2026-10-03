//! Records why the circuit-breaker half of issue #1025 is not covered here.
//!
//! # The gap
//!
//! Issue #1025 asks for two things: a pool-only volatility stress suite, and an
//! on-chain spot-price circuit breaker exercised under stress. This crate
//! delivers the first. The second is not implemented here, and adding it would
//! require work outside this crate's scope.
//!
//! The breaker lives in the root crate, at `src/amm/circuit_breaker.rs`, and is
//! reachable only through that crate's contract surface. The root crate does not
//! currently compile: its package carries unresolved breakage in
//! `src/config.rs`, `src/auth/mod.rs`, `src/bridge/timelock.rs`,
//! `src/events/mod.rs` and `src/errors/mod.rs`, and `src/storage.rs` is missing
//! the `PERSISTENT_TTL_THRESHOLD` constant the breaker imports. Until the root
//! crate builds, the breaker cannot be compiled at all, let alone asserted
//! against.
//!
//! Including the file by path and feeding it a hand-written stub surface — the
//! only way to reach it without the root crate — would mean asserting against a
//! re-creation of the risk control rather than the real thing. That is exactly
//! what a suite like this must not do, so no such shim is present in this crate.
//!
//! # What is verified instead
//!
//! The pool-side mechanism that a breaker is meant to protect is fully covered
//! and the assertions below hold under the real pool:
//!
//! * the effective constant-product invariant `k_eff` never decreases,
//! * the pool stays solvent — its token balances always equal its real reserves,
//! * the virtual core floor always holds, so the curve cannot be drained and a
//!   later depositor cannot be handed a mispriced pool,
//! * price impact is always applied and gets monotonically worse as sell
//!   pressure builds.
//!
//! Those are the invariants a breaker *triggers on top of*. The missing piece is
//! the trigger itself: the deviation window, the freeze, and the
//! `record_observation` / `assert_not_frozen` calls that would gate a swap.
//!
//! # Follow-up
//!
//! Unblocking this needs one change, in order: repair the root crate's
//! compilation. Once the root crate builds, adding `amm-contract` plus that
//! root contract to this harness's dependencies is a small change.

use crate::pool::PoolHarness;
#[cfg(test)]
use crate::stress::{assert_sound, assert_swap_step, capture, sell_until_crash};

/// Genesis deposit used by the gap-demonstration scenarios.
const GENESIS: i128 = 1_000_000;

/// Builds a pool funded and in steady state, ready to be sold into.
fn bootstrapped() -> (PoolHarness, soroban_sdk::Address) {
    let pool = PoolHarness::new();
    let provider = pool.new_account(GENESIS * 20, GENESIS * 20);
    pool.deposit(&provider, GENESIS, GENESIS, 0)
        .expect("genesis deposit");
    let trader = pool.new_account(GENESIS * 20, 0);
    (pool, trader)
}

#[test]
fn the_real_breaker_is_not_stubbed_or_reached_for() {
    // A guard, not a behaviour test. It exists so the shortcut this crate
    // deliberately refuses cannot creep back in unnoticed: a `#[path]` import of
    // the root breaker's source, or a hand-written stand-in module standing in
    // for it, would turn these assertions into claims about a re-creation of the
    // risk control rather than the real thing.
    let lib = include_str!("lib.rs");
    // Only the attribute form counts: the crate docs legitimately name `#[path]`
    // while explaining why it is not used.
    assert!(
        !lib.contains("#[path ="),
        "the root breaker source must not be imported into this crate"
    );
    assert!(
        !lib.contains("mod host_stubs"),
        "a host-stub surface must not stand in for the real breaker"
    );

    // The gap must stay documented, not quietly forgotten: the root breakage is
    // the reason none of the above is possible today.
    let doc = include_str!("breaker_gap.rs");
    assert!(
        doc.contains("PERSISTENT_TTL_THRESHOLD"),
        "the recorded root-crate breakage must stay cited here"
    );
}

#[test]
fn deep_crash_leaves_the_curve_intact() {
    // The closest this crate gets to "the breaker fired": a 60% sell-off, with
    // every step checked. Without a breaker the pool must still refuse to lose
    // value or breach its floor.
    let (pool, trader) = bootstrapped();
    let report = sell_until_crash(&pool, &trader, 5_000, 6_000, 2_000);

    assert!(
        report.crashed_at_least(6_000),
        "expected a >=60% crash, got {} bps",
        report.crash_bps()
    );
    assert!(
        report.ticks > 1,
        "crash was not actually traversed in steps"
    );

    // The invariant held on every tick (asserted inside sell_until_crash), and
    // it still holds at the end.
    let end = capture(&pool);
    assert_sound(&pool, &end, "after a 60% crash");
    assert!(
        end.k_eff.ge(&report.k_start),
        "k_eff fell across the crash: {end:?}"
    );

    // The virtual core is untouched: real reserves are still above it, so the
    // pool has not been drained and a subsequent depositor gets a real curve.
    let (v_a, v_b) = pool.virtual_reserves();
    assert!(
        end.reserves.0 > v_a && end.reserves.1 > v_b,
        "crash reached the virtual core: {end:?}"
    );
}

#[test]
fn oversized_trade_is_rejected_rather_than_draining_the_pool() {
    let (pool, trader) = bootstrapped();
    let before = capture(&pool);

    // Far more token A than the whole B side is worth. The quote is still
    // computable, so the trade must fail on impact or slippage rather than
    // emptying the pool.
    let absurd = GENESIS * 100;
    let result = pool.swap_a_to_b(&trader, absurd, absurd);
    assert!(result.is_err(), "an absurd trade was accepted: {result:?}");

    // Whatever the reason, the pool is unchanged and still solvent.
    let after = capture(&pool);
    assert_eq!(
        before.reserves, after.reserves,
        "a rejected trade moved reserves"
    );
    assert_sound(&pool, &after, "after a rejected oversized trade");
}

#[test]
fn swap_never_breaches_the_virtual_floor() {
    let (pool, trader) = bootstrapped();
    let before = capture(&pool);

    // Repeatedly quote the maximum the curve would pay and confirm the realised
    // reserve never lands on the core.
    let tick = GENESIS / 2;
    let mut ticks = 0;
    while ticks < 64 {
        let after = capture(&pool);
        if after.reserves.1 <= pool.virtual_reserves().1 {
            break;
        }
        let Some(probe) = pool.quote_a_to_b(tick).ok() else {
            break;
        };
        // A long run spends more than any fixed up-front grant covers; topping
        // the trader up keeps the sequence aborting on pool properties rather
        // than on an out-of-balance transfer.
        pool.ensure_funded(&trader, tick + 1, 0);
        let out = pool
            .swap_a_to_b(&trader, tick, 0)
            .expect("swap within the curve");
        assert_eq!(
            out, probe,
            "realised output diverged from the production quote"
        );
        let now = capture(&pool);
        assert_sound(&pool, &now, &format!("floor probe {ticks}"));
        assert_swap_step(&after, &now, tick, out, &format!("floor probe {ticks}"));
        ticks += 1;
    }
    assert!(ticks > 0, "no floor probe executed");

    // Whatever the loop stopped on, the pool ends solvent with a live curve.
    let end = capture(&pool);
    assert_sound(&pool, &end, "after repeated floor probes");
    assert!(
        end.k_eff.ge(&before.k_eff),
        "k_eff fell across the floor probes: {before:?} -> {end:?}"
    );
}
