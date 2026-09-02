#![no_std]

//! Protocol Fee Auto-Burn Engine for Platform Tokens (Issue #735).
//!
//! Routes a configurable portion of collected protocol swap fees into a
//! designated burn module, which automatically invokes the token `burn()`
//! entrypoint — permanently reducing the token's total supply — and emits a
//! `TokensBurned` event carrying the burnt amount plus updated supply metrics.

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, token, Address, Env,
};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum BurnError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    NotAdmin = 3,
    TokenNotRegistered = 4,
    InvalidAmount = 5,
    InvalidRatio = 6,
    Overflow = 7,
    InsufficientFees = 8,
    BurnModuleNotSet = 9,
    AlreadyRegistered = 10,
}

/// Burn module configuration for a single platform token.
///
/// `burn_ratio_bps` defines the portion (in basis points, 10000 = 100%) of
/// every routed swap fee that is destined for permanent destruction.
/// `fees_accumulated` is the pool of routed fees awaiting burn.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct BurnModule {
    /// Platform token address being burned.
    pub token: Address,
    /// Portion of routed fees to destroy, in basis points (10000 = 100%).
    pub burn_ratio_bps: u32,
    /// Token fees received and held awaiting burn.
    pub fees_accumulated: i128,
    /// Cumulative amount of this token permanently destroyed.
    pub total_burnt: i128,
    /// Remaining circulating supply after all burns, as reported by the token.
    pub remaining_supply: i128,
    /// Earliest ledger an accumulated fee pool may be ignited.
    pub auto_burn_threshold: i128,
    /// The address authorized to invoke the token `burn()` (burn module).
    pub burn_module: Address,
}

impl BurnModule {
    fn new(token: Address, burn_module: Address) -> Self {
        Self {
            token,
            burn_ratio_bps: 0,
            fees_accumulated: 0,
            total_burnt: 0,
            remaining_supply: 0,
            auto_burn_threshold: 0,
            burn_module,
        }
    }
}

#[contracttype]
pub enum DataKey {
    Admin,
}

#[contract]
pub struct FeeBurnEngine;

#[contractimpl]
impl FeeBurnEngine {
    /// Initialize the burn engine with an admin.
    ///
    /// # Parameters
    /// - `admin`: Address with management privileges.
    pub fn initialize(env: Env, admin: Address) -> Result<(), BurnError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(BurnError::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        Ok(())
    }

    // -- Burn module configuration -------------------------------------------

    /// Register a burn module for a platform token with an initial burn ratio.
    ///
    /// # Parameters
    /// - `admin`: Admin address.
    /// - `token`: Platform token address to burn.
    /// - `burn_module`: Address authorized to trigger the token `burn()`.
    /// - `burn_ratio_bps`: Portion of routed fees to destroy, in basis points.
    pub fn register_burn_module(
        env: Env,
        admin: Address,
        token: Address,
        burn_module: Address,
        burn_ratio_bps: u32,
    ) -> Result<BurnModule, BurnError> {
        Self::require_admin(&env, &admin)?;
        if burn_ratio_bps > 10_000 {
            return Err(BurnError::InvalidRatio);
        }
        if env.storage().persistent().has(&BurnModuleKey(token.clone())) {
            return Err(BurnError::AlreadyRegistered);
        }
        let mut module = BurnModule::new(token.clone(), burn_module);
        module.burn_ratio_bps = burn_ratio_bps;
        Self::save_module(&env, &module);
        Ok(module)
    }

    /// Update the burn ratio for an existing platform token module.
    pub fn set_burn_ratio(
        env: Env,
        admin: Address,
        token: Address,
        burn_ratio_bps: u32,
    ) -> Result<BurnModule, BurnError> {
        Self::require_admin(&env, &admin)?;
        if burn_ratio_bps > 10_000 {
            return Err(BurnError::InvalidRatio);
        }
        let mut module = Self::load_module(&env, &token)?;
        module.burn_ratio_bps = burn_ratio_bps;
        Self::save_module(&env, &module);
        Ok(module)
    }

    /// Set the ignition threshold: once accumulated fees reach this amount,
    /// an automatic burn is triggered on the next fee route.
    pub fn set_auto_burn_threshold(
        env: Env,
        admin: Address,
        token: Address,
        threshold: i128,
    ) -> Result<BurnModule, BurnError> {
        Self::require_admin(&env, &admin)?;
        let mut module = Self::load_module(&env, &token)?;
        module.auto_burn_threshold = threshold;
        Self::save_module(&env, &module);
        Ok(module)
    }

    pub fn get_burn_module(env: Env, token: Address) -> Option<BurnModule> {
        env.storage().persistent().get(&BurnModuleKey(token))
    }

    /// Read the on-chain balance of `account` for `token` (full supply metric
    /// visibility without writing state).
    pub fn token_balance(env: Env, token: Address, account: Address) -> i128 {
        token::Client::new(&env, &token).balance(&account)
    }

    // -- Fee routing (requirement 1) -----------------------------------------

    /// Receive a routed portion of collected protocol swap fees.
    ///
    /// The portion of `amount` configured by `burn_ratio_bps` accrues into the
    /// token's burn pool in this engine; the remainder (if any) is left for the
    /// caller/protocol to direct elsewhere. If the accrued pool reaches the
    /// auto-burn threshold, the burn is triggered immediately.
    ///
    /// # Parameters
    /// - `admin`: Admin (or the protocol fee router) routing fees.
    /// - `token`: Platform token the fees were collected in.
    /// - `amount`: Total collected swap fee amount routed to this engine.
    pub fn route_fees(
        env: Env,
        admin: Address,
        token: Address,
        amount: i128,
    ) -> Result<BurnModule, BurnError> {
        Self::require_admin(&env, &admin)?;
        if amount <= 0 {
            return Err(BurnError::InvalidAmount);
        }
        let mut module = Self::load_module(&env, &token)?;

        let burnt_portion = (amount * module.burn_ratio_bps as i128) / 10_000;
        module.fees_accumulated = module
            .fees_accumulated
            .checked_add(burnt_portion)
            .ok_or(BurnError::Overflow)?;

        // Auto-burn once the accumulated pool meets the configured threshold.
        let mut result = module;
        if result.auto_burn_threshold > 0 && result.fees_accumulated >= result.auto_burn_threshold {
            result = Self::execute_burn(&env, &result)?;
        }
        Self::save_module(&env, &result);
        Ok(result)
    }

    // -- Automatic burn (requirements 2 & 3) ---------------------------------

    /// Proactively trigger a burn of the entire accumulated fee pool for `token`.
    pub fn burn_accumulated_fees(
        env: Env,
        admin: Address,
        token: Address,
    ) -> Result<BurnModule, BurnError> {
        Self::require_admin(&env, &admin)?;
        let mut module = Self::load_module(&env, &token)?;
        module = Self::execute_burn(&env, &module)?;
        Self::save_module(&env, &module);
        Ok(module)
    }

    /// Ignite a partial burn of exactly `amount` tokens from the pool.
    pub fn burn_exact(
        env: Env,
        admin: Address,
        token: Address,
        amount: i128,
    ) -> Result<BurnModule, BurnError> {
        Self::require_admin(&env, &admin)?;
        if amount <= 0 {
            return Err(BurnError::InvalidAmount);
        }
        let mut module = Self::load_module(&env, &token)?;
        if amount > module.fees_accumulated {
            return Err(BurnError::InsufficientFees);
        }
        module = Self::consume_burn(&env, &module, amount)?;
        Self::save_module(&env, &module);
        Ok(module)
    }

    // -- Internal helpers -----------------------------------------------------

    fn require_admin(env: &Env, admin: &Address) -> Result<(), BurnError> {
        let stored: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(BurnError::NotInitialized)?;
        if admin != &stored {
            return Err(BurnError::NotAdmin);
        }
        admin.require_auth();
        Ok(())
    }

    fn load_module(env: &Env, token: &Address) -> Result<BurnModule, BurnError> {
        env.storage()
            .persistent()
            .get(&BurnModuleKey(token.clone()))
            .ok_or(BurnError::TokenNotRegistered)
    }

    fn save_module(env: &Env, module: &BurnModule) {
        env.storage()
            .persistent()
            .set(&BurnModuleKey(module.token.clone()), module);
    }

    /// Burn the entire accumulated fee pool via the token `burn()` entrypoint
    /// and emit a `TokensBurned` event with updated supply metrics.
    fn execute_burn(env: &Env, module: &BurnModule) -> Result<BurnModule, BurnError> {
        if module.fees_accumulated == 0 {
            return Ok(module.clone());
        }
        Self::consume_burn(env, module, module.fees_accumulated)
    }

    /// Perform the actual token `burn()` (supply reduction, permanent) and
    /// record+emit updated supply metrics.
    fn consume_burn(
        env: &Env,
        module: &BurnModule,
        amount: i128,
    ) -> Result<BurnModule, BurnError> {
        if module.burn_module == module.token {
            return Err(BurnError::BurnModuleNotSet);
        }
        let token_client = token::Client::new(env, &module.token);
        let from = module.burn_module.clone();
        token_client.burn(&from, &amount);

        let new_total_burnt = module
            .total_burnt
            .checked_add(amount)
            .ok_or(BurnError::Overflow)?;
        let new_remaining_supply = if module.remaining_supply > 0 {
            module
                .remaining_supply
                .checked_sub(amount)
                .ok_or(BurnError::Overflow)?
        } else {
            0
        };

        let updated = BurnModule {
            fees_accumulated: module.fees_accumulated - amount,
            total_burnt: new_total_burnt,
            remaining_supply: new_remaining_supply,
            ..module.clone()
        };

        // TokensBurned event: (event_name, token) -> (burnt, total_burnt, remaining_supply)
        env.events().publish(
            (symbol_short!("tok_burn"), module.token.clone()),
            (
                amount,
                updated.total_burnt,
                updated.remaining_supply,
            ),
        );

        Ok(updated)
    }
}

#[contracttype]
struct BurnModuleKey(Address);

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Events};
    use soroban_sdk::{token::StellarAssetClient, Env, Symbol, TryIntoVal};

    fn setup() -> (Env, FeeBurnEngineClient<'static>, Address) {
        let env = Env::default();
        // The engine bundles token burning into its own calls, so non-root
        // auth for the burn-holder must be allowed in these tests.
        env.mock_all_auths_allowing_non_root_auth();
        let id = env.register_contract(None, FeeBurnEngine);
        let client = FeeBurnEngineClient::new(&env, &id);
        let admin = Address::generate(&env);
        client.initialize(&admin);
        (env, client, admin)
    }

    /// Create a platform token where the issuer issues `minted` units and a
    /// dedicated burn-holder `holder` is funded with `funded` units so the
    /// engine may permanently destroy them (the SAC refuses to burn the issuer).
    fn setup_token(
        env: &Env,
        client: &FeeBurnEngineClient<'static>,
        admin: &Address,
        ratio_bps: u32,
        minted: i128,
        funded: i128,
    ) -> (Address, Address) {
        let issuer = Address::generate(env);
        let token = env.register_stellar_asset_contract(issuer.clone());
        let holder = Address::generate(env);
        client.register_burn_module(admin, &token, &holder, &ratio_bps);
        let stellar = StellarAssetClient::new(env, &token.clone());
        stellar.mint(&issuer, &minted);
        if funded > 0 {
            token::Client::new(env, &token).transfer(&issuer, &holder, &funded);
        }
        (token, holder)
    }

    #[test]
    fn test_initialize_sets_admin() {
        let env = Env::default();
        env.mock_all_auths_allowing_non_root_auth();
        let admin = Address::generate(&env);
        let id = env.register_contract(None, FeeBurnEngine);
        let client = FeeBurnEngineClient::new(&env, &id);
        assert!(client.try_initialize(&admin).is_ok());
        // Double init must be rejected.
        assert!(client.try_initialize(&admin).is_err());
    }

    #[test]
    fn test_register_module_and_route_fees() {
        let (env, client, admin) = setup();
        let issuer = Address::generate(&env);
        let token = env.register_stellar_asset_contract(issuer.clone());
        let module = client.register_burn_module(&admin, &token, &issuer, &2500);

        assert_eq!(module.token, token);
        assert_eq!(module.burn_ratio_bps, 2500);

        let routed = client.route_fees(&admin, &token, &4_000);
        // 25% of 4000 = 1000 accumulates for burn.
        assert_eq!(routed.fees_accumulated, 1_000);
        assert_eq!(routed.total_burnt, 0);
    }

    #[test]
    fn test_burn_reduces_supply_and_emits_event() {
        let (env, client, admin) = setup();
        let (token, holder) = setup_token(&env, &client, &admin, 5000, 10_000, 10_000);

        client.route_fees(&admin, &token, &4_000);

        let after = client.burn_accumulated_fees(&admin, &token);
        // 50% of 4000 = 2000 burned from the holder.
        assert_eq!(after.fees_accumulated, 0);
        assert_eq!(after.total_burnt, 2_000);

        // The TokensBurned event was emitted with the burnt count and metrics.
        let events = env.events().all();
        let burned_event = events
            .iter()
            .find(|(_contract, topics, _data)| {
                let topic0: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
                let topic1: Address = topics.get(1).unwrap().try_into_val(&env).unwrap();
                topic0 == symbol_short!("tok_burn") && topic1 == token
            });
        assert!(burned_event.is_some());
        let (_, _, data) = burned_event.unwrap();
        let (burnt, total_burnt, remaining): (i128, i128, i128) =
            data.try_into_val(&env).expect("event payload");
        assert_eq!(burnt, 2_000);
        assert_eq!(total_burnt, 2_000);
        assert_eq!(remaining, 0);
        // Tokens are permanently removed from the burn-holder's balance.
        let balance = client.token_balance(&token, &holder);
        assert_eq!(balance, 8_000);
    }

    #[test]
    fn test_auto_burn_triggers_at_threshold() {
        let (env, client, admin) = setup();
        let (token, _holder) = setup_token(&env, &client, &admin, 10_000, 5_000, 5_000);
        client.set_auto_burn_threshold(&admin, &token, &1_000);

        // 100% ratio, 1000 routed => hits the 1000 threshold => burns immediately.
        let after = client.route_fees(&admin, &token, &1_000);
        assert_eq!(after.fees_accumulated, 0);
        assert_eq!(after.total_burnt, 1_000);
    }

    #[test]
    fn test_burn_exact() {
        let (env, client, admin) = setup();
        let (token, _holder) = setup_token(&env, &client, &admin, 10_000, 5_000, 5_000);

        client.route_fees(&admin, &token, &3_000);
        let partial = client.burn_exact(&admin, &token, &1_250);
        assert_eq!(partial.total_burnt, 1_250);
        assert_eq!(partial.fees_accumulated, 1_750);
    }

    #[test]
    fn test_burn_more_than_accumulated_is_rejected() {
        let (env, client, admin) = setup();
        let (token, _holder) = setup_token(&env, &client, &admin, 5000, 5_000, 5_000);

        client.route_fees(&admin, &token, &1_000);
        let result = client.try_burn_exact(&admin, &token, &2_000);
        assert_eq!(result, Err(Ok(BurnError::InsufficientFees)));
    }

    #[test]
    fn test_unauthorized_route_rejected() {
        let (env, client, admin) = setup();
        let attacker = Address::generate(&env);
        let issuer = Address::generate(&env);
        let token = env.register_stellar_asset_contract(issuer.clone());

        // Only the admin may register a burn module.
        let reg = client.try_register_burn_module(&attacker, &token, &issuer, &1000);
        assert_eq!(reg, Err(Ok(BurnError::NotAdmin)));

        client.register_burn_module(&admin, &token, &issuer, &1000);
        let result = client.try_route_fees(&attacker, &token, &100);
        assert_eq!(result, Err(Ok(BurnError::NotAdmin)));
    }

    #[test]
    fn test_invalid_ratio_rejected() {
        let env = Env::default();
        env.mock_all_auths_allowing_non_root_auth();
        let admin = Address::generate(&env);
        let id = env.register_contract(None, FeeBurnEngine);
        let client = FeeBurnEngineClient::new(&env, &id);
        client.initialize(&admin);

        let issuer = Address::generate(&env);
        let token = env.register_stellar_asset_contract(issuer.clone());
        let result = client.try_register_burn_module(&admin, &token, &issuer, &10_001);
        assert_eq!(result, Err(Ok(BurnError::InvalidRatio)));
    }
}
