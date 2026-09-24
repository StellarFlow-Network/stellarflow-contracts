# Dynamic Slippage Protection - Quick Reference

## 🚀 Quick Start

### 1. Initialize Configuration (Admin)
```rust
use price_oracle::{PriceOracle, SlippageConfig};

let config = SlippageConfig {
    base_tolerance_bps: 50,         // 0.5% base slippage
    min_tolerance_bps: 10,          // 0.1% minimum
    max_tolerance_bps: 500,         // 5% maximum
    volatility_multiplier: 500,     // 5x multiplier
    liquidity_threshold: 5_000_000_000,
    ema_alpha_bps: 2000,            // 20% smoothing
};

PriceOracle::set_slippage_config(env, admin, config)?;
```

### 2. Execute a Swap
```rust
// Simplest form: let oracle handle everything
let output = PriceOracle::execute_swap_with_dynamic_slippage(
    env,
    symbol_short!("NGN"),    // From
    symbol_short!("KES"),    // To
    1_000_000_000,           // Amount
    0,                       // No manual minimum (use dynamic)
    10_000_000_000,          // Liquidity
)?;
```

## 📊 How It Works

### Dynamic Calculation
```
Step 1: Get volatility for both assets
    from_volatility = 300 bps (3%)
    to_volatility = 200 bps (2%)
    max_volatility = 300 bps (higher of the two)

Step 2: Calculate volatility-adjusted tolerance
    volatility_adjusted = 50 * (10_000 + 300 * 500) / 10_000
                        = 50 * 11_500 / 10_000
                        = 57.5 bps

Step 3: Add liquidity penalty if needed
    if liquidity < threshold:
        deficit = (threshold - liquidity) / threshold
        penalty = deficit * 20 bps per 10%
        total = volatility_adjusted + penalty
    else:
        total = volatility_adjusted

Step 4: Clamp to bounds
    final = clamp(total, min_tolerance, max_tolerance)
```

### Example Calculation
```
Config:
- base_tolerance: 50 bps (0.5%)
- volatility_multiplier: 500 (5x)
- liquidity_threshold: 5_000_000_000

Scenario:
- from_volatility: 300 bps (3%)
- to_volatility: 200 bps (2%)
- liquidity: 4_000_000_000 (80% of threshold)

Calculation:
1. max_volatility = 300 bps
2. volatility_adjusted = 50 * (10_000 + 300 * 500) / 10_000 = 57.5 bps
3. liquidity_deficit = 20% below threshold
   liquidity_penalty = 20% * 20 bps / 10% = 40 bps
4. total = 57.5 + 40 = 97.5 bps
5. clamped = clamp(97.5, 10, 500) = 97.5 bps

Result: Dynamic slippage = 97.5 bps (0.975%)
```

## 🎯 Common Use Cases

### Use Case 1: DEX Swap (Fully Dynamic)
```rust
// Best for: Normal trading, trust oracle to adapt
let output = PriceOracle::execute_swap_with_dynamic_slippage(
    env, from, to, amount, 
    0,        // Trust dynamic calculation
    liquidity
)?;
```

### Use Case 2: User-Facing App (Hybrid)
```rust
// Best for: Give users control but add protection
let user_min = calculate_from_user_slippage_preference();

let output = PriceOracle::execute_swap_with_dynamic_slippage(
    env, from, to, amount,
    user_min, // Use stricter of user vs dynamic
    liquidity
)?;
```

### Use Case 3: Liquidation (Manual)
```rust
// Best for: Predictable slippage for liquidators
let output = PriceOracle::execute_swap_with_manual_slippage(
    env, from, to, amount,
    500       // Fixed 5% slippage
)?;
```

### Use Case 4: Check Before Execute
```rust
// Best for: Show users expected slippage before confirming
let slippage = PriceOracle::calculate_dynamic_slippage(
    env.clone(), from.clone(), to.clone(), liquidity
)?;

if slippage > user_max_acceptable {
    return Err("Slippage too high, wait for better conditions");
}

let output = PriceOracle::execute_swap_with_dynamic_slippage(
    env, from, to, amount, 0, liquidity
)?;
```

## 🔧 Configuration Presets

### Conservative (Safety First)
```rust
SlippageConfig {
    base_tolerance_bps: 25,        // 0.25%
    min_tolerance_bps: 10,         // 0.1%
    max_tolerance_bps: 300,        // 3%
    volatility_multiplier: 300,    // 3x
    liquidity_threshold: 10_000_000_000,
    ema_alpha_bps: 2000,           // 20%
}
// When to use: Stable pairs, low-risk applications, regulated environments
// Behavior: Tight slippage, higher rejection rate
```

### Balanced (Recommended)
```rust
SlippageConfig {
    base_tolerance_bps: 50,        // 0.5%
    min_tolerance_bps: 10,         // 0.1%
    max_tolerance_bps: 500,        // 5%
    volatility_multiplier: 500,    // 5x
    liquidity_threshold: 5_000_000_000,
    ema_alpha_bps: 2000,           // 20%
}
// When to use: General purpose, most applications
// Behavior: Good balance between protection and execution rate
```

### Aggressive (Execution Priority)
```rust
SlippageConfig {
    base_tolerance_bps: 100,       // 1%
    min_tolerance_bps: 20,         // 0.2%
    max_tolerance_bps: 1000,       // 10%
    volatility_multiplier: 800,    // 8x
    liquidity_threshold: 2_000_000_000,
    ema_alpha_bps: 3000,           // 30%
}
// When to use: Volatile pairs, high-volume applications, market making
// Behavior: Looser slippage, lower rejection rate
```

## 📈 Monitoring Cheat Sheet

### Key Metrics
| Metric | Healthy Range | Action If Outside |
|--------|---------------|-------------------|
| Rejection Rate | < 5% | Increase max_tolerance |
| Avg Dynamic Slippage | 30-150 bps | Adjust base_tolerance |
| Volatility Spike Frequency | < 1/hour | Review volatility_multiplier |
| At Max Tolerance % | < 10% | Increase max_tolerance |
| At Min Tolerance % | < 90% | Decrease base_tolerance |

### Event Filters
```rust
// Rejected swaps
topic: (symbol_short!("swap"), symbol_short!("rejected"))

// Successful swaps
topic: (symbol_short!("swap"), symbol_short!("executed"))

// Volatility updates
topic: (symbol_short!("volatility"), symbol_short!("updated"))
```

### Quick Diagnostics
```rust
// Check current volatility
let vol = PriceOracle::get_asset_volatility_bps(env, asset);
println!("Current volatility: {} bps", vol);

// Check what slippage would be
let slippage = PriceOracle::calculate_dynamic_slippage(
    env, from, to, liquidity
)?;
println!("Dynamic slippage: {} bps", slippage);

// Check configuration
let config = PriceOracle::get_slippage_config(env);
println!("Config: {:?}", config);
```

## ⚠️ Common Issues & Solutions

### Issue: High Rejection Rate
**Symptoms**: > 10% of swaps rejected
**Cause**: Slippage tolerance too tight for market conditions
**Solution**:
```rust
// Increase max_tolerance_bps
config.max_tolerance_bps = 800; // From 500 to 800 (5% to 8%)
```

### Issue: Dynamic Slippage Always at Maximum
**Symptoms**: Most swaps use max_tolerance
**Cause**: Volatility multiplier too high or max too low
**Solution**:
```rust
// Option 1: Increase max
config.max_tolerance_bps = 800;

// Option 2: Decrease multiplier
config.volatility_multiplier = 300; // From 500 to 300
```

### Issue: Slippage Too Loose
**Symptoms**: Users complaining about poor execution prices
**Cause**: Base tolerance or multiplier too high
**Solution**:
```rust
// Tighten base tolerance
config.base_tolerance_bps = 25; // From 50 to 25

// Reduce volatility response
config.volatility_multiplier = 300; // From 500 to 300
```

### Issue: Volatility Not Tracking Properly
**Symptoms**: Volatility metrics seem stale
**Cause**: EMA alpha too low (not responsive enough)
**Solution**:
```rust
// Increase responsiveness
config.ema_alpha_bps = 3000; // From 2000 to 3000 (20% to 30%)
```

### Issue: Too Sensitive to Price Spikes
**Symptoms**: Slippage jumps dramatically on single price move
**Cause**: EMA alpha too high (too responsive)
**Solution**:
```rust
// Decrease responsiveness (more smoothing)
config.ema_alpha_bps = 1500; // From 2000 to 1500 (20% to 15%)
```

## 🧪 Testing Checklist

### Before Production
- [ ] Test with conservative config
- [ ] Test with balanced config
- [ ] Test with aggressive config
- [ ] Simulate high volatility (rapid price changes)
- [ ] Simulate low liquidity scenarios
- [ ] Test rejection handling in your app
- [ ] Monitor events for 24 hours on testnet
- [ ] Verify gas costs are acceptable
- [ ] Test manual override behavior
- [ ] Test all three execution modes

### Integration Testing
```rust
#[test]
fn test_my_integration() {
    let env = Env::default();
    
    // 1. Configure
    PriceOracle::set_slippage_config(env.clone(), admin, config)?;
    
    // 2. Execute test swap
    let result = PriceOracle::execute_swap_with_dynamic_slippage(
        env.clone(), from, to, amount, 0, liquidity
    );
    
    // 3. Verify behavior
    assert!(result.is_ok());
    
    // 4. Check events
    let events = env.events().all();
    assert!(events.iter().any(|e| matches_execution_event(e)));
}
```

## 📞 API Quick Reference

### Configuration
```rust
set_slippage_config(env, admin, config) -> Result<(), Error>
get_slippage_config(env) -> SlippageConfig
```

### Volatility
```rust
update_volatility_metrics(env, asset, price) -> Result<(), Error>
get_volatility_metrics(env, asset) -> Option<VolatilityMetrics>
get_asset_volatility_bps(env, asset) -> u32
```

### Calculation
```rust
calculate_dynamic_slippage(env, from, to, liquidity) -> Result<u32, Error>
```

### Execution
```rust
execute_swap_with_dynamic_slippage(
    env, from, to, amount, manual_min, liquidity
) -> Result<i128, Error>

execute_swap_with_manual_slippage(
    env, from, to, amount, slippage_bps
) -> Result<i128, Error>
```

## 🎓 Parameter Tuning Guide

### `base_tolerance_bps` (Baseline Slippage)
- **Low (10-25)**: Tight execution, higher rejection rate
- **Medium (25-75)**: Balanced approach
- **High (75-150)**: Looser execution, lower rejection rate
- **Adjust**: Based on desired rejection rate

### `volatility_multiplier` (Volatility Sensitivity)
- **Low (100-300)**: Less sensitive to volatility
- **Medium (300-700)**: Balanced sensitivity
- **High (700-1000)**: Very responsive to volatility
- **Adjust**: Based on market characteristics

### `ema_alpha_bps` (Volatility Smoothing)
- **Low (1000-1500)**: More smoothing, slower response
- **Medium (1500-2500)**: Balanced smoothing
- **High (2500-5000)**: Less smoothing, faster response
- **Adjust**: Based on price update frequency

### `liquidity_threshold` (Liquidity Penalty Trigger)
- **Low (1-5B)**: Penalty applies more often
- **Medium (5-10B)**: Balanced trigger
- **High (10B+)**: Penalty applies rarely
- **Adjust**: Based on typical liquidity levels

## 💡 Pro Tips

1. **Start Conservative**: Begin with tight tolerances and loosen based on observed rejection rate

2. **Monitor First Week**: Track all metrics closely during initial deployment

3. **Seasonal Adjustments**: Consider adjusting for known high-volatility periods

4. **A/B Testing**: Test different configs on different asset pairs

5. **Event-Driven Tuning**: Automate config adjustments based on rejection rates

6. **User Education**: Show users the calculated slippage before execution

7. **Fallback Strategy**: Have manual mode as fallback during extreme conditions

8. **Gradual Changes**: When tuning, make small incremental changes

---

**For Complete Documentation**: See `DYNAMIC_SLIPPAGE_PROTECTION.md`  
**For Examples**: See `examples/dynamic_slippage_example.rs`  
**For Implementation**: See `src/slippage.rs`
