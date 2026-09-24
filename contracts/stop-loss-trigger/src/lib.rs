//! Stop-Loss Execution Trigger Contract Handler (Issue #730).
//!
//! A conditional execution wrapper that swaps user positions when the
//! verified TWAP oracle price crosses a target threshold.
//!
//! # Flow
//!
//! 1. **Register** — a user posts a trigger: they escrow `sell_amount` of
//!    `sell_asset`, pick a `stop_price`, a trigger direction
//!    ([`TriggerCondition::PriceBelow`] for stop-loss protection, or
//!    [`TriggerCondition::PriceAbove`] for the mirror case), and configure
//!    slippage bounds (`min_fill_price`, `max_fill_price`, `min_amount_out`).
//! 2. **Execute** — any keeper may submit an execution for a trigger. The
//!    handler *never* trusts keeper-supplied data: it re-reads the
//!    time-weighted average price straight from the registered price-oracle
//!    contract (`get_twap`) and rejects the keeper submission when the
//!    claimed price does not match the verified TWAP.
//! 3. **Swap** — when the verified TWAP breaches the stop price the handler
//!    atomically executes the market swap: the escrowed `sell_asset` is
//!    absorbed by the pool and `buy_asset` is paid to the owner at the
//!    oracle-verified fill price. If the resulting fill price (or proceeds)
//!    fall outside the slippage bounds configured by the user the whole
//!    execution reverts with [`ContractError::SlippageExceeded`].
//! 4. **Cancel** — the owner may cancel an unfired trigger at any time and
//!    recover the escrowed balance.
//!
//! # Security model
//!
//! * **Oracle-verified pricing** — the trigger condition and the fill price
//!   are derived exclusively from the oracle contract's `get_twap` view,
//!   which averages the last ten verified price updates. Keepers
//!   cannot fabricate a price: `claimed_twap` must equal the on-chain TWAP
//!   or the submission is rejected with
//!   [`ContractError::TriggerPriceNotVerified`]. In production the oracle
//!   address is the `price-oracle` contract shipped with this workspace; the
//!   handler only relies on the `get_twap(Symbol) -> Option<i128>` interface,
//!   so any verified TWAP feed with the same surface can be registered.
//! * **User-configured slippage bounds** — if the fill price leaves the
//!   `[min_fill_price, max_fill_price]` corridor, or the proceeds fall below
//!   `min_amount_out`, execution reverts. Soroban's transaction atomicity
//!   guarantees no partial state or balance movement survives the revert.
//! * **Escrow custody** — funds stay locked in the handler until the trigger
//!   fires or the owner cancels; keepers only get to *trigger* execution,
//!   never to touch the escrow themselves.
//! * **Checked arithmetic** — all price math uses `checked_*` operations so
//!   overflow can never silently mis-price a fill.
//! * **Permissionless, attributable execution** — keepers authenticate via
//!   `require_auth` so every execution is attributable on-chain, but no
//!   keeper allowlist is required (bots are expected to call this).

#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, token, Address, Env,
    IntoVal, Symbol,
};

/// Fixed-point scale for all prices (`stop_price`, fill prices and the oracle
/// TWAP), in units of `buy_asset` per 1 whole unit of `sell_asset`.
/// Matches the fixed-point footprint used by the protocol elsewhere.
pub const PRICE_SCALE: i128 = 10_000_000;

/// Persistent-storage TTL floor / extension target (in ledgers) applied to
/// trigger records so unfired triggers survive rent collection.
const TTL_THRESHOLD: u32 = 120_960;
const TTL_EXTEND_TO: u32 = 535_680;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ContractError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    NotAdmin = 3,
    TriggerNotFound = 4,
    TriggerNotActive = 5,
    NotTriggerOwner = 6,
    InvalidAmount = 7,
    InvalidTriggerPrice = 8,
    InvalidSlippageBounds = 9,
    /// The TWAP oracle returned no verified price for the trigger's feed.
    OraclePriceUnavailable = 10,
    /// The keeper's submitted price does not match the verified TWAP oracle price.
    TriggerPriceNotVerified = 11,
    /// The verified TWAP has not (yet) crossed the configured stop price.
    TriggerConditionNotMet = 12,
    /// The resulting fill price / proceeds exceed the user-configured slippage bounds.
    SlippageExceeded = 13,
    /// The pool does not hold enough `buy_asset` to settle the swap.
    InsufficientLiquidity = 14,
    MathOverflow = 15,
}

/// Direction in which the verified TWAP must cross `stop_price` for the
/// trigger to fire.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TriggerCondition {
    /// Stop-loss: fire when the TWAP drops to (or below) `stop_price`.
    PriceBelow,
    /// Mirror direction: fire when the TWAP rises to (or above) `stop_price`.
    PriceAbove,
}

/// A user-posted conditional swap order held by the handler.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct StopLossTrigger {
    pub id: u64,
    pub owner: Address,
    /// Asset the owner escrows and sells when the trigger fires.
    pub sell_asset: Address,
    /// Asset the owner receives from the pool when the trigger fires.
    pub buy_asset: Address,
    /// TWAP oracle feed symbol used for price validation.
    pub oracle_symbol: Symbol,
    pub sell_amount: i128,
    /// Price threshold at/through which the trigger fires (fixed-point).
    pub stop_price: i128,
    pub condition: TriggerCondition,
    /// Slippage bound: minimum acceptable fill price (fixed-point).
    pub min_fill_price: i128,
    /// Slippage bound: maximum acceptable fill price (fixed-point).
    pub max_fill_price: i128,
    /// Slippage bound: minimum acceptable proceeds of `buy_asset`.
    pub min_amount_out: i128,
    pub created_at: u64,
    pub active: bool,
}

/// Result of a successful trigger execution.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ExecutionResult {
    pub trigger_id: u64,
    pub executed_at: u64,
    /// The verified TWAP read from the oracle at execution time.
    pub twap_price: i128,
    /// Effective fill price (fixed-point) the swap settled at.
    pub fill_price: i128,
    /// Amount of `buy_asset` paid to the owner.
    pub proceeds: i128,
}

/// Contract-level configuration.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub admin: Address,
    /// Address of the price-oracle contract used for TWAP validation.
    pub oracle: Address,
}

#[contracttype]
pub enum DataKey {
    Config,
    NextTriggerId,
    Trigger(u64),
}

#[contract]
pub struct StopLossTriggerContract;

#[contractimpl]
impl StopLossTriggerContract {
    /// Initialize the handler with the admin and the price-oracle contract
    /// whose verified TWAP feeds all trigger validations.
    pub fn initialize(env: Env, admin: Address, oracle: Address) -> Result<(), ContractError> {
        admin.require_auth();
        if env.storage().instance().has(&DataKey::Config) {
            return Err(ContractError::AlreadyInitialized);
        }
        env.storage()
            .instance()
            .set(&DataKey::Config, &Config { admin, oracle });
        Ok(())
    }

    /// Admin-only: point the handler at a different oracle contract.
    pub fn set_oracle(env: Env, admin: Address, oracle: Address) -> Result<(), ContractError> {
        admin.require_auth();
        let mut config = Self::config(&env)?;
        if config.admin != admin {
            return Err(ContractError::NotAdmin);
        }
        config.oracle = oracle;
        env.storage().instance().set(&DataKey::Config, &config);
        Ok(())
    }

    /// Return the current handler configuration, if initialized.
    pub fn get_config(env: Env) -> Option<Config> {
        env.storage().instance().get(&DataKey::Config)
    }

    /// Register a stop-loss (or mirror) trigger and escrow the sold asset.
    ///
    /// # Errors
    /// - [`ContractError::InvalidAmount`] when `sell_amount <= 0`.
    /// - [`ContractError::InvalidTriggerPrice`] when `stop_price <= 0`.
    /// - [`ContractError::InvalidSlippageBounds`] when the slippage
    ///   configuration is inconsistent (`min_fill_price <= 0`,
    ///   `max_fill_price < min_fill_price`, or `min_amount_out < 0`).
    pub fn register_trigger(
        env: Env,
        owner: Address,
        sell_asset: Address,
        buy_asset: Address,
        oracle_symbol: Symbol,
        sell_amount: i128,
        stop_price: i128,
        condition: TriggerCondition,
        min_fill_price: i128,
        max_fill_price: i128,
        min_amount_out: i128,
    ) -> Result<StopLossTrigger, ContractError> {
        owner.require_auth();
        Self::validate_trigger_params(
            sell_amount,
            stop_price,
            min_fill_price,
            max_fill_price,
            min_amount_out,
        )?;

        let sell_client = token::Client::new(&env, &sell_asset);
        sell_client.transfer(&owner, &env.current_contract_address(), &sell_amount);

        let id = Self::next_trigger_id(&env);
        let trigger = StopLossTrigger {
            id,
            owner,
            sell_asset,
            buy_asset,
            oracle_symbol,
            sell_amount,
            stop_price,
            condition,
            min_fill_price,
            max_fill_price,
            min_amount_out,
            created_at: env.ledger().timestamp(),
            active: true,
        };
        let key = DataKey::Trigger(id);
        env.storage().persistent().set(&key, &trigger);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);

        env.events().publish(
            (symbol_short!("trig_reg"), id),
            (trigger.owner.clone(), trigger.sell_amount),
        );

        Ok(trigger)
    }

    /// Keeper entrypoint: validate the trigger submission against the
    /// verified TWAP oracle price and, when the stop condition is breached,
    /// automatically execute the market swap in the same atomic call.
    ///
    /// # Errors
    /// - [`ContractError::TriggerNotFound`] for unknown trigger ids.
    /// - [`ContractError::TriggerNotActive`] for cancelled / already-executed
    ///   triggers.
    /// - [`ContractError::OraclePriceUnavailable`] when the oracle has no
    ///   verified TWAP for the trigger's feed.
    /// - [`ContractError::TriggerPriceNotVerified`] when the keeper's
    ///   `claimed_twap` does not match the verified TWAP oracle price.
    /// - [`ContractError::TriggerConditionNotMet`] when the verified TWAP
    ///   has not crossed `stop_price`.
    /// - [`ContractError::SlippageExceeded`] when the resulting fill price
    ///   (or proceeds) breach the user-configured slippage bounds — the
    ///   whole execution reverts.
    /// - [`ContractError::InsufficientLiquidity`] when the pool cannot
    ///   settle the swap.
    pub fn execute_trigger(
        env: Env,
        keeper: Address,
        trigger_id: u64,
        claimed_twap: i128,
    ) -> Result<ExecutionResult, ContractError> {
        keeper.require_auth();

        let config = Self::config(&env)?;
        let key = DataKey::Trigger(trigger_id);
        let trigger: StopLossTrigger = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::TriggerNotFound)?;
        if !trigger.active {
            return Err(ContractError::TriggerNotActive);
        }

        // 1. Read the verified TWAP straight from the price oracle. Keeper
        //    input is never trusted as the source of truth.
        let twap = Self::read_verified_twap(&env, &config.oracle, &trigger.oracle_symbol)?;

        // 2. Validate the keeper's trigger submission against the verified
        //    TWAP oracle price. A keeper quoting anything else is rejected
        //    before any state is touched.
        if claimed_twap != twap {
            return Err(ContractError::TriggerPriceNotVerified);
        }

        // 3. Stop condition breach check.
        let breached = match trigger.condition {
            TriggerCondition::PriceBelow => twap <= trigger.stop_price,
            TriggerCondition::PriceAbove => twap >= trigger.stop_price,
        };
        if !breached {
            return Err(ContractError::TriggerConditionNotMet);
        }

        // 4. Slippage bounds: revert the execution when the resulting fill
        //    price leaves the corridor configured by the user.
        if twap < trigger.min_fill_price || twap > trigger.max_fill_price {
            return Err(ContractError::SlippageExceeded);
        }

        // 5. Derive proceeds at the oracle-verified fill price.
        let proceeds = trigger
            .sell_amount
            .checked_mul(twap)
            .ok_or(ContractError::MathOverflow)?
            .checked_div(PRICE_SCALE)
            .ok_or(ContractError::MathOverflow)?;

        if proceeds < trigger.min_amount_out {
            return Err(ContractError::SlippageExceeded);
        }

        // 6. Execute the market swap: pay `buy_asset` out of the pool and
        //    retain the sold asset in the pool. Soroban atomicity reverts
        //    every write here if any subsequent step fails.
        let buy_client = token::Client::new(&env, &trigger.buy_asset);
        if buy_client.balance(&env.current_contract_address()) < proceeds {
            return Err(ContractError::InsufficientLiquidity);
        }
        buy_client.transfer(&env.current_contract_address(), &trigger.owner, &proceeds);

        let mut executed = trigger.clone();
        executed.active = false;
        env.storage().persistent().set(&key, &executed);

        env.events().publish(
            (symbol_short!("trig_exec"), trigger_id),
            (trigger.owner, twap, proceeds),
        );

        Ok(ExecutionResult {
            trigger_id,
            executed_at: env.ledger().timestamp(),
            twap_price: twap,
            fill_price: twap,
            proceeds,
        })
    }

    /// Cancel a still-active trigger and refund the escrowed `sell_asset`.
    ///
    /// Only the trigger owner may cancel. Keepers cannot cancel orders.
    pub fn cancel_trigger(
        env: Env,
        owner: Address,
        trigger_id: u64,
    ) -> Result<i128, ContractError> {
        owner.require_auth();

        let key = DataKey::Trigger(trigger_id);
        let mut trigger: StopLossTrigger = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::TriggerNotFound)?;
        if trigger.owner != owner {
            return Err(ContractError::NotTriggerOwner);
        }
        if !trigger.active {
            return Err(ContractError::TriggerNotActive);
        }

        let refund = trigger.sell_amount;
        trigger.active = false;
        env.storage().persistent().set(&key, &trigger);

        if refund > 0 {
            let sell_client = token::Client::new(&env, &trigger.sell_asset);
            sell_client.transfer(&env.current_contract_address(), &owner, &refund);
        }

        env.events()
            .publish((symbol_short!("trig_cxl"), trigger_id), (owner, refund));

        Ok(refund)
    }

    /// Query a trigger by id.
    pub fn get_trigger(env: Env, trigger_id: u64) -> Option<StopLossTrigger> {
        env.storage()
            .persistent()
            .get(&DataKey::Trigger(trigger_id))
    }

    /// Permissionless liquidity provision: deposit `asset` into the handler's
    /// pool so stop-loss executions can be settled.
    pub fn fund_pool(
        env: Env,
        funder: Address,
        asset: Address,
        amount: i128,
    ) -> Result<i128, ContractError> {
        funder.require_auth();
        if amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }
        token::Client::new(&env, &asset).transfer(
            &funder,
            &env.current_contract_address(),
            &amount,
        );
        Ok(amount)
    }

    /// Query the handler pool's balance of `asset`.
    pub fn get_pool_balance(env: Env, asset: Address) -> i128 {
        token::Client::new(&env, &asset).balance(&env.current_contract_address())
    }

    // ── Private helpers ──────────────────────────────────────────────────────

    fn config(env: &Env) -> Result<Config, ContractError> {
        env.storage()
            .instance()
            .get(&DataKey::Config)
            .ok_or(ContractError::NotInitialized)
    }

    /// Cross-contract read of the verified TWAP from the registered oracle.
    ///
    /// The oracle contract must expose the `get_twap(Symbol) ->
    /// Result<Option<i128>, contracterror>` interface (as `price-oracle` in
    /// this workspace does). Any oracle error — or a missing TWAP buffer —
    /// is surfaced as [`ContractError::OraclePriceUnavailable`] so execution
    /// reverts cleanly instead of mis-pricing a fill.
    fn read_verified_twap(
        env: &Env,
        oracle: &Address,
        oracle_symbol: &Symbol,
    ) -> Result<i128, ContractError> {
        let result: Result<Option<i128>, soroban_sdk::Error> = env.invoke_contract(
            oracle,
            &symbol_short!("get_twap"),
            soroban_sdk::vec![env, oracle_symbol.into_val(env)],
        );
        match result {
            Ok(Some(twap)) => Ok(twap),
            _ => Err(ContractError::OraclePriceUnavailable),
        }
    }

    fn next_trigger_id(env: &Env) -> u64 {
        let id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextTriggerId)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::NextTriggerId, &(id + 1));
        id
    }

    fn validate_trigger_params(
        sell_amount: i128,
        stop_price: i128,
        min_fill_price: i128,
        max_fill_price: i128,
        min_amount_out: i128,
    ) -> Result<(), ContractError> {
        if sell_amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }
        if stop_price <= 0 {
            return Err(ContractError::InvalidTriggerPrice);
        }
        if min_fill_price <= 0 || max_fill_price < min_fill_price || min_amount_out < 0 {
            return Err(ContractError::InvalidSlippageBounds);
        }
        Ok(())
    }
}

#[cfg(test)]
mod test;
