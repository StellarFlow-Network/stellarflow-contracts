//! Timelock policy for WASM replacements.

/// Minimum delay between registering a WASM hash and replacing the code.
pub const WASM_UPGRADE_DELAY_SECONDS: u64 = 48 * 60 * 60;

/// Return the earliest ledger timestamp at which an upgrade may execute.
pub fn execution_timestamp(proposed_at: u64) -> Option<u64> {
    proposed_at.checked_add(WASM_UPGRADE_DELAY_SECONDS)
}

/// Check whether the mandatory delay has elapsed.
pub fn is_ready(execute_at: u64, current_timestamp: u64) -> bool {
    current_timestamp >= execute_at
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_the_full_delay() {
        let execute_at = execution_timestamp(1_000).unwrap();

        assert!(!is_ready(execute_at, execute_at - 1));
        assert!(is_ready(execute_at, execute_at));
    }

    #[test]
    fn rejects_deadline_overflow() {
        assert_eq!(execution_timestamp(u64::MAX), None);
    }
}
