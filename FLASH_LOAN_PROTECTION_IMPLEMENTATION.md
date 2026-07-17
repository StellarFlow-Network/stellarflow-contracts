# Flash Loan Protection Implementation

## Overview

This document describes the professional implementation of flash loan attack prevention measures for the StellarFlow contracts. The implementation adds strict volume and reserve balance validation to protect against price manipulation in thin automated liquidity pools.

## Security Problem

**Description**: Tracking conversion spreads across thin automated liquidity pools can expose downstream applications to flash-loan price manipulation vectors.

**Attack Vector**: 
1. Attacker borrows large amounts of capital via flash loan
2. Executes trades in low-liquidity pools to manipulate prices
3. Downstream applications consume manipulated prices
4. Attacker profits from the manipulation
5. Flash loan is repaid within the same transaction

## Implementation

### 1. New Error Types (`src/lib.rs`)

Added three new error variants to `ContractError`:

```rust
/// Telemetry submission rejected: payload timestamp is stale.
StaleTelemetryPayload = 33,

/// Telemetry submission rejected: reported reserve balance is below minimum security threshold.
InsufficientReserveBalance = 34,

/// Telemetry submission rejected: trading volume falls below required minimum.
InsufficientVolume = 35,
```

Also fixed duplicate error code (StaleSequence moved from 26 to 36).

### 2. Validation Module (`src/validation.rs`)

#### Constants

```rust
/// Minimum reserve balance: 100,000 XLM equivalent (1_000_000_000_000 stroops)
pub const MIN_RESERVE_BALANCE: i128 = 1_000_000_000_000;

/// Minimum 24-hour trading volume: 10,000 XLM equivalent (100_000_000_000 stroops)
pub const MIN_TRADING_VOLUME: i128 = 100_000_000_000;

/// Maximum telemetry age: 60 seconds
pub const MAX_TELEMETRY_AGE_SECS: u64 = 60;
```

#### New Validation Functions

##### `validate_reserve_balance(reserve_a: i128, reserve_b: i128)`
- **Purpose**: Ensures both pool reserves meet minimum thresholds
- **Returns**: 
  - `Ok(())` if both reserves ≥ MIN_RESERVE_BALANCE
  - `Err(ContractError::InsufficientReserveBalance)` otherwise
- **Security**: Prevents acceptance of price data from pools susceptible to flash loan manipulation

##### `validate_trading_volume(volume_24h: i128)`
- **Purpose**: Ensures pool has sufficient 24-hour trading activity
- **Returns**:
  - `Ok(())` if volume ≥ MIN_TRADING_VOLUME
  - `Err(ContractError::InsufficientVolume)` otherwise
- **Security**: Prevents stale/abandoned pools from providing price data

##### `validate_telemetry_submission(...)`
- **Purpose**: Comprehensive validation pipeline orchestrating all checks
- **Parameters**:
  - `env`: Soroban environment
  - `node`: Validator address
  - `pool`: Asset pool symbol
  - `payload_timestamp`: Telemetry capture timestamp
  - `reserve_a`: Reserve balance of asset A (stroops)
  - `reserve_b`: Reserve balance of asset B (stroops)
  - `volume_24h`: 24-hour trading volume (stroops)
- **Validation Steps** (fail-fast):
  1. Timestamp freshness check
  2. Reserve balance validation
  3. Trading volume validation
  4. Validator bond capacity verification
- **Returns**: First error encountered or `Ok(())`

### 3. Public API (`src/lib.rs`)

#### `submit_telemetry_data()`

New contract method for validators to submit telemetry with comprehensive security checks:

```rust
pub fn submit_telemetry_data(
    env: Env,
    node: Address,
    pool: Symbol,
    payload_timestamp: u64,
    reserve_a: i128,
    reserve_b: i128,
    volume_24h: i128,
) -> Result<(), ContractError>
```

**Flow**:
1. Check that validator is not revoked
2. Require authentication from validator
3. Run comprehensive validation pipeline
4. Record heartbeat if validation passes
5. Emit success event

**Events Emitted**:
- `telem_ok`: (node, pool, payload_timestamp) on successful submission

## Security Properties

### Defense-in-Depth

The implementation provides multiple layers of protection:

1. **Timestamp Validation**: Rejects stale data (>60s old)
2. **Reserve Balance Threshold**: Ensures pools have sufficient liquidity depth
3. **Volume Requirements**: Confirms active market participation
4. **Bond Capacity**: Requires validators to have adequate stake

### Economic Security

- **MIN_RESERVE_BALANCE (100,000 XLM)**: Makes flash loan attacks economically infeasible
  - Manipulating a pool with 100k XLM on each side requires significant capital
  - Price impact is bounded even with large flash loans
  
- **MIN_TRADING_VOLUME (10,000 XLM/24h)**: Ensures market activity
  - Dormant pools cannot provide price data
  - Active markets recover quickly from manipulation attempts

### Fail-Fast Design

Validations are ordered by cost and failure probability:
1. Timestamp check (cheapest, most common rejection)
2. Reserve validation (core security requirement)
3. Volume validation (secondary requirement)
4. Bond capacity (most expensive, checked last)

## Testing

### Comprehensive Test Suite (`src/validation.rs`)

```
validation_tests/
├── Timestamp Freshness Tests (6 tests)
│   ├── test_fresh_payload_within_60s_passes
│   ├── test_fresh_payload_exactly_at_60s_passes
│   ├── test_stale_payload_beyond_60s_rejected
│   ├── test_payload_from_future_passes
│   ├── test_payload_at_current_time_passes
│   └── test_payload_very_stale_rejected
│
├── Reserve Balance Tests (7 tests)
│   ├── test_reserve_balance_both_above_threshold_passes
│   ├── test_reserve_balance_well_above_threshold_passes
│   ├── test_reserve_balance_first_below_threshold_rejected
│   ├── test_reserve_balance_second_below_threshold_rejected
│   ├── test_reserve_balance_both_below_threshold_rejected
│   ├── test_reserve_balance_negative_reserve_rejected
│   └── test_reserve_balance_zero_rejected
│
├── Trading Volume Tests (6 tests)
│   ├── test_trading_volume_at_threshold_passes
│   ├── test_trading_volume_well_above_threshold_passes
│   ├── test_trading_volume_below_threshold_rejected
│   ├── test_trading_volume_zero_rejected
│   ├── test_trading_volume_negative_rejected
│   └── test_trading_volume_high_activity_pool_passes
│
└── Integrated Validation Tests (5 tests)
    ├── test_telemetry_validation_all_checks_pass
    ├── test_telemetry_validation_stale_timestamp_fails_first
    ├── test_telemetry_validation_insufficient_reserves_fails
    ├── test_telemetry_validation_insufficient_volume_fails
    └── test_telemetry_validation_flash_loan_attack_scenario
```

**Total**: 24 comprehensive tests covering all security scenarios

## Configuration

### Adjusting Security Thresholds

The constants can be tuned based on network conditions:

```rust
// Conservative (higher security, stricter requirements)
pub const MIN_RESERVE_BALANCE: i128 = 5_000_000_000_000;  // 500k XLM
pub const MIN_TRADING_VOLUME: i128 = 500_000_000_000;    // 50k XLM/24h

// Balanced (current defaults)
pub const MIN_RESERVE_BALANCE: i128 = 1_000_000_000_000;  // 100k XLM
pub const MIN_TRADING_VOLUME: i128 = 100_000_000_000;    // 10k XLM/24h

// Permissive (lower requirements, broader pool acceptance)
pub const MIN_RESERVE_BALANCE: i128 = 100_000_000_000;   // 10k XLM
pub const MIN_TRADING_VOLUME: i128 = 10_000_000_000;     // 1k XLM/24h
```

## Usage Examples

### Validator Submitting Telemetry

```rust
use soroban_sdk::{Env, Address, Symbol};

// Validator submits data for XLM/USDC pool
contract.submit_telemetry_data(
    env,
    validator_address,
    Symbol::new(&env, "XLM_USDC"),
    env.ledger().timestamp(),   // Current timestamp
    2_500_000_000_000,          // 250k XLM in reserve A
    1_800_000_000_000,          // 180k USDC in reserve B
    750_000_000_000,            // 75k XLM daily volume
)?;
// Result: Ok(()) - all validations pass
```

### Rejection Scenarios

```rust
// Scenario 1: Stale data
contract.submit_telemetry_data(
    env,
    validator,
    pool,
    env.ledger().timestamp() - 120,  // 2 minutes old
    sufficient_reserve_a,
    sufficient_reserve_b,
    sufficient_volume,
)?;
// Result: Err(ContractError::StaleTelemetryPayload)

// Scenario 2: Thin liquidity (flash loan vulnerable)
contract.submit_telemetry_data(
    env,
    validator,
    pool,
    env.ledger().timestamp(),
    30_000_000_000,              // Only 3k XLM (too low)
    25_000_000_000,              // Only 2.5k USDC (too low)
    sufficient_volume,
)?;
// Result: Err(ContractError::InsufficientReserveBalance)

// Scenario 3: Dormant pool
contract.submit_telemetry_data(
    env,
    validator,
    pool,
    env.ledger().timestamp(),
    sufficient_reserve_a,
    sufficient_reserve_b,
    5_000_000_000,               // Only 500 XLM daily (too low)
)?;
// Result: Err(ContractError::InsufficientVolume)
```

## Integration Points

### Existing Code

- `update_validator_profile()`: Existing function still available, uses only bond capacity check
- `submit_telemetry_data()`: New function with comprehensive validation

### Downstream Applications

Applications consuming price data can now trust that:
1. Prices come from sufficiently liquid markets
2. Markets have active trading volume
3. Data is fresh (<60s old)
4. Validators are properly bonded

## Deployment Checklist

- [x] Error types added to ContractError enum
- [x] Validation constants defined
- [x] Core validation functions implemented
- [x] Comprehensive test suite added
- [x] Public API function added
- [x] Documentation completed
- [ ] Security audit recommended
- [ ] Deploy to testnet
- [ ] Monitor threshold effectiveness
- [ ] Adjust thresholds based on real data
- [ ] Deploy to mainnet

## Monitoring

### Recommended Metrics

1. **Rejection Rate by Reason**:
   - Track `StaleTelemetryPayload` events
   - Track `InsufficientReserveBalance` events
   - Track `InsufficientVolume` events

2. **Pool Health**:
   - Average reserve balances across accepted submissions
   - Average 24h volumes across accepted submissions
   - Distribution of timestamp freshness

3. **Attack Detection**:
   - Sudden spikes in rejection rates
   - Unusual patterns in reserve reporting
   - Correlation with on-chain flash loan activity

## Future Enhancements

1. **Dynamic Thresholds**: Adjust MIN_RESERVE_BALANCE based on asset volatility
2. **Historical Tracking**: Store reserve/volume history for reputation scoring
3. **Graduated Penalties**: Slash validators who repeatedly submit low-liquidity data
4. **Multi-Pool Validation**: Cross-reference prices across multiple pools
5. **Oracle Integration**: External price validation for anomaly detection

## References

- Flash Loan Attack Examples: [TODO: Add links]
- DeFi Security Best Practices: [TODO: Add links]
- Soroban Security Guidelines: [TODO: Add links]

---

**Implementation Date**: 2026-06-28  
**Author**: AI Coding Agent  
**Status**: Complete ✅  
**Version**: 1.0.0
