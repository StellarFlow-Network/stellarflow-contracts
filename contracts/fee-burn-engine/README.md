# Protocol Fee Auto-Burn Engine for Platform Tokens

A Soroban smart contract that routes a configurable portion of collected protocol swap fees into a designated burn module, automatically invokes the token `burn()` entrypoint to permanently reduce total supply, and emits a `TokensBurned` event with the burnt amount and updated supply metrics.

## Features

- **Fee Routing**: Route collected protocol swap fees into the burn engine per platform token
- **Automatic Burn**: The engine automatically triggers the token `burn()` entrypoint, permanently removing tokens from circulation
- **Configurable Burn Ratio**: Set the portion (in basis points, 10000 = 100%) of routed fees destined for destruction
- **Auto-Burn Threshold**: Burn the accumulated fee pool the moment it reaches a configured threshold
- **Partial Burn**: Ignite an exact sub-amount of the accumulated pool
- **Supply Metrics**: Track blazing summary — `total_burnt` and `remaining_supply` per token
- **TokensBurned Event**: Structured event carrying burnt count, cumulative burnt, and updated remaining supply
- **Token Agnostic**: Works with any Soroban token that exposes the burnable interface

## Architecture

### Data Structures

- **BurnModule**: Per-token configuration holding the burn ratio, accumulated fee pool, cumulative burnt amount, remaining supply, auto-burn threshold and the authorized burn-holder address
- **DataKey::Admin**: Single storage key for the engine administrator

### Key Functions

**Initialization:**
- `initialize(admin)`: Initialize the engine with an admin address

**Burn Module Configuration:**
- `register_burn_module(admin, token, burn_module, burn_ratio_bps)`: Register a platform token for burning with an initial ratio
- `set_burn_ratio(admin, token, burn_ratio_bps)`: Update the burn ratio
- `set_auto_burn_threshold(admin, token, threshold)`: Configure the ignition threshold for automatic burning
- `get_burn_module(token)`: Inspect a token's burn module
- `token_balance(token, account)`: Read an on-chain balance for supply-metric visibility

**Fee Routing:**
- `route_fees(admin, token, amount)`: Receive a routed portion of collected swap fees; if the pool meets the auto-burn threshold the burn is triggered immediately

**Automatic Burn:**
- `burn_accumulated_fees(admin, token)`: Burn the entire accumulated fee pool, emitting a `TokensBurned` event
- `burn_exact(admin, token, amount)`: Burn an exact amount from the pool

## TokensBurned Event

Every destruction emits a `TokensBurned` event:

- **Topics**: `(tok_burn, token_address)`
- **Data**: `(burnt_amount, total_burnt, remaining_supply)`

This gives indexers and off-chain monitoring real-time visibility into permanent supply reductions.

## Design Notes

- The `burn_module` address is the holder that must own the fee tokens and authorize the `burn()` call. The engine invokes `token.burn(from = burn_module, amount)`.
- The Stellar Asset Contract refuses to burn from the asset issuer; use a dedicated burn-holder (or the engine's own address) rather than the issuer.
- All state transitions use checked arithmetic and return descriptive errors.

## Error Handling

- `AlreadyInitialized`: Contract already initialized
- `NotInitialized`: Contract not initialized
- `NotAdmin`: Caller is not the admin
- `TokenNotRegistered`: Burn module not registered for the token
- `InvalidAmount`: Amount must be greater than zero
- `InvalidRatio`: Burn ratio exceeds 10000 bps
- `Overflow`: Arithmetic overflow
- `InsufficientFees`: Pool has fewer tokens than requested for burn
- `BurnModuleNotSet`: Burn holder not configured
- `AlreadyRegistered`: Token already has a burn module

## Testing

Run tests with:

```bash
cargo test -p fee-burn-engine
```

## Build and Deploy

```bash
# Build the contract
cargo build --target wasm32-unknown-unknown --release -p fee-burn-engine

# Deploy to Stellar network
stellar contract deploy --wasm target/wasm32-unknown-unknown/release/fee_burn_engine.wasm
```

## License

This project is part of the StellarFlow Network ecosystem.
