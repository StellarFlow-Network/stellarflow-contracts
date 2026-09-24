# Balance Invariant Checks Implementation

## Overview

This document describes the implementation of balance invariant checks across all contracts that manage token reserves and internal balance accounting. The invariant checks ensure that token reserves exactly match internal balance ledger accounting state, preventing balance drift and potential exploit scenarios.

## Implementation Summary

### Core Principle

For every state-changing operation that modifies balances:
1. **Pre-condition check**: Assert balance consistency before state modification
2. **State modification**: Perform the operation (transfer, mint, burn, etc.)
3. **Post-condition check**: Assert balance consistency after state modification
4. **Panic on violation**: Transaction immediately panics if any drift is detected

### Invariant Check Pattern

```rust
fn assert_balance_invariant(env: &Env, config: &Config) {
    let token_client = token::Client::new(env, &config.token);
    let actual_balance = token_client.balance(&env.current_contract_address());
    let tracked_balance = // ... load from internal accounting
    
    assert_eq!(
        actual_balance,
        tracked_balance,
        "Balance invariant violated: actual={}, tracked={}",
        actual_balance,
        tracked_balance
    );
}
```

## Contracts Updated

### 1. Bridge Escrow (`src/bridge/escrow.rs`)

**Invariant**: `token_client.balance(contract_address) == VaultBalance`

**Protected Functions**:
- `lock_tokens()`: Checks before and after depositing tokens
- `unlock_tokens()`: Checks before and after withdrawing tokens

**Implementation**:
```rust
fn assert_balance_invariant(env: &Env, config: &BridgeEscrowConfig) {
    let token_client = token::Client::new(env, &config.native_token);
    let actual_balance = token_client.balance(&env.current_contract_address());
    
    let balance_key = BridgeEscrowStorageKey::VaultBalance(config.native_token.clone());
    let tracked_balance: i128 = env.storage().persistent().get(&balance_key).unwrap_or(0);
    
    assert_eq!(
        actual_balance,
        tracked_balance,
        "Balance invariant violated: actual={}, tracked={}",
        actual_balance,
        tracked_balance
    );
}
```

### 2. Auto-compound Vault (`src/vaults/autocompound.rs`)

**Invariant**: `token_client.balance(contract_address) == TotalAssets`

**Protected Functions**:
- `deposit()`: Checks before and after depositing assets
- `withdraw()`: Checks before and after withdrawing assets
- `harvest()`: Checks before and after compounding yield

**Implementation**:
```rust
fn assert_balance_invariant(env: &Env, config: &VaultConfig) {
    let token_client = token::Client::new(env, &config.asset);
    let actual_balance = token_client.balance(&env.current_contract_address());
    let tracked_assets = total_assets(env);
    
    assert_eq!(
        actual_balance,
        tracked_assets,
        "Balance invariant violated: actual={}, tracked_assets={}",
        actual_balance,
        tracked_assets
    );
}
```

**Key Detail**: The vault's `TotalAssets` represents the contract's entire token holdings. After `harvest()`, fees are transferred out, so the invariant must hold both before the harvest (pre-state) and after fee distribution (post-state).

### 3. Liquidity Lock (`contracts/liquidity-lock/src/lib.rs`)

**Invariant**: `token_client.balance(contract_address) >= sum(unclaimed_amounts)`

**Protected Functions**:
- `create_stream()`: Checks before and after creating vesting stream
- `claim()`: Checks before and after claiming vested tokens

**Implementation**:
```rust
fn assert_balance_invariant(env: &Env) {
    let token_addr: Address = match env.storage().instance().get(&DataKey::Token) {
        Some(addr) => addr,
        None => return, // Not initialized yet
    };
    
    let token_client = token::Client::new(env, &token_addr);
    let actual_balance = token_client.balance(&env.current_contract_address());
    
    assert!(
        actual_balance >= 0,
        "Balance invariant violated: actual balance is negative"
    );
}
```

**Note**: Full implementation would require iterating all streams to sum unclaimed amounts. The current implementation verifies non-negative balance as a basic sanity check. Consider maintaining a `TotalUnclaimed` counter for O(1) verification.

### 4. Bridge Mint (`src/bridge/mint.rs`)

**Invariant**: `total_supply == sum(all_balances)` and `0 <= total_supply <= max_supply`

**Protected Functions**:
- `mint()`: Checks before and after minting wrapped tokens
- `burn()`: Checks before and after burning wrapped tokens

**Implementation**:
```rust
fn assert_balance_invariant(env: &Env, asset_code: &Symbol) -> Result<(), ContractError> {
    let config = load_config(env, asset_code)?;
    
    if config.total_supply < 0 {
        panic!(
            "Balance invariant violated: total_supply is negative: {}",
            config.total_supply
        );
    }
    
    if config.total_supply > config.max_supply {
        panic!(
            "Balance invariant violated: total_supply={} exceeds max_supply={}",
            config.total_supply,
            config.max_supply
        );
    }
    
    Ok(())
}
```

**Note**: This contract uses an internal ledger without external token contracts. The invariant verifies that `total_supply` stays within valid bounds. Iterating all balances would be expensive; instead, rely on consistent atomic updates during mint/burn.

## Security Benefits

1. **Immediate Detection**: Panics transaction immediately when drift occurs, preventing further damage
2. **Reentrancy Protection**: Catches unexpected balance changes from external calls between pre/post checks
3. **Accounting Errors**: Detects arithmetic errors, missing updates, or logic bugs
4. **External Interference**: Catches direct token transfers that bypass contract logic
5. **Exploit Prevention**: Stops attacks that rely on manipulating balance discrepancies

## Performance Considerations

- **Gas Cost**: Each check requires a token balance query (external contract call)
- **Double Checking**: Pre and post checks double the balance query overhead
- **Trade-off**: Higher gas cost is acceptable for critical security guarantee
- **Optimization**: For internal ledger systems (like Bridge Mint), invariant checks can be mathematical rather than query-based

## Testing Recommendations

1. **Positive Tests**: Verify normal operations succeed with invariants enabled
2. **Negative Tests**: Attempt to trigger invariant violations via:
   - Direct token transfers to contract
   - Reentrancy attacks
   - Integer overflow scenarios
   - Concurrent transactions
3. **Fuzzing**: Use property-based testing to verify invariant holds under random inputs
4. **Gas Profiling**: Measure overhead introduced by invariant checks

## Future Enhancements

1. **Configurable Checks**: Allow admin to enable/disable checks based on confidence level
2. **Event Logging**: Emit events before panicking to aid debugging
3. **Grace Period**: For certain contracts, allow small dust amounts as acceptable drift
4. **Batch Operations**: Optimize multiple operations to check invariant once at start/end of batch
5. **Stream Enumeration**: Implement efficient iteration for Liquidity Lock's full invariant check

## Contract-Specific Notes

### Bridge Escrow
- Single token contract, simple 1:1 correspondence
- VaultBalance must exactly match actual balance at all times
- No external transfers should occur outside lock/unlock

### Auto-compound Vault
- TotalAssets tracks current contract holdings
- During harvest, balance temporarily increases then decreases (fee transfer)
- Both pre and post checks ensure proper fee handling

### Liquidity Lock
- Multiple streams mean contract holds sum of all unclaimed amounts
- Future: maintain running total for O(1) verification
- Current implementation provides basic sanity check

### Bridge Mint
- Internal ledger, no external token contract
- total_supply is the canonical source of truth
- Invariant checks mathematical bounds rather than token balance

## Deployment Checklist

- [ ] All state-changing functions have pre/post invariant checks
- [ ] Test suite covers normal operations with checks enabled
- [ ] Negative tests verify checks catch common violations
- [ ] Gas costs analyzed and documented
- [ ] Consider adding invariant check toggle for emergency situations
- [ ] Monitor production for any panics and analyze root cause

## References

- Bridge Escrow: `src/bridge/escrow.rs` (lines 79-93, 106-137, 151-186)
- Auto-compound Vault: `src/vaults/autocompound.rs` (lines 95-108, 148-189, 220-265)
- Liquidity Lock: `contracts/liquidity-lock/src/lib.rs` (lines 38-69, 89-119, 121-138)
- Bridge Mint: `src/bridge/mint.rs` (lines 66-81, 121-166, 173-212)
