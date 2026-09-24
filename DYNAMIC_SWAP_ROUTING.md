# Dynamic Swap Routing Across AMM Pools (Issue #926)

## Overview

`src/router/dynamic.rs` adds a routing layer on top of the individual
constant-product AMM pools. Instead of hard-coding a trading path, callers
deposit the pools they trust into a registry and let the router choose the path
that returns the most output for a given input — i.e. the path that **minimises
slippage**.

The router traverses paths up to **3 hops deep** (`MAX_ROUTE_DEPTH`), quotes
every candidate path against the current reserves, and atomically executes the
winner after all safety guards pass.

## Acceptance criteria mapping

| Criterion | Where it is enforced |
| --- | --- |
| Traverse trading paths up to 3 hops deep | `find_best_route` (3 nested hop loops), `MAX_ROUTE_DEPTH = 3` |
| Verify balance delta `B_out >= B_min_expected` | `execute_swap` guard (1), reverts `SlippageExceeded` |
| Assert `k₁ · k₂ · k₃ >= k_initial` across intermediate pools | `quote_hop` per-hop `k_after >= k_before`; aggregate check in `execute_swap` guard (3), reverts `InvariantViolation` |
| Revert `ContractError::SlippageExceeded` when the effective execution price strays outside tolerance | `execute_swap` guard (2), comparing `price_impact_bps` against the stricter of the caller tolerance and the governance ceiling |

## Data model

### `PoolEdge`

A directed pool edge. A pool that can swap both `A -> B` and `B -> A` is
registered once per direction.

```rust
pub struct PoolEdge {
    pub pool: Address,     // registry key
    pub asset_in: AssetId,
    pub asset_out: AssetId,
    pub reserve_in: u64,
    pub reserve_out: u64,
    pub fee_bps: u32,
}
```

### `RouteQuote`

The winning route plus its accounting: the ordered `hops`, the realised
`amount_out`, the path `price_impact_bps`, and the aggregate invariant products
`initial_k_product` / `final_k_product`.

## Pricing

Each hop deducts the pool fee from the input, then applies the constant-product
formula with floor rounding (dust favours the pool):

```
in_after_fee = amount_in · (10_000 − fee_bps) / 10_000
amount_out   = reserve_out · in_after_fee / (reserve_in + in_after_fee)
k_before     = reserve_in · reserve_out
k_after      = (reserve_in + amount_in) · (reserve_out − amount_out)
```

Because the **full** pre-fee `amount_in` is credited to `reserve_in`, `k_after`
is non-decreasing for every `fee_bps < 10_000`. Across a path the aggregate
product is accumulated with saturating multiplication — which is monotonic — so
`∏k_after >= ∏k_before` holds whenever each hop does.

Widening safety: two `u64` reserves multiplied as `u128` are lossless because
`(2⁶⁴ − 1)² = 2¹²⁸ − 2⁶⁵ + 1 < 2¹²⁸`.

## Path discovery

`find_best_route` walks the pool graph with one nested loop per depth level
(depth is compile-time bounded, so the search has a fixed cost shape).
Infeasible paths (illiquid hops, zero output) are skipped so a single dead pool
cannot block an otherwise valid route. The candidate set is capped at
`MAX_CANDIDATE_POOLS = 8` to keep the worst case well inside the Soroban CPU
budget.

The winning path is the one with the greatest realised `amount_out`. This is the
slippage-minimising objective: for a fixed input size, only the path with the
best effective price can produce the largest output.

## Execution flow

```
execute_swap(trader, source, destination, amount_in, min_amount_out, max_price_impact_bps)
  │
  ├─ require_auth(trader) + reentrancy guard
  ├─ config.enabled?            ── no ─▶ ContractPaused
  ├─ find_best_route(...)       ── none ─▶ PoolNotFound
  ├─ amount_out >= min_amount_out          ── no ─▶ SlippageExceeded   (B_out >= B_min)
  ├─ price_impact_bps <= tolerance         ── no ─▶ SlippageExceeded   (price tolerance)
  ├─ final_k_product >= initial_k_product  ── no ─▶ InvariantViolation (path invariant)
  ├─ persist post-trade reserves per hop
  └─ emit ("dynswap", trader) event
```

Every guard runs **before** the first storage write, so a rejected swap leaves
no partial state (and Soroban reverts the transaction atomically regardless).

## Contract entry points

| Function | Access | Purpose |
| --- | --- | --- |
| `register_amm_pool(admin, edge)` | admin | Register/refresh a pool edge |
| `remove_amm_pool(admin, pool)` | admin | Remove a pool edge |
| `get_amm_pool(pool)` | view | Read one edge |
| `get_amm_pools()` | view | Read every registered edge |
| `set_swap_router_config(admin, config)` | admin | Hop depth, price ceiling, kill switch |
| `get_swap_router_config()` | view | Read the config |
| `quote_best_swap_route(source, destination, amount_in, max_hops)` | view | Simulate the best path |
| `execute_dynamic_swap(trader, source, destination, amount_in, min_amount_out, max_price_impact_bps)` | trader | Execute the best path |

## Configuration

```rust
RouterConfig {
    max_price_impact_bps: 500,   // 5% — default governance ceiling
    max_hops: 3,                 // MAX_ROUTE_DEPTH
    enabled: true,               // global kill switch
}
```

`max_hops` must be in `1..=MAX_ROUTE_DEPTH` and `max_price_impact_bps` in
`0..=10_000`; anything else is rejected with `InvalidInput`.

## Error codes

| Variant | Code | Raised when |
| --- | --- | --- |
| `ZeroSwapAmount` | 79 | Zero input, or input entirely consumed by fee |
| `PoolNotFound` | 80 | No registered pool / no connected path |
| `RouteExecutionFailed` | 81 | A hop fails during execution |
| `InvariantViolation` | 82 | `k_new < k_old`, or the aggregate path product decreased |
| `SlippageExceeded` | 47 | Balance delta or price tolerance violated |
| `InsufficientLiquidityDepth` | 33 | Empty reserves or output rounds to zero |
| `ContractPaused` | 34 | Router disabled |

> Note: `ZeroSwapAmount`, `PoolNotFound`, `RouteExecutionFailed` and
> `InvariantViolation` were already referenced by existing modules
> (`router::multihop`, `amm::invariant`, `settlement::htlc`, `amm::ticks`) but
> had no definition in `ContractError`. They are defined here so those call
> sites compile.

## Testing

Unit tests live alongside the module in `src/router/dynamic.rs`:

- hop pricing matches the closed-form constant-product result;
- zero input / empty pool / same-asset / over-100% fee rejection;
- the router prefers a 2-hop path when it beats a direct pool;
- hop depth is respected (`max_hops = 1` forces a direct route);
- disconnected graphs revert `PoolNotFound`;
- aggregate path invariant is non-decreasing for a 3-hop route;
- storage-backed execution guards: `SlippageExceeded` on insufficient output,
  `SlippageExceeded` on excessive price impact, `ContractPaused` when disabled,
  and reserves update correctly on success.

Run with:

```bash
cargo test -p stellarflow-contracts router::dynamic
```
