# Liquidity Volume Validation - Flash Loan Protection

## Overview

This document describes the liquidity volume validation feature added to the StellarFlow price oracle to prevent flash loan price manipulation attacks.

## Problem Statement

Aggregating market prices from thinly backed liquidity channels can expose downstream financial engines to flash loan price manipulations. An attacker can:

1. Temporarily inject capital into a thin market via flash loan
2. Manipulate the price in that market
3. Submit the manipulated price to the oracle
4. Downstream contracts execute trades/liquidations based on fake price
5. Attacker profits and repays flash loan

## Solution

The oracle now validates that reported pool liquidity meets configured minimum thresholds **before** accepting price submissions. This ensures prices come from markets with sufficient depth to resist manipulation.

## Architecture

### New Module: `src/validation.rs`

Core liquidity validation logic including:
- Threshold configuration and storage
- Validation function called during price submission
- Provider liquidity tracking for reputation
- Graduated slashing for low-liquidity submissions

### Storage Keys (in `types.rs`)

```rust
/// Minimum liquidity threshold per asset (in stroops)
LiquidityThreshold(Symbol)

/// Last reported liquidity from each provider per asset
ProviderReportedLiquidity(Address, Symbol)

/// Timestamp of last successful validation per asset
LastLiquidityValidation(Symbol)
```

### Error Types (in `lib.rs`)

```rust
/// Submission rejected - liquidity below threshold
LiquidityBelowThreshold = 54

/// Invalid liquidity value (must be positive)
InvalidLiquidity = 55

/// Invalid threshold configuration
InvalidLiquidityThreshold = 56
```

## Integration Points

### 1. Price Submission Flow (`update_price`)

**Updated signature:**
```rust
pub fn update_price(
    env: Env,
    source: Address,
    asset: Symbol,
    price: i128,
    decimals: u32,
    confidence_score: u32,
    ttl: u64,
    liquidity: i128,  // NEW: required liquidity parameter
) -> Result<(), ContractError>
```

**Validation checkpoint** (line ~2290 in lib.rs):
```rust
// After price bounds check, before buffer addition
if !bypass_active {
    validation::validate_liquidity(&env, &asset, &source, liquidity)?;
}
```

This placement ensures:
- Early termination (before price enters buffer)
- Can be bypassed via admin safety override
- Executes after all other safety checks pass

### 2. Admin Configuration Functions

#### Set Threshold
```rust
pub fn set_liquidity_threshold(
    env: Env,
    admin: Address,
    asset: Symbol,
    threshold: i128
) -> Result<(), ContractError>
```

Configures minimum liquidity for an asset. Threshold must be within:
- Min: 10_000_000 stroops (1 XLM)
- Max: 1_000_000_000_0000000 stroops (1B XLM)

#### Get Threshold
```rust
pub fn get_liquidity_threshold(
    env: Env,
    asset: Symbol
) -> Option<i128>
```

Returns configured threshold or None if unset.

#### Remove Threshold
```rust
pub fn remove_liquidity_threshold(
    env: Env,
    admin: Address,
    asset: Symbol
)
```

Disables liquidity validation for an asset (use with caution).

#### Get Provider Liquidity
```rust
pub fn get_provider_liquidity(
    env: Env,
    provider: Address,
    asset: Symbol
) -> Option<i128>
```

Query historical liquidity data for reputation scoring.

#### Get Last Validation
```rust
pub fn get_last_liquidity_validation(
    env: Env,
    asset: Symbol
) -> Option<u64>
```

Returns timestamp of last successful validation.

### 3. Slashing Integration

```rust
pub fn slash_for_low_liquidity(
    env: Env,
    executor: Address,
    provider: Address,
    asset: Symbol,
    reported_liquidity: i128,
    base_slash_amount: i128
) -> Result<(), ContractError>
```

Applies graduated penalties for low-liquidity submissions:

| Liquidity (% of threshold) | Multiplier | Severity |
|----------------------------|------------|----------|
| ≥ 100%                     | 1×         | None     |
| 75-99%                     | 2×         | Minor    |
| 50-74%                     | 4×         | Moderate |
| 25-49%                     | 8×         | Significant |
| < 25%                      | 16×        | Severe   |

**Final penalty calculation:**
```
final_slash = base_amount × liquidity_multiplier × missed_blocks_multiplier
```

The liquidity multiplier stacks with existing deviation-based and downtime penalties.

## Security Properties

### 1. Early Termination
Validation occurs **before** price enters the buffer, preventing bad data from ever entering consensus calculation.

### 2. Per-Asset Configuration
Different assets can have different thresholds based on their market characteristics:
- High-volume pairs (XLM/USD): higher threshold
- Emerging markets (exotic pairs): lower threshold

### 3. Provider Accountability
All liquidity submissions are recorded per-provider, enabling:
- Reputation scoring
- Pattern detection (consistently low liquidity)
- Evidence-based slashing

### 4. Audit Trail
Timestamps of validations allow reconstruction of:
- When thresholds were enforced
- Frequency of validation events
- Provider compliance history

### 5. Bypass Mechanism
Admin can temporarily disable validation via existing `bypass_safety_checks` mechanism:
- Useful during emergency operations
- Requires multi-sig governance approval
- Auto-expires after 1 hour

## Events

### Liquidity Validated
```rust
(Symbol::new(env, "liquidity_validated"),)
(asset, provider, reported_liquidity, threshold)
```

Emitted on successful validation.

### Liquidity Violation
```rust
(Symbol::new(env, "liquidity_violation"),)
(asset, provider, reported_liquidity, threshold)
```

Emitted when submission rejected due to insufficient liquidity.

### Threshold Set
```rust
(Symbol::new(env, "liquidity_threshold_set"),)
(asset, threshold)
```

Emitted when admin configures a threshold.

### Threshold Removed
```rust
(Symbol::new(env, "liquidity_threshold_removed"),)
(asset)
```

Emitted when threshold is removed.

### Liquidity Slash Executed
```rust
(Symbol::new(env, "liquidity_slash_executed"),)
(provider, asset, reported_liquidity, threshold, multiplier, slashed_amount)
```

Emitted when provider is slashed for low-liquidity submission.

## Usage Examples

### For Relayers

**Submit price with liquidity data:**
```rust
oracle.update_price(
    &env,
    &relayer_address,
    &Symbol::new(&env, "XLM_USD"),
    &price,           // 1_500_000_000 (normalized to 9 decimals)
    &decimals,        // 9
    &confidence,      // 95
    &ttl,            // 300
    &liquidity       // 50_000_000_000 stroops (5000 XLM)
)
```

If liquidity is below threshold, transaction fails with `LiquidityBelowThreshold` error.

### For Admins

**Configure threshold for new asset:**
```rust
// Set 1000 XLM minimum liquidity for XLM/USD pair
oracle.set_liquidity_threshold(
    &env,
    &admin_address,
    &Symbol::new(&env, "XLM_USD"),
    &10_000_000_000  // 1000 XLM in stroops
);
```

**Query current threshold:**
```rust
let threshold = oracle.get_liquidity_threshold(
    &env,
    &Symbol::new(&env, "XLM_USD")
);

match threshold {
    Some(t) => log!("Threshold: {} stroops", t),
    None => log!("No threshold configured")
}
```

**Monitor provider reputation:**
```rust
let liquidity = oracle.get_provider_liquidity(
    &env,
    &provider_address,
    &Symbol::new(&env, "XLM_USD")
);

if let Some(liq) = liquidity {
    let threshold = oracle.get_liquidity_threshold(
        &env,
        &Symbol::new(&env, "XLM_USD")
    ).unwrap_or(0);
    
    let percentage = (liq * 100) / threshold;
    log!("Provider at {}% of threshold", percentage);
}
```

**Execute slash for repeated violations:**
```rust
// Slash provider reporting only 30% of required liquidity
oracle.slash_for_low_liquidity(
    &env,
    &admin_address,
    &bad_provider,
    &Symbol::new(&env, "XLM_USD"),
    &3_000_000_000,   // 300 XLM (30% of 1000 threshold)
    &1_000_000_000    // Base slash: 100 tokens
);
// Actual slash will be: 100 × 8 (liquidity mult) × missed_blocks_mult
```

## Migration Strategy

### Phase 1: Soft Launch (Week 1-2)
1. Deploy updated contract with validation module
2. **Do not set thresholds yet** - validation disabled by default
3. Monitor relayer submissions to establish baseline liquidity data
4. Analyze liquidity distribution per asset

### Phase 2: Threshold Calibration (Week 3-4)
1. Set conservative thresholds (10th percentile of observed liquidity)
2. Monitor rejection rates
3. Work with relayers to improve data quality
4. Gradually increase thresholds to target (25th percentile)

### Phase 3: Enforcement (Week 5+)
1. Enable slashing for repeated low-liquidity submissions
2. Set production thresholds (50th percentile)
3. Monitor for manipulation attempts
4. Adjust thresholds based on market conditions

### Backward Compatibility

**Breaking change:** `update_price` signature now requires `liquidity` parameter.

**Migration path for relayers:**
1. Update SDKs to include liquidity parameter
2. Calculate liquidity from data sources (DEX pools, order books, etc.)
3. Test against staging oracle with thresholds enabled
4. Deploy to production

**Graceful degradation:**
- Assets without thresholds continue to work (validation skipped)
- Allows per-asset rollout
- Can disable validation via bypass mechanism if issues arise

## Testing

### Unit Tests (in `validation.rs`)

```rust
#[test]
fn test_liquidity_slash_multiplier() {
    // Verify graduated penalty tiers
    assert_eq!(calculate_liquidity_slash_multiplier(100, 100), 1);   // 100%
    assert_eq!(calculate_liquidity_slash_multiplier(80, 100), 2);    // 80%
    assert_eq!(calculate_liquidity_slash_multiplier(60, 100), 4);    // 60%
    assert_eq!(calculate_liquidity_slash_multiplier(40, 100), 8);    // 40%
    assert_eq!(calculate_liquidity_slash_multiplier(20, 100), 16);   // 20%
}

#[test]
fn test_zero_threshold_handling() {
    // Should not panic on division by zero
    assert_eq!(calculate_liquidity_slash_multiplier(100, 0), 1);
}
```

### Integration Tests (recommended in `test.rs`)

```rust
#[test]
fn test_liquidity_validation_rejects_thin_markets() {
    // Setup: admin sets 1000 XLM threshold
    // Action: provider submits with 500 XLM liquidity
    // Assert: transaction fails with LiquidityBelowThreshold
}

#[test]
fn test_liquidity_validation_accepts_sufficient_liquidity() {
    // Setup: admin sets 1000 XLM threshold
    // Action: provider submits with 1500 XLM liquidity
    // Assert: transaction succeeds, price updated
}

#[test]
fn test_bypass_disables_liquidity_check() {
    // Setup: threshold set, bypass enabled
    // Action: submit with insufficient liquidity
    // Assert: transaction succeeds (bypass overrides)
}

#[test]
fn test_provider_liquidity_tracked() {
    // Setup: multiple submissions from provider
    // Action: query get_provider_liquidity
    // Assert: returns most recent submission value
}
```

## Monitoring & Alerting

### Key Metrics

1. **Rejection Rate**: % of submissions rejected due to liquidity
   - Target: < 5% (indicates thresholds are calibrated)
   - Alert: > 20% (thresholds may be too high)

2. **Average Liquidity Gap**: How far below threshold rejections occur
   - Target: 80-90% of threshold (near-misses)
   - Alert: < 50% (severe liquidity issues)

3. **Provider Compliance**: % of providers consistently meeting threshold
   - Target: > 90%
   - Alert: < 75% (systemic data quality issue)

4. **Slash Frequency**: Slashes per week due to liquidity violations
   - Target: 0-2 (rare occurrences)
   - Alert: > 10 (manipulation attempts or misconfigured thresholds)

### Event Indexing

Index these events for dashboard/analytics:
- `liquidity_validated`: Track compliance trends
- `liquidity_violation`: Identify problematic providers/assets
- `liquidity_threshold_set`: Audit configuration changes
- `liquidity_slash_executed`: Monitor enforcement actions

## Security Considerations

### Threshold Manipulation

**Risk**: Admin sets artificially low thresholds to accept thin-market prices.

**Mitigation**:
- Multi-sig governance required for threshold changes
- MIN_LIQUIDITY_THRESHOLD enforces 1 XLM floor
- Threshold changes emit events for public audit
- Off-chain monitoring can alert on suspicious threshold reductions

### Data Source Manipulation

**Risk**: Relayer reports fake liquidity values.

**Mitigation**:
- Multiple independent relayers provide liquidity data
- Median-based consensus (like price)
- Slash mechanism penalizes false reporting
- Off-chain verification cross-checks on-chain liquidity against DEX state

### Flash Loan During Validation

**Risk**: Attacker inflates liquidity during validation window.

**Mitigation**:
- Relayers should report TWAP (time-weighted average) liquidity, not spot
- Multiple relayers with different observation windows make this expensive
- Threshold should be set high enough that even temporary inflation is costly

### Bypass Abuse

**Risk**: Admin permanently bypasses validation.

**Mitigation**:
- Bypass auto-expires after 1 hour
- Requires multi-sig approval to enable
- `bypass_enabled` events are publicly auditable
- Should only be used during emergencies (oracle freeze, etc.)

## Future Enhancements

### 1. TWAP Liquidity Validation
Instead of spot liquidity, require time-weighted average over last N minutes.

### 2. Multi-Source Liquidity Consensus
Aggregate liquidity from multiple relayers and use median (like price).

### 3. Dynamic Thresholds
Automatically adjust thresholds based on:
- Historical liquidity percentiles
- Market volatility
- Time of day (lower thresholds during off-hours)

### 4. Liquidity-Weighted Median
When calculating consensus price, weight submissions by their reported liquidity (higher liquidity = higher weight).

### 5. Cross-Asset Validation
Detect liquidity anomalies across correlated assets (e.g., if XLM/USD liquidity drops but XLM/EUR stays normal, flag as suspicious).

## References

- [Flash Loan Attack Primer](https://consensys.github.io/smart-contract-best-practices/attacks/flash-loan-attacks/)
- [Oracle Manipulation Techniques](https://blog.openzeppelin.com/secure-smart-contract-guidelines-the-dangers-of-price-oracles/)
- [Soroban Smart Contract Best Practices](https://soroban.stellar.org/docs/how-to-guides/best-practices)

## Support

For questions or issues:
- GitHub: [stellarflow-contracts](https://github.com/stellarflow/contracts)
- Discord: #dev-support
- Email: security@stellarflow.io
