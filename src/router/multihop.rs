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

use soroban_sdk::{contracttype, symbol_short, Address, Env, Symbol, Vec};

use crate::events::{emit_simple2, EV_ROUTE_OK};
use crate::fees::{self, CorridorFeePool};
use crate::{AssetId, ContractError};

// ---------------------------------------------------------------------------
// Storage keys
// ---------------------------------------------------------------------------

/// Temporary storage key for the active route execution context.
/// Cleared on success; automatically reverted by the ledger on failure.
const ROUTE_EXEC_KEY: Symbol = symbol_short!("RTEXEC");

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
    env.storage().temporary().set(&ROUTE_EXEC_KEY, &snapshot);

    // ── Phase 3: Sequential hop execution ────────────────────────────────
    let mut running_amount: u64 = 0;
    let mut hop_results: Vec<HopResult> = Vec::new(env);
    let mut total_fees: u64 = 0;

    for i in 0..route.steps.len() {
        let step = route
            .steps
            .get(i)
            .ok_or(ContractError::RouteExecutionFailed)?;

        // Determine input amount: first hop uses step.amount_in, subsequent
        // hops use the output of the previous hop.
        let amount_in = if i == 0 {
            step.amount_in
        } else {
            running_amount
        };

        // Execute the single-hop swap against the pool contract.
        let hop_result = execute_single_hop(env, &step, amount_in, i)?;

        // Enforce slippage tolerance.
        if hop_result.amount_out < step.min_amount_out {
            // Explicitly remove snapshot before returning error to keep
            // temporary storage clean even on the happy-path exit.
            env.storage().temporary().remove(&ROUTE_EXEC_KEY);
            return Err(ContractError::SlippageExceeded);
        }

        running_amount = hop_result.amount_out;
        total_fees = total_fees
            .checked_add(hop_result.fee_collected)
            .ok_or(ContractError::Overflow)?;

        hop_results.push_back(hop_result);
    }

    // ── Phase 4: Finalize — clean up snapshot ───────────────────────────
    env.storage().temporary().remove(&ROUTE_EXEC_KEY);

    // Emit a settlement event for off-chain indexers.
    let _ = emit_simple2(
        &env,
        EV_ROUTE_OK,
        symbol_short!("route"),
        (sender.clone(), running_amount, route.steps.len()),
    );

    Ok(RouteResult {
        final_amount_out: running_amount,
        hop_results,
        total_fees,
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
fn execute_single_hop(
    env: &Env,
    step: &HopStep,
    amount_in: u64,
    hop_index: u32,
) -> Result<HopResult, ContractError> {
    if amount_in == 0 {
        return Err(ContractError::ZeroSwapAmount);
    }

    // Resolve corridor fee pool for the input asset.
    let mut pool = fees::get_corridor_fee_pool(env.clone(), step.asset_in);
    if pool.asset != step.asset_in {
        return Err(ContractError::PoolNotFound);
    }

    // Compute proportional swap output using the corridor fee pool.
    // In a constant-product AMM the output is:
    //   amount_out = (amount_in * reserve_out) / (reserve_in + amount_in)
    //
    // We use the corridor's collected fees as a liquidity proxy and apply the
    // standard fixed-point scale for precision.
    let effective_liquidity = pool.collected as u128 + amount_in as u128;

    if effective_liquidity == 0 {
        return Err(ContractError::InsufficientLiquidityDepth);
    }

    // Numerator: amount_in * reserve_proxy  (reserve_proxy derived from variable_pool)
    let numerator = (amount_in as u128)
        .checked_mul(pool.variable_pool as u128)
        .ok_or(ContractError::Overflow)?;

    // Denominator: effective_liquidity
    let raw_out = numerator
        .checked_div(effective_liquidity)
        .ok_or(ContractError::DivisionByZero)?;

    let amount_out = raw_out.min(u64::MAX as u128) as u64;

    // Corridor fee: 0.3% (30 bps) deducted from the output.
    let fee_bps: u128 = 30;
    let fee_collected = (amount_out as u128)
        .checked_mul(fee_bps)
        .ok_or(ContractError::Overflow)?
        .checked_div(10_000)
        .ok_or(ContractError::DivisionByZero)?;

    let net_out = amount_out
        .checked_sub(fee_collected)
        .ok_or(ContractError::Overflow)?;

    // Update corridor fee pool accounting.
    pool.collected = pool
        .collected
        .checked_add(amount_in)
        .ok_or(ContractError::Overflow)?;
    pool.variable_pool = pool
        .variable_pool
        .checked_add(fee_collected as u64)
        .ok_or(ContractError::Overflow)?;
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
    env.storage().temporary().get(&ROUTE_EXEC_KEY)
}

/// Estimate the output of a route without executing it. Useful for quoting.
pub fn estimate_route(env: &Env, route: &Route) -> Result<u64, ContractError> {
    validate_route(env, route)?;

    let mut running_amount: u64 = 0;

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

        // Apply 0.3% fee.
        let fee = (amount_out as u128)
            .checked_mul(30)
            .ok_or(ContractError::Overflow)?
            .checked_div(10_000)
            .ok_or(ContractError::DivisionByZero)?;
        running_amount = amount_out
            .checked_sub(fee)
            .ok_or(ContractError::Overflow)? as u64;
    }

    Ok(running_amount)
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
        assert_eq!(
            validate_route(&env, &route),
            Err(ContractError::EmptyRoute)
        );
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
        assert_eq!(
            estimate_route(&env, &route),
            Err(ContractError::EmptyRoute)
        );
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
}
