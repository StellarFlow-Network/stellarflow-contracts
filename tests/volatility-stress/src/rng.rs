//! A small, deterministic pseudo-random number generator.
//!
//! Stress runs must be reproducible, and `rand` is not a dependency of the
//! upstream crates, so this is a self-contained `xorshift64*` generator: a
//! fixed seed always yields the same sequence, on every machine, forever.
//!
//! Given that, a failing stress run is a real defect in the pool rather than
//! a symptom of a flaky draw, and a failure can be replayed by printing the
//! seed and the iteration index from the assertion message.

/// Deterministic `xorshift64*` generator.
#[derive(Debug, Clone)]
pub struct DeterministicRng {
    state: u64,
}

impl DeterministicRng {
    /// Seed the generator.
    ///
    /// A zero seed is remapped, because `xorshift64*` cannot leave zero.
    pub fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    /// Uniform `i128` value in `low..=high`.
    ///
    /// Pool amounts and share counts are `i128`, so this is the form the
    /// scenarios actually use; converting at the call site would push an
    /// unchecked cast into every call.
    pub fn next_i128_in_range(&mut self, low: i128, high: i128) -> i128 {
        if high <= low {
            return low;
        }
        let span = (high - low) as u64 + 1;
        low.saturating_add(self.next_below(span) as i128)
    }

    /// Next raw 64-bit value.
    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform value in `0..bound`.
    ///
    /// Returns `1` for a zero bound so callers can use it as a divisor
    /// without guarding.
    pub fn next_below(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            return 1;
        }
        self.next_u64() % bound
    }

    /// Uniform value in `low..=high`.
    ///
    /// Returns `low` if the range is empty, which keeps a mis-specified stress
    /// parameter from producing a modulo-by-zero panic mid-run.
    pub fn next_in_range(&mut self, low: u64, high: u64) -> u64 {
        if high <= low {
            return low;
        }
        low + self.next_below(high - low + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_yields_same_sequence() {
        let mut a = DeterministicRng::new(42);
        let mut b = DeterministicRng::new(42);
        for _ in 0..128 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = DeterministicRng::new(1);
        let mut b = DeterministicRng::new(2);
        let differing = (0..128).filter(|_| a.next_u64() != b.next_u64()).count();
        assert!(differing > 120, "sequences should not track each other");
    }

    #[test]
    fn zero_seed_is_usable() {
        let mut rng = DeterministicRng::new(0);
        assert_ne!(rng.next_u64(), 0);
        assert_ne!(rng.next_u64(), 0);
    }

    #[test]
    fn stays_within_bounds() {
        let mut rng = DeterministicRng::new(7);
        for _ in 0..1000 {
            assert!(rng.next_below(10) < 10);
            let v = rng.next_in_range(100, 105);
            assert!((100..=105).contains(&v));
        }
    }

    #[test]
    fn degenerate_ranges_do_not_panic() {
        let mut rng = DeterministicRng::new(9);
        assert_eq!(rng.next_in_range(5, 5), 5);
        assert_eq!(rng.next_in_range(9, 3), 9);
        assert_eq!(rng.next_below(0), 1);
    }
}
