use core::cmp::Ordering;

/// Compute the floor of the integer square root for a non-negative value.
///
/// This helper stays fully integer-based so it can be used inside Soroban
/// without relying on host-side floating-point or standard library helpers.
pub fn integer_sqrt(value: i128) -> i128 {
    if value <= 0 {
        return 0;
    }

    let mut lo = 1_i128;
    let mut hi = value;

    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        match mid.checked_mul(mid) {
            Some(square) => match square.cmp(&value) {
                Ordering::Equal => return mid,
                Ordering::Less => lo = mid + 1,
                Ordering::Greater => hi = mid - 1,
            },
            None => hi = mid - 1,
        }
    }

    hi
}

/// Compute a root-scaled smoothing update using checked bit shifts.
///
/// The weight is derived from the square root of the supplied alpha and then
/// applied through a power-of-two fixed-point scale so the update stays exact
/// for the host engine without relying on fractional arithmetic.
pub fn compute_smoothed_value(price: i128, previous: i128, alpha: i128) -> i128 {
    if alpha <= 0 {
        return price;
    }

    const SCALE_SHIFT: u32 = 16;
    let alpha_root = integer_sqrt(alpha);
    let max_root = integer_sqrt(10_000);

    let max_weight = match 1_i128.checked_shl(SCALE_SHIFT) {
        Some(weight) => weight,
        None => i128::MAX,
    };

    let alpha_weight = match alpha_root.checked_mul(max_weight) {
        Some(weight) => weight / max_root,
        None => i128::MAX,
    };
    let complement_weight = max_weight.saturating_sub(alpha_weight);

    let numerator = match price.checked_mul(alpha_weight) {
        Some(weighted_price) => match previous.checked_mul(complement_weight) {
            Some(weighted_previous) => weighted_price.saturating_add(weighted_previous),
            None => i128::MAX,
        },
        None => i128::MAX,
    };

    numerator / max_weight
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_sqrt_handles_perfect_squares() {
        assert_eq!(integer_sqrt(64), 8);
        assert_eq!(integer_sqrt(144), 12);
    }

    #[test]
    fn integer_sqrt_rounds_down_for_non_squares() {
        assert_eq!(integer_sqrt(15), 3);
        assert_eq!(integer_sqrt(20), 4);
    }

    #[test]
    fn smoothing_update_uses_root_scaled_weights() {
        assert_eq!(compute_smoothed_value(200, 100, 10_000), 200);
        assert_eq!(compute_smoothed_value(200, 100, 5_000), 150);
        assert_eq!(compute_smoothed_value(300, 100, 1), 100);
    }
}
