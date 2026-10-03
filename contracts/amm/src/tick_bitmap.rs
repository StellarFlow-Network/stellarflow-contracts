//! Concentrated liquidity tick index search (Issue #912).
//!
//! # Bitmap index
//!
//! Initialized ticks are tracked in a packed bitmap instead of a sorted list.
//! A tick is first *compressed* by the pool's `tick_spacing`
//! (`compressed = floor(tick / tick_spacing)`), then split into a word
//! position and a bit position:
//!
//! ```text
//! word_pos = compressed >> 7        (floor division by 128)
//! bit_pos  = compressed & 127
//! ```
//!
//! Each word is a `u128` stored under its own persistent key, so flipping a
//! tick touches exactly one storage entry, and finding the next initialized
//! tick inside a word is a single masked `leading_zeros` / `trailing_zeros`
//! instead of a per-tick iteration. Empty words are removed from storage so
//! they do not accrue rent.
//!
//! # Tick price model
//!
//! Every tick `i` maps to the price `P(i) = 1.0001^i`, expressed as an
//! unsigned Q64.64 fixed-point number. `P(i)` is computed by binary
//! exponentiation over the precomputed constants `1.0001^(2^k)`, so pricing
//! any tick costs at most 19 multiplications. A price is consistent with
//! tick `i` exactly when `P(i) <= price < P(i + 1)`.
//!
//! The tick range is `[-300_000, 300_000]` (prices ~1e-13 .. ~1e13). Beyond
//! that, Q64.64 loses the resolution needed to keep adjacent tick prices
//! strictly increasing at the low end.

use soroban_sdk::{contracttype, Env};

use crate::AmmError;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Lowest tick a position may reference.
pub const MIN_TICK: i32 = -300_000;
/// Highest tick a position may reference.
pub const MAX_TICK: i32 = 300_000;

/// Largest permitted tick spacing. Keeps `compressed * spacing` arithmetic
/// comfortably inside `i32`.
pub const MAX_TICK_SPACING: i32 = 16_384;

/// Number of ticks tracked by one bitmap word.
const WORD_BITS: i32 = 128;

/// `1.0` in Q64.64.
pub const Q64: u128 = 1 << 64;

/// `floor(1.0001^(2^k) * 2^64)` for `k = 0..=18`. `2^19 > MAX_TICK`, so these
/// cover every tick magnitude in range.
const POW_Q64: [u128; 19] = [
    0x1_0006_8db8_bac7_10cb,
    0x1_000d_1b9c_68ab_e5f7,
    0x1_001a_37e4_a234_cb08,
    0x1_0034_7278_ab0e_92ad,
    0x1_0068_efb0_0a52_5480,
    0x1_00d2_0a63_b417_3839,
    0x1_01a4_c11c_742d_d772,
    0x1_034c_35c3_1f64_cfa6,
    0x1_06a3_4b78_c8aa_ffbf,
    0x1_0d72_a6a4_6ccd_8bce,
    0x1_1b9a_258e_6392_8596,
    0x1_3a2e_2bda_04f8_379f,
    0x1_8195_4be6_9e0d_a8fe,
    0x2_44c2_655d_185a_0290,
    0x5_2581_6eeb_9f93_5b1c,
    0x1a_7c8d_00b5_5168_4ff4,
    0x2bd_893d_0b2d_f7c9_7884,
    0x7_8278_e1e1_9e44_8cf8_b95d,
    0x38_651b_58d4_5750_1416_feade319,
];

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TickBitmapKey {
    /// One 128-bit word of the initialized-tick bitmap.
    Word(i32),
}

fn load_word(env: &Env, word_pos: i32) -> u128 {
    env.storage()
        .persistent()
        .get(&TickBitmapKey::Word(word_pos))
        .unwrap_or(0)
}

fn store_word(env: &Env, word_pos: i32, word: u128) {
    let key = TickBitmapKey::Word(word_pos);
    if word == 0 {
        env.storage().persistent().remove(&key);
    } else {
        env.storage().persistent().set(&key, &word);
    }
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

fn check_spacing(tick_spacing: i32) -> Result<(), AmmError> {
    if tick_spacing <= 0 || tick_spacing > MAX_TICK_SPACING {
        return Err(AmmError::InvalidTickSpacing);
    }
    Ok(())
}

fn check_tick(tick: i32) -> Result<(), AmmError> {
    if !(MIN_TICK..=MAX_TICK).contains(&tick) {
        return Err(AmmError::TickOutOfBounds);
    }
    Ok(())
}

/// Lowest tick aligned to `tick_spacing` that is still within range.
pub fn min_usable_tick(tick_spacing: i32) -> i32 {
    (MIN_TICK / tick_spacing) * tick_spacing
}

/// Highest tick aligned to `tick_spacing` that is still within range.
pub fn max_usable_tick(tick_spacing: i32) -> i32 {
    (MAX_TICK / tick_spacing) * tick_spacing
}

/// Split a compressed tick into `(word_pos, bit_pos)`.
fn position(compressed: i32) -> (i32, u32) {
    (compressed >> 7, (compressed & (WORD_BITS - 1)) as u32)
}

// ---------------------------------------------------------------------------
// Bitmap maintenance
// ---------------------------------------------------------------------------

/// Toggle the initialized flag of `tick`. Returns the flag's new value.
///
/// Call this when a tick's gross liquidity transitions between zero and
/// non-zero.
pub fn flip_tick(env: &Env, tick: i32, tick_spacing: i32) -> Result<bool, AmmError> {
    check_spacing(tick_spacing)?;
    check_tick(tick)?;
    if tick % tick_spacing != 0 {
        return Err(AmmError::TickNotAligned);
    }

    let (word_pos, bit_pos) = position(tick / tick_spacing);
    let mask = 1u128 << bit_pos;
    let word = load_word(env, word_pos) ^ mask;
    store_word(env, word_pos, word);
    Ok(word & mask != 0)
}

/// Whether `tick` is currently marked initialized.
pub fn is_initialized(env: &Env, tick: i32, tick_spacing: i32) -> Result<bool, AmmError> {
    check_spacing(tick_spacing)?;
    check_tick(tick)?;
    if tick % tick_spacing != 0 {
        return Ok(false);
    }
    let (word_pos, bit_pos) = position(tick / tick_spacing);
    Ok(load_word(env, word_pos) & (1u128 << bit_pos) != 0)
}

// ---------------------------------------------------------------------------
// Next-initialized-tick search
// ---------------------------------------------------------------------------

/// Search a single bitmap word for the next initialized tick.
///
/// * `lte == true` (price moving down): the greatest initialized tick
///   `<= tick`, looking only within the word containing `tick`.
/// * `lte == false` (price moving up): the least initialized tick `> tick`,
///   looking only within the word containing the next compressed tick.
///
/// Returns `(next, initialized)`. When nothing is set in the word, `next` is
/// the word's far edge (clamped to the usable range) and `initialized` is
/// `false`, so the caller can step to it and continue.
pub fn next_initialized_tick_within_one_word(
    env: &Env,
    tick: i32,
    tick_spacing: i32,
    lte: bool,
) -> Result<(i32, bool), AmmError> {
    check_spacing(tick_spacing)?;
    check_tick(tick)?;

    let compressed = tick.div_euclid(tick_spacing);

    let (next, initialized) = if lte {
        let (word_pos, bit_pos) = position(compressed);
        // All bits at or below `bit_pos`.
        let mask = u128::MAX >> (127 - bit_pos);
        let masked = load_word(env, word_pos) & mask;

        if masked != 0 {
            let msb = 127 - masked.leading_zeros();
            ((compressed - (bit_pos - msb) as i32) * tick_spacing, true)
        } else {
            ((compressed - bit_pos as i32) * tick_spacing, false)
        }
    } else {
        let start = compressed + 1;
        let (word_pos, bit_pos) = position(start);
        // All bits at or above `bit_pos`.
        let mask = u128::MAX << bit_pos;
        let masked = load_word(env, word_pos) & mask;

        if masked != 0 {
            let lsb = masked.trailing_zeros();
            ((start + (lsb - bit_pos) as i32) * tick_spacing, true)
        } else {
            ((start + (127 - bit_pos) as i32) * tick_spacing, false)
        }
    };

    if initialized {
        return Ok((next, true));
    }
    let clamped = next
        .max(min_usable_tick(tick_spacing))
        .min(max_usable_tick(tick_spacing));
    Ok((clamped, false))
}

/// Find the next initialized tick in the swap direction, scanning at most
/// `max_words` bitmap words.
///
/// Semantics of `lte` match [`next_initialized_tick_within_one_word`]. If no
/// initialized tick is found, returns the furthest tick reached (the usable
/// range boundary when the search is exhausted) with `initialized == false`.
/// Bounding the scan with `max_words` caps storage reads per swap step.
pub fn next_initialized_tick(
    env: &Env,
    tick: i32,
    tick_spacing: i32,
    lte: bool,
    max_words: u32,
) -> Result<(i32, bool), AmmError> {
    check_spacing(tick_spacing)?;
    let lower = min_usable_tick(tick_spacing);
    let upper = max_usable_tick(tick_spacing);

    let mut cursor = tick;
    let mut result = (tick, false);

    for _ in 0..max_words.max(1) {
        result = next_initialized_tick_within_one_word(env, cursor, tick_spacing, lte)?;
        let (next, initialized) = result;
        if initialized {
            return Ok(result);
        }
        if lte {
            if next <= lower {
                return Ok((lower, false));
            }
            cursor = next - 1;
        } else {
            if next >= upper {
                return Ok((upper, false));
            }
            cursor = next;
        }
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// Tick <-> price
// ---------------------------------------------------------------------------

/// High 128 bits of the full 256-bit product `a * b`, via a four-product
/// `u64 x u64` decomposition (`u128::widening_mul` is still unstable).
fn mul_high(a: u128, b: u128) -> u128 {
    const MASK: u128 = u64::MAX as u128;

    let a_lo = a & MASK;
    let a_hi = a >> 64;
    let b_lo = b & MASK;
    let b_hi = b >> 64;

    let p_ll = a_lo * b_lo;
    let p_lh = a_lo * b_hi;
    let p_hl = a_hi * b_lo;
    let p_hh = a_hi * b_hi;

    let (mid, carry_mid) = p_lh.overflowing_add(p_hl);
    let low_carry = ((p_ll >> 64) + (mid & MASK)) >> 64;

    p_hh.wrapping_add(mid >> 64)
        .wrapping_add((carry_mid as u128) << 64)
        .wrapping_add(low_carry)
}

/// Q64.64 multiply: `floor(a * b / 2^64)`.
fn mul_q64(a: u128, b: u128) -> Result<u128, AmmError> {
    let hi = mul_high(a, b);
    if hi >> 64 != 0 {
        return Err(AmmError::PriceOutOfBounds);
    }
    let lo = a.wrapping_mul(b);
    Ok((hi << 64) | (lo >> 64))
}

/// `P(tick) = 1.0001^tick` as Q64.64.
pub fn tick_to_price_q64(tick: i32) -> Result<u128, AmmError> {
    check_tick(tick)?;

    let magnitude = tick.unsigned_abs();
    let mut price = Q64;
    for (k, factor) in POW_Q64.iter().enumerate() {
        if magnitude & (1 << k) != 0 {
            price = mul_q64(price, *factor)?;
        }
    }

    if tick < 0 {
        // 1 / P(|tick|) in Q64.64 is 2^128 / P(|tick|).
        price = u128::MAX / price;
    }
    Ok(price)
}

/// Greatest tick `i` with `P(i) <= price`.
///
/// Errors with [`AmmError::PriceOutOfBounds`] if `price` is outside
/// `[P(MIN_TICK), P(MAX_TICK)]`.
pub fn price_q64_to_tick(price: u128) -> Result<i32, AmmError> {
    if price < tick_to_price_q64(MIN_TICK)? || price > tick_to_price_q64(MAX_TICK)? {
        return Err(AmmError::PriceOutOfBounds);
    }

    // Invariant: P(lo) <= price, and either hi == MAX_TICK or price < P(hi + 1).
    let (mut lo, mut hi) = (MIN_TICK, MAX_TICK);
    while lo < hi {
        let mid = lo + (hi - lo + 1) / 2;
        if tick_to_price_q64(mid)? <= price {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    Ok(lo)
}

/// Assert that `price` lies in the band owned by `tick`, i.e.
/// `P(tick) <= price < P(tick + 1)` with `P(i) = 1.0001^i`.
///
/// At `MAX_TICK` the band is the single point `P(MAX_TICK)`.
pub fn assert_tick_price_bounds(tick: i32, price: u128) -> Result<(), AmmError> {
    let lower = tick_to_price_q64(tick)?;
    let within = if tick == MAX_TICK {
        price == lower
    } else {
        lower <= price && price < tick_to_price_q64(tick + 1)?
    };
    if within {
        Ok(())
    } else {
        Err(AmmError::TickPriceMismatch)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::AmmContract;
    use std::collections::BTreeSet;
    use std::vec::Vec as StdVec;

    /// Run `f` inside a registered contract so persistent storage is usable.
    fn with_contract<T>(f: impl FnOnce(&Env) -> T) -> T {
        let env = Env::default();
        let id = env.register_contract(None, AmmContract);
        env.as_contract(&id, || f(&env))
    }

    // ── Tick price: P = 1.0001^i ────────────────────────────────────────

    /// Exact `floor(1.0001^t * 2^64)` and `floor(1.0001^-t * 2^64)`, computed
    /// with arbitrary-precision rationals.
    const REFERENCE: [(i32, u128, u128); 8] = [
        (1, 18448588748116922571, 18444899583751176498),
        (10, 18465199121032091053, 18428307471288117481),
        (100, 18632127618364105992, 18263205034381099368),
        (1_000, 20386703156472572688, 16691387729992146528),
        (10_000, 50140942267140973302, 6786517196026784945),
        (100_000, 406113483393643373014939, 837899702510258),
        (262_144, 4468068147273140139091016147737, 76158723),
        (300_000, 196835207006294262292126797421032, 1728767),
    ];

    /// |actual - exact| <= exact * 1e-12 + 2 ulps.
    fn assert_close(actual: u128, exact: u128) {
        let diff = actual.abs_diff(exact);
        assert!(
            diff <= exact / 1_000_000_000_000 + 2,
            "actual {actual} vs exact {exact} (diff {diff})"
        );
    }

    #[test]
    fn mul_high_max_bounds() {
        // (2^128 - 1)^2 = 2^256 - 2^129 + 1 -> high word = 2^128 - 2.
        assert_eq!(mul_high(u128::MAX, u128::MAX), u128::MAX - 1);
    }

    #[test]
    fn mul_high_small_product_is_zero() {
        assert_eq!(mul_high(5, 7), 0);
        assert_eq!(mul_high(1 << 64, 1 << 64), 1);
    }

    #[test]
    fn tick_zero_is_one() {
        assert_eq!(tick_to_price_q64(0).unwrap(), Q64);
    }

    #[test]
    fn tick_price_matches_1_0001_pow_i() {
        for (tick, pos, neg) in REFERENCE {
            assert_close(tick_to_price_q64(tick).unwrap(), pos);
            assert_close(tick_to_price_q64(-tick).unwrap(), neg);
        }
    }

    #[test]
    fn tick_price_matches_float_model() {
        for tick in [-250_000, -54_321, -777, -3, 5, 999, 12_345, 200_000] {
            let expected = 1.0001f64.powi(tick);
            let actual = tick_to_price_q64(tick).unwrap() as f64 / Q64 as f64;
            let rel = (actual - expected).abs() / expected;
            assert!(rel < 1e-9, "tick {tick}: {actual} vs {expected}");
        }
    }

    #[test]
    fn tick_price_strictly_increasing_at_edges() {
        for start in [MIN_TICK, -1_000, MAX_TICK - 1_000] {
            let mut prev = tick_to_price_q64(start).unwrap();
            for tick in start + 1..start + 1_000 {
                let p = tick_to_price_q64(tick).unwrap();
                assert!(p > prev, "P({tick}) not above P({})", tick - 1);
                prev = p;
            }
        }
    }

    #[test]
    fn tick_price_rejects_out_of_range() {
        assert_eq!(tick_to_price_q64(MAX_TICK + 1), Err(AmmError::TickOutOfBounds));
        assert_eq!(tick_to_price_q64(MIN_TICK - 1), Err(AmmError::TickOutOfBounds));
    }

    #[test]
    fn price_to_tick_round_trips() {
        for tick in [MIN_TICK, -123_456, -1, 0, 1, 4_242, 299_999, MAX_TICK] {
            let p = tick_to_price_q64(tick).unwrap();
            assert_eq!(price_q64_to_tick(p).unwrap(), tick);
            if tick != MAX_TICK {
                // Just below the next tick's price still belongs to `tick`.
                let next = tick_to_price_q64(tick + 1).unwrap();
                assert_eq!(price_q64_to_tick(next - 1).unwrap(), tick);
            }
        }
    }

    #[test]
    fn price_to_tick_rejects_out_of_range() {
        let min = tick_to_price_q64(MIN_TICK).unwrap();
        let max = tick_to_price_q64(MAX_TICK).unwrap();
        assert_eq!(price_q64_to_tick(min - 1), Err(AmmError::PriceOutOfBounds));
        assert_eq!(price_q64_to_tick(max + 1), Err(AmmError::PriceOutOfBounds));
    }

    #[test]
    fn assert_bounds_accepts_price_in_band() {
        let lower = tick_to_price_q64(100).unwrap();
        let upper = tick_to_price_q64(101).unwrap();
        assert_eq!(assert_tick_price_bounds(100, lower), Ok(()));
        assert_eq!(assert_tick_price_bounds(100, (lower + upper) / 2), Ok(()));
        assert_eq!(assert_tick_price_bounds(100, upper - 1), Ok(()));
        let max = tick_to_price_q64(MAX_TICK).unwrap();
        assert_eq!(assert_tick_price_bounds(MAX_TICK, max), Ok(()));
    }

    #[test]
    fn assert_bounds_rejects_price_outside_band() {
        let lower = tick_to_price_q64(100).unwrap();
        let upper = tick_to_price_q64(101).unwrap();
        assert_eq!(
            assert_tick_price_bounds(100, lower - 1),
            Err(AmmError::TickPriceMismatch)
        );
        assert_eq!(
            assert_tick_price_bounds(100, upper),
            Err(AmmError::TickPriceMismatch)
        );
        assert_eq!(assert_tick_price_bounds(0, Q64 * 2), Err(AmmError::TickPriceMismatch));
    }

    // ── Bitmap maintenance ──────────────────────────────────────────────

    #[test]
    fn flip_toggles_and_clears_storage() {
        with_contract(|env| {
            assert!(!is_initialized(env, 120, 60).unwrap());
            assert!(flip_tick(env, 120, 60).unwrap());
            assert!(is_initialized(env, 120, 60).unwrap());
            assert!(!flip_tick(env, 120, 60).unwrap());
            assert!(!is_initialized(env, 120, 60).unwrap());
            // Word emptied -> key removed, no lingering rent.
            let (word_pos, _) = position(2);
            assert!(!env.storage().persistent().has(&TickBitmapKey::Word(word_pos)));
        });
    }

    #[test]
    fn flip_validates_input() {
        with_contract(|env| {
            assert_eq!(flip_tick(env, 30, 60), Err(AmmError::TickNotAligned));
            assert_eq!(flip_tick(env, 0, 0), Err(AmmError::InvalidTickSpacing));
            assert_eq!(flip_tick(env, 0, -1), Err(AmmError::InvalidTickSpacing));
            assert_eq!(
                flip_tick(env, 0, MAX_TICK_SPACING + 1),
                Err(AmmError::InvalidTickSpacing)
            );
            assert_eq!(flip_tick(env, MAX_TICK + 1, 1), Err(AmmError::TickOutOfBounds));
        });
    }

    #[test]
    fn negative_ticks_map_to_distinct_bits() {
        with_contract(|env| {
            flip_tick(env, -1, 1).unwrap();
            assert!(is_initialized(env, -1, 1).unwrap());
            assert!(!is_initialized(env, 0, 1).unwrap());
            assert!(!is_initialized(env, -128, 1).unwrap());
            // -1 lives in word -1, bit 127.
            assert_eq!(position(-1), (-1, 127));
            assert_eq!(position(-128), (-1, 0));
            assert_eq!(position(-129), (-2, 127));
        });
    }

    // ── Search within one word ──────────────────────────────────────────

    #[test]
    fn within_word_lte_finds_tick_at_or_below() {
        with_contract(|env| {
            for t in [-200, 70, 78, 84, 139, 240, 535] {
                flip_tick(env, t, 1).unwrap();
            }
            assert_eq!(next_initialized_tick_within_one_word(env, 78, 1, true), Ok((78, true)));
            assert_eq!(next_initialized_tick_within_one_word(env, 79, 1, true), Ok((78, true)));
            assert_eq!(next_initialized_tick_within_one_word(env, 83, 1, true), Ok((78, true)));
            // Word 0 covers [0, 127]; nothing below 70 in it -> word start.
            assert_eq!(next_initialized_tick_within_one_word(env, 69, 1, true), Ok((0, false)));
            // Word 1 covers [128, 255].
            assert_eq!(next_initialized_tick_within_one_word(env, 255, 1, true), Ok((240, true)));
            assert_eq!(next_initialized_tick_within_one_word(env, 138, 1, true), Ok((128, false)));
        });
    }

    #[test]
    fn within_word_gt_finds_tick_strictly_above() {
        with_contract(|env| {
            for t in [-200, 70, 78, 84, 139, 240, 535] {
                flip_tick(env, t, 1).unwrap();
            }
            assert_eq!(next_initialized_tick_within_one_word(env, 78, 1, false), Ok((84, true)));
            assert_eq!(next_initialized_tick_within_one_word(env, 77, 1, false), Ok((78, true)));
            assert_eq!(next_initialized_tick_within_one_word(env, -55, 1, false), Ok((-1, false)));
            assert_eq!(next_initialized_tick_within_one_word(env, 84, 1, false), Ok((127, false)));
            // Starting at the last bit of a word jumps into the next word.
            assert_eq!(next_initialized_tick_within_one_word(env, 127, 1, false), Ok((139, true)));
            assert_eq!(next_initialized_tick_within_one_word(env, -201, 1, false), Ok((-200, true)));
        });
    }

    #[test]
    fn within_word_respects_tick_spacing() {
        with_contract(|env| {
            flip_tick(env, -120, 60).unwrap();
            flip_tick(env, 600, 60).unwrap();
            // Unaligned input tick compresses toward -inf.
            assert_eq!(next_initialized_tick_within_one_word(env, -61, 60, true), Ok((-120, true)));
            assert_eq!(next_initialized_tick_within_one_word(env, 599, 60, false), Ok((600, true)));
            assert_eq!(next_initialized_tick_within_one_word(env, 601, 60, true), Ok((600, true)));
        });
    }

    #[test]
    fn within_word_clamps_to_usable_range() {
        with_contract(|env| {
            let spacing = 7;
            let (next, init) =
                next_initialized_tick_within_one_word(env, MAX_TICK, spacing, false).unwrap();
            assert!(!init);
            assert_eq!(next, max_usable_tick(spacing));
            let (next, init) =
                next_initialized_tick_within_one_word(env, MIN_TICK, spacing, true).unwrap();
            assert!(!init);
            assert_eq!(next, min_usable_tick(spacing));
        });
    }

    // ── Multi-word search ───────────────────────────────────────────────

    #[test]
    fn multi_word_search_crosses_empty_words() {
        with_contract(|env| {
            flip_tick(env, -5_000, 1).unwrap();
            flip_tick(env, 7_000, 1).unwrap();
            assert_eq!(next_initialized_tick(env, 0, 1, false, 100), Ok((7_000, true)));
            assert_eq!(next_initialized_tick(env, 0, 1, true, 100), Ok((-5_000, true)));
        });
    }

    #[test]
    fn multi_word_search_stops_at_word_budget() {
        with_contract(|env| {
            flip_tick(env, 7_000, 1).unwrap();
            // 7_000 is ~55 words away; 3 words is not enough.
            let (next, init) = next_initialized_tick(env, 0, 1, false, 3).unwrap();
            assert!(!init);
            assert_eq!(next, 3 * 128 - 1);
            // Resuming from where the budget ran out finds it.
            assert_eq!(next_initialized_tick(env, next, 1, false, 100), Ok((7_000, true)));
        });
    }

    #[test]
    fn multi_word_search_rejects_bad_spacing() {
        with_contract(|env| {
            assert_eq!(
                next_initialized_tick(env, 0, 0, false, 1),
                Err(AmmError::InvalidTickSpacing)
            );
            assert_eq!(
                next_initialized_tick(env, 0, -60, true, 1),
                Err(AmmError::InvalidTickSpacing)
            );
        });
    }

    #[test]
    fn multi_word_search_empty_returns_boundaries() {
        with_contract(|env| {
            assert_eq!(
                next_initialized_tick(env, 0, 60, false, u32::MAX),
                Ok((max_usable_tick(60), false))
            );
            assert_eq!(
                next_initialized_tick(env, 0, 60, true, u32::MAX),
                Ok((min_usable_tick(60), false))
            );
        });
    }

    /// Cross-check the bitmap against a brute-force ordered set.
    #[test]
    fn bitmap_search_matches_linear_scan() {
        with_contract(|env| {
            // Hundreds of searches in one frame exceed the default test budget.
            env.budget().reset_unlimited();
            let spacing = 10;
            let mut rng: u64 = 0x9E37_79B9_7F4A_7C15;
            let mut next_rand = move || {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                rng
            };

            let mut set = BTreeSet::new();
            for _ in 0..200 {
                let t = ((next_rand() % 4_001) as i32 - 2_000) * spacing;
                if flip_tick(env, t, spacing).unwrap() {
                    set.insert(t);
                } else {
                    set.remove(&t);
                }
            }
            let ticks: StdVec<i32> = set.iter().copied().collect();

            for _ in 0..500 {
                let tick = (next_rand() % 50_001) as i32 - 25_000;

                let expected_lte = ticks.iter().rev().find(|&&t| t <= tick).copied();
                let (got, init) = next_initialized_tick(env, tick, spacing, true, u32::MAX).unwrap();
                match expected_lte {
                    Some(t) => assert_eq!((got, init), (t, true), "lte from {tick}"),
                    None => assert!(!init, "lte from {tick}"),
                }

                let expected_gt = ticks.iter().find(|&&t| t > tick).copied();
                let (got, init) = next_initialized_tick(env, tick, spacing, false, u32::MAX).unwrap();
                match expected_gt {
                    Some(t) => assert_eq!((got, init), (t, true), "gt from {tick}"),
                    None => assert!(!init, "gt from {tick}"),
                }
            }
        });
    }
}
