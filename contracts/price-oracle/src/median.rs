use soroban_sdk::{contracterror, Vec};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum MedianError {
    EmptyInput = 10,
}

/// Sort a Vec<i128> using insertion sort (no_std compatible).
#[allow(dead_code)]
fn sort_prices(prices: &mut Vec<i128>) {
    let len = prices.len();
    for i in 1..len {
        let mut j = i;
        while j > 0 {
            let a = prices.get(j - 1).unwrap();
            let b = prices.get(j).unwrap();
            if a > b {
                prices.set(j - 1, b);
                prices.set(j, a);
                j -= 1;
            } else {
                break;
            }
        }
    }
}

/// Returns the median of the provided prices.
/// - 0 inputs  → Err(MedianError::EmptyInput)
/// - 1 input   → returns that value
/// - odd count → returns the middle value
/// - even count → returns the average of the two middle values
#[allow(dead_code)]
pub fn calculate_median(mut prices: Vec<i128>) -> Result<i128, MedianError> {
    let len = prices.len();
    if len == 0 {
        return Err(MedianError::EmptyInput);
    }
    sort_prices(&mut prices);
    let mid = len / 2;
    if len % 2 == 1 {
        Ok(prices.get(mid).unwrap())
    } else {
        let lo = prices.get(mid - 1).unwrap();
        let hi = prices.get(mid).unwrap();
        Ok((lo + hi) / 2)
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
}
