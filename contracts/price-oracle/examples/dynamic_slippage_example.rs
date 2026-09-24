//! Dynamic Slippage Protection - Usage Examples
//!
//! This file demonstrates various usage patterns for the dynamic slippage protection
//! feature in the StellarFlow Price Oracle.

#![cfg(test)]

use soroban_sdk::{Env, symbol_short, Address};

// These examples assume the oracle contract is properly initialized

/// Example 1: Basic swap with fully dynamic slippage
///
/// The oracle automatically calculates slippage based on current volatility
/// and liquidity conditions. No manual minimum required.
#[test]
fn example_fully_dynamic_swap() {
    let env = Env::default();
    
    // Initialize oracle contract (implementation details omitted)
    let oracle_address = Address::generate(&env);
    
    // Perform a swap letting the oracle calculate optimal slippage
    let result = env.invoke_contract::<i128>(
        &oracle_address,
        &symbol_short!("swap_dyn"),
        (
            symbol_short!("NGN"),        // from_asset
            symbol_short!("KES"),        // to_asset
            1_000_000_000_i128,          // amount_in (1.0 NGN)
            0_i128,                      // manual_min_out (0 = use dynamic)
            10_000_000_000_i128,         // liquidity
        ),
    );
    
    // Result is the actual output amount or an error if slippage exceeded
    println!("Swap output: {:?}", result);
}

/// Example 2: Dynamic slippage with manual override (safety net)
///
/// Calculate your own worst-case minimum, but let the dynamic calculation
/// provide additional protection. The stricter of the two will be used.
#[test]
fn example_dynamic_with_manual_override() {
    let env = Env::default();
    let oracle_address = Address::generate(&env);
    
    // Your application calculates a minimum based on your logic
    let my_calculated_minimum = 980_000_000_i128; // Accept down to 0.98 output
    
    // But let dynamic slippage add protection if market is more volatile
    let result = env.invoke_contract::<i128>(
        &oracle_address,
        &symbol_short!("swap_dyn"),
        (
            symbol_short!("GHS"),
            symbol_short!("XLM"),
            5_000_000_000_i128,
            my_calculated_minimum,        // Your manual minimum
            8_000_000_000_i128,          // Current liquidity
        ),
    );
    
    // The oracle will use whichever is stricter: your minimum or dynamic minimum
    println!("Swap output with override: {:?}", result);
}

/// Example 3: Manual-only slippage (bypass dynamic calculation)
///
/// For advanced users or protocols that calculate slippage externally.
#[test]
fn example_manual_only_slippage() {
    let env = Env::default();
    let oracle_address = Address::generate(&env);
    
    // You specify exact slippage tolerance in basis points
    let my_slippage_tolerance = 250_u32; // 2.5%
    
    let result = env.invoke_contract::<i128>(
        &oracle_address,
        &symbol_short!("swap_man"),
        (
            symbol_short!("KES"),
            symbol_short!("NGN"),
            10_000_000_000_i128,
            my_slippage_tolerance,
        ),
    );
    
    println!("Manual slippage swap: {:?}", result);
}

/// Example 4: Multi-hop conversion with dynamic protection on each hop
///
/// When converting through multiple assets, apply dynamic slippage at each step.
#[test]
fn example_multi_hop_dynamic() {
    let env = Env::default();
    let oracle_address = Address::generate(&env);
    
    // Hop 1: NGN → XLM
    let xlm_amount = env.invoke_contract::<Result<i128, _>>(
        &oracle_address,
        &symbol_short!("swap_dyn"),
        (
            symbol_short!("NGN"),
            symbol_short!("XLM"),
            1_000_000_000_i128,
            0_i128,
            15_000_000_000_i128,
        ),
    ).unwrap();
    
    // Hop 2: XLM → GHS (using output from hop 1)
    let ghs_amount = env.invoke_contract::<Result<i128, _>>(
        &oracle_address,
        &symbol_short!("swap_dyn"),
        (
            symbol_short!("XLM"),
            symbol_short!("GHS"),
            xlm_amount,
            0_i128,
            12_000_000_000_i128,
        ),
    ).unwrap();
    
    println!("Multi-hop result: {} GHS", ghs_amount);
}

/// Example 5: Query volatility metrics before executing swap
///
/// Check current market volatility to make informed decisions.
#[test]
fn example_check_volatility_before_swap() {
    let env = Env::default();
    let oracle_address = Address::generate(&env);
    
    // Query current volatility for the asset pair
    let ngn_volatility = env.invoke_contract::<u32>(
        &oracle_address,
        &symbol_short!("vol_bps"),
        (symbol_short!("NGN"),),
    );
    
    let kes_volatility = env.invoke_contract::<u32>(
        &oracle_address,
        &symbol_short!("vol_bps"),
        (symbol_short!("KES"),),
    );
    
    println!("NGN volatility: {} bps", ngn_volatility);
    println!("KES volatility: {} bps", kes_volatility);
    
    // Calculate what slippage would be with current conditions
    let dynamic_slippage = env.invoke_contract::<Result<u32, _>>(
        &oracle_address,
        &symbol_short!("calc_slip"),
        (
            symbol_short!("NGN"),
            symbol_short!("KES"),
            10_000_000_000_i128, // liquidity
        ),
    ).unwrap();
    
    println!("Dynamic slippage would be: {} bps", dynamic_slippage);
    
    // Decide whether to proceed based on calculated slippage
    if dynamic_slippage < 100 {
        // Low slippage, safe to proceed
        let _result = env.invoke_contract::<Result<i128, _>>(
            &oracle_address,
            &symbol_short!("swap_dyn"),
            (
                symbol_short!("NGN"),
                symbol_short!("KES"),
                1_000_000_000_i128,
                0_i128,
                10_000_000_000_i128,
            ),
        );
    } else {
        println!("Slippage too high, waiting for better conditions");
    }
}

/// Example 6: Configure slippage parameters (admin only)
///
/// Set up the dynamic slippage configuration for the oracle.
#[test]
fn example_configure_slippage() {
    let env = Env::default();
    let oracle_address = Address::generate(&env);
    let admin = Address::generate(&env);
    
    // Define slippage configuration (this would use actual SlippageConfig struct)
    // SlippageConfig {
    //     base_tolerance_bps: 50,        // 0.5% base
    //     min_tolerance_bps: 10,         // 0.1% minimum
    //     max_tolerance_bps: 500,        // 5% maximum
    //     volatility_multiplier: 500,    // 5x multiplier
    //     liquidity_threshold: 5_000_000_000,
    //     ema_alpha_bps: 2000,           // 20% smoothing
    // }
    
    // Set configuration (implementation details omitted)
    // let result = env.invoke_contract::<Result<(), _>>(
    //     &oracle_address,
    //     &symbol_short!("set_slip"),
    //     (admin, config),
    // );
    
    println!("Slippage configuration updated");
}

/// Example 7: Handling slippage rejection gracefully
///
/// Implement proper error handling when swaps are rejected due to slippage.
#[test]
fn example_handle_rejection() {
    let env = Env::default();
    let oracle_address = Address::generate(&env);
    
    let result = env.invoke_contract::<Result<i128, String>>(
        &oracle_address,
        &symbol_short!("swap_dyn"),
        (
            symbol_short!("NGN"),
            symbol_short!("KES"),
            1_000_000_000_i128,
            990_000_000_i128,  // Very strict minimum
            10_000_000_000_i128,
        ),
    );
    
    match result {
        Ok(output) => {
            println!("Swap succeeded: {} output", output);
        }
        Err(e) if e.contains("SlippageToleranceExceeded") => {
            println!("Swap rejected due to slippage");
            // Retry with more lenient parameters or wait for better conditions
        }
        Err(e) => {
            println!("Swap failed with error: {}", e);
        }
    }
}

/// Example 8: Monitor swap execution events
///
/// Listen for events to track swap success/rejection rates and volatility updates.
#[test]
fn example_monitor_events() {
    let env = Env::default();
    
    // After executing swaps, check events
    let events = env.events().all();
    
    for event in events {
        // Filter for swap-related events
        // Event topics would be (symbol_short!("swap"), symbol_short!("executed"))
        // or (symbol_short!("swap"), symbol_short!("rejected"))
        
        println!("Event: {:?}", event);
        
        // Parse SwapExecutionEvent or SlippageRejectionEvent from event data
        // to track metrics like:
        // - Rejection rate
        // - Average slippage applied
        // - Volatility trends
    }
}

/// Example 9: Low liquidity scenario
///
/// Demonstrate how dynamic slippage adjusts for low liquidity conditions.
#[test]
fn example_low_liquidity() {
    let env = Env::default();
    let oracle_address = Address::generate(&env);
    
    // Very low liquidity for this pair
    let low_liquidity = 1_000_000_000_i128; // Only 1 unit available
    
    // Calculate dynamic slippage with low liquidity
    let dynamic_slippage = env.invoke_contract::<Result<u32, _>>(
        &oracle_address,
        &symbol_short!("calc_slip"),
        (
            symbol_short!("GHS"),
            symbol_short!("NGN"),
            low_liquidity,
        ),
    ).unwrap();
    
    // Slippage will be higher due to liquidity penalty
    println!("Slippage with low liquidity: {} bps", dynamic_slippage);
    
    // Execute swap with awareness of higher slippage
    let result = env.invoke_contract::<Result<i128, _>>(
        &oracle_address,
        &symbol_short!("swap_dyn"),
        (
            symbol_short!("GHS"),
            symbol_short!("NGN"),
            500_000_000_i128,  // Smaller amount
            0_i128,
            low_liquidity,
        ),
    );
    
    println!("Low liquidity swap result: {:?}", result);
}

/// Example 10: High volatility scenario
///
/// Show how dynamic slippage adapts during volatile market conditions.
#[test]
fn example_high_volatility() {
    let env = Env::default();
    let oracle_address = Address::generate(&env);
    
    // In a real scenario, volatility would be tracked automatically by the oracle
    // as prices are updated. Here we demonstrate the behavior:
    
    // Assume NGN has experienced high volatility (tracked via EMA)
    let ngn_volatility = 800_u32; // 8% recent volatility
    
    println!("Current NGN volatility: {} bps", ngn_volatility);
    
    // Dynamic slippage will automatically adjust higher
    let dynamic_slippage = env.invoke_contract::<Result<u32, _>>(
        &oracle_address,
        &symbol_short!("calc_slip"),
        (
            symbol_short!("NGN"),
            symbol_short!("XLM"),
            10_000_000_000_i128,
        ),
    ).unwrap();
    
    println!("Adjusted slippage for high volatility: {} bps", dynamic_slippage);
    
    // Execute swap with adjusted tolerance
    let result = env.invoke_contract::<Result<i128, _>>(
        &oracle_address,
        &symbol_short!("swap_dyn"),
        (
            symbol_short!("NGN"),
            symbol_short!("XLM"),
            1_000_000_000_i128,
            0_i128,
            10_000_000_000_i128,
        ),
    );
    
    println!("High volatility swap result: {:?}", result);
}

// ============================================================================
// Integration Patterns
// ============================================================================

/// Pattern 1: DEX Integration
///
/// How a decentralized exchange would integrate dynamic slippage protection.
mod dex_integration {
    use super::*;
    
    struct MockDex {
        oracle_address: Address,
    }
    
    impl MockDex {
        fn execute_trade(
            &self,
            env: &Env,
            from_asset: &str,
            to_asset: &str,
            amount: i128,
            user_slippage_pct: u32,
        ) -> Result<i128, String> {
            // 1. Get current liquidity from pool
            let liquidity = self.get_pool_liquidity(from_asset, to_asset);
            
            // 2. Convert user's slippage percentage to minimum output
            let expected_output = self.calculate_expected_output(amount);
            let user_min_output = expected_output * (10_000 - user_slippage_pct) as i128 / 10_000;
            
            // 3. Execute with dynamic slippage (using stricter of dynamic vs user preference)
            let result = env.invoke_contract::<Result<i128, _>>(
                &self.oracle_address,
                &symbol_short!("swap_dyn"),
                (
                    symbol_short!(from_asset),
                    symbol_short!(to_asset),
                    amount,
                    user_min_output,
                    liquidity,
                ),
            );
            
            result.map_err(|e| format!("Trade failed: {:?}", e))
        }
        
        fn get_pool_liquidity(&self, _from: &str, _to: &str) -> i128 {
            // Implementation would query liquidity pool
            10_000_000_000
        }
        
        fn calculate_expected_output(&self, _amount: i128) -> i128 {
            // Implementation would use AMM formula
            1_000_000_000
        }
    }
}

/// Pattern 2: Lending Protocol Integration
///
/// How a lending protocol uses dynamic slippage for liquidations.
mod lending_integration {
    use super::*;
    
    struct MockLendingProtocol {
        oracle_address: Address,
    }
    
    impl MockLendingProtocol {
        fn liquidate_position(
            &self,
            env: &Env,
            collateral_asset: &str,
            debt_asset: &str,
            collateral_amount: i128,
        ) -> Result<i128, String> {
            // During liquidations, we want conservative slippage
            // to ensure liquidators are properly compensated
            
            // Check current market volatility
            let volatility = env.invoke_contract::<u32>(
                &self.oracle_address,
                &symbol_short!("vol_bps"),
                (symbol_short!(collateral_asset),),
            );
            
            println!("Collateral volatility: {} bps", volatility);
            
            // If volatility is high, use manual slippage for predictability
            if volatility > 500 {
                // Use fixed 5% slippage during high volatility
                env.invoke_contract::<Result<i128, _>>(
                    &self.oracle_address,
                    &symbol_short!("swap_man"),
                    (
                        symbol_short!(collateral_asset),
                        symbol_short!(debt_asset),
                        collateral_amount,
                        500_u32, // 5% fixed
                    ),
                ).map_err(|e| format!("Liquidation failed: {:?}", e))
            } else {
                // Use dynamic slippage for normal conditions
                let liquidity = 10_000_000_000_i128;
                env.invoke_contract::<Result<i128, _>>(
                    &self.oracle_address,
                    &symbol_short!("swap_dyn"),
                    (
                        symbol_short!(collateral_asset),
                        symbol_short!(debt_asset),
                        collateral_amount,
                        0_i128,
                        liquidity,
                    ),
                ).map_err(|e| format!("Liquidation failed: {:?}", e))
            }
        }
    }
}

/// Pattern 3: Payment Processor Integration
///
/// How a payment processor uses dynamic slippage for currency conversions.
mod payment_integration {
    use super::*;
    
    struct MockPaymentProcessor {
        oracle_address: Address,
    }
    
    impl MockPaymentProcessor {
        fn process_cross_border_payment(
            &self,
            env: &Env,
            from_currency: &str,
            to_currency: &str,
            amount: i128,
            max_acceptable_slippage_bps: u32,
        ) -> Result<i128, String> {
            // For payments, we want tight slippage to give users predictability
            
            // Calculate what dynamic slippage would be
            let dynamic_slippage = env.invoke_contract::<Result<u32, _>>(
                &self.oracle_address,
                &symbol_short!("calc_slip"),
                (
                    symbol_short!(from_currency),
                    symbol_short!(to_currency),
                    20_000_000_000_i128, // Assume good liquidity
                ),
            ).unwrap();
            
            // If dynamic slippage exceeds user's maximum, reject the payment
            if dynamic_slippage > max_acceptable_slippage_bps {
                return Err(format!(
                    "Current market conditions require {}bps slippage, exceeds your maximum of {}bps",
                    dynamic_slippage, max_acceptable_slippage_bps
                ));
            }
            
            // Execute payment with user's maximum as a safety net
            let expected_output = self.estimate_output(amount);
            let user_min_output = expected_output * 
                (10_000 - max_acceptable_slippage_bps) as i128 / 10_000;
            
            env.invoke_contract::<Result<i128, _>>(
                &self.oracle_address,
                &symbol_short!("swap_dyn"),
                (
                    symbol_short!(from_currency),
                    symbol_short!(to_currency),
                    amount,
                    user_min_output,
                    20_000_000_000_i128,
                ),
            ).map_err(|e| format!("Payment failed: {:?}", e))
        }
        
        fn estimate_output(&self, _amount: i128) -> i128 {
            // Implementation would query oracle for current rate
            1_000_000_000
        }
    }
}
