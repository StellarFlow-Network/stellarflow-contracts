//! FE-198: Formal verification preparation — doc-tests for public functions.
//! Every public function must have an `# Example` block that passes `cargo test`.

/// Returns the sum of two unsigned 64-bit integers.
///
/// # Example
/// ```
/// assert_eq!(doc_tests::add(2, 3), 5);
/// ```
pub fn add(a: u64, b: u64) -> u64 {
    a + b
}

/// Checks whether a value is within an inclusive range.
///
/// # Example
/// ```
/// assert!(doc_tests::in_range(5, 1, 10));
/// assert!(!doc_tests::in_range(0, 1, 10));
/// ```
pub fn in_range(value: u64, min: u64, max: u64) -> bool {
    value >= min && value <= max
}

/// Saturating subtraction — returns 0 instead of underflowing.
///
/// # Example
/// ```
/// assert_eq!(doc_tests::saturating_sub(3, 5), 0);
/// assert_eq!(doc_tests::saturating_sub(10, 3), 7);
/// ```
pub fn saturating_sub(a: u64, b: u64) -> u64 {
    a.saturating_sub(b)
}