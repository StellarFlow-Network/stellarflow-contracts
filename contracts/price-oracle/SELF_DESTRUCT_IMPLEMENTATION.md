# Oracle Self-Destruct Cleanup Logic Implementation

## Overview

This document describes the implementation of the Oracle "Self-Destruct" Cleanup Logic for the StellarFlow Price Oracle contract. This feature provides a safe way to clear storage and return remaining funds during migration to a new contract version.

## Technical Requirements Met

✅ **2/3 Multi-Sig Approval**: The implementation requires exactly 2 out of 3 registered admin signatures for the final action.  
✅ **Safe Storage Clearing**: Comprehensive clearing of all contract storage across temporary, persistent, and instance storage.  
✅ **Fund Return Mechanism**: Automatic return of remaining contract funds to a specified recipient.  
✅ **Irreversible Action**: Once executed, the contract is permanently destroyed and unusable.  

## Implementation Details

### Function Signature

```rust
pub fn self_destruct(
    env: Env, 
    admin1: Address, 
    admin2: Address, 
    recipient: Option<Address>
) -> Result<(), Error>
```

### Multi-Signature Validation

The function implements strict 2/3 multi-signature validation:

1. **Distinct Admins**: `admin1` and `admin2` must be different addresses
2. **Authorization**: Both admins must be in the authorized admin list
3. **Minimum Admins**: At least 2 admins must be registered in the system
4. **Cryptographic Signatures**: Both admins must provide valid signatures via `require_auth()`

### Storage Clearing Logic

The implementation performs comprehensive storage clearing:

#### Instance Storage
- Admin list and related keys
- Base currency pairs
- Pending admin transfers
- Recent events and logs
- Initialization flags
- Pause state
- Query fees
- Price update subscribers
- Community council address
- Emergency freeze state

#### Price-Related Storage
For each tracked asset:
- Verified price data (temporary storage)
- Community price data (temporary storage)
- Asset metadata (persistent storage)
- TWAP buffers (persistent storage)
- Price bounds data (persistent storage)
- Price floor data (persistent storage)

#### Provider Storage
- Provider whitelists
- Provider weights
- Active relayers list

#### Global Storage
- Legacy price data maps
- Price buffers
- Global price bounds and floor data

### Fund Return Mechanism

The function includes a robust fund return mechanism:

1. **Balance Check**: Retrieves current contract balance
2. **Recipient Determination**: Uses provided recipient or defaults to `admin1`
3. **Fund Transfer**: Transfers entire balance to recipient
4. **Event Emission**: Emits `RescueTokensEvent` for transparency

### Safety Features

#### Pre-Execution Checks
- Contract not already destroyed
- Contract not in emergency freeze state
- Valid multi-signature authorization
- Minimum admin requirements met

#### Post-Execution State
- `Destroyed` flag set to prevent further operations
- All storage keys removed
- Funds transferred to recipient
- Comprehensive event logging

## Usage Examples

### Basic Self-Destruct (Default Recipient)

```rust
// Two admins authorize destruction, funds go to admin1
let result = PriceOracle::self_destruct(
    env,
    admin1_address,
    admin2_address,
    None  // Default to admin1
);
```

### Self-Destruct with Custom Recipient

```rust
// Two admins authorize destruction, funds go to treasury
let result = PriceOracle::self_destruct(
    env,
    admin1_address,
    admin2_address,
    Some(treasury_address)  // Custom recipient
);
```

## Error Handling

The function returns specific errors for different failure scenarios:

- `MultiSigValidationFailed`: Invalid admin combination or insufficient admins
- `NotAuthorized`: One or both callers are not authorized admins
- `ContractDestroyed`: Contract already destroyed
- Contract will be in frozen state (handled by `_require_not_frozen`)

## Event Emission

The function emits two types of events:

1. **RescueTokensEvent**: When funds are returned to recipient
2. **contract_destroyed**: Comprehensive destruction event with:
   - Admin1 address
   - Admin2 address  
   - Final recipient address
   - Amount transferred

## Security Considerations

### Multi-Sig Protection
- Prevents single admin compromise
- Requires collusion between at least 2 admins
- Maintains security during migration scenarios

### Fund Safety
- All remaining funds are automatically returned
- Recipient can be explicitly specified
- Full transparency via event emission

### Irreversibility
- Once executed, contract cannot be recovered
- All storage is permanently wiped
- Destroyed flag prevents any future operations

## Migration Workflow

1. **Preparation**: Deploy new oracle contract version
2. **Data Migration**: Migrate necessary price data to new contract
3. **Fund Transfer**: Move operational funds to new contract
4. **Self-Destruct**: Execute self-destruct on old contract with 2/3 admin signatures
5. **Verification**: Confirm old contract is destroyed and funds returned

## Testing

The implementation includes comprehensive test coverage in `self_destruct_test.rs`:

- Multi-signature validation tests
- Storage clearing verification
- Fund return mechanism testing
- Error condition handling

## Integration

The function is integrated into the main `StellarFlowTrait` interface, making it available to:

- Admin dashboard interfaces
- Migration scripts
- Emergency response procedures
- Automated deployment systems

## Conclusion

This implementation provides a secure, transparent, and comprehensive solution for oracle contract migration. The 2/3 multi-signature requirement ensures that no single admin can unilaterally destroy the contract, while the fund return mechanism guarantees that no funds are left stranded during migration.
