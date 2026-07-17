use soroban_sdk::{contracterror, contracttype, env, Error};

// ==========================================
// --- 1. CORE IMPLEMENTATION CODE ---
// ==========================================

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[into_val]
pub enum ContractError {
    // Technical Requirement: Return a clear, custom exception error code early
    InvalidDivisionFactor = 536,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpreadResult {
    pub raw_spread: u64,
    pub averaged_spread: u64,
}

pub struct MultiHopSpreadCalculator;

impl MultiHopSpreadCalculator {
    /// Evaluates asset spreads across multiple corridor hops safely.
    /// Proactively guards against division-by-zero panics when zero-activity anomalies occur.
    pub fn calculate_spread(
        total_corridor_spread: u64,
        active_hops_count: u64,
    ) -> Result<SpreadResult, ContractError> {
        // Proactive non-zero validation check
        if active_hops_count == 0 {
            return Err(ContractError::InvalidDivisionFactor);
        }

        let raw_spread = total_corridor_spread;
        let averaged_spread = total_corridor_spread / active_hops_count;

        Ok(SpreadResult {
            raw_spread,
            averaged_spread,
        })
    }
}

// ==========================================
// --- 2. TDD AUTOMATED TEST SUITE ---
// ==========================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_spread_success_with_valid_hops() {
        // Arrange
        let total_spread = 120;
        let hops = 3;

        // Act
        let result = MultiHopSpreadCalculator::calculate_spread(total_spread, hops);

        // Assert
        assert!(result.is_ok());
        let spread_data = result.unwrap();
        assert_eq!(spread_data.raw_spread, 120);
        assert_eq!(spread_data.averaged_spread, 40); // 120 / 3
    }

    #[test]
    fn test_calculate_spread_rejects_zero_hops_proactively() {
        // Arrange
        let total_spread = 500;
        let zero_hops = 0;

        // Act
        let result = MultiHopSpreadCalculator::calculate_spread(total_spread, zero_hops);

        // Assert: Confirm the panic gate blocks execution and bubbles up the custom error enum
        assert!(result.is_err());
        assert_eq!(result.err().unwrap(), ContractError::InvalidDivisionFactor);
    }
}