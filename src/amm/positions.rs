//! Concentrated-liquidity position lifecycle (Issue #986): opening ranged
//! positions atop `amm::ticks`'s tick-indexed liquidity accounting, and
//! splitting an existing position's range into two sub-ranges without
//! withdrawing the underlying pool liquidity.
//!
//! # Position receipts
//!
//! A position is identified by an incrementing `u64` id — its "receipt
//! token". [`open_position`] mints a fresh id for the caller; [`split_position`]
//! burns the original id and mints two new ones for the sub-ranges. This
//! mirrors the existing LP-share model in `settlement::fees`, which is also a
//! plain persistent-storage record rather than a separate SEP-41 token
//! contract.
//!
//! # Liquidity allocation on split
//!
//! The original position's liquidity is split between the two sub-ranges in
//! proportion to each sub-range's tick width:
//!
//! ```text
//! liquidity_lower = liquidity * (tick_mid - tick_lower) / (tick_upper - tick_lower)
//! liquidity_upper = liquidity - liquidity_lower
//! ```
//!
//! This conserves total liquidity exactly and is deterministic. It is a
//! linear approximation, not a sqrt-price-exact capital-conserving split
//! (which would additionally require tracking each position's underlying
//! token0/token1 composition — a model this codebase does not have; see
//! `amm::ticks`'s module docs).
//!
//! # Fee growth on split
//!
//! Fees earned by the original position since it was opened (or last
//! touched) are settled against `fee_growth_inside_last` and apportioned to
//! `tokens_owed` on the two new positions in the same tick-width proportion
//! as the liquidity split. Each new position's `fee_growth_inside_last` is
//! then checkpointed to the current fee-growth-inside value for its own
//! (narrower) range, so future accrual is tracked independently per
//! sub-range going forward.

use soroban_sdk::{contracttype, symbol_short, Address, Env, Symbol};

use crate::amm::ticks;
use crate::{AssetId, ContractError};

/// Persistent storage key for an individual position record, keyed by its
/// receipt-token id.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PositionKey(u64);

/// Instance storage key for the monotonically increasing position-id
/// counter (shared across all pools — receipt-token ids are globally
/// unique, matching how an NFT-style id allocator would behave).
const POSITION_COUNTER_KEY: Symbol = symbol_short!("POSCTR");

/// A concentrated-liquidity range position.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Position {
    /// This position's receipt-token id.
    pub id: u64,
    /// The address that owns this position and may split or (in a future
    /// close/withdraw operation) redeem it.
    pub owner: Address,
    /// Pool asset identifier.
    pub asset: AssetId,
    /// Lower tick boundary (inclusive).
    pub tick_lower: i32,
    /// Upper tick boundary (exclusive).
    pub tick_upper: i32,
    /// Liquidity committed to this range.
    pub liquidity: u64,
    /// `fee_growth_inside` (see `ticks::get_fee_growth_inside`) snapshotted
    /// the last time this position's owed fees were settled.
    pub fee_growth_inside_last: u128,
    /// Fees settled and not yet claimed, in the pool's fee unit.
    pub tokens_owed: u64,
}

/// Result of [`split_position`]: the two new sub-range positions.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct SplitPositionResult {
    pub lower: Position,
    pub upper: Position,
}

fn get_position_record(env: &Env, position_id: u64) -> Result<Position, ContractError> {
    env.storage()
        .persistent()
        .get(&PositionKey(position_id))
        .ok_or(ContractError::PositionNotFound)
}

fn set_position_record(env: &Env, position: &Position) {
    env.storage()
        .persistent()
        .set(&PositionKey(position.id), position);
}

fn remove_position_record(env: &Env, position_id: u64) {
    env.storage().persistent().remove(&PositionKey(position_id));
}

fn mint_position_id(env: &Env) -> Result<u64, ContractError> {
    let next: u64 = env
        .storage()
        .instance()
        .get(&POSITION_COUNTER_KEY)
        .unwrap_or(0);
    let id = next.checked_add(1).ok_or(ContractError::Overflow)?;
    env.storage().instance().set(&POSITION_COUNTER_KEY, &id);
    Ok(id)
}

/// Load a position by its receipt-token id.
pub fn get_position(env: &Env, position_id: u64) -> Result<Position, ContractError> {
    get_position_record(env, position_id)
}

/// Open a new concentrated-liquidity range position, placing `liquidity` at
/// both boundary ticks of `[tick_lower, tick_upper)` and minting a fresh
/// position-receipt id to `owner`.
pub fn open_position(
    env: &Env,
    owner: Address,
    asset: AssetId,
    tick_lower: i32,
    tick_upper: i32,
    liquidity: u64,
) -> Result<Position, ContractError> {
    owner.require_auth();

    if tick_lower >= tick_upper {
        return Err(ContractError::InvalidSplitBoundary);
    }
    if liquidity == 0 {
        return Err(ContractError::AmountTooLow);
    }

    let liquidity_i64 = i64::try_from(liquidity).map_err(|_| ContractError::Overflow)?;

    ticks::ensure_tick_initialized(env, asset, tick_lower)?;
    ticks::ensure_tick_initialized(env, asset, tick_upper)?;
    ticks::place_liquidity(env, asset, tick_lower, liquidity_i64)?;
    ticks::place_liquidity(env, asset, tick_upper, -liquidity_i64)?;

    let fee_growth_inside_last = ticks::get_fee_growth_inside(env, asset, tick_lower, tick_upper)?;

    let id = mint_position_id(env)?;
    let position = Position {
        id,
        owner,
        asset,
        tick_lower,
        tick_upper,
        liquidity,
        fee_growth_inside_last,
        tokens_owed: 0,
    };
    set_position_record(env, &position);
    Ok(position)
}

/// Split `position_id`'s range at `tick_mid` into `[tick_lower, tick_mid]`
/// and `[tick_mid, tick_upper]`, without withdrawing the underlying pool
/// liquidity. Burns `position_id` and mints two new position-receipt ids.
///
/// See the module docs for the liquidity-allocation and fee-growth
/// recalculation rules.
pub fn split_position(
    env: &Env,
    caller: Address,
    position_id: u64,
    tick_mid: i32,
) -> Result<SplitPositionResult, ContractError> {
    caller.require_auth();

    let position = get_position_record(env, position_id)?;
    if position.owner != caller {
        return Err(ContractError::Unauthorized);
    }
    if tick_mid <= position.tick_lower || tick_mid >= position.tick_upper {
        return Err(ContractError::InvalidSplitBoundary);
    }

    let meta = ticks::get_tick_index(env, position.asset)?;
    if tick_mid % meta.tick_spacing != 0 {
        return Err(ContractError::TickNotAligned);
    }

    // ── Settle fees owed on the original range up to now ────────────────
    let fee_growth_inside_now = ticks::get_fee_growth_inside(
        env,
        position.asset,
        position.tick_lower,
        position.tick_upper,
    )?;
    let fee_growth_delta = fee_growth_inside_now.wrapping_sub(position.fee_growth_inside_last);
    let accrued = fee_growth_delta
        .checked_mul(position.liquidity as u128)
        .ok_or(ContractError::Overflow)?
        / ticks::FEE_GROWTH_SCALE;
    let total_owed_u128 = (position.tokens_owed as u128)
        .checked_add(accrued)
        .ok_or(ContractError::Overflow)?;
    let total_owed = u64::try_from(total_owed_u128).map_err(|_| ContractError::Overflow)?;

    // ── Proportional liquidity split by tick width ──────────────────────
    let width_total = (position.tick_upper - position.tick_lower) as u128;
    let width_lower = (tick_mid - position.tick_lower) as u128;

    let liquidity_lower = ((position.liquidity as u128)
        .checked_mul(width_lower)
        .ok_or(ContractError::Overflow)?
        / width_total) as u64;
    let liquidity_upper = position
        .liquidity
        .checked_sub(liquidity_lower)
        .ok_or(ContractError::Overflow)?;

    if liquidity_lower == 0 || liquidity_upper == 0 {
        // The range is too narrow (in tick-width terms) for this split to
        // leave both sub-ranges with non-zero liquidity.
        return Err(ContractError::AmountTooLow);
    }

    let owed_lower = ((total_owed as u128)
        .checked_mul(width_lower)
        .ok_or(ContractError::Overflow)?
        / width_total) as u64;
    let owed_upper = total_owed
        .checked_sub(owed_lower)
        .ok_or(ContractError::Overflow)?;

    // ── Re-point tick boundaries at the new mid tick ────────────────────
    //
    // The original range contributed +L at tick_lower and -L at
    // tick_upper. After the split: the lower sub-range must contribute
    // +L_lower at tick_lower and -L_lower at tick_mid; the upper sub-range
    // must contribute +L_upper at tick_mid and -L_upper at tick_upper.
    // Deltas below move each tick from its pre-split net contribution to
    // its post-split one; `insert_tick_sorted`'s MAX_TICKS_PER_POOL check
    // inside `place_liquidity` guards tick_mid's insertion.
    let liquidity_lower_i64 = i64::try_from(liquidity_lower).map_err(|_| ContractError::Overflow)?;
    let liquidity_upper_i64 = i64::try_from(liquidity_upper).map_err(|_| ContractError::Overflow)?;

    ticks::ensure_tick_initialized(env, position.asset, tick_mid)?;

    // tick_lower: +L -> +L_lower
    ticks::place_liquidity(env, position.asset, position.tick_lower, -liquidity_upper_i64)?;
    // tick_mid: 0 -> +L_upper - L_lower
    ticks::place_liquidity(
        env,
        position.asset,
        tick_mid,
        liquidity_upper_i64 - liquidity_lower_i64,
    )?;
    // tick_upper: -L -> -L_upper
    ticks::place_liquidity(env, position.asset, position.tick_upper, liquidity_lower_i64)?;

    // ── Checkpoint fee growth for each new (narrower) sub-range ─────────
    let fee_growth_inside_lower =
        ticks::get_fee_growth_inside(env, position.asset, position.tick_lower, tick_mid)?;
    let fee_growth_inside_upper =
        ticks::get_fee_growth_inside(env, position.asset, tick_mid, position.tick_upper)?;

    remove_position_record(env, position_id);

    let lower_id = mint_position_id(env)?;
    let lower = Position {
        id: lower_id,
        owner: position.owner.clone(),
        asset: position.asset,
        tick_lower: position.tick_lower,
        tick_upper: tick_mid,
        liquidity: liquidity_lower,
        fee_growth_inside_last: fee_growth_inside_lower,
        tokens_owed: owed_lower,
    };
    set_position_record(env, &lower);

    let upper_id = mint_position_id(env)?;
    let upper = Position {
        id: upper_id,
        owner: position.owner.clone(),
        asset: position.asset,
        tick_lower: tick_mid,
        tick_upper: position.tick_upper,
        liquidity: liquidity_upper,
        fee_growth_inside_last: fee_growth_inside_upper,
        tokens_owed: owed_upper,
    };
    set_position_record(env, &upper);

    crate::events::publish_position_split(
        env,
        &position.owner,
        position.asset,
        position_id,
        position.tick_lower,
        tick_mid,
        position.tick_upper,
        lower_id,
        liquidity_lower,
        upper_id,
        liquidity_upper,
    );

    Ok(SplitPositionResult { lower, upper })
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    fn setup_pool(env: &Env, asset: AssetId) {
        ticks::initialize_tick_index(env, asset, 1).unwrap();
    }

    #[test]
    fn open_position_places_liquidity_at_both_boundaries() {
        let env = Env::default();
        env.mock_all_auths();
        let asset: AssetId = 1;
        setup_pool(&env, asset);
        let owner = Address::generate(&env);

        let position = open_position(&env, owner.clone(), asset, -10, 10, 1000).unwrap();
        assert_eq!(position.id, 1);
        assert_eq!(position.liquidity, 1000);
        assert_eq!(position.tokens_owed, 0);

        let meta = ticks::get_tick_index(&env, asset).unwrap();
        assert_eq!(meta.active_liquidity, 1000);
        assert_eq!(meta.tick_count, 2);
    }

    #[test]
    fn open_position_rejects_inverted_range() {
        let env = Env::default();
        env.mock_all_auths();
        let asset: AssetId = 1;
        setup_pool(&env, asset);
        let owner = Address::generate(&env);

        assert_eq!(
            open_position(&env, owner, asset, 10, -10, 1000),
            Err(ContractError::InvalidSplitBoundary)
        );
    }

    #[test]
    fn open_position_rejects_zero_liquidity() {
        let env = Env::default();
        env.mock_all_auths();
        let asset: AssetId = 1;
        setup_pool(&env, asset);
        let owner = Address::generate(&env);

        assert_eq!(
            open_position(&env, owner, asset, -10, 10, 0),
            Err(ContractError::AmountTooLow)
        );
    }

    #[test]
    fn split_position_conserves_total_liquidity() {
        let env = Env::default();
        env.mock_all_auths();
        let asset: AssetId = 1;
        setup_pool(&env, asset);
        let owner = Address::generate(&env);

        let position = open_position(&env, owner.clone(), asset, -10, 10, 1000).unwrap();
        let result = split_position(&env, owner, position.id, 0).unwrap();

        assert_eq!(result.lower.liquidity + result.upper.liquidity, 1000);
        // Midpoint split of a symmetric [-10, 10] range: equal halves.
        assert_eq!(result.lower.liquidity, 500);
        assert_eq!(result.upper.liquidity, 500);
        assert_eq!(result.lower.tick_lower, -10);
        assert_eq!(result.lower.tick_upper, 0);
        assert_eq!(result.upper.tick_lower, 0);
        assert_eq!(result.upper.tick_upper, 10);
    }

    #[test]
    fn split_position_burns_the_original_id() {
        let env = Env::default();
        env.mock_all_auths();
        let asset: AssetId = 1;
        setup_pool(&env, asset);
        let owner = Address::generate(&env);

        let position = open_position(&env, owner.clone(), asset, -10, 10, 1000).unwrap();
        split_position(&env, owner, position.id, 0).unwrap();

        assert_eq!(
            get_position(&env, position.id),
            Err(ContractError::PositionNotFound)
        );
    }

    #[test]
    fn split_position_recalculates_active_liquidity_when_mid_is_above_current_tick() {
        let env = Env::default();
        env.mock_all_auths();
        let asset: AssetId = 1;
        setup_pool(&env, asset);
        let owner = Address::generate(&env);

        // current_tick defaults to 0; range [-10, 10] (width 20) straddles it,
        // so its full liquidity counts as active.
        let position = open_position(&env, owner.clone(), asset, -10, 10, 1000).unwrap();
        let meta_before = ticks::get_tick_index(&env, asset).unwrap();
        assert_eq!(meta_before.active_liquidity, 1000);

        // Splitting at tick_mid = 5 (itself above current_tick = 0) leaves
        // only the lower sub-range [-10, 5) straddling the current price;
        // the upper sub-range [5, 10) is now entirely above it and so no
        // longer contributes to active liquidity.
        let result = split_position(&env, owner, position.id, 5).unwrap();
        assert_eq!(result.lower.liquidity, 750); // width 15/20 of 1000
        assert_eq!(result.upper.liquidity, 250); // width 5/20 of 1000

        let meta_after = ticks::get_tick_index(&env, asset).unwrap();
        assert_eq!(meta_after.active_liquidity, 750);
    }

    #[test]
    fn split_position_at_current_tick_moves_all_active_liquidity_to_one_side() {
        let env = Env::default();
        env.mock_all_auths();
        let asset: AssetId = 1;
        setup_pool(&env, asset);
        let owner = Address::generate(&env);

        // current_tick defaults to 0; splitting exactly there means neither
        // new sub-range still straddles the current price in the way the
        // original range did ([-10, 0) no longer includes 0, since the upper
        // bound is exclusive), so total active liquidity reflects only the
        // ticks at or below 0 under the standard net-liquidity convention.
        let position = open_position(&env, owner.clone(), asset, -10, 10, 1000).unwrap();
        split_position(&env, owner, position.id, 0).unwrap();

        let meta_after = ticks::get_tick_index(&env, asset).unwrap();
        assert_eq!(meta_after.active_liquidity, 500);
    }

    #[test]
    fn split_position_rejects_non_owner() {
        let env = Env::default();
        env.mock_all_auths();
        let asset: AssetId = 1;
        setup_pool(&env, asset);
        let owner = Address::generate(&env);
        let intruder = Address::generate(&env);

        let position = open_position(&env, owner, asset, -10, 10, 1000).unwrap();
        assert_eq!(
            split_position(&env, intruder, position.id, 0),
            Err(ContractError::Unauthorized)
        );
    }

    #[test]
    fn split_position_rejects_mid_outside_range() {
        let env = Env::default();
        env.mock_all_auths();
        let asset: AssetId = 1;
        setup_pool(&env, asset);
        let owner = Address::generate(&env);

        let position = open_position(&env, owner.clone(), asset, -10, 10, 1000).unwrap();
        assert_eq!(
            split_position(&env, owner.clone(), position.id, 10),
            Err(ContractError::InvalidSplitBoundary)
        );
        assert_eq!(
            split_position(&env, owner.clone(), position.id, -10),
            Err(ContractError::InvalidSplitBoundary)
        );
        assert_eq!(
            split_position(&env, owner, position.id, 20),
            Err(ContractError::InvalidSplitBoundary)
        );
    }

    #[test]
    fn split_position_rejects_unaligned_mid() {
        let env = Env::default();
        env.mock_all_auths();
        let asset: AssetId = 1;
        ticks::initialize_tick_index(&env, asset, 10).unwrap();
        let owner = Address::generate(&env);

        let position = open_position(&env, owner.clone(), asset, -20, 20, 1000).unwrap();
        assert_eq!(
            split_position(&env, owner, position.id, 5),
            Err(ContractError::TickNotAligned)
        );
    }

    #[test]
    fn split_position_rejects_unknown_id() {
        let env = Env::default();
        env.mock_all_auths();
        let asset: AssetId = 1;
        setup_pool(&env, asset);
        let owner = Address::generate(&env);

        assert_eq!(
            split_position(&env, owner, 999, 0),
            Err(ContractError::PositionNotFound)
        );
    }

    #[test]
    fn split_position_apportions_accrued_fees_proportionally() {
        let env = Env::default();
        env.mock_all_auths();
        let asset: AssetId = 1;
        setup_pool(&env, asset);
        let owner = Address::generate(&env);

        // Symmetric range so the fee split should also be symmetric.
        let position = open_position(&env, owner.clone(), asset, -10, 10, 1000).unwrap();
        ticks::accrue_fee_growth(&env, asset, 1000).unwrap();

        let result = split_position(&env, owner, position.id, 0).unwrap();
        assert_eq!(result.lower.tokens_owed + result.upper.tokens_owed, 1000);
        assert_eq!(result.lower.tokens_owed, 500);
        assert_eq!(result.upper.tokens_owed, 500);
    }

    #[test]
    fn split_position_asymmetric_width_splits_liquidity_proportionally() {
        let env = Env::default();
        env.mock_all_auths();
        let asset: AssetId = 1;
        setup_pool(&env, asset);
        let owner = Address::generate(&env);

        // [-10, 30]: width 40. Splitting at 10 -> widths 20/20 -> even split
        // despite the asymmetric absolute tick values.
        let position = open_position(&env, owner.clone(), asset, -10, 30, 1000).unwrap();
        let result = split_position(&env, owner, position.id, 10).unwrap();
        assert_eq!(result.lower.liquidity, 500);
        assert_eq!(result.upper.liquidity, 500);

        // [-10, 30] split at 0 -> widths 10/30 -> 1:3 split.
        let env2 = Env::default();
        env2.mock_all_auths();
        setup_pool(&env2, asset);
        let owner2 = Address::generate(&env2);
        let position2 = open_position(&env2, owner2.clone(), asset, -10, 30, 1000).unwrap();
        let result2 = split_position(&env2, owner2, position2.id, 0).unwrap();
        assert_eq!(result2.lower.liquidity, 250);
        assert_eq!(result2.upper.liquidity, 750);
    }
}
