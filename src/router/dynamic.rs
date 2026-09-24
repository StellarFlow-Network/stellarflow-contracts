//! Dynamic single-hop and multi-hop swap routing across AMM pools (issue #926).
//!
//! This module adds a *routing* layer on top of the individual AMM pools. Given
//! a registry of constant-product pools, it searches every trading path up to
//! [`MAX_ROUTE_DEPTH`] (3) hops deep and picks the one that maximises the
//! trader's realised output — i.e. the path that minimises slippage for the
//! requested input size.
//!
//! # Acceptance criteria
//!
//! 1. **Balance delta** — after the winning path is selected, the realised
//!    output `B_out` is compared against the caller's floor `B_min_expected`.
//!    A shortfall reverts with [`ContractError::SlippageExceeded`].
//! 2. **Path invariant** — every pool on the path must satisfy `k_new >= k_old`
//!    (fees accrue to LPs), so the aggregate product
//!    `k₁ · k₂ · k₃ >= k_initial` never decreases. A violation reverts with
//!    [`ContractError::InvariantViolation`].
//! 3. **Price tolerance** — the effective execution price (measured as price
//!    impact against the zero-impact spot conversion) must stay inside the
//!    caller/governance tolerance, otherwise the swap reverts with
//!    [`ContractError::SlippageExceeded`].
//!
//! # Unit conventions
//!
//! All reserve/amount values are unsigned `u64` stroops. Intermediate products
//! of two `u64` reserves are widened to `u128`, which is lossless because
//! `(2⁶⁴ − 1)² = 2¹²⁸ − 2⁶⁵ + 1 < 2¹²⁸`. Aggregate invariant products are
//! combined with saturating multiplication; saturating multiplication is
//! monotonic, so `∏k_after >= ∏k_before` is preserved whenever each individual
//! factor is non-decreasing.

use soroban_sdk::{contracttype, symbol_short, Address, Env, Symbol, Vec};

use crate::{AssetId, ContractError};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of hops the router will traverse in a single trade path.
///
/// The acceptance criteria require paths up to 3 hops deep; deeper paths add
/// compute cost without meaningfully improving execution on the pool topology
/// this router targets.
pub const MAX_ROUTE_DEPTH: u32 = 3;

/// Upper bound on the number of pools considered per routing call. The search
/// is `O(n³)` in the worst case (three nested hop loops), so the candidate set
/// is capped to keep a single routing call well inside the Soroban CPU budget.
pub const MAX_CANDIDATE_POOLS: u32 = 8;

/// Basis-point denominator (100% == 10 000 bps).
pub const BPS_DENOMINATOR: u32 = 10_000;

/// Default cap on the acceptable effective price impact of a routed swap.
pub const DEFAULT_MAX_PRICE_IMPACT_BPS: u32 = 500; // 5%

/// Event topic for a successfully routed dynamic swap.
pub const EV_DYNAMIC_SWAP: Symbol = symbol_short!("dynswap");

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A directed constant-product AMM pool edge made available to the router.
///
/// Edges are directed: a pool that can swap `A -> B` is registered once for
/// that direction. `reserve_in`/`reserve_out` are the pool's current reserves
/// for `asset_in`/`asset_out` respectively.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct PoolEdge {
    /// Pool contract address (also the registry key).
    pub pool: Address,
    /// Asset sold into this pool.
    pub asset_in: AssetId,
    /// Asset bought out of this pool.
    pub asset_out: AssetId,
    /// Pool reserve of `asset_in`.
    pub reserve_in: u64,
    /// Pool reserve of `asset_out`.
    pub reserve_out: u64,
    /// Swap fee in basis points, deducted from the hop input.
    pub fee_bps: u32,
}

/// Quote for a single executed hop.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct HopQuote {
    pub pool: Address,
    pub asset_in: AssetId,
    pub asset_out: AssetId,
    /// Amount of `asset_in` consumed by this hop.
    pub amount_in: u64,
    /// Amount of `asset_out` delivered by this hop (net of fee).
    pub amount_out: u64,
    /// Fee retained by the pool for this hop.
    pub fee_paid: u64,
    /// Constant-product invariant before the hop: `reserve_in · reserve_out`.
    pub k_before: u128,
    /// Constant-product invariant after the hop.
    pub k_after: u128,
}

/// The winning route discovered by the router, together with its accounting.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct RouteQuote {
    pub source: AssetId,
    pub destination: AssetId,
    /// Ordered hops to execute (1..=[`MAX_ROUTE_DEPTH`]).
    pub hops: Vec<HopQuote>,
    /// Input amount offered to the first hop.
    pub amount_in: u64,
    /// Realised output `B_out` delivered by the final hop.
    pub amount_out: u64,
    /// Effective price impact of the whole path, in basis points.
    pub price_impact_bps: u32,
    /// `k_initial` — product of every pool's pre-swap constant.
    pub initial_k_product: u128,
    /// `k₁ · k₂ · k₃` — product of every pool's post-swap constant.
    pub final_k_product: u128,
}

/// Governance-configurable router parameters.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct RouterConfig {
    /// Hard ceiling on the acceptable price impact of a routed swap.
    pub max_price_impact_bps: u32,
    /// Maximum hops the router may traverse (1..=[`MAX_ROUTE_DEPTH`]).
    pub max_hops: u32,
    /// Global kill switch. When false, all routed swaps revert.
    pub enabled: bool,
}

impl RouterConfig {
    /// Conservative defaults used until governance overrides them.
    pub fn default_config() -> Self {
        Self {
            max_price_impact_bps: DEFAULT_MAX_PRICE_IMPACT_BPS,
            max_hops: MAX_ROUTE_DEPTH,
            enabled: true,
        }
    }
}

/// Storage keys for the dynamic router's pool registry and config.
#[contracttype]
#[derive(Clone)]
pub enum DynamicRouterKey {
    /// A registered pool edge, keyed by pool address.
    Pool(Address),
    /// The ordered list of registered pool addresses.
    Registry,
    /// The active [`RouterConfig`].
    Config,
}

// ---------------------------------------------------------------------------
// Arithmetic helpers
// ---------------------------------------------------------------------------

/// Compute `a · b / divisor` with a checked 128-bit intermediate product.
fn mul_div(a: u128, b: u128, divisor: u128) -> Result<u128, ContractError> {
    if divisor == 0 {
        return Err(ContractError::DivisionByZero);
    }
    let product = a.checked_mul(b).ok_or(ContractError::Overflow)?;
    Ok(product / divisor)
}

/// Reject structurally invalid pool edges before they enter the registry.
fn validate_edge(edge: &PoolEdge) -> Result<(), ContractError> {
    if edge.asset_in == edge.asset_out {
        return Err(ContractError::InvalidInput);
    }
    if edge.reserve_in == 0 || edge.reserve_out == 0 {
        return Err(ContractError::InsufficientLiquidityDepth);
    }
    if edge.fee_bps > BPS_DENOMINATOR {
        return Err(ContractError::InvalidInput);
    }
    Ok(())
}

/// Load the protocol admin from contract state and assert `caller` matches.
fn assert_admin(env: &Env, caller: &Address) -> Result<(), ContractError> {
    let data: crate::ContractData = env
        .storage()
        .instance()
        .get(&crate::DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;
    if data.admin != *caller {
        return Err(ContractError::NotAdmin);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Single-hop pricing
// ---------------------------------------------------------------------------

/// Price a single constant-product hop without touching storage.
///
/// The fee is taken off the input (Uniswap-style), then the constant-product
/// formula `out = reserve_out · in_after_fee / (reserve_in + in_after_fee)` is
/// applied with floor rounding so the pool always keeps the dust. The full
/// pre-fee input is added to `reserve_in`, which guarantees `k` is
/// non-decreasing for any `fee_bps < 10 000`.
///
/// # Errors
/// * [`ContractError::ZeroSwapAmount`] — zero input, or input entirely consumed
///   by the fee.
/// * [`ContractError::InsufficientLiquidityDepth`] — empty reserves or an output
///   that rounds to zero.
/// * [`ContractError::InvalidInput`] — fee above 10 000 bps or `asset_in ==
///   asset_out`.
/// * [`ContractError::InvariantViolation`] — `k_new < k_old` (should be
///   unreachable for well-formed inputs; kept as defence-in-depth).
pub fn quote_hop(edge: &PoolEdge, amount_in: u64) -> Result<HopQuote, ContractError> {
    if amount_in == 0 {
        return Err(ContractError::ZeroSwapAmount);
    }
    validate_edge(edge)?;

    let amount_in_after_fee = mul_div(
        amount_in as u128,
        (BPS_DENOMINATOR - edge.fee_bps) as u128,
        BPS_DENOMINATOR as u128,
    )?;
    if amount_in_after_fee == 0 {
        return Err(ContractError::ZeroSwapAmount);
    }
    let fee_paid = (amount_in as u128) - amount_in_after_fee;

    let new_reserve_in = (edge.reserve_in as u128)
        .checked_add(amount_in as u128)
        .ok_or(ContractError::Overflow)?;
    let numerator = (edge.reserve_out as u128)
        .checked_mul(amount_in_after_fee)
        .ok_or(ContractError::Overflow)?;
    let denominator = (edge.reserve_in as u128)
        .checked_add(amount_in_after_fee)
        .ok_or(ContractError::Overflow)?;
    let amount_out = numerator / denominator;

    // A hop that rounds to zero (or drains the pool) is not usable.
    if amount_out == 0 || amount_out >= edge.reserve_out as u128 {
        return Err(ContractError::InsufficientLiquidityDepth);
    }

    // Constant-product invariant: the fee makes k_before <= k_after.
    let k_before = (edge.reserve_in as u128) * (edge.reserve_out as u128);
    let new_reserve_out = (edge.reserve_out as u128) - amount_out;
    let k_after = new_reserve_in * new_reserve_out;
    if k_after < k_before {
        return Err(ContractError::InvariantViolation);
    }

    Ok(HopQuote {
        pool: edge.pool.clone(),
        asset_in: edge.asset_in,
        asset_out: edge.asset_out,
        amount_in,
        amount_out: amount_out as u64,
        fee_paid: fee_paid as u64,
        k_before,
        k_after,
    })
}

// ---------------------------------------------------------------------------
// Path pricing
// ---------------------------------------------------------------------------

/// Price an explicit ordered path (a slice of indices into `edges`).
///
/// The output of each hop becomes the input of the next. `initial_k_product`
/// and `final_k_product` are the saturating products of the per-hop `k_before`
/// and `k_after` respectively, so callers can assert the aggregate acceptance
/// invariant `k₁ · k₂ · k₃ >= k_initial` in one comparison.
fn quote_path(
    env: &Env,
    edges: &Vec<PoolEdge>,
    path: &[u32],
    amount_in: u64,
    source: AssetId,
    destination: AssetId,
) -> Result<RouteQuote, ContractError> {
    let mut hops: Vec<HopQuote> = Vec::new(env);
    let mut running = amount_in;
    let mut initial_k_product: u128 = 1;
    let mut final_k_product: u128 = 1;
    // Zero-price-impact reference: chained application of the spot reserve
    // ratios (no fee, no curve movement).
    let mut ideal_output: u128 = amount_in as u128;

    for &index in path.iter() {
        let edge = edges.get(index).ok_or(ContractError::PoolNotFound)?;
        let hop = quote_hop(&edge, running)?;

        initial_k_product = initial_k_product.saturating_mul(hop.k_before);
        final_k_product = final_k_product.saturating_mul(hop.k_after);
        ideal_output =
            ideal_output.saturating_mul(edge.reserve_out as u128) / edge.reserve_in as u128;
        running = hop.amount_out;

        hops.push_back(hop);
    }

    // Price impact = how far the realised output falls below the ideal spot
    // conversion, expressed in bps of the ideal.
    let price_impact_bps = if ideal_output > 0 && ideal_output > running as u128 {
        ((ideal_output - running as u128).saturating_mul(BPS_DENOMINATOR as u128) / ideal_output)
            as u32
    } else {
        0
    };

    Ok(RouteQuote {
        source,
        destination,
        hops,
        amount_in,
        amount_out: running,
        price_impact_bps,
        initial_k_product,
        final_k_product,
    })
}

/// Record `candidate` as the new best route when it improves the realised
/// output. Infeasible paths (illiquid hops, zero output) are silently skipped
/// so a single dead pool cannot block an otherwise valid route.
fn consider(
    env: &Env,
    edges: &Vec<PoolEdge>,
    path: &[u32],
    amount_in: u64,
    source: AssetId,
    destination: AssetId,
    best: &mut Option<RouteQuote>,
) {
    if let Ok(quote) = quote_path(env, edges, path, amount_in, source, destination) {
        let improves = match best {
            Some(current) => quote.amount_out > current.amount_out,
            None => true,
        };
        if improves {
            *best = Some(quote);
        }
    }
}

/// Search all simple trading paths up to `max_hops` deep and return the one
/// that maximises the trader's realised output (i.e. minimises slippage).
///
/// The graph is walked with one nested loop per depth level (depth is bounded
/// by [`MAX_ROUTE_DEPTH`], so this stays a fixed-cost search). Pools are never
/// reused within a path, and a path may not immediately return to `source`.
///
/// # Errors
/// * [`ContractError::ZeroSwapAmount`] — zero input.
/// * [`ContractError::InvalidInput`] — `max_hops` out of range, `source ==
///   destination`, or too many candidate pools.
/// * [`ContractError::PoolNotFound`] — no path connects `source` to
///   `destination` through the supplied edges.
pub fn find_best_route(
    env: &Env,
    edges: &Vec<PoolEdge>,
    source: AssetId,
    destination: AssetId,
    amount_in: u64,
    max_hops: u32,
) -> Result<RouteQuote, ContractError> {
    if amount_in == 0 {
        return Err(ContractError::ZeroSwapAmount);
    }
    if source == destination {
        return Err(ContractError::InvalidInput);
    }
    if max_hops == 0 || max_hops > MAX_ROUTE_DEPTH {
        return Err(ContractError::InvalidInput);
    }
    if edges.len() > MAX_CANDIDATE_POOLS {
        return Err(ContractError::InvalidInput);
    }
    if edges.len() == 0 {
        return Err(ContractError::PoolNotFound);
    }

    let mut best: Option<RouteQuote> = None;
    let n = edges.len();

    for i in 0..n {
        let first = edges.get(i).ok_or(ContractError::PoolNotFound)?;
        if first.asset_in != source {
            continue;
        }

        // ── Depth 1 ──────────────────────────────────────────────────────
        if first.asset_out == destination {
            consider(env, edges, &[i], amount_in, source, destination, &mut best);
        }

        if max_hops < 2 {
            continue;
        }

        // ── Depth 2 ──────────────────────────────────────────────────────
        for j in 0..n {
            if j == i {
                continue;
            }
            let second = edges.get(j).ok_or(ContractError::PoolNotFound)?;
            if second.asset_in != first.asset_out || second.asset_out == source {
                continue;
            }

            if second.asset_out == destination {
                consider(
                    env,
                    edges,
                    &[i, j],
                    amount_in,
                    source,
                    destination,
                    &mut best,
                );
            }

            if max_hops < 3 {
                continue;
            }

            // ── Depth 3 ──────────────────────────────────────────────────
            for k in 0..n {
                if k == i || k == j {
                    continue;
                }
                let third = edges.get(k).ok_or(ContractError::PoolNotFound)?;
                if third.asset_in != second.asset_out
                    || third.asset_out != destination
                    || third.asset_out == source
                {
                    continue;
                }
                consider(
                    env,
                    edges,
                    &[i, j, k],
                    amount_in,
                    source,
                    destination,
                    &mut best,
                );
            }
        }
    }

    best.ok_or(ContractError::PoolNotFound)
}

// ---------------------------------------------------------------------------
// Pool registry
// ---------------------------------------------------------------------------

/// Register (or update) a pool edge. Restricted to the protocol admin.
///
/// When the edge's pool address is new it is appended to the ordered registry;
/// re-registering an existing pool only refreshes its reserves.
pub fn register_pool(env: &Env, admin: Address, edge: PoolEdge) -> Result<(), ContractError> {
    admin.require_auth();
    assert_admin(env, &admin)?;
    validate_edge(&edge)?;

    let key = DynamicRouterKey::Pool(edge.pool.clone());
    let exists = env.storage().instance().has(&key);

    if !exists {
        let mut registry: Vec<Address> = env
            .storage()
            .instance()
            .get(&DynamicRouterKey::Registry)
            .unwrap_or_else(|| Vec::new(env));
        if registry.len() >= MAX_CANDIDATE_POOLS {
            return Err(ContractError::InvalidInput);
        }
        registry.push_back(edge.pool.clone());
        env.storage()
            .instance()
            .set(&DynamicRouterKey::Registry, &registry);
    }

    env.storage().instance().set(&key, &edge);
    Ok(())
}

/// Remove a pool edge from the registry. Restricted to the protocol admin.
pub fn remove_pool(env: &Env, admin: Address, pool: Address) -> Result<(), ContractError> {
    admin.require_auth();
    assert_admin(env, &admin)?;

    let key = DynamicRouterKey::Pool(pool.clone());
    if !env.storage().instance().has(&key) {
        return Err(ContractError::PoolNotFound);
    }
    env.storage().instance().remove(&key);

    let registry: Vec<Address> = env
        .storage()
        .instance()
        .get(&DynamicRouterKey::Registry)
        .unwrap_or_else(|| Vec::new(env));
    let mut rebuilt: Vec<Address> = Vec::new(env);
    for i in 0..registry.len() {
        if let Some(addr) = registry.get(i) {
            if addr != pool {
                rebuilt.push_back(addr);
            }
        }
    }
    env.storage()
        .instance()
        .set(&DynamicRouterKey::Registry, &rebuilt);
    Ok(())
}

/// Read a single registered pool edge, if present.
pub fn get_pool(env: &Env, pool: Address) -> Option<PoolEdge> {
    env.storage().instance().get(&DynamicRouterKey::Pool(pool))
}

/// Read every registered pool edge in registration order.
pub fn registered_pool_edges(env: &Env) -> Vec<PoolEdge> {
    let mut edges: Vec<PoolEdge> = Vec::new(env);
    let registry: Vec<Address> = env
        .storage()
        .instance()
        .get(&DynamicRouterKey::Registry)
        .unwrap_or_else(|| Vec::new(env));
    for i in 0..registry.len() {
        if let Some(pool) = registry.get(i) {
            if let Some(edge) = env
                .storage()
                .instance()
                .get::<_, PoolEdge>(&DynamicRouterKey::Pool(pool))
            {
                edges.push_back(edge);
            }
        }
    }
    edges
}

// ---------------------------------------------------------------------------
// Router configuration
// ---------------------------------------------------------------------------

/// Read the active router configuration (falls back to safe defaults).
pub fn get_router_config(env: &Env) -> RouterConfig {
    env.storage()
        .instance()
        .get(&DynamicRouterKey::Config)
        .unwrap_or_else(RouterConfig::default_config)
}

/// Update the router configuration. Restricted to the protocol admin.
pub fn set_router_config(
    env: &Env,
    admin: Address,
    config: RouterConfig,
) -> Result<(), ContractError> {
    admin.require_auth();
    assert_admin(env, &admin)?;
    if config.max_hops == 0 || config.max_hops > MAX_ROUTE_DEPTH {
        return Err(ContractError::InvalidInput);
    }
    if config.max_price_impact_bps > BPS_DENOMINATOR {
        return Err(ContractError::InvalidInput);
    }
    env.storage()
        .instance()
        .set(&DynamicRouterKey::Config, &config);
    Ok(())
}

// ---------------------------------------------------------------------------
// Views + execution
// ---------------------------------------------------------------------------

/// Quote the best route across the registered pools without mutating state.
pub fn quote_route(
    env: &Env,
    source: AssetId,
    destination: AssetId,
    amount_in: u64,
    max_hops: u32,
) -> Result<RouteQuote, ContractError> {
    let edges = registered_pool_edges(env);
    find_best_route(env, &edges, source, destination, amount_in, max_hops)
}

/// Execute a routed swap across registered AMM pools.
///
/// Steps:
/// 1. Select the best path (up to the configured hop depth) maximising output.
/// 2. Enforce the caller's balance-delta floor: `B_out >= min_amount_out`.
/// 3. Enforce the effective-price tolerance using the stricter of the caller's
///    `max_price_impact_bps` and the governance ceiling.
/// 4. Assert the aggregate path invariant `∏k_after >= ∏k_before`.
/// 5. Persist the post-trade reserves for every hop and emit an event.
///
/// Any failure returns before the first storage write, so no partial state is
/// left behind (and Soroban would revert the whole transaction regardless).
pub fn execute_swap(
    env: &Env,
    trader: Address,
    source: AssetId,
    destination: AssetId,
    amount_in: u64,
    min_amount_out: u64,
    max_price_impact_bps: u32,
) -> Result<RouteQuote, ContractError> {
    trader.require_auth();
    let _guard = crate::security::reentrancy::ReentrancyGuard::new(env)?;

    let config = get_router_config(env);
    if !config.enabled {
        return Err(ContractError::ContractPaused);
    }

    let edges = registered_pool_edges(env);
    let quote = find_best_route(env, &edges, source, destination, amount_in, config.max_hops)?;

    // (1) Balance delta — the trader must receive at least B_min_expected.
    if quote.amount_out < min_amount_out {
        return Err(ContractError::SlippageExceeded);
    }

    // (2) Effective execution price tolerance. The caller may tighten the
    //     governance ceiling but never loosen it.
    let tolerance = if max_price_impact_bps < config.max_price_impact_bps {
        max_price_impact_bps
    } else {
        config.max_price_impact_bps
    };
    if quote.price_impact_bps > tolerance {
        return Err(ContractError::SlippageExceeded);
    }

    // (3) Path execution invariant across intermediate pools:
    //     k₁ · k₂ · k₃ >= k_initial.
    if quote.final_k_product < quote.initial_k_product {
        return Err(ContractError::InvariantViolation);
    }

    // (4) Commit the post-trade reserves for every hop. Each hop was priced
    //     from the same snapshot the registry returned, so the updates are
    //     internally consistent.
    for hop in quote.hops.iter() {
        let key = DynamicRouterKey::Pool(hop.pool.clone());
        let mut edge: PoolEdge = env
            .storage()
            .instance()
            .get(&key)
            .ok_or(ContractError::PoolNotFound)?;
        edge.reserve_in = edge
            .reserve_in
            .checked_add(hop.amount_in)
            .ok_or(ContractError::Overflow)?;
        edge.reserve_out = edge
            .reserve_out
            .checked_sub(hop.amount_out)
            .ok_or(ContractError::Overflow)?;
        env.storage().instance().set(&key, &edge);
    }

    // (5) Emit an observability event for off-chain indexers.
    let hops = quote.hops.len();
    env.events().publish(
        (EV_DYNAMIC_SWAP, trader),
        (source, destination, amount_in, quote.amount_out, hops),
    );

    Ok(quote)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    fn edge(
        env: &Env,
        asset_in: AssetId,
        asset_out: AssetId,
        reserve_in: u64,
        reserve_out: u64,
        fee_bps: u32,
    ) -> PoolEdge {
        PoolEdge {
            pool: Address::generate(env),
            asset_in,
            asset_out,
            reserve_in,
            reserve_out,
            fee_bps,
        }
    }

    #[test]
    fn max_route_depth_is_three() {
        assert_eq!(MAX_ROUTE_DEPTH, 3);
        assert_eq!(RouterConfig::default_config().max_hops, 3);
    }

    #[test]
    fn quote_hop_matches_constant_product_formula() {
        let env = Env::default();
        let e = edge(&env, 1, 2, 1_000_000, 2_000_000, 30);
        let hop = quote_hop(&e, 10_000).unwrap();

        // in_after_fee = 10_000 * 9970 / 10_000 = 9_970
        // out = 2_000_000 * 9_970 / 1_009_970 = 19_743
        assert_eq!(hop.amount_out, 19_743);
        assert_eq!(hop.fee_paid, 30);
        assert_eq!(hop.k_before, 1_000_000u128 * 2_000_000u128);
        assert!(hop.k_after >= hop.k_before);
    }

    #[test]
    fn quote_hop_rejects_zero_amount() {
        let env = Env::default();
        let e = edge(&env, 1, 2, 1_000, 1_000, 30);
        assert_eq!(quote_hop(&e, 0), Err(ContractError::ZeroSwapAmount));
    }

    #[test]
    fn quote_hop_rejects_empty_pool() {
        let env = Env::default();
        let e = edge(&env, 1, 2, 0, 1_000, 30);
        assert_eq!(
            quote_hop(&e, 100),
            Err(ContractError::InsufficientLiquidityDepth)
        );
    }

    #[test]
    fn quote_hop_rejects_same_asset() {
        let env = Env::default();
        let e = edge(&env, 7, 7, 1_000, 1_000, 30);
        assert_eq!(quote_hop(&e, 100), Err(ContractError::InvalidInput));
    }

    #[test]
    fn quote_hop_rejects_fee_above_denominator() {
        let env = Env::default();
        let e = edge(&env, 1, 2, 1_000, 1_000, 10_001);
        assert_eq!(quote_hop(&e, 100), Err(ContractError::InvalidInput));
    }

    #[test]
    fn find_best_route_prefers_higher_output_multi_hop() {
        let env = Env::default();
        let mut edges = Vec::new(&env);
        // Direct A->C pool with a poor reserve ratio.
        edges.push_back(edge(&env, 1, 3, 1_000, 1_000, 30));
        // A->B->C pools with a better combined ratio.
        edges.push_back(edge(&env, 1, 2, 1_000, 2_000, 30));
        edges.push_back(edge(&env, 2, 3, 1_000, 2_000, 30));

        let route = find_best_route(&env, &edges, 1, 3, 100, MAX_ROUTE_DEPTH).unwrap();
        assert_eq!(route.hops.len(), 2);
        // The two-hop route must beat the direct hop's output.
        let direct = quote_hop(&edges.get(0).unwrap(), 100).unwrap();
        assert!(route.amount_out > direct.amount_out);
    }

    #[test]
    fn find_best_route_respects_max_hops() {
        let env = Env::default();
        let mut edges = Vec::new(&env);
        edges.push_back(edge(&env, 1, 3, 1_000, 1_000, 30));
        edges.push_back(edge(&env, 1, 2, 1_000, 2_000, 30));
        edges.push_back(edge(&env, 2, 3, 1_000, 2_000, 30));

        let route = find_best_route(&env, &edges, 1, 3, 100, 1).unwrap();
        assert_eq!(route.hops.len(), 1);
    }

    #[test]
    fn find_best_route_returns_pool_not_found_when_disconnected() {
        let env = Env::default();
        let mut edges = Vec::new(&env);
        edges.push_back(edge(&env, 1, 2, 1_000, 1_000, 30));

        assert_eq!(
            find_best_route(&env, &edges, 1, 9, 100, MAX_ROUTE_DEPTH),
            Err(ContractError::PoolNotFound)
        );
    }

    #[test]
    fn find_best_route_rejects_invalid_depth() {
        let env = Env::default();
        let mut edges = Vec::new(&env);
        edges.push_back(edge(&env, 1, 2, 1_000, 1_000, 30));

        assert_eq!(
            find_best_route(&env, &edges, 1, 2, 100, 0),
            Err(ContractError::InvalidInput)
        );
        assert_eq!(
            find_best_route(&env, &edges, 1, 2, 100, MAX_ROUTE_DEPTH + 1),
            Err(ContractError::InvalidInput)
        );
    }

    #[test]
    fn path_invariant_product_is_non_decreasing() {
        let env = Env::default();
        let mut edges = Vec::new(&env);
        edges.push_back(edge(&env, 1, 2, 1_000, 2_000, 30));
        edges.push_back(edge(&env, 2, 3, 1_500, 2_500, 5));
        edges.push_back(edge(&env, 3, 4, 2_000, 1_000, 100));

        let route = find_best_route(&env, &edges, 1, 4, 500, MAX_ROUTE_DEPTH).unwrap();
        assert_eq!(route.hops.len(), 3);
        // k₁ · k₂ · k₃ >= k_initial
        assert!(route.final_k_product >= route.initial_k_product);
    }

    // ── Storage-backed execution guards ─────────────────────────────────────
    //
    // Storage is only reachable from inside a contract context, so these tests
    // register the main contract and run through `Env::as_contract` — the same
    // pattern used by the other storage-backed module tests in this crate.

    fn setup_contract(env: &Env, admin: &Address) {
        let data = crate::ContractData {
            admin: admin.clone(),
            value: 0,
            max_fee_ceiling: 10_000,
        };
        env.storage().instance().set(&crate::DATA_KEY, &data);
    }

    fn register_all(env: &Env, admin: &Address, edges: &Vec<PoolEdge>) {
        for e in edges.iter() {
            register_pool(env, admin.clone(), e).unwrap();
        }
    }

    #[test]
    fn execute_swap_reverts_with_slippage_exceeded_below_min() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let admin = Address::generate(&env);
        let trader = Address::generate(&env);

        env.as_contract(&contract_id, || {
            setup_contract(&env, &admin);
            let mut edges = Vec::new(&env);
            edges.push_back(edge(&env, 1, 2, 1_000_000, 1_000_000, 30));
            register_all(&env, &admin, &edges);

            // Demand more than the pool can possibly deliver.
            let result = execute_swap(&env, trader.clone(), 1, 2, 1_000, u64::MAX, 10_000);
            assert_eq!(result, Err(ContractError::SlippageExceeded));
        });
    }

    #[test]
    fn execute_swap_reverts_when_price_impact_exceeds_tolerance() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let admin = Address::generate(&env);
        let trader = Address::generate(&env);

        env.as_contract(&contract_id, || {
            setup_contract(&env, &admin);
            // Small reserves + a large trade produce a non-zero price impact.
            let mut edges = Vec::new(&env);
            edges.push_back(edge(&env, 1, 2, 1_000, 1_000, 30));
            register_all(&env, &admin, &edges);

            let result = execute_swap(&env, trader.clone(), 1, 2, 500, 1, 0);
            assert_eq!(result, Err(ContractError::SlippageExceeded));
        });
    }

    #[test]
    fn execute_swap_updates_reserves_on_success() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let admin = Address::generate(&env);
        let trader = Address::generate(&env);

        env.as_contract(&contract_id, || {
            setup_contract(&env, &admin);
            let mut edges = Vec::new(&env);
            edges.push_back(edge(&env, 1, 2, 1_000_000, 1_000_000, 30));
            let pool = edges.get(0).unwrap().pool.clone();
            register_all(&env, &admin, &edges);

            let quote = execute_swap(&env, trader.clone(), 1, 2, 1_000, 1, 10_000).unwrap();
            assert!(quote.amount_out > 0);

            let updated = get_pool(&env, pool).unwrap();
            assert_eq!(updated.reserve_in, 1_001_000);
            assert_eq!(updated.reserve_out, 1_000_000u64 - quote.amount_out);
        });
    }

    #[test]
    fn execute_swap_reverts_when_disabled() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let admin = Address::generate(&env);
        let trader = Address::generate(&env);

        env.as_contract(&contract_id, || {
            setup_contract(&env, &admin);
            set_router_config(
                &env,
                admin.clone(),
                RouterConfig {
                    max_price_impact_bps: 500,
                    max_hops: MAX_ROUTE_DEPTH,
                    enabled: false,
                },
            )
            .unwrap();

            let result = execute_swap(&env, trader.clone(), 1, 2, 1_000, 1, 10_000);
            assert_eq!(result, Err(ContractError::ContractPaused));
        });
    }
}
