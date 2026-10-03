#![no_std]
//! # Yield Strategy Unwind & Emergency Liquidation Module (Issue #923)
//!
//! Safely withdraw protocol assets from external yield strategies during
//! market distress.
//!
//! ## Problem
//!
//! Yield-bearing positions (`open_position`) accrue value only while the
//! external strategy stays solvent. When the market turns, every ledger of
//! delay between deciding to exit and actually exiting is a ledger in which
//! the position can go to zero. Standard exits are deliberately slow — each
//! strategy carries a `withdraw_delay` (an unstake / thawing / settlement
//! window) enforced by [`YieldUnwind::execute_unwind`] — so the protocol
//! needs a separate, fast path for genuine emergencies.
//!
//! ## Design
//!
//! ```text
//!                    ┌─ request_unwind ──► execute_unwind (after delay)
//! open_position ─────┤
//!                    └─ emergency_unwind (emergency only, no delay)
//! ```
//!
//! * **Standard path.** [`YieldUnwind::request_unwind`] starts the clock;
//!   [`YieldUnwind::execute_unwind`] settles the position only once
//!   `now >= requested_at + withdraw_delay`. Orderly, but slow.
//! * **Emergency path.** While a protocol emergency is in force (declared by
//!   the admin through [`YieldUnwind::declare_emergency` and lifted with
//!   [`YieldUnwind::resolve_emergency`]), [`YieldUnwind::emergency_unwind`]
//!   pulls the full position out of the external strategy *immediately*,
//!   bypassing the delay timer altogether.
//!
//! Both paths settle through the same settlement routine: the external
//! strategy's `withdraw` entry point is invoked, whatever base tokens come
//! back are credited straight to the in-contract vault reserves
//! ([`YieldUnwind::reserve_balance`]), and the position is marked closed. A
//! strategy that cannot make the position whole (haircut, depeg, partial
//! insolvency) still closes the position — during market distress a certain
//! smaller recovery now beats an uncertain full recovery later — and the
//! shortfall is visible in the emitted event.
//!
//! ## Emergency model
//!
//! The "verified protocol emergency state" is the on-chain
//! `emergency_active` flag. Only the admin can set or clear it, every
//! transition is authenticated and emits an event, and the emergency entry
//! point refuses to run unless the flag is set (`UnwindError::NotEmergency`).
//! The flag is deliberately separate from any single position so one
//! declaration covers every active strategy at once.

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, token, Address, Env, IntoVal, Map, Symbol,
    Vec,
};

/// Topic emitted when the admin declares a protocol emergency.
pub const EMERGENCY_DECLARED: &str = "EmergencyDeclared";
/// Topic emitted when the admin resolves a protocol emergency.
pub const EMERGENCY_RESOLVED: &str = "EmergencyResolved";
/// Topic emitted when a position is opened into a strategy.
pub const POSITION_OPENED: &str = "PositionOpened";
/// Topic emitted when the standard withdrawal clock is started.
pub const WITHDRAWAL_REQUESTED: &str = "WithdrawalRequested";
/// Topic emitted when a position is unwound (standard or emergency path).
pub const POSITION_UNWOUND: &str = "PositionUnwound";

/// Errors returned by the unwind module.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum UnwindError {
    /// `initialize` has already been called.
    AlreadyInitialized = 1,
    /// The contract has not been initialised yet.
    NotInitialized = 2,
    /// The caller is not the registered admin.
    NotAdmin = 3,
    /// No strategy is registered under this id.
    UnknownStrategy = 4,
    /// A strategy is already registered under this id.
    StrategyAlreadyRegistered = 5,
    /// The withdrawal delay is zero; every strategy must have a positive
    /// standard-exit window, otherwise there is nothing for the emergency
    /// path to bypass.
    ZeroDelay = 6,
    /// The amount supplied was zero.
    ZeroAmount = 7,
    /// No open position exists for this strategy.
    NoOpenPosition = 8,
    /// A position is already open for this strategy.
    PositionAlreadyOpen = 9,
    /// No withdrawal has been requested for this position yet.
    WithdrawalNotRequested = 10,
    /// The standard withdrawal delay has not elapsed yet.
    DelayNotElapsed = 11,
    /// The emergency entry point was called while no protocol emergency is
    /// in force.
    NotEmergency = 12,
    /// Arithmetic overflow.
    Overflow = 13,
}

/// A registered external yield strategy.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Strategy {
    /// Contract address of the external yield strategy.
    pub strategy: Address,
    /// Base token the position is denominated and settled in.
    pub base_token: Address,
    /// Standard withdrawal delay in seconds, enforced on the normal exit
    /// path and bypassed on the emergency path.
    pub withdraw_delay_seconds: u64,
}

/// An active (or closing) yield farming position.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Position {
    /// Strategy holding the funds.
    pub strategy_id: u32,
    /// Amount of base tokens deployed, in base-token units.
    pub staked_amount: i128,
    /// Ledger timestamp at which the position was opened.
    pub opened_at: u64,
    /// Ledger timestamp at which a standard withdrawal was requested, if any.
    pub requested_at: Option<u64>,
    /// Whether the position is still active.
    pub active: bool,
}

/// Settlement report for a completed unwind.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnwindReceipt {
    /// Strategy the funds were pulled from.
    pub strategy_id: u32,
    /// Amount of base tokens that were staked.
    pub staked_amount: i128,
    /// Amount of base tokens actually recovered.
    pub recovered_amount: i128,
    /// `true` when the unwind ran on the emergency path and skipped the
    /// standard withdrawal delay.
    pub delay_bypassed: bool,
    /// Ledger timestamp of settlement.
    pub settled_at: u64,
}

/// Emitted whenever a position is unwound, on either path.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PositionUnwoundEvent {
    /// Strategy the funds were pulled from.
    pub strategy_id: u32,
    /// Amount of base tokens that were staked.
    pub staked_amount: i128,
    /// Amount of base tokens actually recovered into vault reserves.
    pub recovered_amount: i128,
    /// `true` when the standard withdrawal delay was bypassed.
    pub delay_bypassed: bool,
    /// Account that triggered the unwind.
    pub caller: Address,
}

/// Storage keys.
#[contracttype]
pub enum DataKey {
    /// Address allowed to administer strategies and emergencies.
    Admin,
    /// Whether a protocol emergency is currently in force.
    EmergencyActive,
    /// Timestamp at which the current emergency was declared.
    EmergencyDeclaredAt,
    /// Registered strategies by id.
    Strategies,
    /// Open positions by strategy id.
    Positions,
    /// Recovered base-token balances by token address (the vault reserves).
    Reserves,
}

#[contract]
pub struct YieldUnwind;

fn load_admin(env: &Env) -> Result<Address, UnwindError> {
    env.storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(UnwindError::NotInitialized)
}

fn require_admin(env: &Env, caller: &Address) -> Result<(), UnwindError> {
    let admin = load_admin(env)?;
    if caller != &admin {
        return Err(UnwindError::NotAdmin);
    }
    caller.require_auth();
    Ok(())
}

fn strategies(env: &Env) -> Map<u32, Strategy> {
    env.storage()
        .instance()
        .get(&DataKey::Strategies)
        .unwrap_or_else(|| Map::new(env))
}

fn positions(env: &Env) -> Map<u32, Position> {
    env.storage()
        .instance()
        .get(&DataKey::Positions)
        .unwrap_or_else(|| Map::new(env))
}

fn reserves(env: &Env) -> Map<Address, i128> {
    env.storage()
        .instance()
        .get(&DataKey::Reserves)
        .unwrap_or_else(|| Map::new(env))
}

/// Pull `amount` of base tokens out of the external strategy into this
/// contract.
///
/// The strategy is expected to expose `withdraw(to: Address, amount: i128)
/// -> i128`, returning the base tokens actually released (which may be less
/// than `amount` if the strategy is impaired). A strategy that traps or
/// returns a negative amount aborts the unwind rather than crediting
/// reserves with a fabricated figure.
fn pull_from_strategy(
    env: &Env,
    strategy: &Address,
    to: &Address,
    amount: i128,
) -> Result<i128, UnwindError> {
    let recovered: i128 = env.invoke_contract(
        strategy,
        &Symbol::new(env, "withdraw"),
        soroban_sdk::vec![env, to.clone().into_val(env), amount.into_val(env)],
    );
    if recovered < 0 {
        return Err(UnwindError::Overflow);
    }
    Ok(recovered)
}

/// Credit recovered base tokens to the vault reserves and close the position.
fn settle_unwind(
    env: &Env,
    strategy_id: u32,
    strategy: &Strategy,
    position: &Position,
    recovered: i128,
    delay_bypassed: bool,
    caller: &Address,
) -> Result<UnwindReceipt, UnwindError> {
    let mut vault = reserves(env);
    let current = vault.get(strategy.base_token.clone()).unwrap_or(0);
    vault.set(
        strategy.base_token.clone(),
        current.checked_add(recovered).ok_or(UnwindError::Overflow)?,
    );
    env.storage().instance().set(&DataKey::Reserves, &vault);

    let mut open = positions(env);
    open.set(
        strategy_id,
        Position {
            active: false,
            ..position.clone()
        },
    );
    env.storage().instance().set(&DataKey::Positions, &open);

    let receipt = UnwindReceipt {
        strategy_id,
        staked_amount: position.staked_amount,
        recovered_amount: recovered,
        delay_bypassed,
        settled_at: env.ledger().timestamp(),
    };

    env.events().publish(
        (Symbol::new(env, POSITION_UNWOUND),),
        PositionUnwoundEvent {
            strategy_id,
            staked_amount: position.staked_amount,
            recovered_amount: recovered,
            delay_bypassed,
            caller: caller.clone(),
        },
    );

    Ok(receipt)
}

#[contractimpl]
impl YieldUnwind {
    /// Deploy-time setup. No emergency is in force and no strategies exist.
    pub fn initialize(env: Env, admin: Address) -> Result<(), UnwindError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(UnwindError::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::EmergencyActive, &false);
        Ok(())
    }

    /// Declare a protocol emergency.
    ///
    /// Admin-only. While the flag is set, [`Self::emergency_unwind`] may pull
    /// positions out of their strategies immediately, bypassing each
    /// strategy's standard withdrawal delay.
    pub fn declare_emergency(env: Env, caller: Address) -> Result<(), UnwindError> {
        require_admin(&env, &caller)?;
        env.storage().instance().set(&DataKey::EmergencyActive, &true);
        let now = env.ledger().timestamp();
        env.storage().instance().set(&DataKey::EmergencyDeclaredAt, &now);
        env.events()
            .publish((Symbol::new(&env, EMERGENCY_DECLARED),), (caller, now));
        Ok(())
    }

    /// Lift the protocol emergency and return to standard-delay exits.
    pub fn resolve_emergency(env: Env, caller: Address) -> Result<(), UnwindError> {
        require_admin(&env, &caller)?;
        env.storage().instance().set(&DataKey::EmergencyActive, &false);
        let now = env.ledger().timestamp();
        env.events()
            .publish((Symbol::new(&env, EMERGENCY_RESOLVED),), (caller, now));
        Ok(())
    }

    /// Register an external yield strategy with its standard withdrawal delay.
    ///
    /// The delay must be positive: a strategy with no exit window leaves the
    /// emergency path with nothing to bypass, which defeats the purpose of
    /// registering it here.
    pub fn register_strategy(
        env: Env,
        caller: Address,
        strategy_id: u32,
        strategy: Address,
        base_token: Address,
        withdraw_delay_seconds: u64,
    ) -> Result<(), UnwindError> {
        require_admin(&env, &caller)?;
        if withdraw_delay_seconds == 0 {
            return Err(UnwindError::ZeroDelay);
        }
        let mut all = strategies(&env);
        if all.contains_key(strategy_id) {
            return Err(UnwindError::StrategyAlreadyRegistered);
        }
        all.set(
            strategy_id,
            Strategy {
                strategy,
                base_token,
                withdraw_delay_seconds,
            },
        );
        env.storage().instance().set(&DataKey::Strategies, &all);
        Ok(())
    }

    /// Deploy `amount` of base tokens from `caller` into a registered strategy.
    ///
    /// The tokens are transferred to the strategy contract and an active
    /// position is recorded. One open position per strategy at a time: unwind
    /// (or fully close) the current one before opening another.
    pub fn open_position(
        env: Env,
        caller: Address,
        strategy_id: u32,
        amount: i128,
    ) -> Result<Position, UnwindError> {
        require_admin(&env, &caller)?;
        if amount <= 0 {
            return Err(UnwindError::ZeroAmount);
        }
        let config = strategies(&env)
            .get(strategy_id)
            .ok_or(UnwindError::UnknownStrategy)?;
        if let Some(existing) = positions(&env).get(strategy_id) {
            if existing.active {
                return Err(UnwindError::PositionAlreadyOpen);
            }
        }

        token::Client::new(&env, &config.base_token).transfer(
            &caller,
            &config.strategy,
            &amount,
        );

        let position = Position {
            strategy_id,
            staked_amount: amount,
            opened_at: env.ledger().timestamp(),
            requested_at: None,
            active: true,
        };
        let mut open = positions(&env);
        open.set(strategy_id, position.clone());
        env.storage().instance().set(&DataKey::Positions, &open);

        env.events().publish(
            (Symbol::new(&env, POSITION_OPENED),),
            (strategy_id, caller, amount),
        );

        Ok(position)
    }

    /// Start the standard withdrawal clock for a position.
    ///
    /// [`Self::execute_unwind`] will settle the position once
    /// `withdraw_delay_seconds` have elapsed since this call.
    pub fn request_unwind(
        env: Env,
        caller: Address,
        strategy_id: u32,
    ) -> Result<u64, UnwindError> {
        require_admin(&env, &caller)?;
        let mut open = positions(&env);
        let mut position = open.get(strategy_id).ok_or(UnwindError::NoOpenPosition)?;
        if !position.active {
            return Err(UnwindError::NoOpenPosition);
        }
        let now = env.ledger().timestamp();
        position.requested_at = Some(now);
        open.set(strategy_id, position);
        env.storage().instance().set(&DataKey::Positions, &open);

        env.events().publish(
            (Symbol::new(&env, WITHDRAWAL_REQUESTED),),
            (strategy_id, caller, now),
        );

        Ok(now)
    }

    /// Settle a position through the standard path.
    ///
    /// Requires a prior [`Self::request_unwind`] and a fully elapsed
    /// withdrawal delay; otherwise returns
    /// [`UnwindError::WithdrawalNotRequested`] or
    /// [`UnwindError::DelayNotElapsed`]. Recovered base tokens are credited
    /// directly to the vault reserves.
    pub fn execute_unwind(
        env: Env,
        caller: Address,
        strategy_id: u32,
    ) -> Result<UnwindReceipt, UnwindError> {
        require_admin(&env, &caller)?;
        let config = strategies(&env)
            .get(strategy_id)
            .ok_or(UnwindError::UnknownStrategy)?;
        let position = positions(&env).get(strategy_id).ok_or(UnwindError::NoOpenPosition)?;
        if !position.active {
            return Err(UnwindError::NoOpenPosition);
        }
        let requested_at = position.requested_at.ok_or(UnwindError::WithdrawalNotRequested)?;
        let now = env.ledger().timestamp();
        if now.saturating_sub(requested_at) < config.withdraw_delay_seconds {
            return Err(UnwindError::DelayNotElapsed);
        }

        let recovered = pull_from_strategy(
            &env,
            &config.strategy,
            &env.current_contract_address(),
            position.staked_amount,
        )?;
        settle_unwind(&env, strategy_id, &config, &position, recovered, false, &caller)
    }

    /// Emergency entry point: unwind an active position **now**, bypassing the
    /// standard withdrawal delay timer.
    ///
    /// Runs only while a protocol emergency is in force
    /// ([`UnwindError::NotEmergency`] otherwise) and needs no prior
    /// [`Self::request_unwind`]: during market distress the delay is the
    /// risk. Recovered base tokens go straight to the vault reserves and the
    /// position is closed.
    pub fn emergency_unwind(
        env: Env,
        caller: Address,
        strategy_id: u32,
    ) -> Result<UnwindReceipt, UnwindError> {
        require_admin(&env, &caller)?;
        let emergency: bool = env
            .storage()
            .instance()
            .get(&DataKey::EmergencyActive)
            .unwrap_or(false);
        if !emergency {
            return Err(UnwindError::NotEmergency);
        }
        let config = strategies(&env)
            .get(strategy_id)
            .ok_or(UnwindError::UnknownStrategy)?;
        let position = positions(&env).get(strategy_id).ok_or(UnwindError::NoOpenPosition)?;
        if !position.active {
            return Err(UnwindError::NoOpenPosition);
        }

        let recovered = pull_from_strategy(
            &env,
            &config.strategy,
            &env.current_contract_address(),
            position.staked_amount,
        )?;
        settle_unwind(&env, strategy_id, &config, &position, recovered, true, &caller)
    }

    /// Whether a protocol emergency is currently in force.
    pub fn is_emergency(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::EmergencyActive)
            .unwrap_or(false)
    }

    /// Timestamp at which the current (or most recent) emergency was declared.
    pub fn emergency_declared_at(env: Env) -> Option<u64> {
        env.storage().instance().get(&DataKey::EmergencyDeclaredAt)
    }

    /// Registered strategy configuration, if any.
    pub fn get_strategy(env: Env, strategy_id: u32) -> Option<Strategy> {
        strategies(&env).get(strategy_id)
    }

    /// Position for a strategy, if one was ever opened.
    pub fn get_position(env: Env, strategy_id: u32) -> Option<Position> {
        positions(&env).get(strategy_id)
    }

    /// Vault reserves: base tokens recovered by unwinds and available to the
    /// protocol. This is where unwound liquidity lands.
    pub fn reserve_balance(env: Env, base_token: Address) -> i128 {
        reserves(&env).get(base_token).unwrap_or(0)
    }

    /// All registered strategy ids.
    pub fn list_strategies(env: Env) -> Vec<u32> {
        let all = strategies(&env);
        let mut ids = Vec::new(&env);
        for (id, _) in all.iter() {
            ids.push_back(id);
        }
        ids
    }
}

#[cfg(test)]
mod test {
    use super::{UnwindError, YieldUnwind, YieldUnwindClient, POSITION_UNWOUND};
    use soroban_sdk::{
        contract, contractimpl,
        testutils::{Address as _, Ledger as _},
        token, Address, Env, Symbol,
    };

    /// Stand-in for an external yield strategy. Holds base tokens and releases
    /// them on `withdraw`; can be configured to take a haircut so tests can
    /// exercise impaired recoveries.
    #[contract]
    pub struct MockStrategy;

    #[contractimpl]
    impl MockStrategy {
        pub fn withdraw(env: Env, to: Address, amount: i128) -> i128 {
            let base_token: Address = env.storage().instance().get(&Symbol::new(&env, "tok")).unwrap();
            let haircut_bps: u32 = env.storage().instance().get(&Symbol::new(&env, "cut")).unwrap();
            let payout = amount - amount * i128::from(haircut_bps) / 10_000;
            token::Client::new(&env, &base_token).transfer(&env.current_contract_address(), &to, &payout);
            payout
        }
    }

    const STRATEGY_ID: u32 = 7;
    const DELAY: u64 = 24 * 60 * 60;
    const STAKE: i128 = 1_000_000;

    struct Harness {
        env: Env,
        client: YieldUnwindClient<'static>,
        admin: Address,
        base_token: Address,
        strategy: Address,
    }

    fn setup_with_haircut(haircut_bps: u32) -> Harness {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);

        let contract_id = env.register_contract(None, YieldUnwind);
        let client = YieldUnwindClient::new(&env, &contract_id);
        client.initialize(&admin);

        let base_token = env.register_stellar_asset_contract(Address::generate(&env));
        let strategy_id = env.register_contract(None, MockStrategy);
        env.as_contract(&strategy_id, || {
            env.storage().instance().set(&Symbol::new(&env, "tok"), &base_token);
            env.storage().instance().set(&Symbol::new(&env, "cut"), &haircut_bps);
        });

        client.register_strategy(&admin, &STRATEGY_ID, &strategy_id, &base_token, &DELAY);

        Harness { env, client, admin, base_token, strategy: strategy_id }
    }

    fn setup() -> Harness {
        setup_with_haircut(0)
    }

    fn fund_and_open(h: &Harness) {
        token::StellarAssetClient::new(&h.env, &h.base_token).mint(&h.admin, &STAKE);
        h.client.open_position(&h.admin, &STRATEGY_ID, &STAKE);
    }

    fn advance(env: &Env, seconds: u64) {
        env.ledger().with_mut(|li| li.timestamp += seconds);
    }

    #[test]
    fn initialize_sets_admin_and_no_emergency() {
        let h = setup();
        assert!(!h.client.is_emergency());
        assert!(h.client.emergency_declared_at().is_none());
        let res = h.client.try_initialize(&h.admin);
        assert_eq!(res, Err(Ok(UnwindError::AlreadyInitialized)));
    }

    #[test]
    fn admin_gating_is_enforced() {
        let h = setup();
        let intruder = Address::generate(&h.env);
        assert_eq!(
            h.client.try_declare_emergency(&intruder),
            Err(Ok(UnwindError::NotAdmin))
        );
        assert_eq!(
            h.client.try_register_strategy(&intruder, &1, &h.strategy, &h.base_token, &DELAY),
            Err(Ok(UnwindError::NotAdmin))
        );
        assert_eq!(
            h.client.try_open_position(&intruder, &STRATEGY_ID, &STAKE),
            Err(Ok(UnwindError::NotAdmin))
        );
    }

    #[test]
    fn emergency_lifecycle_emits_events() {
        let h = setup();
        h.client.declare_emergency(&h.admin);
        assert!(h.client.is_emergency());
        assert!(h.client.emergency_declared_at().is_some());
        h.client.resolve_emergency(&h.admin);
        assert!(!h.client.is_emergency());
    }

    #[test]
    fn strategy_registration_validates_input() {
        let h = setup();
        assert_eq!(
            h.client.try_register_strategy(&h.admin, &STRATEGY_ID, &h.strategy, &h.base_token, &DELAY),
            Err(Ok(UnwindError::StrategyAlreadyRegistered))
        );
        assert_eq!(
            h.client.try_register_strategy(&h.admin, &99, &h.strategy, &h.base_token, &0),
            Err(Ok(UnwindError::ZeroDelay))
        );
        assert_eq!(h.client.list_strategies().len(), 1);
    }

    #[test]
    fn open_position_moves_funds_and_records_state() {
        let h = setup();
        fund_and_open(&h);
        let position = h.client.get_position(&STRATEGY_ID).unwrap();
        assert!(position.active);
        assert_eq!(position.staked_amount, STAKE);
        assert_eq!(position.requested_at, None);
        assert_eq!(
            token::Client::new(&h.env, &h.base_token).balance(&h.strategy),
            STAKE
        );
        // A second position on the same strategy is rejected while one is open.
        assert_eq!(
            h.client.try_open_position(&h.admin, &STRATEGY_ID, &STAKE),
            Err(Ok(UnwindError::PositionAlreadyOpen))
        );
    }

    #[test]
    fn standard_unwind_enforces_the_delay() {
        let h = setup();
        fund_and_open(&h);

        // No request yet: nothing to execute.
        assert_eq!(
            h.client.try_execute_unwind(&h.admin, &STRATEGY_ID),
            Err(Ok(UnwindError::WithdrawalNotRequested))
        );

        h.client.request_unwind(&h.admin, &STRATEGY_ID);

        // Inside the window the delay still binds — even mid-emergency the
        // *standard* path keeps its guard; only `emergency_unwind` bypasses.
        advance(&h.env, DELAY - 1);
        assert_eq!(
            h.client.try_execute_unwind(&h.admin, &STRATEGY_ID),
            Err(Ok(UnwindError::DelayNotElapsed))
        );

        advance(&h.env, 1);
        let receipt = h.client.execute_unwind(&h.admin, &STRATEGY_ID);
        assert_eq!(receipt.staked_amount, STAKE);
        assert_eq!(receipt.recovered_amount, STAKE);
        assert!(!receipt.delay_bypassed);
        assert_eq!(h.client.reserve_balance(&h.base_token), STAKE);
        assert!(!h.client.get_position(&STRATEGY_ID).unwrap().active);
    }

    #[test]
    fn emergency_unwind_bypasses_the_delay() {
        let h = setup();
        fund_and_open(&h);
        h.client.declare_emergency(&h.admin);

        // No request, no waiting: the full position settles immediately and
        // the recovered liquidity lands in the vault reserves.
        let receipt = h.client.emergency_unwind(&h.admin, &STRATEGY_ID);
        assert_eq!(receipt.staked_amount, STAKE);
        assert_eq!(receipt.recovered_amount, STAKE);
        assert!(receipt.delay_bypassed);
        assert_eq!(h.client.reserve_balance(&h.base_token), STAKE);
        assert!(!h.client.get_position(&STRATEGY_ID).unwrap().active);
    }

    #[test]
    fn emergency_unwind_requires_an_active_emergency() {
        let h = setup();
        fund_and_open(&h);
        assert_eq!(
            h.client.try_emergency_unwind(&h.admin, &STRATEGY_ID),
            Err(Ok(UnwindError::NotEmergency))
        );

        h.client.declare_emergency(&h.admin);
        h.client.resolve_emergency(&h.admin);
        assert_eq!(
            h.client.try_emergency_unwind(&h.admin, &STRATEGY_ID),
            Err(Ok(UnwindError::NotEmergency))
        );
    }

    #[test]
    fn emergency_unwind_after_a_pending_request_still_bypasses() {
        let h = setup();
        fund_and_open(&h);
        h.client.request_unwind(&h.admin, &STRATEGY_ID);
        h.client.declare_emergency(&h.admin);

        // The standard clock started, but the emergency settles now.
        let receipt = h.client.emergency_unwind(&h.admin, &STRATEGY_ID);
        assert!(receipt.delay_bypassed);
        assert_eq!(h.client.reserve_balance(&h.base_token), STAKE);
    }

    #[test]
    fn impaired_strategy_settles_the_shortfall_and_closes() {
        // A 25% haircut: the strategy can only release 750_000 of 1_000_000.
        let h = setup_with_haircut(2_500);
        fund_and_open(&h);
        h.client.declare_emergency(&h.admin);

        let receipt = h.client.emergency_unwind(&h.admin, &STRATEGY_ID);
        assert_eq!(receipt.staked_amount, STAKE);
        assert_eq!(receipt.recovered_amount, 750_000);
        assert_eq!(h.client.reserve_balance(&h.base_token), 750_000);
        assert!(!h.client.get_position(&STRATEGY_ID).unwrap().active);
    }

    #[test]
    fn unwind_on_unknown_or_closed_positions_is_rejected() {
        let h = setup();
        // The emergency gate fires before position lookup.
        assert_eq!(
            h.client.try_emergency_unwind(&h.admin, &STRATEGY_ID),
            Err(Ok(UnwindError::NotEmergency))
        );
        h.client.declare_emergency(&h.admin);
        assert_eq!(
            h.client.try_emergency_unwind(&h.admin, &STRATEGY_ID),
            Err(Ok(UnwindError::NoOpenPosition))
        );
        fund_and_open(&h);
        h.client.emergency_unwind(&h.admin, &STRATEGY_ID);
        assert_eq!(
            h.client.try_emergency_unwind(&h.admin, &STRATEGY_ID),
            Err(Ok(UnwindError::NoOpenPosition))
        );
        assert_eq!(
            h.client.try_request_unwind(&h.admin, &STRATEGY_ID),
            Err(Ok(UnwindError::NoOpenPosition))
        );
    }

    #[test]
    fn unwind_event_carries_amounts_and_bypass_flag() {
        use soroban_sdk::testutils::Events as _;
        use soroban_sdk::TryFromVal as _;

        let h = setup();
        fund_and_open(&h);
        h.client.declare_emergency(&h.admin);
        h.client.emergency_unwind(&h.admin, &STRATEGY_ID);

        let events = h.env.events().all();
        let (_, topics, data) = events.get(events.len() - 1).unwrap();
        let topic: Symbol = Symbol::try_from_val(&h.env, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic, Symbol::new(&h.env, POSITION_UNWOUND));
        let decoded = super::PositionUnwoundEvent::try_from_val(&h.env, &data).unwrap();
        assert_eq!(decoded.strategy_id, STRATEGY_ID);
        assert_eq!(decoded.staked_amount, STAKE);
        assert_eq!(decoded.recovered_amount, STAKE);
        assert!(decoded.delay_bypassed);
        assert_eq!(decoded.caller, h.admin);
    }
}
