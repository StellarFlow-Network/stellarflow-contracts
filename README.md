# StellarFlow Contracts - Time-Locked Upgrade Implementation

This repository contains smart contracts for the StellarFlow Network with a time-locked upgrade mechanism to prevent "flash-upgrades" by enforcing a 48-hour delay between contract upgrade proposals and execution.

## Features

- **Time-Locked Upgrades**: 48-hour mandatory delay between upgrade proposal and execution
- **Pending State Management**: Secure storage of new WASM hash in pending state
- **Timestamp Validation**: Uses `ledger().timestamp()` for accurate time validation
- **Admin-Only Operations**: Only contract administrators can propose and execute upgrades
- **Upgrade Cancellation**: Ability to cancel pending upgrades before execution
- **Timelock Monitoring**: Functions to check remaining timelock time

## Architecture

### Core Components

1. **PendingUpgrade Struct**: Stores information about pending upgrades
   - `new_wasm_hash`: The hash of the new contract code
   - `proposed_at`: Timestamp when the upgrade was proposed
   - `proposer`: Address of who proposed the upgrade

2. **ContractData Struct**: Stores contract state
   - `admin`: Administrator address with upgrade permissions
   - `value`: Sample storage value for testing

### Key Functions

- `initialize()`: Sets up the contract with an admin address
- `propose_upgrade()`: Initiates the 48-hour timelock period
- `execute_upgrade()`: Executes the upgrade after timelock expires
- `cancel_upgrade()`: Cancels a pending upgrade
- `get_pending_upgrade()`: Retrieves pending upgrade information
- `get_upgrade_timelock_remaining()`: Returns remaining timelock time

## Security Features

### Flash Upgrade Prevention

The contract prevents flash upgrades through:

1. **48-Hour Timelock**: Mandatory delay between proposal and execution
2. **Pending State Storage**: New WASM hash stored in pending state until timelock expires
3. **Timestamp Validation**: Uses Stellar ledger timestamp for accurate time measurement
4. **Authorization Checks**: Only admin can propose/execute upgrades

### Access Control

- **Admin-Only Operations**: Critical functions require admin authorization
- **Proposal Tracking**: All proposals are tracked with proposer identity
- **Cancellation Rights**: Admin can cancel pending upgrades

## Usage Example

```rust
// Initialize contract
contract.initialize(&admin_address);

// Propose upgrade (starts 48-hour timelock)
let new_wasm_hash = BytesN::from_array(&env, &[1u8; 32]);
contract.propose_upgrade(&new_wasm_hash, &admin_address);

// Check timelock status
let remaining = contract.get_upgrade_timelock_remaining();
println!("Time remaining: {} seconds", remaining.unwrap());

// After 48 hours, execute upgrade
contract.execute_upgrade(&admin_address);
```

## Testing

The contract includes comprehensive tests covering:

- Basic functionality and initialization
- Upgrade proposal and execution flow
- Timelock enforcement and countdown
- Unauthorized operation prevention
- Upgrade cancellation

Run tests with:

```bash
cargo test
```

## Technical Requirements Met

✅ **Ledger Timestamp Validation**: Uses `ledger().timestamp()` for time validation  
✅ **Pending State Storage**: New WASM hash stored in pending state before commitment  
✅ **48-Hour Delay**: Enforced delay between proposal and execution  
✅ **Flash Upgrade Prevention**: Complete protection against immediate upgrades  

## Build and Deploy

```bash
# Build the contract
cargo build --target wasm32-unknown-unknown --release

# Deploy to Stellar network
stellar contract deploy --wasm target/wasm32-unknown-unknown/release/stellarflow_contracts.wasm
```

## License

This project is part of the StellarFlow Network ecosystem.
