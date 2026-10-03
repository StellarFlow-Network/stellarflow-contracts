//! End-to-end volatility scenarios against the real `amm-contract` pool.
//!
//! Each test states a market path and the crash depth it demands; the invariants
//! themselves are asserted once, in `volatility_stress::scenario`.

mod support;

use support::{assert_record_holds, scenario_selloff_then_rebalance};

/// The headline requirement from issue #1025: a 50% crash must not cost the
/// pool value, must not breach the virtual core, and must leave the pair usable
/// for LPs arriving and leaving afterwards.
#[test]
fn fifty_percent_crash_preserves_the_invariant() {
    let scenario = scenario_selloff_then_rebalance(10_000, 5_000, 200);
    assert_record_holds(&scenario, 5_000);
}

/// A crash past the usual threshold, to show the property is not tuned to one
/// number.
#[test]
fn seventy_five_percent_crash_preserves_the_invariant() {
    let scenario = scenario_selloff_then_rebalance(10_000, 7_500, 300);
    assert_record_holds(&scenario, 7_500);
}

/// A very deep crash, close to what the virtual core can absorb, still must not
/// drain the pool or breach the floor.
#[test]
fn ninety_percent_crash_preserves_the_invariant() {
    let scenario = scenario_selloff_then_rebalance(10_000, 9_000, 400);
    assert_record_holds(&scenario, 9_000);
}

/// A calm market must hold the same invariants; a property that only appears
/// under stress is not a property. Small ticks and a shallow target, so this
/// exercises the same code path at low volatility.
#[test]
fn quiet_market_holds_the_same_invariants() {
    let scenario = scenario_selloff_then_rebalance(1_000, 150, 20);
    assert_record_holds(&scenario, 150);
}

/// The same deep crash repeated under several seeds. The path is deterministic,
/// so this checks that the assertions do not accidentally depend on incidental
/// details of one particular fixture.
#[test]
fn crash_invariants_hold_across_seeds() {
    for seed in [1u64, 7, 42, 0xDEAD_BEEF, u64::MAX] {
        let scenario = volatility_stress::scenario::scenario_selloff_then_rebalance_seeded(
            seed, 10_000, 5_000, 200,
        );
        assert_record_holds(&scenario, 5_000);
        assert_eq!(scenario.seed, seed);
    }
}
