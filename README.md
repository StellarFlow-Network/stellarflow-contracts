# StellarFlow Contracts - Time-Locked Upgrade Implementation

This repository contains smart contracts for the StellarFlow Network with a time-locked upgrade mechanism to prevent "flash-upgrades" by enforcing a 48-hour delay between contract upgrade proposals and execution.

## Features

- **Time-Locked Upgrades**: 48-hour mandatory delay between upgrade proposal and execution
- **Pending State Management**: Secure storage of new WASM hash in pending state
- **Timestamp Validation**: Uses `ledger().timestamp()` for accurate time validation
- **Admin-Only Operations**: Only contract administrators can propose and execute upgrades
- **Upgrade Cancellation**: Ability to cancel pending upgrades before execution
- **Timelock Monitoring**: Functions to check remaining timelock time
- **Storage Optimization**: Vault storage footprint audit completed for sub-map balance tracking.

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
- `get_pending_upgrade()`: Returns pending upgrade info
