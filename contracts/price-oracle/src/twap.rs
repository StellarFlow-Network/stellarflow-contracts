//! Single-ledger price-impact (TWAP) guard — closes #970.
//!
//! The oracle already rejects a *single* price submission that moves more than
//! the configured percentage away from the last stored price
//! (`ContractError::FlashCrashDetected`) and keeps an EWMA (`DataKey::Twap`)
//! that feeds a ±15% feed filter. Neither of those compares the *instant* price
//! against a *window* of recent ledger prices, so a burst of small
//! same-direction moves spread across consecutive ledgers can still walk the
//! oracle far from its recent average while every individual step stays inside
//! the per-update limits.
//!
//! This module adds the missing guard. It maintains a rolling window of the last
//! [`PRICE_WINDOW_LEDGERS`] per-ledger prices, computes the moving average
//! `P_ma`, and trips a per-asset flag whenever the instant price `P_inst`
//! differs from `P_ma` by more than [`PRICE_IMPACT_THRESHOLD_BPS`] (5%).
//! Lending / vault contracts call
//! [`PriceOracle::check_price_impact_guard`](crate::PriceOracle) before opening
//! a borrow; while the flag is set the call reverts with
//! [`ContractError::PriceImpactGuardTriggered`]. The flag clears itself on the
//! first later price write whose deviation falls back inside the threshold, so
//! normal vault operations resume automatically once volatility stabilises.
//!
//! # Storage layout
//!
//! | Key                                 | Bucket     | Type          | Description                                |
//! |-------------------------------------|------------|---------------|--------------------------------------------|
//! | `DataKey::PriceWindow(Symbol)`      | temporary  | `PriceWindow` | Last ≤5 per-ledger (ledger, price) samples |
//! | `DataKey::PriceImpactGuard(Symbol)` | persistent | `bool`        | `true` while borrows are paused            |

use soroban_sdk::{Env, Symbol};

use crate::types::{DataKey, PriceWindow, PriceWindowEntry};
use crate::ContractError;

/// Number of ledgers tracked in the moving-average window.
pub const PRICE_WINDOW_LEDGERS: u32 = 5;

/// Maximum allowed `|P_inst - P_ma| / P_ma`, in basis points (5% = 500 bps).
pub const PRICE_IMPACT_THRESHOLD_BPS: i128 = 500;

/// Read the rolling price window for `asset`.
///
/// Returns an empty window when the asset has no recorded history yet, so the
/// guard bootstraps without special-casing the first writes.
pub fn read_window(env: &Env, asset: &Symbol) -> PriceWindow {
    env.storage()
        .temporary()
        .get(&DataKey::PriceWindow(asset.clone()))
        .unwrap_or(PriceWindow {
            entries: soroban_sdk::Vec::new(env),
        })
}

/// Simple arithmetic mean of every sample currently held in `window`.
///
/// Returns `None` when the window is empty, or when the running sum would
/// overflow `i128`.
pub fn moving_average(window: &PriceWindow) -> Option<i128> {
    let len = window.entries.len();
    if len == 0 {
        return None;
    }

    let mut sum: i128 = 0;
    for i in 0..len {
        let entry = window.entries.get(i)?;
        sum = sum.checked_add(entry.price)?;
    }
    sum.checked_div(len as i128)
}

/// Mean of the window samples recorded on ledgers **strictly before**
/// `reference_ledger`.
///
/// The sample for `reference_ledger` is deliberately excluded: at the moment a
/// price is evaluated that sample *is* the instant price under test, and folding
/// it into the baseline would dilute the very move the guard exists to catch.
///
/// Returns `None` when no earlier sample exists.
pub fn trailing_average(window: &PriceWindow, reference_ledger: u32) -> Option<i128> {
    let mut sum: i128 = 0;
    let mut count: u32 = 0;
    for i in 0..window.entries.len() {
        let entry = window.entries.get(i)?;
        if entry.ledger_sequence < reference_ledger {
            sum = sum.checked_add(entry.price)?;
            count = count.checked_add(1)?;
        }
    }

    if count == 0 {
        None
    } else {
        sum.checked_div(count as i128)
    }
}

/// Append (or replace) the price observed for `ledger_sequence` and trim the
/// window to the most recent [`PRICE_WINDOW_LEDGERS`] samples.
fn write_sample(env: &Env, asset: &Symbol, price: i128, ledger_sequence: u32) {
    let window = read_window(env, asset);

    // Rebuild the window, dropping any earlier sample for this ledger: a second
    // write in the same ledger must replace, not duplicate, the entry.
    let mut entries = soroban_sdk::Vec::new(env);
    for i in 0..window.entries.len() {
        if let Some(entry) = window.entries.get(i) {
            if entry.ledger_sequence != ledger_sequence {
                entries.push_back(entry);
            }
        }
    }
    entries.push_back(PriceWindowEntry {
        ledger_sequence,
        price,
    });

    // Keep only the newest PRICE_WINDOW_LEDGERS samples (oldest first).
    let len = entries.len();
    let start = if len > PRICE_WINDOW_LEDGERS {
        len - PRICE_WINDOW_LEDGERS
    } else {
        0
    };
    let mut trimmed = soroban_sdk::Vec::new(env);
    for i in start..len {
        if let Some(entry) = entries.get(i) {
            trimmed.push_back(entry);
        }
    }

    let key = DataKey::PriceWindow(asset.clone());
    env.storage()
        .temporary()
        .set(&key, &PriceWindow { entries: trimmed });
    env.storage()
        .temporary()
        .extend_ttl(&key, 10_000u32, 10_000u32);
}

/// Whether the single-ledger price-impact guard is currently tripped for `asset`.
pub fn is_tripped(env: &Env, asset: &Symbol) -> bool {
    env.storage()
        .persistent()
        .get(&DataKey::PriceImpactGuard(asset.clone()))
        .unwrap_or(false)
}

/// Persist the trip flag (with a TTL bump) and publish the matching transition
/// event.
fn set_tripped(
    env: &Env,
    asset: &Symbol,
    instant: i128,
    average: i128,
    deviation_bps: i128,
    tripped: bool,
) {
    let key = DataKey::PriceImpactGuard(asset.clone());
    env.storage().persistent().set(&key, &tripped);
    env.storage()
        .persistent()
        .extend_ttl(&key, 10_000u32, 10_000u32);

    if tripped {
        crate::event_topics::publish_price_impact_guard_triggered(
            env,
            asset.clone(),
            instant,
            average,
            deviation_bps,
        );
    } else {
        crate::event_topics::publish_price_impact_guard_cleared(
            env,
            asset.clone(),
            instant,
            average,
            deviation_bps,
        );
    }
}

/// Record a freshly accepted price and (re)evaluate the single-ledger guard.
///
/// Called from every canonical price-write path (`set_price` and the relayer
/// consensus `update_price`). The comparison uses the trailing moving average of
/// the ledgers *before* the current one, and the new price then becomes part of
/// the window. The trip flag is written only when it actually flips, so the
/// `price_impact_guard_triggered` / `price_impact_guard_cleared` events fire
/// exactly on the trigger and on the recovery.
///
/// Returns the measured `|P_inst - P_ma| / P_ma` in basis points (0 when there
/// was no usable history).
pub fn record_and_evaluate(env: &Env, asset: &Symbol, price: i128) -> i128 {
    let reference_ledger: u32 = env.ledger().sequence().into();
    let window = read_window(env, asset);

    let (average, deviation_bps) = match trailing_average(&window, reference_ledger) {
        Some(avg) if avg > 0 => {
            let deviation = crate::calculate_percentage_difference_bps(avg, price).unwrap_or(0);
            (avg, deviation)
        }
        // No usable history yet (first write, or every prior sample shares the
        // current ledger) — the guard cannot evaluate and keeps its state.
        _ => {
            write_sample(env, asset, price, reference_ledger);
            return 0;
        }
    };

    let tripped = deviation_bps > PRICE_IMPACT_THRESHOLD_BPS;
    write_sample(env, asset, price, reference_ledger);

    if tripped != is_tripped(env, asset) {
        set_tripped(env, asset, price, average, deviation_bps, tripped);
    }

    deviation_bps
}

/// Borrow guard: reject while the single-ledger price-impact flag is set.
///
/// Lending / vault contracts invoke this through
/// `PriceOracle::check_price_impact_guard` at the top of their borrow
/// entrypoints. It is a plain read: no event is emitted here because a reverting
/// host call discards its events — the transition events are published from
/// [`record_and_evaluate`] when the flag actually flips.
pub fn enforce_price_impact_guard(env: &Env, asset: &Symbol) -> Result<(), ContractError> {
    if is_tripped(env, asset) {
        return Err(ContractError::PriceImpactGuardTriggered);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{
        contract, contractimpl,
        testutils::{Events, Ledger},
        Env, Symbol,
    };

    /// Minimal host for the module's internal API. Every method runs in the
    /// context of the registered contract so the storage helpers behave exactly
    /// as they do when called from `PriceOracle`.
    #[contract]
    struct TwapHarness;

    #[contractimpl]
    impl TwapHarness {
        pub fn record(env: Env, asset: Symbol, price: i128) {
            record_and_evaluate(&env, &asset, price);
        }

        pub fn is_tripped_now(env: Env, asset: Symbol) -> bool {
            is_tripped(&env, &asset)
        }

        pub fn check(env: Env, asset: Symbol) -> Result<(), ContractError> {
            enforce_price_impact_guard(&env, &asset)
        }

        pub fn average(env: Env, asset: Symbol) -> Option<i128> {
            moving_average(&read_window(&env, &asset))
        }
    }

    const BASE: i128 = 1_000_000_000;

    fn setup(asset_symbol: &str) -> (Env, TwapHarnessClient<'static>, Symbol) {
        let env = Env::default();
        let contract_id = env.register_contract(None, TwapHarness);
        let client = TwapHarnessClient::new(&env, &contract_id);
        (env, client, Symbol::new(&env, asset_symbol))
    }

    /// Record `price` on ledgers 1..=5 so the window is full.
    fn seed_five_ledgers(
        env: &Env,
        client: &TwapHarnessClient<'_>,
        asset: &Symbol,
        price: i128,
    ) {
        for ledger in 1u32..=PRICE_WINDOW_LEDGERS {
            env.ledger().with_mut(|li| li.sequence_number = ledger);
            client.record(asset, &price);
        }
    }

    #[test]
    fn guard_allows_move_exactly_at_five_percent() {
        let (env, client, asset) = setup("NGN");
        seed_five_ledgers(&env, &client, &asset, BASE);

        // Ledger 6: exactly +5.00% (500 bps) — inside the inclusive threshold.
        env.ledger().with_mut(|li| li.sequence_number = 6);
        client.record(&asset, &1_050_000_000);

        assert!(!client.is_tripped_now(&asset));
        assert!(client.try_check(&asset).is_ok());
        // Window is now ledgers 2..6: four samples at BASE plus the new price.
        assert_eq!(client.average(&asset), Some(1_010_000_000));
    }

    #[test]
    fn guard_trips_just_over_five_percent() {
        let (env, client, asset) = setup("NGN");
        seed_five_ledgers(&env, &client, &asset, BASE);

        // Ledger 6: +5.10% (510 bps) — just over the threshold.
        env.ledger().with_mut(|li| li.sequence_number = 6);
        client.record(&asset, &1_051_000_000);

        assert!(client.is_tripped_now(&asset));
        match client.try_check(&asset) {
            Err(Ok(e)) => assert_eq!(e, ContractError::PriceImpactGuardTriggered),
            other => panic!("expected PriceImpactGuardTriggered, got {:?}", other),
        }

        let debug = alloc::format!("{:?}", env.events().all());
        assert!(debug.contains("price_impact_guard_triggered"));
    }

    #[test]
    fn guard_trips_on_large_downside_move() {
        let (env, client, asset) = setup("KES");
        seed_five_ledgers(&env, &client, &asset, BASE);

        // Ledger 6: −5.50% (550 bps) in the other direction.
        env.ledger().with_mut(|li| li.sequence_number = 6);
        client.record(&asset, &945_000_000);

        assert!(client.is_tripped_now(&asset));
        match client.try_check(&asset) {
            Err(Ok(e)) => assert_eq!(e, ContractError::PriceImpactGuardTriggered),
            other => panic!("expected PriceImpactGuardTriggered, got {:?}", other),
        }
    }

    #[test]
    fn guard_resumes_once_window_stabilises() {
        let (env, client, asset) = setup("GHS");
        seed_five_ledgers(&env, &client, &asset, BASE);

        // Ledger 6: a 20% single-ledger jump trips the guard.
        env.ledger().with_mut(|li| li.sequence_number = 6);
        client.record(&asset, &1_200_000_000);
        assert!(client.is_tripped_now(&asset));
        assert!(client.try_check(&asset).is_err());

        // The spike still dominates 4 of the 5 samples, so the guard holds
        // through ledger 9.
        for ledger in 7u32..=9 {
            env.ledger().with_mut(|li| li.sequence_number = ledger);
            client.record(&asset, &1_200_000_000);
        }
        assert!(client.is_tripped_now(&asset));

        // By ledger 10 the pre-spike baseline has rolled out of the window and
        // the deviation is back inside the band, so borrowing resumes.
        env.ledger().with_mut(|li| li.sequence_number = 10);
        client.record(&asset, &1_200_000_000);

        assert!(!client.is_tripped_now(&asset));
        assert!(client.try_check(&asset).is_ok());

        let debug = alloc::format!("{:?}", env.events().all());
        assert!(debug.contains("price_impact_guard_cleared"));
    }

    #[test]
    fn guard_is_disarmed_before_any_history() {
        let (env, client, asset) = setup("CFA");

        // The very first write has no trailing window to compare against.
        env.ledger().with_mut(|li| li.sequence_number = 1);
        client.record(&asset, &BASE);

        assert!(!client.is_tripped_now(&asset));
        assert!(client.try_check(&asset).is_ok());
        assert_eq!(client.average(&asset), Some(BASE));
    }

    #[test]
    fn same_ledger_writes_replace_their_sample() {
        let (env, client, asset) = setup("XLM");

        env.ledger().with_mut(|li| li.sequence_number = 1);
        client.record(&asset, &BASE);
        client.record(&asset, &1_010_000_000);

        // One ledger, one sample — replaced rather than appended.
        assert_eq!(client.average(&asset), Some(1_010_000_000));
        assert!(!client.is_tripped_now(&asset));
    }
}
