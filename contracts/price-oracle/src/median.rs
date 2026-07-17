use soroban_sdk::{contracterror, Vec};

/// Hard cap matching the on-chain `MAX_MEDIAN_ENTRIES` constant (11) with
/// headroom for future validator-set growth.  All sorting is done inside a
/// stack buffer of exactly this size — no heap allocation occurs during the
/// median computation.
pub const MAX_VALIDATORS: usize = 15;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum MedianError {
    EmptyInput = 10,
    /// Arithmetic operation overflow detected.
    ArithmeticOverflow = 11,
    /// Input length exceeds the static stack-buffer capacity (MAX_VALIDATORS).
    InputTooLarge = 12,
}

// ── Stack-only sort helpers ────────────────────────────────────────────────

/// Insertion sort on a primitive `i128` slice — all work stays on the stack.
///
/// Insertion sort is chosen over quicksort because:
///  - No recursion (no hidden stack frames beyond the single call frame).
///  - O(n) best-case on already-sorted input (common for sequential relayer feeds).
///  - Negligible overhead for the small n ≤ MAX_VALIDATORS bound.
#[inline]
fn sort_stack_i128(buf: &mut [i128], len: usize) {
    for i in 1..len {
        let key = buf[i];
        let mut j = i;
        // Shift elements greater than `key` one position to the right.
        while j > 0 && buf[j - 1] > key {
            buf[j] = buf[j - 1];
            j -= 1;
        }
        buf[j] = key;
    }
}

/// Insertion sort on the value component of `(i128, u32)` pairs stored in a
/// primitive stack array — all work stays on the stack.
#[inline]
fn sort_stack_pairs(buf: &mut [(i128, u32)], len: usize) {
    for i in 1..len {
        let key = buf[i];
        let mut j = i;
        while j > 0 && buf[j - 1].0 > key.0 {
            buf[j] = buf[j - 1];
            j -= 1;
        }
        buf[j] = key;
    }
}

// ── Public median functions ────────────────────────────────────────────────

/// Returns the median of the provided prices.
///
/// # Gas profile
/// The Soroban `Vec` is traversed once to copy values into a
/// stack-allocated `[i128; MAX_VALIDATORS]` buffer.  All sorting and index
/// arithmetic are then performed entirely in stack memory — no further host
/// object calls are made after the copy step.
///
/// # Errors
/// - [`MedianError::EmptyInput`]     — slice is empty.
/// - [`MedianError::InputTooLarge`]  — more than `MAX_VALIDATORS` entries.
/// - [`MedianError::ArithmeticOverflow`] — even-count average overflows `i128`.
#[allow(dead_code)]
pub fn calculate_median(prices: Vec<i128>) -> Result<i128, MedianError> {
    let len = prices.len() as usize;
    if len == 0 {
        return Err(MedianError::EmptyInput);
    }
    if len > MAX_VALIDATORS {
        return Err(MedianError::InputTooLarge);
    }

    // Copy host-side Vec into a stack-allocated primitive array in one pass.
    let mut buf = [0i128; MAX_VALIDATORS];
    for i in 0..len {
        buf[i] = prices.get(i as u32).unwrap();
    }

    // Sort entirely on the stack — zero host allocations.
    sort_stack_i128(&mut buf, len);

    let mid = len / 2;
    if len % 2 == 1 {
        Ok(buf[mid])
    } else {
        let sum = buf[mid - 1]
            .checked_add(buf[mid])
            .ok_or(MedianError::ArithmeticOverflow)?;
        sum.checked_div(2).ok_or(MedianError::ArithmeticOverflow)
    }
}

/// Value at multiset index `target` (0-based) in an ascending `(value, count)`
/// stack buffer, located via cumulative counts.
#[inline]
fn value_at_stack(buf: &[(i128, u32)], len: usize, target: u64) -> i128 {
    let mut cum: u64 = 0;
    let mut last: i128 = 0;
    for i in 0..len {
        let (v, c) = buf[i];
        last = v;
        cum += c as u64;
        if target < cum {
            return v;
        }
    }
    // `target` is always < total by construction; fall back to the largest value.
    last
}

/// Median of a multiset represented as compacted `(value, count)` pairs.
///
/// The insertion sort runs only over the DISTINCT values (up to `MAX_VALIDATORS`
/// distinct prices), while `count` preserves each value's true multiplicity.
/// The result is therefore identical to sorting the full expanded multiset.
///
/// # Gas profile
/// The Soroban `Vec` is traversed once to copy `(i128, u32)` pairs into a
/// stack-allocated `[(i128, u32); MAX_VALIDATORS]` buffer.  All subsequent
/// sorting, cumulative-count traversal, and index arithmetic run on the stack.
///
/// # Errors
/// - [`MedianError::EmptyInput`]     — zero pairs or all counts are zero.
/// - [`MedianError::InputTooLarge`]  — more than `MAX_VALIDATORS` distinct values.
/// - [`MedianError::ArithmeticOverflow`] — count sum or average overflows.
#[allow(dead_code)]
pub fn calculate_median_compacted(pairs: Vec<(i128, u32)>) -> Result<i128, MedianError> {
    let len = pairs.len() as usize;
    if len == 0 {
        return Err(MedianError::EmptyInput);
    }
    if len > MAX_VALIDATORS {
        return Err(MedianError::InputTooLarge);
    }

    // Copy host-side Vec into a stack-allocated primitive array in one pass,
    // accumulating the total count at the same time.
    let mut buf = [(0i128, 0u32); MAX_VALIDATORS];
    let mut total: u64 = 0;
    for i in 0..len {
        let (v, c) = pairs.get(i as u32).unwrap();
        buf[i] = (v, c);
        total = total
            .checked_add(c as u64)
            .ok_or(MedianError::ArithmeticOverflow)?;
    }
    if total == 0 {
        return Err(MedianError::EmptyInput);
    }

    // Sort distinct values entirely on the stack — zero host allocations.
    sort_stack_pairs(&mut buf, len);

    if total % 2 == 1 {
        Ok(value_at_stack(&buf, len, total / 2))
    } else {
        let lo = value_at_stack(&buf, len, total / 2 - 1);
        let hi = value_at_stack(&buf, len, total / 2);
        let sum = lo
            .checked_add(hi)
            .ok_or(MedianError::ArithmeticOverflow)?;
        sum.checked_div(2).ok_or(MedianError::ArithmeticOverflow)
    }
}

#[cfg(test)]
mod median_tests {
    use crate::median::{calculate_median, MedianError};
    use soroban_sdk::{vec, Env};

    #[test]
    fn test_odd_number_median() {
        let env = Env::default();
        let prices = vec![&env, 748_i128, 750_i128, 752_i128];
        assert_eq!(calculate_median(prices), Ok(750));
    }

    #[test]
    fn test_even_number_median() {
        let env = Env::default();
        let prices = vec![&env, 740_i128, 750_i128, 760_i128, 770_i128];
        assert_eq!(calculate_median(prices), Ok(755));
    }

    #[test]
    fn test_single_input_returns_itself() {
        let env = Env::default();
        let prices = vec![&env, 999_i128];
        assert_eq!(calculate_median(prices), Ok(999));
    }

    #[test]
    fn test_empty_input_returns_error() {
        let env = Env::default();
        let prices = soroban_sdk::Vec::<i128>::new(&env);
        assert_eq!(calculate_median(prices), Err(MedianError::EmptyInput));
    }

    #[test]
    fn test_input_too_large_returns_error() {
        let env = Env::default();
        // 16 entries exceeds MAX_VALIDATORS (15).
        let prices = vec![
            &env,
            1_i128, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
        ];
        assert_eq!(calculate_median(prices), Err(MedianError::InputTooLarge));
    }

    #[test]
    fn test_compacted_matches_expanded_with_duplicates() {
        // Five votes of 750 and one of 900: true median is 750.
        // Naive dedup would give median(750, 900) = 825 — the bug we avoid.
        let env = Env::default();
        let pairs = vec![&env, (750_i128, 5_u32), (900_i128, 1_u32)];
        assert_eq!(crate::median::calculate_median_compacted(pairs), Ok(750));
    }

    #[test]
    fn test_compacted_even_total() {
        // Expanded multiset: [740,740,760,770] → median = (740+760)/2 = 750.
        let env = Env::default();
        let pairs = vec![&env, (740_i128, 2_u32), (760_i128, 1_u32), (770_i128, 1_u32)];
        assert_eq!(crate::median::calculate_median_compacted(pairs), Ok(750));
    }

    #[test]
    fn test_compacted_unsorted_input() {
        // [800,800,750,900,900,900] sorted → median = (800+900)/2 = 850.
        let env = Env::default();
        let pairs = vec![&env, (800_i128, 2_u32), (750_i128, 1_u32), (900_i128, 3_u32)];
        assert_eq!(crate::median::calculate_median_compacted(pairs), Ok(850));
    }

    #[test]
    fn test_compacted_single_bucket() {
        let env = Env::default();
        let pairs = vec![&env, (999_i128, 4_u32)];
        assert_eq!(crate::median::calculate_median_compacted(pairs), Ok(999));
    }

    #[test]
    fn test_compacted_empty_returns_error() {
        let env = Env::default();
        let pairs = soroban_sdk::Vec::<(i128, u32)>::new(&env);
        assert_eq!(
            crate::median::calculate_median_compacted(pairs),
            Err(MedianError::EmptyInput)
        );
    }

    #[test]
    fn test_compacted_too_large_returns_error() {
        use crate::median::MAX_VALIDATORS;
        let env = Env::default();
        // Build a pairs Vec with MAX_VALIDATORS + 1 distinct entries.
        let mut pairs = soroban_sdk::Vec::<(i128, u32)>::new(&env);
        for i in 0..=(MAX_VALIDATORS as i128) {
            pairs.push_back((i * 100, 1_u32));
        }
        assert_eq!(
            crate::median::calculate_median_compacted(pairs),
            Err(MedianError::InputTooLarge)
        );
    }
}
