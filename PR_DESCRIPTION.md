# Pull Request Descriptions

---

## PR 1 — feat/verified-community-price-buckets

**Branch:** `feat/verified-community-price-buckets`
**Base:** `main`

### Summary

Splits price storage into two isolated `DataKey` buckets to prevent accidental overwrites between verified and community-submitted prices.

### Motivation

Previously all prices shared a single flat `PriceData` map under `DataKey::PriceData`. A community submission could silently overwrite a verified price, corrupting the data used by internal math and downstream consumers.

### Changes

**`contracts/price-oracle/src/types.rs`**
- Added `DataKey::VerifiedPrice(Symbol)` — written only by whitelisted providers and admins; used by all internal math.
- Added `DataKey::CommunityPrice(Symbol)` — written by any caller; never used in internal math.
- Added `DataKey::AssetDescription(Symbol)` — was referenced in `lib.rs` but missing from the enum.

**`contracts/price-oracle/src/lib.rs`**
- `get_price(env, asset, verified: bool)` — `true` reads `VerifiedPrice` (default), `false` reads `CommunityPrice`.
- `get_price_safe`, `get_price_with_status`, `get_prices`, `get_prices_with_status`, `get_last_price` — all read from `VerifiedPrice`.
- `update_price` — writes exclusively to `VerifiedPrice`.
- `set_price` — writes exclusively to `VerifiedPrice`.
- `add_asset` — initialises zero-price placeholder in `VerifiedPrice`.
- `remove_asset` — cleans up both `VerifiedPrice` and `CommunityPrice` atomically.
- New `submit_community_price(source, asset, price, decimals, ttl)` — open to any caller, writes to `CommunityPrice` only.
- Fixed duplicate `Error` discriminant (`NotAuthorized` and `FlashCrashDetected` both had value `5`).
- Fixed `toggle_pause`, `register_admin`, `remove_admin` — moved duplicate-address check before `require_auth()` to avoid `Abort` instead of a proper contract error; replaced `_require_authorized` (panics) with `_is_authorized` (returns bool) for proper error propagation.

**`contracts/price-oracle/src/test.rs`**
- Fixed pre-existing corrupted test bodies (interleaved test functions from a bad merge).
- Updated all `get_price` / `try_get_price` call sites to pass the new `verified: bool` parameter.
- Fixed `set_price` / `update_price` call sites with missing arguments.
- Fixed `toggle_pause` assertions (`Ok(true/false)` → `true/false`).

### Testing

```
cargo test --manifest-path contracts/price-oracle/Cargo.toml
# 133 passed; 0 failed
```

---

## PR 2 — feat/cross-call-volatility-events

**Branch:** `feat/cross-call-volatility-events`
**Base:** `main` (or `feat/verified-community-price-buckets`)

### Summary

Publishes a dedicated `cross_call` event topic whenever a verified price moves more than 5%, enabling downstream contracts (e.g. liquidation bots) to subscribe to volatility signals without polling.

### Motivation

Liquidation bots and risk engines need to react to large price moves in real time. Rather than polling `get_price` every ledger, they can subscribe to the specific `("cross_call", asset_symbol)` topic pair and only wake up when a meaningful move occurs.

### Changes

**`contracts/price-oracle/src/lib.rs`**
- Added constant `VOLATILITY_THRESHOLD_BPS: i128 = 500` (5% = 500 basis points).
- In `update_price`, after the new price is committed to `VerifiedPrice`, emit:

```rust
env.events().publish(
    (Symbol::new(&env, "cross_call"), asset.clone()),
    (old_price, price, pct_change_bps),
);
```

  only when `pct_change_bps > VOLATILITY_THRESHOLD_BPS` and `old_price > 0`.

- The topic pair `("cross_call", asset_symbol)` is the stable subscription key for downstream contracts.
- The data payload `(old_price, new_price, pct_change_bps)` gives consumers everything needed to act without a follow-up read.

**`contracts/price-oracle/src/test.rs`**
- `test_update_price_emits_cross_call_event_on_5pct_move` — verifies the event fires on a >5% move.
- `test_update_price_no_cross_call_event_below_5pct` — verifies the event is silent on a <5% move.

### Example consumer pattern

```rust
// In a Liquidation Bot contract
let oracle = StellarFlowClient::new(&env, &oracle_address);

// Subscribe by filtering events with topic[0] == "cross_call" and topic[1] == asset
// When triggered, read the current price and evaluate positions
let price = oracle.get_price(&asset, &true)?;
// ... liquidation logic
```

### Testing

```
cargo test --manifest-path contracts/price-oracle/Cargo.toml
# 135 passed; 0 failed
```

---

## PR 3 — feat/relayer-gas-compensation-tank

**Branch:** `feat/relayer-gas-compensation-tank`
**Base:** `main` (or previous feature branches)

### Summary

Implements a centralized gas tank escrow contract where third-party consumers can pre-fund gas allowances and configures the Price Oracle to automatically trigger relayer payouts right after price updates hit the ledger.

### Motivation

Relayers incur on-chain network transaction fees to continuously upload price updates, which can quickly drain their operation accounts. By introducing a centralized gas tank, third-party consumers of the oracle's price feeds can pre-fund fee allowances, ensuring sustainable decentralized relayer operations.

### Changes

**`Cargo.toml`**
- Registered the new `"contracts/gas-tank"` crate as a member of the cargo workspace.

**`contracts/gas-tank` [NEW]**
- Implemented `deposit` and `withdraw` entrypoints allowing consumers to pre-fund and reclaim token assets.
- Implemented `set_allowance` and `get_allowance` to let consumers set per-update limits for individual relayers.
- Implemented the `reimburse` loop, callable only by the authorized Price Oracle, which iterates through active funders and transfers funds (up to the consumer's available balance and allowance) to the relayer.
- Structured with a custom `#[contracterror]` enum, returning `Result<(), Error>` from all entrypoints to support clean error propagation and test assertion without causing host aborts.

**`contracts/price-oracle/src/types.rs`**
- Added the `GasTank` storage slot to the `DataKey` enum to persist the registered Gas Tank address.

**`contracts/price-oracle/src/lib.rs`**
- Added `set_gas_tank` and `get_gas_tank` admin functions.
- Modified `update_price` to check if a Gas Tank address is configured, and if so, automatically trigger the Gas Tank's `reimburse` loop for the calling provider.

**`contracts/gas-tank/src/test.rs` [NEW]**
- Implemented a suite of 10 tests covering:
  - Token deposits and withdrawals.
  - Allowance configurations.
  - Multi-consumer allowances and balance-capped reimbursement payouts.
  - Unauthorized access rejection.

### Testing

```bash
cargo test -p gas-tank
# 10 passed; 0 failed
```

