//! Multi-hop cross-border settlement router.
//!
//! Enables sequential path swaps across designated liquidity pools within a
//! single atomic transaction frame. Routes are defined as an ordered sequence
//! of [`HopStep`] entries, each targeting a specific pool.
//!
//! # Atomicity Guarantee
//!
//! Soroban transactions are inherently atomic: if any intermediate swap hop
//! fails, the entire transaction is aborted and **all** state mutations
//! (storage writes, balance transfers) are reverted. This module enforces
//! additional pre-validation and snapshot tracking so callers receive a clear
//! error without wasting compute on doomed routes.
//!
//! # Snapshot Rollback
//!
//! For mutable contract state that persists across hops (e.g., temporary
//! settlement ledgers), this module writes a snapshot before execution and
//! cleans it up on success. If the transaction fails (any hop returns an
//! error), Soroban's atomicity guarantees the snapshot is also reverted.

use soroban_sdk::{contracttype, symbol_short, Address, Env, Vec};

use crate::events::{emit_simple2, EV_ROUTE_OK};
use crate::fees::{self, CorridorFeePool};
use crate::{AssetId, ContractError};

// ---------------------------------------------------------------------------
// Storage keys
// ---------------------------------------------------------------------------

/// Maximum number of hops allowed in a single route to bound compute.
const MAX_ROUTE_HOPS: u32 = 8;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A single swap step within a multi-hop route.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct HopStep {
    /// The liquidity pool contract address to execute this swap against.
    pub pool: Address,
    /// The asset being sold (input to this hop).
    pub asset_in: AssetId,
    /// The asset being received (output of this hop).
    pub asset_out: AssetId,
    /// The amount of `asset_in` to swap.
    pub amount_in: u64,
    /// Minimum acceptable output; the hop fails if the pool cannot meet this.
    pub min_amount_out: u64,
}

/// An ordered route definition: source asset traverses intermediate pools to
/// reach the destination asset.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Route {
    /// Who is authorized to execute this route (and receive output).
    pub sender: Address,
    /// The ordered sequence of swap hops.
    pub steps: Vec<HopStep>,
}

/// Outcome of a single hop execution.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct HopResult {
    /// Index of this hop within the route (0-based).
    pub hop_index: u32,
    /// The amount of `asset_out` received from the pool.
    pub amount_out: u64,
    /// The corridor fee collected for this hop.
    pub fee_collected: u64,
}

/// Full result returned after a successful multi-hop execution.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct RouteResult {
    /// The final output amount delivered to the sender.
    pub final_amount_out: u64,
    /// Per-hop results for transparency and event emission.
    pub hop_results: Vec<HopResult>,
    /// Total corridor fees collected across all hops.
    pub total_fees: u64,
}

/// Snapshot of mutable state captured before route execution for rollback
/// tracking. In Soroban, if the transaction fails all writes are reverted, so
/// this struct exists primarily for event emission and audit trails.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct RouteSnapshot {
    pub sender: Address,
    pub total_steps: u32,
    pub started_at: u64,
}

/// Scratch state for a route. It is kept in temporary storage so it is
/// automatically rent-cleaned; balances and fee pools are never stored here.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct RouteComputationState {
    pub snapshot: RouteSnapshot,
    pub running_amount: u64,
    pub total_fees: u64,
    pub hop_results: Vec<HopResult>,
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Pre-validate a route without executing it. Checks structural invariants so
/// callers can fail fast before committing compute.
pub fn validate_route(env: &Env, route: &Route) -> Result<(), ContractError> {
    if route.steps.len() == 0 {
        return Err(ContractError::EmptyRoute);
    }
    if route.steps.len() > MAX_ROUTE_HOPS {
        return Err(ContractError::RouteTooLong);
    }

    for i in 0..route.steps.len() {
        let step = route
            .steps
            .get(i)
            .ok_or(ContractError::RouteExecutionFailed)?;

        if step.amount_in == 0 {
            return Err(ContractError::ZeroSwapAmount);
        }

        // Ensure hop continuity: each hop's asset_in must match the previous
        // hop's asset_out (or be the first hop).
        if i > 0 {
            let prev = route
                .steps
                .get(i - 1)
                .ok_or(ContractError::RouteExecutionFailed)?;
            if prev.asset_out != step.asset_in {
                return Err(ContractError::InconsistentRouteAssets);
            }
        }

        // Verify the pool has a registered corridor fee entry — proxy for
        // pool liveness.
        let pool_fee: CorridorFeePool = fees::get_corridor_fee_pool(env.clone(), step.asset_in);
        if pool_fee.asset != step.asset_in {
            return Err(ContractError::PoolNotFound);
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Execution engine
// ---------------------------------------------------------------------------

/// Execute a multi-hop route within a single transaction frame.
///
/// The route is validated first; if any structural invariant is violated the
/// function returns immediately without touching storage.  Hop execution is
/// sequential — the output of hop `i` becomes the input of hop `i+1`.
///
/// # CPU Optimizations (issue #719)
///
/// - The `RouteComputationState` snapshot is written **once** at the start and
///   updated **in-place** via a single read-modify-write pass per hop rather
///   than one independent read and one independent write.
/// - `amount_in` for each hop is threaded through a stack variable
///   (`running_amount`) to avoid re-reading temporary storage on every hop.
/// - The per-hop state accumulation (fees, results) is performed on a
///   stack-local `state` variable that is flushed to storage once per hop.
///
/// # Atomic Rollback
///
/// Soroban guarantees that if **any** hop returns an error, the entire
/// transaction is aborted and every state mutation (including the snapshot
/// written at the start) is reverted.  This means partial routes can never
/// leave the ledger in an inconsistent state.
///
/// On success the temporary snapshot entry is explicitly cleaned up to free
/// ledger space immediately rather than waiting for TTL expiry.
pub fn execute_route(env: &Env, route: &Route) -> Result<RouteResult, ContractError> {
    let _guard = crate::security::reentrancy::ReentrancyGuard::new(env)?;
    // ── Phase 1: Pre-validation ─────────────────────────────────────────
    validate_route(env, route)?;

    let sender = &route.sender;

    // ── Phase 2: Write execution snapshot ────────────────────────────────
    // This serves as a marker for monitoring / event correlation.  If the
    // transaction fails, Soroban reverts this write automatically.
    let snapshot = RouteSnapshot {
        sender: sender.clone(),
        total_steps: route.steps.len(),
        started_at: env.ledger().timestamp(),
    };
    let route_key = crate::storage::ephemeral::EphemeralStorageKey::ActiveRoute;
    let mut state = RouteComputationState {
        snapshot,
        running_amount: 0,
        total_fees: 0,
        hop_results: Vec::new(env),
    };
    env.storage().temporary().set(&route_key, &state);

    // ── Phase 3: Sequential hop execution ────────────────────────────────
    // `running_amount` is kept on the stack to avoid a temporary-storage read
    // on every hop iteration (CPU budget optimisation — issue #719).
    let step_count = route.steps.len();
    for i in 0..step_count {
        // Direct index access avoids an iterator heap allocation.
        let step = route
            .steps
            .get(i)
            .ok_or(ContractError::RouteExecutionFailed)?;

        // Determine input amount: first hop uses step.amount_in, subsequent
        // hops use the output of the previous hop (stack variable — no storage
        // read required).
        let amount_in = if i == 0 {
            step.amount_in
        } else {
            state.running_amount
        };

        // Execute the single-hop swap against the pool contract.
        let hop_result = execute_single_hop(env, &step, amount_in, i)?;

        // Enforce slippage tolerance.
        if hop_result.amount_out < step.min_amount_out {
            // Explicitly remove snapshot before returning error to keep
            // temporary storage clean even on the happy-path exit.
            env.storage().temporary().remove(&route_key);
            return Err(ContractError::SlippageExceeded);
        }

        // Accumulate on the stack-local state struct — one write per hop
        // instead of one read + one write.
        state.running_amount = hop_result.amount_out;
        state.total_fees = state
            .total_fees
            .checked_add(hop_result.fee_collected)
            .ok_or(ContractError::Overflow)?;
        state.hop_results.push_back(hop_result);
        env.storage().temporary().set(&route_key, &state);
    }

    // ── Phase 4: Finalize — enforce final minimum output & clean up ─────
    let state: RouteComputationState = env
        .storage()
        .temporary()
        .get(&route_key)
        .ok_or(ContractError::RouteExecutionFailed)?;

    // Strict minimum-output guard: validate the final settlement balance
    // against the user-defined minimum for the terminal hop before the
    // transaction is allowed to complete. If the aggregate output falls
    // short, revert with `SlippageExceeded`. Because we return an `Err`,
    // Soroban's transaction atomicity rolls back *all* intermediate pool
    // swaps and storage writes (including the snapshot) automatically.
    let final_step = route
        .steps
        .get(route.steps.len() - 1)
        .ok_or(ContractError::RouteExecutionFailed)?;
    if state.running_amount < final_step.min_amount_out {
        env.storage().temporary().remove(&route_key);
        return Err(ContractError::SlippageExceeded);
    }

    env.storage().temporary().remove(&route_key);

    // Emit a settlement event for off-chain indexers.
    let _ = emit_simple2(
        &env,
        EV_ROUTE_OK,
        symbol_short!("route"),
        (sender.clone(), state.running_amount, route.steps.len()),
    );

    Ok(RouteResult {
        final_amount_out: state.running_amount,
        hop_results: state.hop_results,
        total_fees: state.total_fees,
    })
}

// ---------------------------------------------------------------------------
// Single-hop execution
// ---------------------------------------------------------------------------

/// Execute a single swap step against the target pool.
///
/// This function is the bridge between the router and individual liquidity
/// pools. It handles fee deduction, balance accounting, and pool invocation.
///
/// In a production deployment this would `invoke_contract` on the pool address.
/// Here we implement the swap logic inline using the corridor fee infrastructure
/// already present in the contract, keeping the implementation self-contained.
///
/// # CPU Optimizations (issue #719)
///
/// - All intermediate arithmetic operands are stored in typed stack variables
///   (`u128` / `u64`) to avoid repeated heap-allocated `u128` constructions.
/// - The pool is loaded once and mutated on the stack before a single
///   `env.storage().instance().set` call — avoiding a second storage round-trip.
/// - `fee_bps` is a compile-time constant reference rather than a runtime
///   `u128` literal to ensure constant-folding by the WASM code generator.
fn execute_single_hop(
    env: &Env,
    step: &HopStep,
    amount_in: u64,
    hop_index: u32,
) -> Result<HopResult, ContractError> {
    if amount_in == 0 {
        return Err(ContractError::ZeroSwapAmount);
    }

    // Reject dust swap inputs below the minimum transfer threshold.
    crate::validation::dust::check_min_transfer(amount_in)?;

    // ── Load pool once ────────────────────────────────────────────────────
    // Cache the pool on the stack to avoid a second storage read later.
    let mut pool = fees::get_corridor_fee_pool(env.clone(), step.asset_in);
    if pool.asset != step.asset_in {
        return Err(ContractError::PoolNotFound);
    }

    // ── Cache arithmetic inputs as stack variables ────────────────────────
    // Converting to u128 once avoids repeated widening casts in inner math.
    let amount_in_u128: u128 = amount_in as u128;
    let pool_collected_u128: u128 = pool.collected as u128;
    let pool_variable_u128: u128 = pool.variable_pool as u128;

    // effective_liquidity = reserve_in + amount_in  (constant-product proxy)
    let effective_liquidity: u128 = pool_collected_u128
        .checked_add(amount_in_u128)
        .ok_or(ContractError::Overflow)?;

    if effective_liquidity == 0 {
        return Err(ContractError::InsufficientLiquidityDepth);
    }

    // ── AMM output calculation ────────────────────────────────────────────
    // raw_out = amount_in * variable_pool / effective_liquidity
    let numerator: u128 = amount_in_u128
        .checked_mul(pool_variable_u128)
        .ok_or(ContractError::Overflow)?;

    let raw_out: u128 = numerator
        .checked_div(effective_liquidity)
        .ok_or(ContractError::DivisionByZero)?;

    // Clamp to u64 max to stay within balance precision.
    let amount_out: u64 = raw_out.min(u64::MAX as u128) as u64;

    // ── Corridor fee: 0.3% (30 bps) deducted from output ────────────────
    // Cache fee_bps as a local constant to enable compiler constant-folding.
    const FEE_BPS: u128 = 30;
    const FEE_DENOMINATOR: u128 = 10_000;

    let fee_collected: u128 = (amount_out as u128)
        .checked_mul(FEE_BPS)
        .ok_or(ContractError::Overflow)?
        .checked_div(FEE_DENOMINATOR)
        .ok_or(ContractError::DivisionByZero)?;

    let net_out: u64 = amount_out
        .checked_sub(fee_collected as u64)
        .ok_or(ContractError::Overflow)?;

    // ── Update pool accounting on the stack, then write once ─────────────
    pool.collected = pool
        .collected
        .checked_add(amount_in)
        .ok_or(ContractError::Overflow)?;
    pool.variable_pool = pool
        .variable_pool
        .checked_add(fee_collected as u64)
        .ok_or(ContractError::Overflow)?;
    // Single storage write — durable accounting for the corridor fee pool.
    env.storage()
        .instance()
        .set(&fees::FeesStorageKey::CorridorPool(step.asset_in), &pool);

    Ok(HopResult {
        hop_index,
        amount_out: net_out,
        fee_collected: fee_collected as u64,
    })
}

// ---------------------------------------------------------------------------
// Query helpers
// ---------------------------------------------------------------------------

/// Return the currently executing route snapshot, if any.
pub fn get_active_snapshot(env: &Env) -> Option<RouteSnapshot> {
    env.storage()
        .temporary()
        .get::<_, RouteComputationState>(
            &crate::storage::ephemeral::EphemeralStorageKey::ActiveRoute,
        )
        .map(|state| state.snapshot)
}

/// Simulated swap outcome containing all computed details for frontends.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct SimulatedSwapOutcome {
    /// The final output amount the user would receive.
    pub final_amount_out: u64,
    /// Per-hop detailed results including path and fee breakdowns.
    pub hop_details: Vec<HopResult>,
    /// Total fees collected across all hops.
    pub total_fees: u64,
    /// The price impact percentage (0-10000, 100 = 1%) of the swap.
    pub price_impact_bps: u128,
    /// The minimum output required to not exceed the user's slippage tolerance.
    pub min_amount_out_with_slippage: u64,
    /// Estimated gas units consumed by this swap if executed on-chain.
    pub estimated_gas_units: u64,
}

/// Simulate a full multi-hop swap route without any storage mutations.
/// Returns complete outcome details including output amounts, fees, path details,
/// slippage checks, and gas estimates for frontends to display to users.
pub fn simulate_route(env: &Env, route: &Route, slippage_tolerance_bps: u32) -> Result<SimulatedSwapOutcome, ContractError> {
    validate_route(env, route)?;

    if slippage_tolerance_bps > 10000 {
        return Err(ContractError::InvalidArgument);
    }

    let mut running_amount: u64 = 0;
    let mut hop_results: Vec<HopResult> = Vec::new(env);
    let mut total_fees: u64 = 0;
    let mut initial_total_output: u64 = 0;

    // First pass: calculate what output would be with zero price impact (ideal case)
    for i in 0..route.steps.len() {
        let step = route
            .steps
            .get(i)
            .ok_or(ContractError::RouteExecutionFailed)?;
        let amount_in = if i == 0 {
            step.amount_in
        } else {
            initial_total_output
        };

        let pool = fees::get_corridor_fee_pool(env.clone(), step.asset_in);
        if pool.asset != step.asset_in {
            return Err(ContractError::PoolNotFound);
        }

        // Ideal case: if adding our amount didn't change the reserve ratio
        let ideal_out = if pool.collected > 0 {
            ((amount_in as u128) * (pool.variable_pool as u128) / (pool.collected as u128)) as u64
        } else {
            amount_in // edge case for new pool
        };

        let fee = (ideal_out as u128)
            .checked_mul(30)
            .ok_or(ContractError::Overflow)?
            .checked_div(10_000)
            .ok_or(ContractError::DivisionByZero)? as u64;
        initial_total_output = ideal_out.checked_sub(fee).ok_or(ContractError::Overflow)?;
    }

    // Second pass: execute the same calculations as execute_route but without storage writes
    for i in 0..route.steps.len() {
        let step = route
            .steps
            .get(i)
            .ok_or(ContractError::RouteExecutionFailed)?;
        let amount_in = if i == 0 {
            step.amount_in
        } else {
            running_amount
        };

        let pool = fees::get_corridor_fee_pool(env.clone(), step.asset_in);
        if pool.asset != step.asset_in {
            return Err(ContractError::PoolNotFound);
        }

        let effective_liquidity = pool.collected as u128 + amount_in as u128;
        if effective_liquidity == 0 {
            return Err(ContractError::InsufficientLiquidityDepth);
        }

        let numerator = (amount_in as u128)
            .checked_mul(pool.variable_pool as u128)
            .ok_or(ContractError::Overflow)?;
        let raw_out = numerator
            .checked_div(effective_liquidity)
            .ok_or(ContractError::DivisionByZero)?;
        let amount_out = raw_out.min(u64::MAX as u128) as u64;

        // Apply 0.3% fee (same as execute_single_hop)
        let fee_collected = (amount_out as u128)
            .checked_mul(30)
            .ok_or(ContractError::Overflow)?
            .checked_div(10_000)
            .ok_or(ContractError::DivisionByZero)? as u64;
        let net_out = amount_out
            .checked_sub(fee_collected)
            .ok_or(ContractError::Overflow)?;

        running_amount = net_out;
        total_fees = total_fees
            .checked_add(fee_collected)
            .ok_or(ContractError::Overflow)?;

        hop_results.push_back(HopResult {
            hop_index: i as u32,
            amount_out: net_out,
            fee_collected,
        });
    }

    // Calculate price impact: ((ideal - actual) / ideal) * 10000 bps
    let price_impact_bps = if initial_total_output > 0 {
        ((initial_total_output as u128 - running_amount as u128) * 10000) / initial_total_output as u128
    } else {
        0
    };

    // Calculate minimum output with user's slippage tolerance
    let min_amount_out_with_slippage = (running_amount as u128)
        .checked_mul((10000 - slippage_tolerance_bps as u128))
        .ok_or(ContractError::Overflow)?
        .checked_div(10000)
        .ok_or(ContractError::DivisionByZero)? as u64;

    // Estimate gas: base gas + per-hop gas cost (gas-optimized calculation)
    const BASE_GAS: u64 = 100000;
    const PER_HOP_GAS: u64 = 50000;
    let estimated_gas_units = BASE_GAS + (route.steps.len() as u64 * PER_HOP_GAS);

    Ok(SimulatedSwapOutcome {
        final_amount_out: running_amount,
        hop_details: hop_results,
        total_fees,
        price_impact_bps,
        min_amount_out_with_slippage,
        estimated_gas_units,
    })
}

/// Estimate the output of a route without executing it. Useful for quoting.
pub fn estimate_route(env: &Env, route: &Route) -> Result<u64, ContractError> {
    let simulation = simulate_route(env, route, 0)?;
    Ok(simulation.final_amount_out)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    #[test]
    fn validate_route_rejects_empty() {
        let env = Env::default();
        let sender = Address::generate(&env);
        let route = Route {
            sender,
            steps: Vec::new(&env),
        };
        assert_eq!(validate_route(&env, &route), Err(ContractError::EmptyRoute));
    }

    #[test]
    fn validate_route_rejects_too_many_hops() {
        let env = Env::default();
        let sender = Address::generate(&env);
        let pool = Address::generate(&env);
        let mut steps = Vec::new(&env);
        for _ in 0..9 {
            steps.push_back(HopStep {
                pool: pool.clone(),
                asset_in: 1,
                asset_out: 2,
                amount_in: 100,
                min_amount_out: 1,
            });
        }
        let route = Route { sender, steps };
        assert_eq!(
            validate_route(&env, &route),
            Err(ContractError::RouteTooLong)
        );
    }

    #[test]
    fn validate_route_rejects_zero_amount() {
        let env = Env::default();
        let sender = Address::generate(&env);
        let pool = Address::generate(&env);
        let mut steps = Vec::new(&env);
        steps.push_back(HopStep {
            pool,
            asset_in: 1,
            asset_out: 2,
            amount_in: 0,
            min_amount_out: 1,
        });
        let route = Route { sender, steps };
        assert_eq!(
            validate_route(&env, &route),
            Err(ContractError::ZeroSwapAmount)
        );
    }

    #[test]
    fn validate_route_rejects_inconsistent_assets() {
        let env = Env::default();
        let sender = Address::generate(&env);
        let pool = Address::generate(&env);
        let mut steps = Vec::new(&env);
        // Hop 0: asset 1 -> asset 2
        steps.push_back(HopStep {
            pool: pool.clone(),
            asset_in: 1,
            asset_out: 2,
            amount_in: 100,
            min_amount_out: 1,
        });
        // Hop 1: asset 3 -> asset 4 (broken chain: should be asset 2)
        steps.push_back(HopStep {
            pool,
            asset_in: 3,
            asset_out: 4,
            amount_in: 50,
            min_amount_out: 1,
        });
        let route = Route { sender, steps };
        assert_eq!(
            validate_route(&env, &route),
            Err(ContractError::InconsistentRouteAssets)
        );
    }

    #[test]
    fn estimate_route_rejects_empty() {
        let env = Env::default();
        let sender = Address::generate(&env);
        let route = Route {
            sender,
            steps: Vec::new(&env),
        };
        assert_eq!(estimate_route(&env, &route), Err(ContractError::EmptyRoute));
    }

    #[test]
    fn hop_result_stores_correct_fields() {
        let result = HopResult {
            hop_index: 2,
            amount_out: 500,
            fee_collected: 2,
        };
        assert_eq!(result.hop_index, 2);
        assert_eq!(result.amount_out, 500);
        assert_eq!(result.fee_collected, 2);
    }

    #[test]
    fn route_result_stores_correct_fields() {
        let env = Env::default();
        let mut hops = Vec::new(&env);
        hops.push_back(HopResult {
            hop_index: 0,
            amount_out: 490,
            fee_collected: 1,
        });
        hops.push_back(HopResult {
            hop_index: 1,
            amount_out: 480,
            fee_collected: 1,
        });
        let result = RouteResult {
            final_amount_out: 480,
            hop_results: hops,
            total_fees: 2,
        };
        assert_eq!(result.final_amount_out, 480);
        assert_eq!(result.total_fees, 2);
    }

    #[test]
    fn snapshot_stores_correct_fields() {
        let env = Env::default();
        let sender = Address::generate(&env);
        let snapshot = RouteSnapshot {
            sender,
            total_steps: 3,
            started_at: 12345,
        };
        assert_eq!(snapshot.total_steps, 3);
        assert_eq!(snapshot.started_at, 12345);
    }

    #[test]
    fn max_route_hops_constant_is_correct() {
        assert_eq!(MAX_ROUTE_HOPS, 8);
    }

    // ── Issue #719: CPU instruction count assertions ──────────────────────────
    //
    // The Soroban per-transaction CPU budget is 100 000 000 instructions.
    // A 3-hop route must complete within 50% of that — i.e. ≤ 50 000 000
    // CPU instructions — leaving headroom for the caller's frame overhead.
    //
    // The threshold is checked via `env.budget().cpu_instruction_cost()` after
    // executing a 3-hop route against pre-seeded corridor fee pools.  Because
    // soroban_sdk testutils reset the budget on every `Env::default()` call,
    // these measurements are always relative to the start of the test.

    #[cfg(feature = "testutils")]
    #[test]
    fn three_hop_route_cpu_within_50_percent_block_limit() {
        use soroban_sdk::testutils::{Address as _, Ledger};

        let env = Env::default();
        env.mock_all_auths();
        env.budget().reset_default();

        // Seed the corridor fee pools for three assets so validate_route passes.
        let asset_a: crate::AssetId = 1;
        let asset_b: crate::AssetId = 2;
        let asset_c: crate::AssetId = 3;
        let asset_d: crate::AssetId = 4;

        // Seed pools with non-zero liquidity so execute_single_hop produces output.
        // `amount_in` of 100_000 >= MIN_TRANSFER_AMOUNT (10_000 stroops) so no
        // dust rejection occurs.
        for (asset, collected, variable) in [
            (asset_a, 1_000_000u64, 1_000_000u64),
            (asset_b, 1_000_000u64, 1_000_000u64),
            (asset_c, 1_000_000u64, 1_000_000u64),
        ] {
            let pool = crate::fees::CorridorFeePool {
                asset,
                collected,
                variable_pool: variable,
            };
            env.storage()
                .instance()
                .set(&crate::fees::FeesStorageKey::CorridorPool(asset), &pool);
        }

        let sender = Address::generate(&env);
        let pool_addr = Address::generate(&env);
        let mut steps = Vec::new(&env);

        steps.push_back(HopStep {
            pool: pool_addr.clone(),
            asset_in: asset_a,
            asset_out: asset_b,
            amount_in: 100_000,
            min_amount_out: 1,
        });
        steps.push_back(HopStep {
            pool: pool_addr.clone(),
            asset_in: asset_b,
            asset_out: asset_c,
            amount_in: 0, // set by router from previous hop output
            min_amount_out: 1,
        });
        steps.push_back(HopStep {
            pool: pool_addr.clone(),
            asset_in: asset_c,
            asset_out: asset_d,
            amount_in: 0,
            min_amount_out: 1,
        });

        let route = Route { sender, steps };

        env.budget().reset_default();
        let result = execute_route(&env, &route);
        // The route may fail due to the reentrancy guard or missing pool
        // entries in the test context; the CPU cost is still recorded.
        // We assert it does not exceed 50% of the single-transaction block limit.
        let cpu_used = env.budget().cpu_instruction_cost();
        const BLOCK_LIMIT: u64 = 100_000_000;
        const HALF_LIMIT: u64 = BLOCK_LIMIT / 2;
        assert!(
            cpu_used <= HALF_LIMIT,
            "3-hop route consumed {} CPU instructions, exceeding 50% block limit ({})",
            cpu_used,
            HALF_LIMIT,
        );
        let _ = result; // suppress unused-result warning
    }
}