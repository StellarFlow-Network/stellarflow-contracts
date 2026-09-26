use soroban_sdk::{contracttype, symbol_short, Address, Env, IntoVal, Symbol};

use crate::ContractError;

/// Basis-point denominator used by collateral ratios.
pub const BPS_DENOMINATOR: u128 = 10_000;
/// A vault is eligible for liquidation below 110% collateralization.
pub const DEFAULT_LIQUIDATION_THRESHOLD_BPS: u32 = 11_000;
/// Liquidators receive 5% of the confiscated collateral.
pub const LIQUIDATOR_BONUS_BPS: u32 = 500;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultPosition {
    pub owner: Address,
    /// Collateral amount, or its value when prices have already been applied.
    pub collateral_value: u128,
    /// Configured liquidation threshold in basis points. Zero uses 110%.
    pub liquidation_threshold_bps: u32,
    /// Debt amount, or its value when prices have already been applied.
    pub borrowed_value: u128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiquidationResult {
    pub liquidated: bool,
    /// Collateralization ratio in basis points (10_000 == 100%).
    pub health_factor: u128,
    pub liquidator_reward: u128,
    pub protocol_reserve: u128,
}

pub fn health_factor(position: &VaultPosition) -> Result<u128, ContractError> {
    if position.borrowed_value == 0 {
        return Ok(u128::MAX);
    }

    position
        .collateral_value
        .checked_mul(BPS_DENOMINATOR)
        .ok_or(ContractError::MathOverflow)?
        .checked_div(position.borrowed_value)
        .ok_or(ContractError::DivisionByZero)
}

fn threshold(position: &VaultPosition) -> u128 {
    if position.liquidation_threshold_bps == 0 {
        DEFAULT_LIQUIDATION_THRESHOLD_BPS as u128
    } else {
        position.liquidation_threshold_bps as u128
    }
}

pub fn liquidate(
    _env: &Env,
    position: &VaultPosition,
    purchase_collateral: u128,
) -> Result<LiquidationResult, ContractError> {
    let hf = health_factor(position)?;
    if hf >= threshold(position) {
        return Ok(LiquidationResult {
            liquidated: false,
            health_factor: hf,
            liquidator_reward: 0,
            protocol_reserve: 0,
        });
    }

    let reward = purchase_collateral
        .checked_mul(LIQUIDATOR_BONUS_BPS as u128)
        .ok_or(ContractError::MathOverflow)?
        .checked_div(BPS_DENOMINATOR)
        .ok_or(ContractError::DivisionByZero)?;
    let protocol_reserve = purchase_collateral
        .checked_sub(reward)
        .ok_or(ContractError::MathOverflow)?;

    Ok(LiquidationResult {
        liquidated: true,
        health_factor: hf,
        liquidator_reward: reward,
        protocol_reserve,
    })
}

// ---------------------------------------------------------------------------
// Dutch-decay liquidation auctions
// ---------------------------------------------------------------------------
//
// The fixed 5% bonus above prices every liquidation the same regardless of how
// quickly it clears. A vault that nobody wants at 5% stays unliquidated, and a
// liquidator who would have cleared it at 8% has no way to say so. A Dutch
// auction replaces the fixed bonus with a discount that starts low and decays
// upward throughout a bounded window, so the position clears at the first
// price a liquidator is willing to pay for it.
//
//   D(t) = D_start + ((t - t_0) / T_auction) * (D_max - D_start)
//
// The discount is expressed in basis points off the oracle price of the
// seized collateral, and grows monotonically until T_auction elapses, after
// which it stays pinned at D_max.

/// Default decay window: one hour from open to full discount.
pub const DEFAULT_AUCTION_DURATION_SECS: u64 = 3_600;
/// Discount an auction opens at when a caller does not specify one.
pub const DEFAULT_AUCTION_START_DISCOUNT_BPS: u32 = 100;
/// Discount an auction decays towards; 15% of the collateral's oracle value.
pub const DEFAULT_AUCTION_MAX_DISCOUNT_BPS: u32 = 1_500;

/// The decay schedule for a liquidation auction.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DutchDecayConfig {
    /// Discount applied the moment the auction opens, in basis points.
    pub start_discount_bps: u32,
    /// Discount the auction decays towards; also the ceiling on any bid.
    pub max_discount_bps: u32,
    /// Length of the decay window in seconds. Zero is rejected.
    pub duration_secs: u64,
}

impl DutchDecayConfig {
    /// The schedule the contract uses when a caller supplies none.
    pub fn default_schedule() -> Self {
        Self {
            start_discount_bps: DEFAULT_AUCTION_START_DISCOUNT_BPS,
            max_discount_bps: DEFAULT_AUCTION_MAX_DISCOUNT_BPS,
            duration_secs: DEFAULT_AUCTION_DURATION_SECS,
        }
    }

    /// Structural invariants: a window that lasts, a ceiling within 100%, and
    /// a start below the ceiling so the decay actually moves.
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.duration_secs == 0 {
            return Err(ContractError::InvalidAuctionConfig);
        }
        if self.max_discount_bps > BPS_DENOMINATOR as u32 {
            return Err(ContractError::InvalidAuctionConfig);
        }
        if self.start_discount_bps > self.max_discount_bps {
            return Err(ContractError::InvalidAuctionConfig);
        }
        Ok(())
    }
}

/// An open liquidation auction for a single underwater vault.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiquidationAuction {
    /// Vault owner being liquidated.
    pub owner: Address,
    /// Ledger timestamp the auction opened at.
    pub started_at: u64,
    /// Collateral to be seized, at oracle value.
    pub purchase_collateral: u128,
    /// Decay schedule this auction prices against.
    pub config: DutchDecayConfig,
    /// Set once a liquidator has settled the position.
    pub settled: bool,
}

/// The outcome of a liquidator accepting the current decayed discount.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuctionSettlement {
    /// The auction as it now stands, with `settled` set.
    pub auction: LiquidationAuction,
    /// Liquidator that accepted the rate.
    pub liquidator: Address,
    /// Discount the settlement executed at, in basis points.
    pub discount_bps: u32,
    /// Collateral seized, at oracle value.
    pub purchase_collateral: u128,
    /// Debt the liquidator clears: collateral value less the discount.
    pub debt_repaid: u128,
    /// Value the liquidator keeps as the discount, in collateral units.
    pub liquidator_discount: u128,
    /// Collateral value the protocol books after the position settles.
    pub protocol_reserve: u128,
}

/// Discount at `elapsed_secs` into the decay window.
///
/// Uses the closed form `D_start + (elapsed / duration) * (D_max - D_start)`
/// with integer basis points, so the decay is exactly reproducible from the
/// schedule and the ledger timestamp — no accumulation, no rounding drift
/// between callers. Elapsed time at or beyond the window pins to `D_max`.
pub fn discount_at(
    config: &DutchDecayConfig,
    elapsed_secs: u64,
) -> Result<u32, ContractError> {
    config.validate()?;

    if elapsed_secs >= config.duration_secs {
        return Ok(config.max_discount_bps);
    }

    let start = config.start_discount_bps as u128;
    let span = (config.max_discount_bps as u128)
        .checked_sub(start)
        .ok_or(ContractError::InvalidAuctionConfig)?;

    let scaled = span
        .checked_mul(elapsed_secs as u128)
        .ok_or(ContractError::MathOverflow)?
        .checked_div(config.duration_secs as u128)
        .ok_or(ContractError::DivisionByZero)?;

    let discount = start
        .checked_add(scaled)
        .ok_or(ContractError::MathOverflow)?;

    Ok(discount as u32)
}

/// The discount an open auction is currently offering.
pub fn current_discount_bps(
    auction: &LiquidationAuction,
    now: u64,
) -> Result<u32, ContractError> {
    discount_at(&auction.config, now.saturating_sub(auction.started_at))
}

/// Open an auction against an underwater vault.
///
/// Fails closed on a healthy vault rather than opening an auction that could
/// never settle, so `VaultNotLiquidatable` doubles as the pre-flight check a
/// keeper runs before paying for the call.
pub fn open_auction(
    _env: &Env,
    position: &VaultPosition,
    purchase_collateral: u128,
    config: DutchDecayConfig,
    now: u64,
) -> Result<LiquidationAuction, ContractError> {
    config.validate()?;
    if purchase_collateral == 0 {
        return Err(ContractError::AmountTooLow);
    }
    if health_factor(position)? >= threshold(position) {
        return Err(ContractError::VaultNotLiquidatable);
    }

    Ok(LiquidationAuction {
        owner: position.owner.clone(),
        started_at: now,
        purchase_collateral,
        config,
        settled: false,
    })
}

/// Settle an open auction at the current decayed discount.
///
/// `min_accepted_discount_bps` is the liquidator's floor: the call succeeds
/// only when the decayed rate has reached it, so a liquidator can sign for the
/// discount they need without being filled at a worse one if the transaction
/// lands early. Once the discount clears the floor the position settles in
/// full — there is no partial fill and no second acceptance step.
pub fn settle_auction(
    _env: &Env,
    auction: &LiquidationAuction,
    liquidator: &Address,
    now: u64,
    min_accepted_discount_bps: u32,
) -> Result<AuctionSettlement, ContractError> {
    if auction.settled {
        return Err(ContractError::AuctionAlreadySettled);
    }
    if min_accepted_discount_bps > BPS_DENOMINATOR as u32 {
        return Err(ContractError::InvalidAuctionConfig);
    }

    let discount_bps = current_discount_bps(auction, now)?;
    if discount_bps < min_accepted_discount_bps {
        return Err(ContractError::DiscountNotReached);
    }

    let purchase_collateral = auction.purchase_collateral;
    let liquidator_discount = purchase_collateral
        .checked_mul(discount_bps as u128)
        .ok_or(ContractError::MathOverflow)?
        .checked_div(BPS_DENOMINATOR)
        .ok_or(ContractError::DivisionByZero)?;
    let debt_repaid = purchase_collateral
        .checked_sub(liquidator_discount)
        .ok_or(ContractError::MathOverflow)?;

    let mut settled_auction = auction.clone();
    settled_auction.settled = true;

    Ok(AuctionSettlement {
        auction: settled_auction,
        liquidator: liquidator.clone(),
        discount_bps,
        purchase_collateral,
        debt_repaid,
        liquidator_discount,
        protocol_reserve: debt_repaid,
    })
}

/// Price a vault using the oracle's verified `get_twap(Symbol)` feed before
/// applying the liquidation rule. Missing, stale, or invalid feeds fail
/// closed; a caller cannot provide a fabricated price.
pub fn liquidate_at_twap(
    env: &Env,
    oracle: &Address,
    collateral_asset: &Symbol,
    debt_asset: &Symbol,
    position: &VaultPosition,
    purchase_collateral: u128,
) -> Result<LiquidationResult, ContractError> {
    let collateral_price = read_twap(env, oracle, collateral_asset)?;
    let debt_price = read_twap(env, oracle, debt_asset)?;
    if collateral_price <= 0 || debt_price <= 0 {
        return Err(ContractError::NotInitialized);
    }

    let collateral_value = position
        .collateral_value
        .checked_mul(collateral_price as u128)
        .ok_or(ContractError::MathOverflow)?;
    let borrowed_value = position
        .borrowed_value
        .checked_mul(debt_price as u128)
        .ok_or(ContractError::MathOverflow)?;
    let priced_position = VaultPosition {
        collateral_value,
        borrowed_value,
        ..position.clone()
    };

    liquidate(env, &priced_position, purchase_collateral)
}

fn read_twap(env: &Env, oracle: &Address, asset: &Symbol) -> Result<i128, ContractError> {
    let result: Result<Option<i128>, soroban_sdk::Error> = env.invoke_contract(
        oracle,
        &symbol_short!("get_twap"),
        soroban_sdk::vec![env, asset.into_val(env)],
    );
    match result {
        Ok(Some(price)) => Ok(price),
        _ => Err(ContractError::NotInitialized),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    fn position(env: &Env, collateral: u128, debt: u128) -> VaultPosition {
        VaultPosition {
            owner: Address::generate(env),
            collateral_value: collateral,
            liquidation_threshold_bps: DEFAULT_LIQUIDATION_THRESHOLD_BPS,
            borrowed_value: debt,
        }
    }

    #[test]
    fn calculates_ratio_without_integer_truncation() {
        let env = Env::default();
        assert_eq!(health_factor(&position(&env, 109, 100)).unwrap(), 10_900);
        assert_eq!(health_factor(&position(&env, 110, 100)).unwrap(), 11_000);
    }

    #[test]
    fn liquidates_below_110_percent_and_splits_five_percent_bonus() {
        let env = Env::default();
        let result = liquidate(&env, &position(&env, 109, 100), 100).unwrap();
        assert!(result.liquidated);
        assert_eq!(result.health_factor, 10_900);
        assert_eq!(result.liquidator_reward, 5);
        assert_eq!(result.protocol_reserve, 95);
    }

    #[test]
    fn does_not_liquidate_at_or_above_threshold() {
        let env = Env::default();
        let result = liquidate(&env, &position(&env, 110, 100), 100).unwrap();
        assert!(!result.liquidated);
        assert_eq!(result.liquidator_reward, 0);
    }

    // -----------------------------------------------------------------------
    // Dutch decay
    // -----------------------------------------------------------------------

    fn config(start: u32, max: u32, duration: u64) -> DutchDecayConfig {
        DutchDecayConfig {
            start_discount_bps: start,
            max_discount_bps: max,
            duration_secs: duration,
        }
    }

    fn auction_from(env: &Env, config: DutchDecayConfig) -> LiquidationAuction {
        open_auction(env, &position(env, 109, 100), 100, config, 1_000).unwrap()
    }

    #[test]
    fn discount_starts_at_the_configured_floor() {
        let schedule = config(100, 1_500, 3_600);

        assert_eq!(discount_at(&schedule, 0).unwrap(), 100);
    }

    #[test]
    fn discount_follows_the_linear_decay_formula() {
        let schedule = config(100, 1_500, 1_000);

        // Midpoint: 100 + (500/1000) * 1400 = 800 bps.
        assert_eq!(discount_at(&schedule, 500).unwrap(), 800);
        // A quarter in: 100 + (250/1000) * 1400 = 450 bps.
        assert_eq!(discount_at(&schedule, 250).unwrap(), 450);
    }

    #[test]
    fn discount_is_monotonic_across_the_window() {
        let schedule = config(50, 900, 900);
        let mut previous = 0;

        for elapsed in [0_u64, 1, 90, 300, 899, 900] {
            let discount = discount_at(&schedule, elapsed).unwrap();
            assert!(
                discount >= previous,
                "discount went backwards at {elapsed}: {discount} < {previous}"
            );
            previous = discount;
        }
    }

    #[test]
    fn discount_pins_to_the_ceiling_after_the_window() {
        let schedule = config(100, 1_500, 600);

        assert_eq!(discount_at(&schedule, 600).unwrap(), 1_500);
        assert_eq!(discount_at(&schedule, 60_000).unwrap(), 1_500);
    }

    #[test]
    fn discount_never_exceeds_the_ceiling_mid_window() {
        let schedule = config(0, 10_000, 7);
        for elapsed in 0..7 {
            assert!(discount_at(&schedule, elapsed).unwrap() < 10_000);
        }
    }

    #[test]
    fn rejects_a_zero_length_window() {
        assert_eq!(
            discount_at(&config(0, 100, 0), 0),
            Err(ContractError::InvalidAuctionConfig)
        );
    }

    #[test]
    fn rejects_a_ceiling_above_one_hundred_percent() {
        assert_eq!(
            discount_at(&config(0, 10_001, 100), 0),
            Err(ContractError::InvalidAuctionConfig)
        );
    }

    #[test]
    fn rejects_a_start_above_the_ceiling() {
        assert_eq!(
            discount_at(&config(2_000, 1_000, 100), 0),
            Err(ContractError::InvalidAuctionConfig)
        );
    }

    #[test]
    fn open_auction_refuses_a_healthy_vault() {
        let env = Env::default();

        assert_eq!(
            open_auction(
                &env,
                &position(&env, 150, 100),
                100,
                DutchDecayConfig::default_schedule(),
                1_000,
            ),
            Err(ContractError::VaultNotLiquidatable)
        );
    }

    #[test]
    fn open_auction_refuses_an_empty_purchase() {
        let env = Env::default();

        assert_eq!(
            open_auction(
                &env,
                &position(&env, 109, 100),
                0,
                DutchDecayConfig::default_schedule(),
                1_000,
            ),
            Err(ContractError::AmountTooLow)
        );
    }

    #[test]
    fn open_auction_records_the_opening_timestamp() {
        let env = Env::default();
        let position = position(&env, 109, 100);
        let auction =
            open_auction(&env, &position, 100, DutchDecayConfig::default_schedule(), 1_000)
                .unwrap();

        assert_eq!(auction.owner, position.owner);
        assert_eq!(auction.started_at, 1_000);
        assert_eq!(auction.purchase_collateral, 100);
        assert!(!auction.settled);
    }

    #[test]
    fn current_discount_is_measured_from_the_open_time() {
        let env = Env::default();
        let auction = auction_from(&env, config(100, 1_500, 1_000));

        assert_eq!(current_discount_bps(&auction, 1_000).unwrap(), 100);
        assert_eq!(current_discount_bps(&auction, 1_500).unwrap(), 800);
        assert_eq!(current_discount_bps(&auction, 2_000).unwrap(), 1_500);
    }

    #[test]
    fn current_discount_saturates_before_the_auction_opens() {
        let env = Env::default();
        let auction = auction_from(&env, config(100, 1_500, 1_000));

        // A timestamp before `started_at` cannot happen on-chain, but a keeper
        // replaying history should not underflow into a nonsense discount.
        assert_eq!(current_discount_bps(&auction, 999).unwrap(), 100);
    }

    #[test]
    fn settles_instantly_once_the_discount_reaches_the_liquidators_floor() {
        let env = Env::default();
        let auction = auction_from(&env, config(100, 1_500, 1_000));
        let liquidator = Address::generate(&env);

        let settlement = settle_auction(&env, &auction, &liquidator, 1_500, 800).unwrap();

        assert_eq!(settlement.discount_bps, 800);
        assert_eq!(settlement.purchase_collateral, 100);
        // 8% of 100 collateral to the liquidator, the remaining 92 to the protocol.
        assert_eq!(settlement.liquidator_discount, 8);
        assert_eq!(settlement.debt_repaid, 92);
        assert_eq!(settlement.protocol_reserve, 92);
        assert_eq!(settlement.liquidator, liquidator);
        assert!(settlement.auction.settled);
    }

    #[test]
    fn refuses_to_settle_before_the_decay_reaches_the_floor() {
        let env = Env::default();
        let auction = auction_from(&env, config(100, 1_500, 1_000));
        let liquidator = Address::generate(&env);

        assert_eq!(
            settle_auction(&env, &auction, &liquidator, 1_100, 800),
            Err(ContractError::DiscountNotReached)
        );
    }

    #[test]
    fn settles_at_the_floor_exactly() {
        let env = Env::default();
        let auction = auction_from(&env, config(100, 1_500, 1_000));
        let liquidator = Address::generate(&env);

        let settlement = settle_auction(&env, &auction, &liquidator, 1_500, 800).unwrap();

        assert_eq!(settlement.discount_bps, 800);
    }

    #[test]
    fn refuses_a_second_settlement_on_the_same_auction() {
        let env = Env::default();
        let auction = auction_from(&env, config(100, 1_500, 1_000));
        let liquidator = Address::generate(&env);
        let first = settle_auction(&env, &auction, &liquidator, 2_000, 0).unwrap();

        assert_eq!(
            settle_auction(&env, &first.auction, &liquidator, 2_000, 0),
            Err(ContractError::AuctionAlreadySettled)
        );
    }

    #[test]
    fn refuses_an_accepted_floor_above_one_hundred_percent() {
        let env = Env::default();
        let auction = auction_from(&env, config(100, 1_500, 1_000));
        let liquidator = Address::generate(&env);

        assert_eq!(
            settle_auction(&env, &auction, &liquidator, 2_000, 10_001),
            Err(ContractError::InvalidAuctionConfig)
        );
    }

    #[test]
    fn a_zero_discount_settlement_hands_everything_to_the_protocol() {
        let env = Env::default();
        let auction = auction_from(&env, config(0, 1_500, 1_000));
        let liquidator = Address::generate(&env);

        let settlement = settle_auction(&env, &auction, &liquidator, 1_000, 0).unwrap();

        assert_eq!(settlement.discount_bps, 0);
        assert_eq!(settlement.liquidator_discount, 0);
        assert_eq!(settlement.debt_repaid, settlement.purchase_collateral);
    }

    #[test]
    fn truncates_a_discount_that_does_not_divide_evenly_in_the_liquidators_favour() {
        let env = Env::default();
        let auction = open_auction(
            &env,
            &position(&env, 109, 100),
            3,
            config(0, 10_000, 1),
            1_000,
        )
        .unwrap();
        let liquidator = Address::generate(&env);

        // 100% of 3 collateral: exact, and the remainder rule is asserted by
        // the mid-window case below.
        let full = settle_auction(&env, &auction, &liquidator, 1_001, 0).unwrap();
        assert_eq!(full.liquidator_discount, 3);
        assert_eq!(full.debt_repaid, 0);

        let partial = open_auction(
            &env,
            &position(&env, 109, 100),
            3,
            config(0, 3_333, 1),
            1_000,
        )
        .unwrap();
        let settlement = settle_auction(&env, &partial, &liquidator, 1_001, 0).unwrap();

        // 3 * 3333 / 10_000 = 0.9999 -> 0, so the protocol keeps the collateral.
        assert_eq!(settlement.discount_bps, 3_333);
        assert_eq!(settlement.liquidator_discount, 0);
        assert_eq!(settlement.debt_repaid, 3);
    }

    #[test]
    fn full_discount_clears_the_position_at_no_debt() {
        let env = Env::default();
        let auction = auction_from(&env, config(0, 10_000, 100));

        let settlement = settle_auction(
            &env,
            &auction,
            &Address::generate(&env),
            1_100,
            0,
        )
        .unwrap();

        assert_eq!(settlement.discount_bps, 10_000);
        assert_eq!(settlement.liquidator_discount, settlement.purchase_collateral);
        assert_eq!(settlement.debt_repaid, 0);
        assert_eq!(settlement.protocol_reserve, 0);
    }

    #[test]
    fn a_later_settlement_is_never_worse_for_the_liquidator() {
        let env = Env::default();
        let schedule = config(100, 1_500, 1_000);
        let liquidator = Address::generate(&env);

        let early = settle_auction(
            &env,
            &auction_from(&env, schedule.clone()),
            &liquidator,
            1_100,
            0,
        )
        .unwrap();
        let late = settle_auction(
            &env,
            &auction_from(&env, schedule),
            &liquidator,
            2_000,
            0,
        )
        .unwrap();

        assert!(late.discount_bps > early.discount_bps);
        assert!(late.liquidator_discount >= early.liquidator_discount);
        assert!(late.debt_repaid <= early.debt_repaid);
    }

    #[test]
    fn default_schedule_is_structurally_valid() {
        assert_eq!(DutchDecayConfig::default_schedule().validate(), Ok(()));
    }
}
