//! Concentrated liquidity position ownership + collateral transfer guard.
//!
//! A concentrated liquidity position is a unit of liquidity deployed over a
//! discrete tick range `[lower_tick, upper_tick]`. Because the range, not the
//! account, is what references the pool liquidity, ownership of a position can
//! move between accounts without touching the underlying tick accounting.
//!
//! # Uncollected fee growth
//!
//! A position also carries the accumulators that describe fees it has already
//! earned but not yet collected: a snapshot of the pool's fee-growth-inside
//! index plus the two per-token owed balances. These values belong to the
//! *position*, not to whichever account happens to own it, so an ownership
//! change must copy them verbatim rather than resetting them.
//!
//! # Transfer guard
//!
//! [`transfer_position`] rewrites the position record exactly once, so the tick
//! range moves owner atomically within the Soroban invocation. The transfer is
//! rejected unless:
//!   * the caller is the current owner and authorizes the call,
//!   * the destination differs from the current owner, and
//!   * the position is not pledged as collateral.
//!
//! On success a `PositionTransferred` event carrying the old and new owner
//! account keys is published in the same invocation.

use soroban_sdk::{contracttype, Address, Env, Symbol};

use crate::amm::ticks::{get_tick_index, MAX_TICK_INDEX, MIN_TICK_INDEX};
use crate::{AssetId, ContractError};

/// Persistent storage keys for the concentrated liquidity position registry.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PositionKey {
    /// Position record for a pool tick range, keyed by `(asset, lower, upper)`.
    Position(AssetId, i32, i32),
    /// Monotonic counter of positions opened for a pool.
    Count(AssetId),
}

/// A concentrated liquidity position deployed over `[lower_tick, upper_tick]`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConcentratedPosition {
    /// Account that currently owns the tick range.
    pub owner: Address,
    /// Pool (asset pair) the position belongs to.
    pub asset: AssetId,
    /// Lower bound of the owned tick range (inclusive).
    pub lower_tick: i32,
    /// Upper bound of the owned tick range (exclusive).
    pub upper_tick: i32,
    /// Liquidity deployed by this position.
    pub liquidity: u64,
    /// Snapshot of the pool's fee-growth-inside accumulator at the position's
    /// last update. Copied unchanged when ownership moves.
    pub fee_growth_inside_last: i128,
    /// Uncollected token-A fees owed to the position, in stroops.
    pub tokens_owed_a: u64,
    /// Uncollected token-B fees owed to the position, in stroops.
    pub tokens_owed_b: u64,
    /// When true the position is pledged as collateral and cannot be
    /// transferred.
    pub collateral_locked: bool,
}

/// Emitted when ownership of a concentrated liquidity position moves from
/// `old_owner` to `new_owner`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PositionTransferredEvent {
    pub asset: AssetId,
    pub lower_tick: i32,
    pub upper_tick: i32,
    pub old_owner: Address,
    pub new_owner: Address,
    pub liquidity: u64,
    pub fee_growth_inside_last: i128,
}

fn position_key(asset: AssetId, lower_tick: i32, upper_tick: i32) -> PositionKey {
    PositionKey::Position(asset, lower_tick, upper_tick)
}

/// Number of positions opened for a pool so far.
pub fn position_count(env: &Env, asset: AssetId) -> u32 {
    env.storage()
        .persistent()
        .get(&PositionKey::Count(asset))
        .unwrap_or(0)
}

/// Look up the position recorded for a pool tick range.
pub fn get_position(
    env: &Env,
    asset: AssetId,
    lower_tick: i32,
    upper_tick: i32,
) -> Option<ConcentratedPosition> {
    env.storage()
        .persistent()
        .get(&position_key(asset, lower_tick, upper_tick))
}

fn load_position(
    env: &Env,
    asset: AssetId,
    lower_tick: i32,
    upper_tick: i32,
) -> Result<ConcentratedPosition, ContractError> {
    get_position(env, asset, lower_tick, upper_tick).ok_or(ContractError::PositionNotFound)
}

fn save_position(env: &Env, position: &ConcentratedPosition) {
    let key = position_key(position.asset, position.lower_tick, position.upper_tick);
    env.storage().persistent().set(&key, position);
}

/// Validate that `[lower_tick, upper_tick]` is a well-formed range for the
/// pool: non-empty, strictly ordered, aligned to the pool's configured tick
/// spacing, and inside the global price bounds.
fn validate_tick_range(
    env: &Env,
    asset: AssetId,
    lower_tick: i32,
    upper_tick: i32,
) -> Result<(), ContractError> {
    let meta = get_tick_index(env, asset)?;
    if lower_tick >= upper_tick {
        return Err(ContractError::InvalidTickRange);
    }
    if lower_tick < MIN_TICK_INDEX || upper_tick > MAX_TICK_INDEX {
        return Err(ContractError::TickOutOfBounds);
    }
    if lower_tick % meta.tick_spacing != 0 || upper_tick % meta.tick_spacing != 0 {
        return Err(ContractError::TickNotAligned);
    }
    Ok(())
}

/// Open a concentrated liquidity position over `[lower_tick, upper_tick]`.
///
/// The position starts with zeroed fee-growth accumulators; fees accrue through
/// [`accrue_fees`].
pub fn open_position(
    env: &Env,
    owner: Address,
    asset: AssetId,
    lower_tick: i32,
    upper_tick: i32,
    liquidity: u64,
) -> Result<ConcentratedPosition, ContractError> {
    owner.require_auth();

    if liquidity == 0 {
        return Err(ContractError::InvalidStakeAmount);
    }
    validate_tick_range(env, asset, lower_tick, upper_tick)?;
    if get_position(env, asset, lower_tick, upper_tick).is_some() {
        return Err(ContractError::PositionAlreadyExists);
    }

    let position = ConcentratedPosition {
        owner: owner.clone(),
        asset,
        lower_tick,
        upper_tick,
        liquidity,
        fee_growth_inside_last: 0,
        tokens_owed_a: 0,
        tokens_owed_b: 0,
        collateral_locked: false,
    };
    save_position(env, &position);

    let counter_key = PositionKey::Count(asset);
    let count: u32 = env.storage().persistent().get(&counter_key).unwrap_or(0);
    env.storage().persistent().set(&counter_key, &(count + 1));

    Ok(position)
}

/// Accrue uncollected fees onto a position.
///
/// `fee_growth_delta` is the amount by which the pool's fee-growth-inside index
/// advanced since the position's last update; `tokens_a` / `tokens_b` are the
/// newly earned per-token fees. Only the owner may accrue.
pub fn accrue_fees(
    env: &Env,
    owner: Address,
    asset: AssetId,
    lower_tick: i32,
    upper_tick: i32,
    fee_growth_delta: i128,
    tokens_a: u64,
    tokens_b: u64,
) -> Result<ConcentratedPosition, ContractError> {
    owner.require_auth();

    let mut position = load_position(env, asset, lower_tick, upper_tick)?;
    if position.owner != owner {
        return Err(ContractError::PositionNotOwned);
    }

    position.fee_growth_inside_last = position
        .fee_growth_inside_last
        .checked_add(fee_growth_delta)
        .ok_or(ContractError::Overflow)?;
    position.tokens_owed_a = position
        .tokens_owed_a
        .checked_add(tokens_a)
        .ok_or(ContractError::Overflow)?;
    position.tokens_owed_b = position
        .tokens_owed_b
        .checked_add(tokens_b)
        .ok_or(ContractError::Overflow)?;
    save_position(env, &position);

    Ok(position)
}

/// Pledge or release a position as collateral.
///
/// While locked the position cannot change owner; only the current owner may
/// toggle the flag.
pub fn set_collateral_lock(
    env: &Env,
    owner: Address,
    asset: AssetId,
    lower_tick: i32,
    upper_tick: i32,
    locked: bool,
) -> Result<ConcentratedPosition, ContractError> {
    owner.require_auth();

    let mut position = load_position(env, asset, lower_tick, upper_tick)?;
    if position.owner != owner {
        return Err(ContractError::PositionNotOwned);
    }
    position.collateral_locked = locked;
    save_position(env, &position);

    Ok(position)
}

/// Transfer ownership of a concentrated liquidity position to `new_owner`.
///
/// The tick range ownership moves atomically in a single storage write: only
/// the `owner` field changes, so the uncollected fee-growth accumulators
/// (`fee_growth_inside_last`, `tokens_owed_a`, `tokens_owed_b`) follow the
/// position unchanged. A `PositionTransferred` event with the old and new owner
/// keys is published in the same invocation.
pub fn transfer_position(
    env: &Env,
    owner: Address,
    asset: AssetId,
    lower_tick: i32,
    upper_tick: i32,
    new_owner: Address,
) -> Result<ConcentratedPosition, ContractError> {
    owner.require_auth();

    let mut position = load_position(env, asset, lower_tick, upper_tick)?;
    if position.owner != owner {
        return Err(ContractError::PositionNotOwned);
    }
    if new_owner == owner {
        return Err(ContractError::PositionTransferToSelf);
    }
    if position.collateral_locked {
        return Err(ContractError::PositionCollateralLocked);
    }

    let old_owner = position.owner.clone();
    // Ownership moves; every accrued-fee field is intentionally preserved.
    position.owner = new_owner.clone();
    save_position(env, &position);

    let event = PositionTransferredEvent {
        asset,
        lower_tick,
        upper_tick,
        old_owner,
        new_owner,
        liquidity: position.liquidity,
        fee_growth_inside_last: position.fee_growth_inside_last,
    };
    env.events().publish(
        (
            Symbol::new(env, "PositionTransferred"),
            asset,
            lower_tick,
            upper_tick,
        ),
        event,
    );

    Ok(position)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amm::ticks::initialize_tick_index;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::{IntoVal, TryFromVal, Val};

    fn setup(env: &Env, asset: AssetId) {
        env.mock_all_auths();
        initialize_tick_index(env, asset, 10).unwrap();
    }

    #[test]
    fn open_position_records_owner_and_range() {
        let env = Env::default();
        setup(&env, 1);
        let owner = Address::generate(&env);

        let pos = open_position(&env, owner.clone(), 1, -100, 100, 5_000).unwrap();

        assert_eq!(pos.owner, owner);
        assert_eq!(pos.asset, 1);
        assert_eq!(pos.lower_tick, -100);
        assert_eq!(pos.upper_tick, 100);
        assert_eq!(pos.liquidity, 5_000);
        assert_eq!(pos.fee_growth_inside_last, 0);
        assert_eq!(pos.tokens_owed_a, 0);
        assert_eq!(pos.tokens_owed_b, 0);
        assert!(!pos.collateral_locked);

        let stored = get_position(&env, 1, -100, 100).unwrap();
        assert_eq!(stored, pos);
        assert_eq!(position_count(&env, 1), 1);
    }

    #[test]
    fn open_position_rejects_empty_range() {
        let env = Env::default();
        setup(&env, 1);
        let owner = Address::generate(&env);

        assert_eq!(
            open_position(&env, owner.clone(), 1, 100, 100, 1_000),
            Err(ContractError::InvalidTickRange)
        );
        assert_eq!(
            open_position(&env, owner, 1, 200, 100, 1_000),
            Err(ContractError::InvalidTickRange)
        );
    }

    #[test]
    fn open_position_rejects_unaligned_ticks() {
        let env = Env::default();
        setup(&env, 1);
        let owner = Address::generate(&env);

        assert_eq!(
            open_position(&env, owner, 1, -95, 100, 1_000),
            Err(ContractError::TickNotAligned)
        );
    }

    #[test]
    fn open_position_rejects_zero_liquidity() {
        let env = Env::default();
        setup(&env, 1);
        let owner = Address::generate(&env);

        assert_eq!(
            open_position(&env, owner, 1, -100, 100, 0),
            Err(ContractError::InvalidStakeAmount)
        );
    }

    #[test]
    fn open_position_rejects_duplicate_range() {
        let env = Env::default();
        setup(&env, 1);
        let owner = Address::generate(&env);

        open_position(&env, owner.clone(), 1, -100, 100, 1_000).unwrap();
        assert_eq!(
            open_position(&env, owner, 1, -100, 100, 1_000),
            Err(ContractError::PositionAlreadyExists)
        );
    }

    #[test]
    fn transfer_moves_ownership() {
        let env = Env::default();
        setup(&env, 1);
        let owner = Address::generate(&env);
        let new_owner = Address::generate(&env);

        open_position(&env, owner.clone(), 1, -100, 100, 5_000).unwrap();
        let moved = transfer_position(&env, owner.clone(), 1, -100, 100, new_owner.clone()).unwrap();

        assert_eq!(moved.owner, new_owner);
        let stored = get_position(&env, 1, -100, 100).unwrap();
        assert_eq!(stored.owner, new_owner);
        assert_eq!(stored.lower_tick, -100);
        assert_eq!(stored.upper_tick, 100);
        assert_eq!(stored.liquidity, 5_000);
    }

    #[test]
    fn transfer_preserves_uncollected_fee_growth() {
        let env = Env::default();
        setup(&env, 1);
        let owner = Address::generate(&env);
        let new_owner = Address::generate(&env);

        open_position(&env, owner.clone(), 1, -100, 100, 5_000).unwrap();
        let accrued =
            accrue_fees(&env, owner.clone(), 1, -100, 100, 987_654_321, 42, 7).unwrap();
        assert_eq!(accrued.fee_growth_inside_last, 987_654_321);
        assert_eq!(accrued.tokens_owed_a, 42);
        assert_eq!(accrued.tokens_owed_b, 7);

        let moved = transfer_position(&env, owner, 1, -100, 100, new_owner.clone()).unwrap();

        assert_eq!(moved.owner, new_owner);
        assert_eq!(moved.fee_growth_inside_last, 987_654_321);
        assert_eq!(moved.tokens_owed_a, 42);
        assert_eq!(moved.tokens_owed_b, 7);

        let stored = get_position(&env, 1, -100, 100).unwrap();
        assert_eq!(stored.fee_growth_inside_last, 987_654_321);
        assert_eq!(stored.tokens_owed_a, 42);
        assert_eq!(stored.tokens_owed_b, 7);
    }

    #[test]
    fn transfer_requires_current_owner() {
        let env = Env::default();
        setup(&env, 1);
        let owner = Address::generate(&env);
        let stranger = Address::generate(&env);
        let new_owner = Address::generate(&env);

        open_position(&env, owner, 1, -100, 100, 5_000).unwrap();
        assert_eq!(
            transfer_position(&env, stranger, 1, -100, 100, new_owner),
            Err(ContractError::PositionNotOwned)
        );
    }

    #[test]
    fn transfer_rejects_self_transfer() {
        let env = Env::default();
        setup(&env, 1);
        let owner = Address::generate(&env);

        open_position(&env, owner.clone(), 1, -100, 100, 5_000).unwrap();
        assert_eq!(
            transfer_position(&env, owner.clone(), 1, -100, 100, owner),
            Err(ContractError::PositionTransferToSelf)
        );
    }

    #[test]
    fn transfer_rejects_unknown_position() {
        let env = Env::default();
        setup(&env, 1);
        let owner = Address::generate(&env);
        let new_owner = Address::generate(&env);

        assert_eq!(
            transfer_position(&env, owner, 1, -100, 100, new_owner),
            Err(ContractError::PositionNotFound)
        );
    }

    #[test]
    fn collateral_locked_position_cannot_transfer() {
        let env = Env::default();
        setup(&env, 1);
        let owner = Address::generate(&env);
        let new_owner = Address::generate(&env);

        open_position(&env, owner.clone(), 1, -100, 100, 5_000).unwrap();
        set_collateral_lock(&env, owner.clone(), 1, -100, 100, true).unwrap();

        assert_eq!(
            transfer_position(&env, owner.clone(), 1, -100, 100, new_owner.clone()),
            Err(ContractError::PositionCollateralLocked)
        );

        // Releasing the collateral re-enables transfer.
        set_collateral_lock(&env, owner.clone(), 1, -100, 100, false).unwrap();
        let moved = transfer_position(&env, owner, 1, -100, 100, new_owner).unwrap();
        assert!(!moved.collateral_locked);
    }

    #[test]
    fn transfer_emits_position_transferred_event() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let asset: AssetId = 7;
        let owner = Address::generate(&env);
        let new_owner = Address::generate(&env);

        env.as_contract(&contract_id, || {
            initialize_tick_index(&env, asset, 10).unwrap();
            open_position(&env, owner.clone(), asset, -100, 100, 5_000).unwrap();
            accrue_fees(&env, owner.clone(), asset, -100, 100, 5, 10, 20).unwrap();
            transfer_position(&env, owner.clone(), asset, -100, 100, new_owner.clone()).unwrap();

            let topic: Val = Symbol::new(&env, "PositionTransferred").into_val(&env);
            let mut found = false;
            for item in env.events().all().iter() {
                if item.1.contains(topic) {
                    let ev = PositionTransferredEvent::try_from_val(&env, &item.2).unwrap();
                    assert_eq!(ev.asset, asset);
                    assert_eq!(ev.lower_tick, -100);
                    assert_eq!(ev.upper_tick, 100);
                    assert_eq!(ev.old_owner, owner);
                    assert_eq!(ev.new_owner, new_owner);
                    assert_eq!(ev.liquidity, 5_000);
                    assert_eq!(ev.fee_growth_inside_last, 5);
                    found = true;
                }
            }
            assert!(found, "PositionTransferred event not emitted");
        });
    }
}
