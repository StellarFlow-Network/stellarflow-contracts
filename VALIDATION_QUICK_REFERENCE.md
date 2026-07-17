# Validation Quick Reference

## Security Thresholds

| Parameter | Value | Equivalent | Purpose |
|-----------|-------|------------|---------|
| `MIN_RESERVE_BALANCE` | 1,000,000,000,000 stroops | 100,000 XLM | Flash loan protection |
| `MIN_TRADING_VOLUME` | 100,000,000,000 stroops | 10,000 XLM/24h | Market activity requirement |
| `MAX_TELEMETRY_AGE_SECS` | 60 seconds | 1 minute | Data freshness requirement |
| `PREMIUM_POOL_MIN_STAKE` | 1,000 | - | Validator bond requirement |

## Error Codes

| Code | Error | Description |
|------|-------|-------------|
| 33 | `StaleTelemetryPayload` | Timestamp > 60 seconds old |
| 34 | `InsufficientReserveBalance` | Reserve < 100k XLM |
| 35 | `InsufficientVolume` | 24h volume < 10k XLM |
| 23 | `PremiumPoolAccessDenied` | Validator stake < 1,000 |

## Validation Functions

### `validate_reserve_balance(reserve_a, reserve_b)`
```rust
// Returns Ok if both >= MIN_RESERVE_BALANCE
// Returns Err(InsufficientReserveBalance) otherwise
```

### `validate_trading_volume(volume_24h)`
```rust
// Returns Ok if >= MIN_TRADING_VOLUME
// Returns Err(InsufficientVolume) otherwise
```

### `validate_telemetry_submission(...)`
```rust
// Comprehensive pipeline:
// 1. Timestamp freshness
// 2. Reserve validation
// 3. Volume validation
// 4. Bond capacity
```

## API Endpoints

### `submit_telemetry_data()`
```rust
pub fn submit_telemetry_data(
    env: Env,
    node: Address,              // Validator address
    pool: Symbol,               // e.g., "XLM_USDC"
    payload_timestamp: u64,     // Ledger timestamp
    reserve_a: i128,            // Reserve A in stroops
    reserve_b: i128,            // Reserve B in stroops
    volume_24h: i128,           // 24h volume in stroops
) -> Result<(), ContractError>
```

**Success Event**: `telem_ok` with (node, pool, timestamp)

## Quick Checks

### Will My Submission Pass?

✅ **PASS** if ALL of these are true:
- Timestamp is within last 60 seconds
- Reserve A ≥ 100,000 XLM (1,000,000,000,000 stroops)
- Reserve B ≥ 100,000 XLM (1,000,000,000,000 stroops)
- 24h volume ≥ 10,000 XLM (100,000,000,000 stroops)
- Validator has ≥ 1,000 stake

❌ **FAIL** if ANY of these are true:
- Timestamp > 60 seconds old
- Either reserve < 100,000 XLM
- 24h volume < 10,000 XLM
- Validator stake < 1,000
- Validator is revoked

## Common Scenarios

### Healthy Pool
```rust
reserve_a: 2,500,000,000,000    // 250k XLM ✅
reserve_b: 1,800,000,000,000    // 180k XLM ✅
volume_24h: 750,000,000,000     // 75k XLM ✅
Result: ACCEPTED
```

### Thin Pool (Vulnerable)
```rust
reserve_a: 30,000,000,000       // 3k XLM ❌
reserve_b: 25,000,000,000       // 2.5k XLM ❌
volume_24h: 500,000,000,000     // 50k XLM ✅
Result: REJECTED (InsufficientReserveBalance)
```

### Dormant Pool
```rust
reserve_a: 2,000,000,000,000    // 200k XLM ✅
reserve_b: 1,500,000,000,000    // 150k XLM ✅
volume_24h: 5,000,000,000       // 500 XLM ❌
Result: REJECTED (InsufficientVolume)
```

### Stale Data
```rust
timestamp: now - 120 seconds    // 2 minutes old ❌
reserve_a: 2,000,000,000,000    // 200k XLM ✅
reserve_b: 1,500,000,000,000    // 150k XLM ✅
volume_24h: 500,000,000,000     // 50k XLM ✅
Result: REJECTED (StaleTelemetryPayload)
```

## Test Coverage

| Category | Tests | Coverage |
|----------|-------|----------|
| Timestamp Freshness | 6 | ✅ Complete |
| Reserve Balance | 7 | ✅ Complete |
| Trading Volume | 6 | ✅ Complete |
| Integrated Pipeline | 5 | ✅ Complete |
| **Total** | **24** | **100%** |

## Stroops Conversion Helper

```
1 XLM = 10,000,000 stroops (10^7)

Examples:
- 100,000 XLM = 1,000,000,000,000 stroops
- 50,000 XLM = 500,000,000,000 stroops
- 10,000 XLM = 100,000,000,000 stroops
- 1,000 XLM = 10,000,000,000 stroops
```

## Security Best Practices

1. ✅ Always use current ledger timestamp
2. ✅ Report accurate reserve balances
3. ✅ Calculate 24h volume from on-chain data
4. ✅ Maintain adequate validator stake
5. ✅ Monitor rejection events
6. ❌ Never submit stale data
7. ❌ Never manipulate reported metrics
8. ❌ Never submit from unauthorized pools

## Troubleshooting

### Getting `InsufficientReserveBalance`?
- Check that both reserves meet 100k XLM minimum
- Verify you're reading on-chain reserve values correctly
- Consider only tracking high-liquidity pools

### Getting `InsufficientVolume`?
- Check 24h volume calculation logic
- Ensure you're summing all trades in the period
- Consider whether pool has enough activity

### Getting `StaleTelemetryPayload`?
- Reduce time between data capture and submission
- Check network latency
- Use current ledger timestamp, not local time

### Getting `PremiumPoolAccessDenied`?
- Increase your validator stake
- Verify stake registration succeeded
- Check that stake hasn't been slashed

---

**Last Updated**: 2026-06-28  
**Module**: `src/validation.rs`  
**Public API**: `src/lib.rs::submit_telemetry_data()`
