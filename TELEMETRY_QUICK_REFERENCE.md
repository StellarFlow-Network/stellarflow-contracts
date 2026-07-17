# Telemetry Tracking Quick Reference

## ⚡ Quick Start

### Record a Heartbeat
```rust
Self::_record_heartbeat(&env, symbol_short!("ASSET"));
```

### Check if Data is Fresh
```rust
if Self::is_data_fresh(env.clone(), symbol_short!("ASSET")) {
    // Data is current
} else {
    // Data is stale
}
```

### Get Last Update Time
```rust
match Self::get_last_update_timestamp(env.clone(), symbol_short!("ASSET")) {
    Some(timestamp) => { /* use timestamp */ },
    None => { /* never updated */ }
}
```

## 📊 Configuration Constants

```rust
// Default heartbeat interval: 5 minutes
const DEFAULT_HEARTBEAT_INTERVAL: u64 = 5 * 60;

// TTL: ~24 hours (17,280 ledgers at 5s each)
const HEARTBEAT_TTL_LEDGERS: u32 = 17_280;

// Auto-extend when < 5000 ledgers remain
const HEARTBEAT_TTL_THRESHOLD: u32 = 5_000;
```

## 🔧 Public API Functions

| Function | Purpose | Returns |
|----------|---------|---------|
| `update_heartbeat(env, asset, admin)` | Manually record timestamp | `Result<(), ContractError>` |
| `is_data_fresh(env, asset)` | Check if within interval | `bool` |
| `get_last_update_timestamp(env, asset)` | Get last update time | `Option<u64>` |
| `set_heartbeat_interval(env, interval, admin)` | Configure interval | `Result<(), ContractError>` |
| `get_heartbeat_interval(env)` | Get current interval | `u64` |
| `finalize_consensus(env)` | Clean up temp storage | `()` |

## 🎯 Common Patterns

### Pattern 1: Update with Telemetry
```rust
pub fn set_value(env: Env, new_value: u64, caller: Address, ...) -> Result<(), ContractError> {
    // 1. Validate and authorize
    let mut data = Self::get_data(env.clone())?;
    if data.admin != caller { return Err(ContractError::NotAdmin); }
    caller.require_auth();
    
    // 2. Update state
    data.value = new_value;
    env.storage().instance().set(&DATA_KEY, &data);
    
    // 3. Record telemetry (temporary storage)
    Self::_record_heartbeat(&env, symbol_short!("VALUE"));
    
    Ok(())
}
```

### Pattern 2: Freshness Validation
```rust
pub fn get_price(env: Env, asset: Symbol) -> Result<u64, ContractError> {
    // 1. Check freshness first
    if !Self::is_data_fresh(env.clone(), asset.clone()) {
        return Err(ContractError::StaleData);
    }
    
    // 2. Safe to use data
    let data = Self::get_data(env)?;
    Ok(data.value)
}
```

### Pattern 3: Custom Interval Configuration
```rust
pub fn initialize_with_custom_interval(env: Env, admin: Address, interval: u64) -> Result<(), ContractError> {
    // 1. Initialize contract
    Self::initialize(env.clone(), admin.clone())?;
    
    // 2. Set custom interval (e.g., 10 minutes)
    Self::set_heartbeat_interval(env, interval, admin)?;
    
    Ok(())
}
```

### Pattern 4: Staleness Recovery
```rust
pub fn try_get_with_fallback(env: Env, asset: Symbol, fallback: u64) -> u64 {
    if Self::is_data_fresh(env.clone(), asset.clone()) {
        Self::get_data(env).map(|d| d.value).unwrap_or(fallback)
    } else {
        // Use fallback for stale data
        fallback
    }
}
```

## ⏱️ Timing Calculations

### Convert Time to Ledgers
```rust
// 1 hour in ledgers (at 5s per ledger)
let one_hour_ledgers = (60 * 60) / 5; // = 720

// 24 hours in ledgers
let one_day_ledgers = 24 * 720; // = 17,280
```

### Convert Ledgers to Time
```rust
// Ledgers to seconds
let seconds = ledgers * 5;

// Ledgers to minutes
let minutes = (ledgers * 5) / 60;

// Ledgers to hours
let hours = (ledgers * 5) / 3600;
```

## 🚨 Common Asset Symbols

| Symbol | Use Case |
|--------|----------|
| `symbol_short!("VALUE")` | Generic value updates |
| `symbol_short!("STAKE")` | Stake registration events |
| `symbol_short!("NGN")` | Nigerian Naira price feed |
| `symbol_short!("KES")` | Kenyan Shilling price feed |
| `symbol_short!("GHS")` | Ghanaian Cedi price feed |
| `symbol_short!("ZAR")` | South African Rand price feed |

## ⚠️ Error Cases

| Error | Scenario | Solution |
|-------|----------|----------|
| `InvalidHeartbeatInterval` | Interval set to 0 | Use interval ≥ 1 second |
| `NotAdmin` | Non-admin tries update | Ensure caller is admin |
| `StaleData` | Data past interval | Wait for next update or trigger manually |

## 🔍 Debugging Tips

### Check Current Timestamp
```rust
let now = env.ledger().timestamp();
```

### Calculate Age
```rust
if let Some(last_update) = Self::get_last_update_timestamp(env.clone(), asset) {
    let age = env.ledger().timestamp().saturating_sub(last_update);
    let interval = Self::get_heartbeat_interval(env.clone());
    
    if age > interval {
        // Stale!
    }
}
```

### Manual Refresh
```rust
// Admin can manually refresh stale data
client.update_heartbeat(&symbol_short!("ASSET"), &admin);
```

## 📝 Testing Checklist

```rust
#[test]
fn test_telemetry() {
    let env = Env::default();
    env.mock_all_auths();
    
    // 1. Initialize
    let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
    let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);
    client.initialize(&admin);
    
    // 2. Update and verify fresh
    client.update_heartbeat(&asset, &admin);
    assert!(client.is_data_fresh(&asset));
    
    // 3. Fast-forward and verify stale
    let interval = client.get_heartbeat_interval();
    advance_ledger_timestamp(&env, interval + 1);
    assert!(!client.is_data_fresh(&asset));
    
    // 4. Refresh and verify fresh again
    client.update_heartbeat(&asset, &admin);
    assert!(client.is_data_fresh(&asset));
}
```

## 💡 Best Practices

### ✅ DO
- Record heartbeat after every state change
- Check freshness before using data
- Use descriptive asset symbols
- Set interval to match update frequency
- Test staleness scenarios

### ❌ DON'T
- Set interval to 0 (will fail)
- Forget to record heartbeat
- Use temporary storage for permanent data
- Skip freshness validation
- Assume data is always current

## 🔗 Storage Type Decision Tree

```
Need to store data?
│
├─ Short-lived (< 24 hours)?
│  ├─ Yes → Use TEMPORARY storage ✅
│  │        (heartbeats, caches)
│  │
│  └─ No → Go to next question
│
├─ Configuration data?
│  ├─ Yes → Use INSTANCE storage ✅
│  │        (admin, intervals, settings)
│  │
│  └─ No → Go to next question
│
└─ Long-term user data?
   └─ Yes → Use PERSISTENT storage ✅
            (stakes, profiles, balances)
```

## 📚 Related Files

- **Implementation:** `src/lib.rs`
- **Consensus Module:** `src/consensus.rs`
- **Tests:** `src/test.rs`
- **Documentation:** `TELEMETRY_STORAGE_REFACTORING.md`
- **Integration Guide:** `src/CONSENSUS_TELEMETRY_INTEGRATION.md`

## 🆘 Quick Troubleshooting

### "Data shows as stale immediately"
- Check that interval is set correctly
- Verify heartbeat is being recorded
- Ensure timestamp comparison logic

### "Data never shows as stale"
- Verify interval isn't too large
- Check time advancement in tests
- Confirm get_heartbeat_interval() returns expected value

### "Heartbeat not updating"
- Verify admin authorization
- Check _record_heartbeat() is called
- Ensure no early returns before heartbeat call

### "TTL not extending"
- Confirm temporary storage usage
- Check extend_ttl() parameters
- Verify threshold and TTL constants

---

**Quick Access:** Keep this reference handy when working with telemetry tracking in the StellarFlow contracts.
