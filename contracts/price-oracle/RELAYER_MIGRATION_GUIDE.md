# Relayer Migration Guide - Liquidity Validation Update

## What Changed?

The `update_price` function now requires an additional `liquidity` parameter to prevent flash loan manipulation attacks. This is a **breaking change** that requires all relayers to update their integration.

## Updated Function Signature

### Before
```rust
pub fn update_price(
    env: Env,
    source: Address,
    asset: Symbol,
    price: i128,
    decimals: u32,
    confidence_score: u32,
    ttl: u64
) -> Result<(), ContractError>
```

### After
```rust
pub fn update_price(
    env: Env,
    source: Address,
    asset: Symbol,
    price: i128,
    decimals: u32,
    confidence_score: u32,
    ttl: u64,
    liquidity: i128  // NEW PARAMETER
) -> Result<(), ContractError>
```

## What is Liquidity?

The `liquidity` parameter should represent the **total depth of the market** you're sourcing the price from, measured in stroops (1 XLM = 10,000,000 stroops).

### For DEX Sources
Total liquidity = reserves in both sides of the pool

**Example (Soroban Constant Product Pool):**
```rust
// XLM/USD pool with reserves
let xlm_reserve = pool.get_reserve_a(); // 1,000,000 XLM
let usd_reserve = pool.get_reserve_b(); // 150,000 USD

// Calculate total liquidity in base asset (XLM) stroops
let liquidity_xlm = xlm_reserve * 10_000_000; // Convert to stroops
let liquidity = liquidity_xlm; // 10,000,000,000,000 stroops

// Submit to oracle
oracle.update_price(
    &env,
    &relayer_addr,
    &Symbol::new(&env, "XLM_USD"),
    &price,
    &9,
    &95,
    &300,
    &liquidity // Include liquidity
)?;
```

### For Order Book Sources
Total liquidity = sum of orders within X% of mid-price

**Example (Order Book with 1% depth):**
```rust
let mid_price = (best_bid + best_ask) / 2;
let lower_bound = mid_price * 99 / 100;  // 1% below
let upper_bound = mid_price * 101 / 100; // 1% above

let mut liquidity = 0_i128;

// Sum bids within range
for bid in bids.iter() {
    if bid.price >= lower_bound && bid.price <= mid_price {
        liquidity += bid.amount * 10_000_000; // Convert to stroops
    }
}

// Sum asks within range
for ask in asks.iter() {
    if ask.price <= upper_bound && ask.price >= mid_price {
        liquidity += ask.amount * 10_000_000; // Convert to stroops
    }
}

// Submit to oracle
oracle.update_price(
    &env,
    &relayer_addr,
    &Symbol::new(&env, "XLM_USD"),
    &price,
    &9,
    &95,
    &300,
    &liquidity
)?;
```

### For Aggregated Sources
Total liquidity = sum across all sources (weighted or simple)

**Example (Multiple DEXes):**
```rust
let stellar_dex_liquidity = get_stellar_dex_liquidity(&asset);
let phoenix_liquidity = get_phoenix_liquidity(&asset);
let soroswap_liquidity = get_soroswap_liquidity(&asset);

let total_liquidity = stellar_dex_liquidity 
    + phoenix_liquidity 
    + soroswap_liquidity;

oracle.update_price(
    &env,
    &relayer_addr,
    &asset,
    &aggregated_price,
    &9,
    &95,
    &300,
    &total_liquidity
)?;
```

## Migration Steps

### Step 1: Update Your SDK/Client

If using the auto-generated Soroban client:

```rust
// Regenerate client from updated WASM
soroban contract bindings rust \
    --wasm target/wasm32-unknown-unknown/release/price_oracle.wasm \
    --output-dir ./client
```

### Step 2: Add Liquidity Calculation

Add a function to calculate liquidity from your data source:

```rust
fn calculate_liquidity(env: &Env, asset: &Symbol) -> Result<i128, Error> {
    // Your implementation here
    // Should return total market depth in stroops
    
    // Example for Stellar DEX
    let xlm_reserve = get_xlm_reserve(asset)?;
    let quote_reserve = get_quote_reserve(asset)?;
    
    // Use base asset reserve as liquidity measure
    Ok(xlm_reserve * 10_000_000)
}
```

### Step 3: Update Your Price Submission Logic

```rust
// OLD CODE
fn submit_price(env: &Env, relayer: &Address, asset: &Symbol) -> Result<(), Error> {
    let price_data = fetch_price_data(asset)?;
    
    oracle.update_price(
        env,
        relayer,
        asset,
        &price_data.price,
        &9,
        &price_data.confidence,
        &300
    )?;
    
    Ok(())
}

// NEW CODE
fn submit_price(env: &Env, relayer: &Address, asset: &Symbol) -> Result<(), Error> {
    let price_data = fetch_price_data(asset)?;
    let liquidity = calculate_liquidity(env, asset)?; // NEW
    
    oracle.update_price(
        env,
        relayer,
        asset,
        &price_data.price,
        &9,
        &price_data.confidence,
        &300,
        &liquidity // NEW PARAMETER
    )?;
    
    Ok(())
}
```

### Step 4: Test Against Staging

Before deploying to production:

```bash
# 1. Deploy to testnet
soroban contract deploy \
    --wasm target/wasm32-unknown-unknown/release/price_oracle.wasm \
    --network testnet

# 2. Test with real liquidity data
soroban contract invoke \
    --id <CONTRACT_ID> \
    --network testnet \
    -- update_price \
    --source <YOUR_ADDRESS> \
    --asset XLM_USD \
    --price 15000000000 \
    --decimals 9 \
    --confidence-score 95 \
    --ttl 300 \
    --liquidity 10000000000000  # NEW: Test with realistic value
```

### Step 5: Deploy to Production

Once testing passes:

```rust
// Update production relayer
// Ensure liquidity calculation is accurate
// Monitor for rejection errors
```

## Error Handling

### New Error: LiquidityBelowThreshold

Your submission may be rejected if liquidity is too low:

```rust
match oracle.update_price(env, source, asset, price, decimals, confidence, ttl, liquidity) {
    Ok(()) => {
        log!("Price submitted successfully");
    }
    Err(ContractError::LiquidityBelowThreshold) => {
        // Liquidity too low - improve data source or wait for better market conditions
        log!("ERROR: Liquidity {} below threshold", liquidity);
        // Consider switching to a higher-liquidity source
    }
    Err(ContractError::InvalidLiquidity) => {
        // Liquidity value is negative or zero
        log!("ERROR: Invalid liquidity value: {}", liquidity);
        // Fix your liquidity calculation
    }
    Err(e) => {
        log!("ERROR: {}", e);
    }
}
```

### Handling Rejections

If your submissions are consistently rejected:

1. **Check your liquidity calculation**
   - Ensure you're reporting in stroops (not XLM)
   - Verify you're not double-counting reserves
   - Confirm you're querying the right pool/market

2. **Verify the threshold**
   ```rust
   let threshold = oracle.get_liquidity_threshold(env, asset);
   match threshold {
       Some(t) => log!("Threshold for {}: {} stroops", asset, t),
       None => log!("No threshold set for {}", asset)
   }
   ```

3. **Switch to higher-liquidity sources**
   - Aggregate multiple DEXes
   - Use order book depth instead of just best bid/ask
   - Report TWAP liquidity instead of spot

4. **Contact governance**
   - If threshold is unrealistic, propose adjustment
   - Provide evidence of your liquidity calculations
   - Show market data justifying lower threshold

## Best Practices

### 1. Conservative Reporting

Report the **minimum** liquidity available, not the maximum:

```rust
// GOOD: Use minimum of recent observations
let liquidity = observations
    .iter()
    .map(|obs| obs.liquidity)
    .min()
    .unwrap_or(0);

// BAD: Use maximum or average
let liquidity = observations
    .iter()
    .map(|obs| obs.liquidity)
    .max() // Might overestimate
    .unwrap_or(0);
```

### 2. Time-Weighted Average

If possible, report TWAP liquidity over your observation window:

```rust
fn calculate_twap_liquidity(observations: &Vec<LiquidityObservation>) -> i128 {
    if observations.is_empty() {
        return 0;
    }
    
    let total: i128 = observations.iter().map(|obs| obs.liquidity).sum();
    total / observations.len() as i128
}
```

### 3. Sanity Checks

Validate liquidity before submission:

```rust
fn validate_liquidity(liquidity: i128) -> bool {
    // Must be positive
    if liquidity <= 0 {
        return false;
    }
    
    // Sanity check: not unrealistically high (> 1B XLM)
    if liquidity > 1_000_000_000_0000000 {
        return false;
    }
    
    // Sanity check: not unrealistically low (< 10 XLM)
    if liquidity < 100_000_000 {
        return false;
    }
    
    true
}
```

### 4. Logging

Log liquidity submissions for debugging:

```rust
oracle.update_price(
    env, source, asset, price, decimals, confidence, ttl, liquidity
)?;

log!(
    "Submitted: asset={}, price={}, liquidity={}, confidence={}",
    asset, price, liquidity, confidence
);
```

### 5. Monitoring

Track your rejection rate:

```rust
struct SubmissionStats {
    total: u32,
    accepted: u32,
    rejected_liquidity: u32,
}

impl SubmissionStats {
    fn rejection_rate(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        (self.rejected_liquidity as f64) / (self.total as f64)
    }
}

// Alert if rejection rate > 10%
if stats.rejection_rate() > 0.10 {
    alert!("High rejection rate: {}%", stats.rejection_rate() * 100.0);
}
```

## Temporary Workaround (NOT RECOMMENDED)

If you need to buy time during migration, you can submit a very high liquidity value:

```rust
// TEMPORARY WORKAROUND ONLY
oracle.update_price(
    env,
    source,
    asset,
    price,
    decimals,
    confidence,
    ttl,
    i128::MAX  // Will pass any threshold
)?;
```

**WARNING**: This defeats the security mechanism and should only be used:
- During testing/staging
- For immediate hotfix while implementing proper solution
- With clear timeline to implement real liquidity calculation

Governance may slash relayers who consistently report fake liquidity data.

## FAQ

### Q: What units should liquidity be in?
**A**: Stroops (1 XLM = 10,000,000 stroops). Same units as price.

### Q: Can I report zero liquidity?
**A**: No, zero and negative values are rejected with `InvalidLiquidity` error.

### Q: What if my asset has no threshold configured?
**A**: Validation is skipped - any positive liquidity value will be accepted. You can check with `get_liquidity_threshold()`.

### Q: How often do thresholds change?
**A**: Rarely. Governance must approve changes via multi-sig. You'll see `liquidity_threshold_set` events on-chain.

### Q: What if I can't access liquidity data?
**A**: Consider:
1. Using a different data source that provides liquidity
2. Estimating based on historical volume
3. Partnering with another relayer who has this data
4. Proposing removal of threshold for that asset (if justified)

### Q: Can I report liquidity in USD instead of XLM?
**A**: No, always use the base asset (first symbol in the pair). For XLM_USD, report in XLM stroops. For NGN_USD, report in NGN smallest unit.

### Q: Does liquidity affect my rewards?
**A**: Not directly, but:
- Higher liquidity may give your price more weight (future feature)
- Consistently low liquidity leads to slashing
- Meeting thresholds builds reputation

### Q: How do I test my liquidity calculation?
**A**: Compare against:
1. On-chain pool states (for DEXes)
2. Other relayers' submissions (via `get_provider_liquidity`)
3. Off-chain analytics tools
4. Manual calculations from raw order book data

## Support Channels

- **Technical Issues**: GitHub issues or Discord #relayer-support
- **Threshold Concerns**: Governance forum or Discord #governance
- **Integration Help**: Email relayers@stellarflow.io
- **Emergency**: Discord DM @admin (for critical production issues only)

## Timeline

- **2026-06-24**: Contract deployed, documentation released
- **2026-06-30**: Deadline for relayer SDK updates
- **2026-07-01**: Thresholds enabled for major assets (XLM_USD, etc.)
- **2026-07-08**: Slashing enabled for repeat violations
- **2026-07-15**: All assets have thresholds configured

## Appendix: Example Implementations

### Stellar DEX Integration

```rust
use soroban_sdk::{Address, Env, Symbol};

fn get_stellar_dex_liquidity(
    env: &Env,
    base_asset: &Address,
    quote_asset: &Address
) -> Result<i128, Error> {
    // Query Stellar DEX liquidity pool
    let pool_id = compute_pool_id(base_asset, quote_asset);
    let pool = env.invoke_contract(
        &pool_id,
        &Symbol::new(env, "get_reserves"),
        ()
    );
    
    let (reserve_a, reserve_b): (i128, i128) = pool;
    
    // Use base asset reserve as liquidity measure
    Ok(reserve_a)
}
```

### Phoenix DEX Integration

```rust
use soroban_sdk::{Address, Env, Symbol, Vec};

fn get_phoenix_liquidity(
    env: &Env,
    pair_address: &Address
) -> Result<i128, Error> {
    // Query Phoenix pair contract
    let info: PairInfo = env.invoke_contract(
        pair_address,
        &Symbol::new(env, "query_pool_info"),
        ()
    );
    
    // Phoenix stores liquidity in first reserve
    Ok(info.asset_a_reserve)
}
```

### Aggregated Multi-Source

```rust
fn get_aggregated_liquidity(
    env: &Env,
    asset: &Symbol
) -> Result<i128, Error> {
    let mut total_liquidity = 0_i128;
    
    // Source 1: Stellar DEX
    if let Ok(sdex_liq) = get_stellar_dex_liquidity(env, asset) {
        total_liquidity += sdex_liq;
    }
    
    // Source 2: Phoenix
    if let Ok(phoenix_liq) = get_phoenix_liquidity(env, asset) {
        total_liquidity += phoenix_liq;
    }
    
    // Source 3: Soroswap
    if let Ok(soroswap_liq) = get_soroswap_liquidity(env, asset) {
        total_liquidity += soroswap_liq;
    }
    
    if total_liquidity == 0 {
        return Err(Error::NoLiquidityData);
    }
    
    Ok(total_liquidity)
}
```

---

**Last Updated**: 2026-06-24  
**Version**: 1.0  
**Contact**: relayers@stellarflow.io
