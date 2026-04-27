# Proxy Pattern Documentation

This directory contains comprehensive documentation for the Proxy Pattern (Storage/Logic Separation) implementation in the StellarFlow Price Oracle contract.

## Files

### 1. [IMPLEMENTATION.md](./PROXY_PATTERN_IMPLEMENTATION.md)
Detailed architecture and design documentation covering:
- Pattern overview and benefits
- Architecture diagrams
- Storage layer design
- Cross-contract communication
- Migration path
- Security considerations
- Future enhancements

**Best for**: Understanding the overall design and architecture

### 2. [QUICK_START.md](./PROXY_PATTERN_QUICK_START.md)
Developer quick reference guide with:
- Project structure overview
- Building instructions
- Storage contract API reference
- Common usage patterns
- Error handling
- Testing guide
- Deployment steps
- Troubleshooting

**Best for**: Getting started quickly and finding common patterns

### 3. [SUMMARY.md](./PROXY_PATTERN_SUMMARY.md)
Implementation summary including:
- Issue resolution details
- What was implemented
- Architecture overview
- Benefits summary
- Technical details
- Build status
- Next steps
- File changes

**Best for**: High-level overview of what was delivered

### 4. [INTEGRATION_EXAMPLE.rs](./PROXY_PATTERN_INTEGRATION_EXAMPLE.rs)
Example Rust code showing:
- How to use the storage contract client
- Integration patterns
- Error handling examples
- Authorization patterns
- Subscriber management examples
- Key implementation points

**Best for**: Learning by example and copy-paste patterns

## Quick Navigation

### I want to...

- **Understand the architecture** → Read [IMPLEMENTATION.md](./PROXY_PATTERN_IMPLEMENTATION.md)
- **Get started quickly** → Read [QUICK_START.md](./PROXY_PATTERN_QUICK_START.md)
- **See code examples** → Check [INTEGRATION_EXAMPLE.rs](./PROXY_PATTERN_INTEGRATION_EXAMPLE.rs)
- **Know what was delivered** → Read [SUMMARY.md](./PROXY_PATTERN_SUMMARY.md)

## Key Concepts

### Storage/Logic Separation

The proxy pattern separates the price oracle into two contracts:

1. **Storage Contract** (Immutable)
   - Manages all persistent, temporary, and instance storage
   - Provides type-safe storage operations
   - Cannot be upgraded
   - Auditable and trustless

2. **Logic Contract** (Upgradeable)
   - Contains business logic and calculations
   - Calls storage contract for data access
   - Can be upgraded without touching storage
   - Maintains authorization and event emission

### Benefits

✅ **Immutable Storage**: Storage contract remains unchanged, providing audit confidence
✅ **Upgradeable Logic**: Logic can be upgraded independently
✅ **Clear Separation**: Storage concerns isolated from business logic
✅ **Easier Auditing**: Auditors can verify storage independently
✅ **Reduced Risk**: Minimal storage contract reduces attack surface

## Storage Contract API

### Admin Operations
```rust
set_admin(admin: Address)
get_admin() -> Result<Address, Error>
is_admin(address: Address) -> bool
```

### Price Operations
```rust
set_verified_price(asset: Symbol, price: PriceData)
get_verified_price(asset: Symbol) -> Result<PriceData, Error>
set_community_price(asset: Symbol, price: PriceData)
get_community_price(asset: Symbol) -> Result<PriceData, Error>
```

### Asset Operations
```rust
add_asset(asset: Symbol) -> Result<(), Error>
get_all_assets() -> Vec<Symbol>
get_asset_count() -> u32
set_asset_meta(asset: Symbol, meta: AssetMeta)
get_asset_meta(asset: Symbol) -> Result<AssetMeta, Error>
```

### Subscriber Operations
```rust
subscribe(callback_contract: Address) -> Result<(), Error>
unsubscribe(callback_contract: Address) -> Result<(), Error>
get_subscribers() -> Vec<Address>
```

### Initialization
```rust
initialize(admin: Address) -> Result<(), Error>
is_initialized() -> bool
```

## Building the Storage Contract

```bash
cd contracts/price-oracle-storage
cargo build --target wasm32-unknown-unknown --release
```

## Testing

```bash
cd contracts/price-oracle-storage
cargo test
```

## Related Resources

- **Issue**: [#193 - Proxy Pattern Support for Immutable Auditing](https://github.com/StellarFlow-Network/stellarflow-contracts/issues/193)
- **PR**: [#244 - Proxy Pattern Implementation](https://github.com/StellarFlow-Network/stellarflow-contracts/pull/244)
- **Storage Contract**: `contracts/price-oracle-storage/`
- **Soroban Docs**: https://developers.stellar.org/docs/learn/soroban

## Next Steps

### Phase 2: Logic Contract Refactoring
- Update price-oracle to use storage contract client
- Replace direct storage access with cross-contract calls
- Maintain all existing functionality
- Update tests for separated contracts

### Phase 3: Integration & Testing
- Integration tests with storage contract
- Cross-contract call tests
- End-to-end price update flow tests
- Subscriber notification tests

### Phase 4: Deployment & Verification
- Deploy storage contract (immutable)
- Initialize storage
- Deploy logic contract
- Configure logic contract with storage address
- Verify all operations work correctly

## Questions?

Refer to the troubleshooting section in [QUICK_START.md](./PROXY_PATTERN_QUICK_START.md) or check the [INTEGRATION_EXAMPLE.rs](./PROXY_PATTERN_INTEGRATION_EXAMPLE.rs) for common patterns.
