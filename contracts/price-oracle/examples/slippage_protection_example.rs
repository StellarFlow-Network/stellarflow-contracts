// Example: Cross-Currency Conversion with Slippage Protection
//
// This example demonstrates how to integrate the slippage protection framework
// into cross-currency conversion functions to prevent toxic arbitrage.

#![cfg(test)]

use soroban_sdk::{symbol_short, Env, Symbol};

// Import slippage protection functions
use crate::math::{
    calculate_max_acceptable_rate, calculate_min_acceptable_rate, calculate_rate_deviation_bps,
    enforce_slippage_tolerance,
};
use crate::Error;

/// Example 1: Simple Conversion with Slippage Protection
///
/// This example shows the basic pattern for protecting a single-hop conversion.
#[test]
fn example_simple_conversion_with_slippage() {
    // Setup
    let expected_rate = 1_500_000_000; // Expected: 1.5 units (9 decimals)
    let user_slippage_tolerance = 200; // User accepts 2% slippage

    // Pre-calculate acceptable bounds
    let min_acceptable = calculate_min_acceptable_rate(expected_rate, user_slippage_tolerance)
        .expect("Failed to calculate min rate");
    let max_acceptable = calculate_max_acceptable_rate(expected_rate, user_slippage_tolerance)
        .expect("Failed to calculate max rate");

    println!(
        "Acceptable rate range: {} to {}",
        min_acceptable, max_acceptable
    );

    // Scenario A: Rate within bounds (conversion succeeds)
    let actual_rate_good = 1_520_000_000; // 1.3% above expected

    let result =
        enforce_slippage_tolerance(expected_rate, actual_rate_good, user_slippage_tolerance);
    assert!(result.is_ok(), "Conversion should succeed within tolerance");

    // Scenario B: Rate exceeds bounds (conversion fails)
    let actual_rate_bad = 1_540_000_000; // 2.67% above expected (exceeds 2% tolerance)

    let result =
        enforce_slippage_tolerance(expected_rate, actual_rate_bad, user_slippage_tolerance);
    assert_eq!(
        result,
        Err(Error::SlippageToleranceExceeded),
        "Conversion should fail when tolerance exceeded"
    );
}

/// Example 2: Multi-Hop Conversion (NGN → XLM → GHS)
///
/// Demonstrates slippage protection across multiple conversion hops,
/// which is critical for corridors without direct liquidity.
#[test]
fn example_multi_hop_conversion() {
    let ngn_amount = 10_000_000_000; // 10 NGN (9 decimals)

    // Expected rates for each hop
    let expected_ngn_to_xlm = 50_000_000; // 1 NGN = 0.05 XLM
    let expected_xlm_to_ghs = 80_000_000; // 1 XLM = 0.08 GHS

    // Conservative slippage for each hop
    let slippage_per_hop = 100; // 1% per hop

    // --- First Hop: NGN → XLM ---
    let actual_ngn_to_xlm = 50_500_000; // Slightly higher (1% deviation)

    enforce_slippage_tolerance(expected_ngn_to_xlm, actual_ngn_to_xlm, slippage_per_hop)
        .expect("First hop should succeed");

    let xlm_amount = (ngn_amount * actual_ngn_to_xlm) / 1_000_000_000;

    // --- Second Hop: XLM → GHS ---
    let actual_xlm_to_ghs = 79_200_000; // Slightly lower (1% deviation)

    enforce_slippage_tolerance(expected_xlm_to_ghs, actual_xlm_to_ghs, slippage_per_hop)
        .expect("Second hop should succeed");

    let ghs_amount = (xlm_amount * actual_xlm_to_ghs) / 1_000_000_000;

    println!("Converted {} NGN to {} GHS via XLM", ngn_amount, ghs_amount);

    // Total effective rate: (50.5 * 79.2) / 10000 = 4 NGN per GHS
    let effective_rate = (ngn_amount * 1_000_000_000) / ghs_amount;
    println!("Effective NGN→GHS rate: {}", effective_rate);
}

/// Example 3: Dynamic Slippage Based on Market Conditions
///
/// Shows how to adjust slippage tolerance based on volatility indicators.
#[test]
fn example_dynamic_slippage() {
    let expected_rate = 1_000_000_000;

    // Market condition indicators
    let recent_volatility_bps = 150; // Recent price swings of 1.5%
    let liquidity_score = 80; // Scale 0-100 (80 = good liquidity)

    // Calculate dynamic slippage tolerance
    let base_slippage = 100; // Base 1%
    let volatility_adjustment = recent_volatility_bps / 2; // Add half of recent volatility
    let liquidity_adjustment = (100 - liquidity_score) / 10; // Add more for lower liquidity

    let dynamic_slippage = base_slippage + volatility_adjustment + liquidity_adjustment;

    println!("Dynamic slippage tolerance: {} bps", dynamic_slippage);
    // Result: 100 + 75 + 2 = 177 bps (1.77%)

    // Apply the dynamic tolerance
    let actual_rate = 1_015_000_000; // 1.5% above expected

    let result = enforce_slippage_tolerance(expected_rate, actual_rate, dynamic_slippage);
    assert!(
        result.is_ok(),
        "Should succeed with dynamically calculated tolerance"
    );
}

/// Example 4: Slippage Monitoring and Analytics
///
/// Demonstrates how to track and report slippage metrics.
#[test]
fn example_slippage_monitoring() {
    let conversions = vec![
        (1_000_000_000, 1_010_000_000), // 1% deviation
        (1_000_000_000, 1_005_000_000), // 0.5% deviation
        (1_000_000_000, 995_000_000),   // -0.5% deviation
        (1_000_000_000, 1_020_000_000), // 2% deviation
        (1_000_000_000, 980_000_000),   // -2% deviation
    ];

    let tolerance = 150; // 1.5% tolerance

    let mut successful_conversions = 0;
    let mut rejected_conversions = 0;
    let mut total_deviation = 0u32;

    for (expected, actual) in conversions.iter() {
        let deviation = calculate_rate_deviation_bps(*expected, *actual)
            .expect("Failed to calculate deviation");

        total_deviation += deviation;

        match enforce_slippage_tolerance(*expected, *actual, tolerance) {
            Ok(_) => successful_conversions += 1,
            Err(Error::SlippageToleranceExceeded) => rejected_conversions += 1,
            Err(e) => panic!("Unexpected error: {:?}", e),
        }
    }

    let total = conversions.len();
    let success_rate = (successful_conversions * 100) / total;
    let avg_deviation = total_deviation / total as u32;

    println!("Conversion Analytics:");
    println!("  Total conversions: {}", total);
    println!(
        "  Successful: {} ({}%)",
        successful_conversions, success_rate
    );
    println!("  Rejected: {}", rejected_conversions);
    println!("  Average deviation: {} bps", avg_deviation);

    // Expected: 3 successful (1%, 0.5%, -0.5%), 2 rejected (2%, -2%)
    assert_eq!(successful_conversions, 3);
    assert_eq!(rejected_conversions, 2);
}

/// Example 5: Integration with Price Oracle Contract
///
/// Demonstrates a complete conversion function integrated into a contract.
struct MockPriceOracle;

impl MockPriceOracle {
    /// Convert tokens with slippage protection.
    ///
    /// # Arguments
    /// * `from_asset` - Source asset symbol
    /// * `to_asset` - Target asset symbol  
    /// * `amount` - Amount to convert
    /// * `expected_rate` - Expected conversion rate (from oracle or aggregator)
    /// * `max_slippage_bps` - Maximum acceptable slippage in basis points
    ///
    /// # Returns
    /// Converted amount, or error if slippage exceeded
    fn convert_with_protection(
        from_asset: &str,
        to_asset: &str,
        amount: i128,
        expected_rate: i128,
        max_slippage_bps: u32,
    ) -> Result<i128, Error> {
        // Step 1: Get current prices (mocked here)
        let from_price = Self::mock_get_price(from_asset);
        let to_price = Self::mock_get_price(to_asset);

        // Step 2: Calculate actual conversion rate
        let scale = 1_000_000_000i128;
        let actual_rate = (from_price * scale) / to_price;

        // Step 3: Enforce slippage protection (CRITICAL)
        enforce_slippage_tolerance(expected_rate, actual_rate, max_slippage_bps)?;

        // Step 4: Execute conversion
        let output_amount = (amount * actual_rate) / scale;

        Ok(output_amount)
    }

    fn mock_get_price(asset: &str) -> i128 {
        match asset {
            "NGN" => 1_600_000_000,  // 1.6 (in 9 decimals)
            "XLM" => 32_000_000_000, // 32.0
            "GHS" => 400_000_000,    // 0.4
            _ => 1_000_000_000,
        }
    }
}

#[test]
fn example_oracle_integration() {
    // Convert 100 NGN to GHS
    let amount = 100_000_000_000; // 100 NGN
    let expected_rate = 4_000_000_000; // Expect 4:1 ratio
    let slippage = 200; // 2% tolerance

    // Execute conversion
    let result =
        MockPriceOracle::convert_with_protection("NGN", "GHS", amount, expected_rate, slippage);

    match result {
        Ok(ghs_amount) => {
            println!("Conversion successful: {} NGN → {} GHS", amount, ghs_amount);
            println!("Effective rate: {}", (amount * 1_000_000_000) / ghs_amount);
        }
        Err(Error::SlippageToleranceExceeded) => {
            println!("Conversion rejected: slippage tolerance exceeded");
        }
        Err(e) => {
            println!("Conversion failed: {:?}", e);
        }
    }

    assert!(result.is_ok(), "Conversion should succeed");
}

/// Example 6: Handling Edge Cases
#[test]
fn example_edge_cases() {
    // Edge case 1: Very tight slippage (0.1%)
    let tight_tolerance = 10;
    let expected = 1_000_000_000;
    let actual = 1_001_000_000; // 0.1% deviation

    let result = enforce_slippage_tolerance(expected, actual, tight_tolerance);
    assert!(result.is_ok(), "Exact boundary should pass");

    // Edge case 2: Zero slippage tolerance
    let zero_tolerance = 0;
    let result = enforce_slippage_tolerance(expected, expected, zero_tolerance);
    assert!(result.is_ok(), "Zero slippage with exact match should pass");

    let result = enforce_slippage_tolerance(expected, actual, zero_tolerance);
    assert_eq!(
        result,
        Err(Error::SlippageToleranceExceeded),
        "Zero slippage with any deviation should fail"
    );

    // Edge case 3: Maximum slippage (100%)
    let max_tolerance = 10_000;
    let wildly_different = 2_000_000_000; // 100% above expected

    let result = enforce_slippage_tolerance(expected, wildly_different, max_tolerance);
    assert!(
        result.is_ok(),
        "Maximum tolerance should accept large deviations"
    );

    // Edge case 4: Small values
    let small_expected = 100;
    let small_actual = 105; // 5% deviation
    let result = enforce_slippage_tolerance(small_expected, small_actual, 500);
    assert!(result.is_ok(), "Small values should work correctly");
}
