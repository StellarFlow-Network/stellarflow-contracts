# Fixed Reward Distribution Splitter

A Soroban smart contract for distributing tokens among multiple recipients according to fixed percentage allocations.

## Features

- **Fixed Percentage Allocations**: Set fixed percentage shares for recipients (in basis points: 10000 = 100%)
- **Admin Controls**: Only admin can manage recipients and allocations
- **Automatic Distribution**: Distribute tokens to all recipients according to their fixed shares
- **Flexible Management**: Add, remove, and update recipient shares
- **Token Agnostic**: Works with any Soroban token contract
- **Default Parameter Reset**: Reset all parameters to their initial default values

## Architecture

### Data Structures

- **Recipient**: Stores recipient address and their fixed share percentage
- **DataKey**: Storage keys for contract state (admin, token, recipients, defaults, etc.)

### Key Functions

- `initialize(admin, token)`: Initialize contract with admin address and token to distribute
- `add_recipient(admin, recipient, share)`: Add a recipient with a fixed share (basis points)
- `remove_recipient(admin, recipient)`: Remove a recipient
- `update_recipient_share(admin, recipient, new_share)`: Update a recipient's share
- `distribute(amount)`: Distribute tokens to all recipients according to their shares
- `get_recipients()`: Get all recipients
- `get_total_shares()`: Get total shares (should equal 10000 for full distribution)
- `get_admin()`: Get admin address
- `get_token()`: Get token address
- `get_default_admin()`: Get the default admin address
- `get_default_token()`: Get the default token address
- `transfer_admin(current_admin, new_admin)`: Transfer admin to new address
- `update_token(admin, new_token)`: Update the token to distribute
- `reset_parameters(admin)`: Reset all parameters to their default values

## Default Parameter Reset

The contract includes a built-in mechanism to reset all parameters to their initial default values. This is useful for:

- Emergency recovery from misconfiguration
- Governance actions to restore contract to original state
- Testing and development scenarios

When `initialize()` is called, the initial admin and token addresses are stored as defaults. The `reset_parameters()` function can then be called by the current admin to restore:

- Admin address to the default admin
- Token address to the default token
- Clear all recipients
- Reset total shares to 0

**Note**: This is a destructive action that cannot be undone. Use with caution.

## Usage Example

```rust
// Initialize contract
let admin = Address::generate(&env);
let token = Address::generate(&env);
contract.initialize(&admin, &token);

// Add recipients with fixed shares (basis points)
let recipient1 = Address::generate(&env);
let recipient2 = Address::generate(&env);
contract.add_recipient(&admin, &recipient1, &5000); // 50%
contract.add_recipient(&admin, &recipient2, &5000); // 50%

// Verify total shares
assert_eq!(contract.get_total_shares(), 10000);

// Distribute 1000 tokens
contract.distribute(&1000);

// Each recipient receives 500 tokens

// Reset parameters to defaults (emergency recovery)
contract.reset_parameters(&admin);

// Contract is now back to initial state
assert_eq!(contract.get_total_shares(), 0);
assert_eq!(contract.get_recipients().len(), 0);
```

## Error Handling

- `AlreadyInitialized`: Contract already initialized
- `Unauthorized`: Caller is not authorized
- `InvalidShare`: Share must be between 1 and 10000
- `TotalSharesExceeded`: Total shares would exceed 10000 (100%)
- `NoRecipients`: No recipients configured
- `InsufficientBalance`: Contract has insufficient token balance
- `ZeroAmount`: Distribution amount must be greater than zero
- `TokenNotSet`: Token address not configured

## Testing

Run tests with:

```bash
cargo test -p reward-splitter
```

## Build and Deploy

```bash
# Build the contract
cargo build --target wasm32-unknown-unknown --release -p reward-splitter

# Deploy to Stellar network
stellar contract deploy --wasm target/wasm32-unknown-unknown/release/reward_splitter.wasm
```

## License

This project is part of the StellarFlow Network ecosystem.
