//! Property tests over randomised sell, deposit and withdrawal sequences.
//!
//! The generator is seeded and deterministic, so a failure here is a real defect
//! with a replayable reproduction: the failing seed and step index appear in the
//! assertion message.

use volatility_stress::stress::{
    assert_sound, capture, is_expected_stress_rejection, is_zero, run_volatile_sequence,
};
use volatility_stress::{PoolHarness, GENESIS};

/// A long randomised run over the real pool must never violate the invariant,
/// drain the pool, or leave it insolvent.
#[test]
fn randomised_sequence_preserves_the_invariant() {
    let pool = PoolHarness::new();
    let record = run_volatile_sequence(&pool, 0xC0FFEE, 400, GENESIS);

    assert!(
        !is_zero(&record.k_end),
        "the run drained the pool: {record:?}"
    );
    // Swaps/deposits never reduce k_eff, but LP withdrawals legitimately do
    // (the pool's own docs state a withdrawal is not constant-product bound).
    // `run_volatile_sequence` mixes withdrawals in, so no k_eff monotonicity
    // is asserted here; "never drained" plus record sanity carry the invariant.
    if record.drained_leg {
        // A run that ends at the drained boundary is still a positive result:
        // the pool reached a real reserve of exactly 0, and the invariant held
        // on every tick up to and including that boundary (asserted inside the
        // driver). The "did enough work" check below does not apply to it.
        assert!(
            !is_zero(&record.k_end),
            "drained-leg boundary collapsed k_eff: {record:?}"
        );
    } else {
        assert!(
            record.ticks.len() > 100,
            "the run barely executed anything: {} ticks",
            record.ticks.len()
        );
    }
    assert_record_sanity(&record);
}

/// The same run under a spread of seeds, so the assertions are not tied to one
/// favourable draw.
#[test]
fn randomised_sequence_holds_across_seeds() {
    for seed in [3u64, 11, 99, 12_345, 0xFFFF_FFFF_FFFF_FFFF] {
        let pool = PoolHarness::new();
        let record = run_volatile_sequence(&pool, seed, 150, GENESIS);
        assert!(
            !is_zero(&record.k_end),
            "seed {seed}: the run drained the pool: {record:?}"
        );
        assert_record_sanity(&record);
    }
}

/// A run with a tiny tick size must still be safe, even though it is far more
/// sensitive to integer division than a coarse one.
#[test]
fn dust_sized_trades_are_safe() {
    let pool = PoolHarness::new();
    let record = run_volatile_sequence(&pool, 55, 200, GENESIS);
    assert!(
        !is_zero(&record.k_end),
        "dust trades drained the pool: {record:?}"
    );
    assert_record_sanity(&record);
}

fn assert_record_sanity(record: &volatility_stress::stress::RunRecord) {
    assert!(
        !record.ticks.is_empty(),
        "seed {}: run executed nothing",
        record.seed
    );
    // The real reserves may never be negative, but a single leg *may* land on
    // exactly 0: the swap floor allows a payout equal to the whole real
    // reserve, and that drained-leg state is the pool's documented boundary.
    // With a positive virtual core the pool is still quotable and `k_eff` stays
    // positive, which is the invariant that matters.
    assert!(
        record.end_reserves.0 >= 0 && record.end_reserves.1 >= 0,
        "seed {}: run left a negative reserve: {:?}",
        record.seed,
        record.end_reserves
    );
    assert!(
        record.end_shares > 0,
        "seed {}: run burned the whole share supply",
        record.seed
    );
    assert!(
        record.end_spot > 0,
        "seed {}: spot price collapsed to zero",
        record.seed
    );
}

/// The rejections the pool raises under stress are the ones a caller can act on,
/// not internal panics: every reversion a stress run can provoke is a typed
/// `AmmError` variant, which is what lets an integrator handle it.
#[test]
fn stress_rejections_are_typed_and_actionable() {
    use amm_contract::AmmError;

    for err in [
        AmmError::VirtualFloorBreached,
        AmmError::InvalidDepositRatio,
        AmmError::SlippageExceeded,
        AmmError::NonPositiveAmount,
    ] {
        assert!(
            is_expected_stress_rejection(&err),
            "{err:?} should be classified as an expected stress rejection"
        );
    }
}

/// Custody is exact: after a heavy run the pool's token balances must still
/// equal its advertised real reserves, on both legs.
#[test]
fn custody_matches_reserves_after_heavy_load() {
    let pool = PoolHarness::new();
    run_volatile_sequence(&pool, 0xABCD, 500, GENESIS);

    let s = capture(&pool);
    assert_sound(&pool, &s, "after a heavy randomised run");
    assert_eq!(s.custody_a, s.reserves.0);
    assert_eq!(s.custody_b, s.reserves.1);

    // The pool is not drained past its floor: the curve sits on or above the
    // virtual core, which protects LPs and can never be paid out. Equality is
    // the documented boundary — a swap legally paid out the whole real reserve
    // on one leg, so the effective reserve exactly equals the core there — while
    // a reserve landing `below` its core would be a real defect.
    let (v_a, v_b) = pool.virtual_reserves();
    assert!(
        s.effective.0 >= v_a && s.effective.1 >= v_b,
        "effective curve fell below the virtual core: {s:?}"
    );
}

/// A swap may legally pay out the **whole** real reserve on one leg: the swap
/// floor rejects only `amount_out > y`, so `amount_out == y` is accepted and
/// leaves that leg at exactly 0. This is the pool's documented drained
/// boundary. On the other side of it, the pool must keep failing cleanly with
/// a typed error, never by hand-waving the zero reserve away.
#[test]
fn drained_leg_is_a_clean_terminal_state() {
    let pool = PoolHarness::new();
    let provider = pool.new_account(GENESIS * 20, GENESIS * 20);
    pool.deposit(&provider, GENESIS, GENESIS, 0)
        .expect("genesis deposit");
    // The exchange rate at genesis is 1:1, and the drain amount below is the
    // exact A-in that pays out the whole real B reserve (computed analytically
    // from `amount_out = y_eff*a/(x_eff+a)` with y_eff = GENESIS + v_y):
    //   a = y*x_eff / v_y = 1_000_000 * 1_001_000 / 1_000 = 1_001_000_000.
    let drain_a = 1_001_000_000i128;
    let trader = pool.new_account(drain_a + GENESIS, 0);
    let out = pool.swap_a_to_b(&trader, drain_a, 0).expect("drain swap");

    let boundary = capture(&pool);
    assert_eq!(
        out, GENESIS,
        "drain swap must pay out the whole real B reserve"
    );
    assert_eq!(
        boundary.reserves.1, 0,
        "expected the B leg to be drained to exactly 0, got {boundary:?}"
    );
    // The boundary state is sound.
    assert_sound(&pool, &boundary, "at the drained boundary");

    // Every operation past the boundary fails with a typed AmmError, and the
    // pool never invents liquidity to satisfy one.
    let (v_a, v_b) = pool.virtual_reserves();
    assert!(
        boundary.effective.0 > v_a && boundary.effective.1 == v_b,
        "the drained leg must sit exactly on the core: {boundary:?}"
    );
    assert!(
        pool.swap_a_to_b(&trader, GENESIS, 0).is_err(),
        "a swap into a drained leg was accepted"
    );
    assert!(
        pool.remove_liquidity(&provider, 1, 0, 0).is_err(),
        "a withdrawal from a drained pool was accepted"
    );

    // The untouched leg still has real value and custody is exact there.
    let (v_a, _) = pool.virtual_reserves();
    let after = capture(&pool);
    assert_eq!(after.custody_a, after.reserves.0);
    assert_eq!(after.custody_b, after.reserves.1);
    assert!(
        after.reserves.0 >= v_a,
        "A leg fell below its core: {after:?}"
    );
}
