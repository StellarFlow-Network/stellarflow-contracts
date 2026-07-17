# Fixed Reward Distribution Splitter

A Soroban smart contract for distributing tokens among multiple recipients according to fixed percentage allocations with built-in multi-stage cooldown gateway for governance actions.

## Features

- **Fixed Percentage Allocations**: Set fixed percentage shares for recipients (in basis points: 10000 = 100%)
- **Admin Controls**: Only admin can manage recipients and allocations
- **Automatic Distribution**: Distribute tokens to all recipients according to their fixed shares
- **Flexible Management**: Add, remove, and update recipient shares
- **Token Agnostic**: Works with any Soroban token contract
- **Default Parameter Reset**: Reset all parameters to their initial default values
- **Multi-Stage Cooldown Gateway**: Protocol-safety mechanism with time-delayed governance executions

## Architecture

### Data Structures

- **Recipient**: Stores recipient address and their fixed share percentage
- **DataKey**: Storage keys for contract state (admin, token, recipients, defaults, etc.)

### Key Functions

**Core Functions:**
- `initialize(admin, token)`: Initialize contract with admin address and token to distribute
- `add_recipient(admin, recipient, share)`: Add a recipient with a fixed share (basis points)
- `remove_recipient(admin, recipient)`: Remove a recipient
- `update_recipient_share(admin, recipient, new_share)`: Update a recipient's share
- `distribute(amount)`: Distribute tokens to all recipients according to their shares

**Getter Functions:**
- `get_recipients()`: Get all recipients
- `get_total_shares()`: Get total shares (should equal 10000 for full distribution)
- `get_admin()`: Get admin address
- `get_token()`: Get token address
- `get_default_admin()`: Get the default admin address
- `get_default_token()`: Get the default token address

**Admin Functions:**
- `transfer_admin(current_admin, new_admin)`: Transfer admin to new address
- `update_token(admin, new_token)`: Update the token to distribute
- `reset_parameters(admin)`: Reset all parameters to their default values

**Multi-Stage Cooldown Gateway Functions:**
- `propose_action(admin, action_type, data)`: Propose a governance action through the cooldown gateway
- `advance_action(admin, action_id)`: Advance an action to the next cooldown stage
- `execute_action(admin, action_id)`: Execute a governance action after all cooldown stages complete
- `cancel_action(admin, action_id)`: Cancel a pending governance action
- `get_action(action_id)`: Get the status of a governance action
- `get_cooldown_remaining(action_id)`: Get the cooldown time remaining for the current stage
- `configure_cooldown_stage(admin, stage_number, cooldown_seconds, description)`: Configure a cooldown stage
- `get_cooldown_stage(stage_number)`: Get a cooldown stage configuration

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

## Multi-Stage Cooldown Gateway

The contract includes a sophisticated multi-stage cooldown gateway for protocol-safe governance executions. This mechanism prevents rushed or malicious governance actions by requiring multiple time-delayed approval stages before execution.

### Cooldown Stages

The contract uses three default cooldown stages:

1. **Stage 1 (1 hour)**: Initial proposal stage - allows community review
2. **Stage 2 (8 hours)**: Review stage - provides time for security analysis
3. **Stage 3 (24 hours)**: Final approval stage - final cooldown before execution

### Supported Action Types

- **UpdateToken**: Update the token address for distribution
- **ResetParameters**: Reset all parameters to default values
- **EmergencyStop**: Emergency stop mechanism (placeholder for future implementation)

### Usage Example

```rust
// Propose a governance action
let action_id = contract.propose_action(
    &admin,
    &CooldownActionType::ResetParameters,
    &String::from_str(&env, "Emergency reset"),
);

// Wait for stage 1 cooldown (1 hour)
env.ledger().set_timestamp(env.ledger().timestamp() + 4000);
contract.advance_action(&admin, &action_id);

// Wait for stage 2 cooldown (8 hours)
env.ledger().set_timestamp(env.ledger().timestamp() + 29000);
contract.advance_action(&admin, &action_id);

// Wait for stage 3 cooldown (24 hours)
env.ledger().set_timestamp(env.ledger().timestamp() + 87000);
contract.advance_action(&admin, &action_id);

// Execute the action
contract.execute_action(&admin, &action_id);

// Check action status
let action = contract.get_action(&action_id).unwrap();
assert_eq!(action.executed, true);
```

### Configuration

Admins can configure custom cooldown periods for each stage:

```rust
contract.configure_cooldown_stage(
    &admin,
    &1,
    &7200, // 2 hours
    &String::from_str(&env, "Extended initial stage"),
);
```

### Security Benefits

- **Prevents Flash Governance**: Multiple time delays prevent rushed decisions
- **Community Review**: Each stage provides time for community oversight
- **Emergency Cancellation**: Actions can be cancelled at any stage
- **Configurable**: Cooldown periods can be adjusted based on risk assessment
- **Audit Trail**: All actions are tracked with timestamps and proposer information

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
- `CooldownNotExpired`: Cooldown period has not expired yet
- `ActionNotFound`: Governance action not found
- `ActionAlreadyExecuted`: Action has already been executed
- `ActionAlreadyCancelled`: Action has already been cancelled
- `InvalidStage`: Invalid cooldown stage number
- `InvalidActionType`: Invalid action type

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
