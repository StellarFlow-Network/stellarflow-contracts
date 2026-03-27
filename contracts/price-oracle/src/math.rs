pub fn normalize_to_seven(value: i128, input_decimals: u32) -> i128 {
    if input_decimals < 7 {
        let diff = 7 - input_decimals;
        let multiplier = 10_i128.checked_pow(diff).expect("Overflow on multiplier pow");
        value.checked_mul(multiplier).expect("Overflow on multiplication")
    } else if input_decimals > 7 {
        let diff = input_decimals - 7;
        let divisor = 10_i128.checked_pow(diff).expect("Overflow on divisor pow");
        value.checked_div(divisor).expect("Overflow on division")
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_to_seven_scale_up() {
        assert_eq!(normalize_to_seven(150, 2), 15_000_000);
    }

    #[test]
    fn test_normalize_to_seven_scale_down() {
        assert_eq!(normalize_to_seven(100_000_000, 9), 1_000_000);
    }

    #[test]
    fn test_normalize_to_seven_no_scale() {
        assert_eq!(normalize_to_seven(1234567, 7), 1234567);
    }
}
