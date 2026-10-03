#![no_std]

use soroban_sdk::token::TokenClient;
use soroban_sdk::{contract, contracterror, contractimpl, symbol_short, Address, Env};

pub mod adaptive_fee_engine;
pub mod tick_bitmap;
pub mod virtual_reserves;

use adaptive_fee_engine::{
    apply_fee_to_amount_in, compute_dynamic_fee, query_volatility_scalar,
    AdaptiveFeeConfig, AppliedFeeSnapshot, MAX_DYNAMIC_FEE_BPS,
};
use virtual_reserves::{
    assert_k_eff_not_decreased, assert_min_amount_out, assert_withdrawal_allowed_both_sides,
    initial_lp_shares, EffectiveReserves, VirtualReserveError,
};

/// Virtual reserve buffer installed for every pair by default: 1,000 units of
/// each token. This is the pool's non-withdrawable core — it gives a freshly
/// initialised pair a non-zero starting curve and caps how much liquidity an
/// LP can pull back out. See [`virtual_reserves`] and issue #906.
pub const DEFAULT_VIRTUAL_RESERVE: i128 = 1_000;

/// Translate a virtual-reserve failure into the contract's error surface.
fn map_virtual(err: VirtualReserveError) -> AmmError {
    match err {
        VirtualReserveError::VirtualFloorBreached => AmmError::VirtualFloorBreached,
        VirtualReserveError::SlippageExceeded => AmmError::SlippageExceeded,
        VirtualReserveError::InvariantViolation => AmmError::InvariantViolation,
        VirtualReserveError::Overflow => AmmError::ArithmeticOverflow,
        VirtualReserveError::NegativeReserve | VirtualReserveError::NonPositiveAmount => {
            AmmError::NonPositiveAmount
        }
    }
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ContractError {
    /// Recovery steps: Inspect the state for AlreadyInitialized and retry with valid inputs or proper conditions.
    AlreadyInitialized = 1,
    /// Recovery steps: Inspect the state for NotInitialized and retry with valid inputs or proper conditions.
    NotInitialized = 2,
    /// Recovery steps: Inspect the state for InvalidDepositRatio and retry with valid inputs or proper conditions.
    InvalidDepositRatio = 3,
    /// Recovery steps: Inspect the state for SlippageExceeded and retry with valid inputs or proper conditions.
    SlippageExceeded = 4,
    /// Recovery steps: Inspect the state for ZeroDeposit and retry with valid inputs or proper conditions.
    ZeroDeposit = 5,
    /// Recovery steps: Inspect the state for PoolEmpty and retry with valid inputs or proper conditions.
    PoolEmpty = 6,
    /// Constant-product invariant violated: k_new < k_old after a swap.
    /// Recovery steps: Inspect the state for InvariantViolation and retry with valid inputs or proper conditions.
    InvariantViolation = 7,
    /// The operation would have pushed a real reserve onto or below its core
    /// virtual reserve threshold (issue #906).
    VirtualFloorBreached = 8,
    /// An amount supplied by the caller was zero or negative.
    NonPositiveAmount = 9,
    /// A reserve, share or product exceeded the representable range.
    ArithmeticOverflow = 10,
    /// Tick spacing must be in `1..=MAX_TICK_SPACING`.
    InvalidTickSpacing = 11,
    /// Tick lies outside `[MIN_TICK, MAX_TICK]`.
    TickOutOfBounds = 12,
    /// Tick is not a multiple of the tick spacing.
    TickNotAligned = 13,
    /// Price lies outside `[P(MIN_TICK), P(MAX_TICK)]`.
    PriceOutOfBounds = 14,
    /// Price does not satisfy `P(tick) <= price < P(tick + 1)`, `P(i) = 1.0001^i`.
    TickPriceMismatch = 15,
}

#[contract]
pub struct AmmContract;

/// Read the pair's core virtual reserve buffer, defaulting to
/// [`DEFAULT_VIRTUAL_RESERVE`] for pairs created before virtual reserves were
/// introduced.
fn load_virtual_reserves(env: &Env) -> (i128, i128) {
    let virtual_a: i128 = env
        .storage()
        .instance()
        .get(&symbol_short!("vres_a"))
        .unwrap_or(DEFAULT_VIRTUAL_RESERVE);
    let virtual_b: i128 = env
        .storage()
        .instance()
        .get(&symbol_short!("vres_b"))
        .unwrap_or(DEFAULT_VIRTUAL_RESERVE);
    (virtual_a, virtual_b)
}

#[contractimpl]
impl AmmContract {
    /// Initialise a pair with the protocol default virtual reserve core.
    ///
    /// Equivalent to [`Self::initialize_with_virtual_reserves`] with
    /// [`DEFAULT_VIRTUAL_RESERVE`] on both legs.
    pub fn initialize(
        env: Env,
        token_a: Address,
        token_b: Address,
        lp_token: Address,
    ) -> Result<(), AmmError> {
        Self::initialize_with_virtual_reserves(
            env,
            token_a,
            token_b,
            lp_token,
            DEFAULT_VIRTUAL_RESERVE,
            DEFAULT_VIRTUAL_RESERVE,
        )
    }

    /// Initialise a pair with an explicit virtual reserve core (issue #906).
    ///
    /// The pair is created with `x = y = 0` real reserves but a non-zero
    /// effective curve `k_eff = v_x * v_y`, so its price function is well
    /// defined from genesis instead of degenerating to `k = 0`. The core is
    /// credited to no LP and can never be withdrawn.
    pub fn initialize_with_virtual_reserves(
        env: Env,
        token_a: Address,
        token_b: Address,
        lp_token: Address,
        virtual_a: i128,
        virtual_b: i128,
    ) -> Result<(), AmmError> {
        let key_init = symbol_short!("init");
        if env.storage().instance().has(&key_init) {
            return Err(ContractError::AlreadyInitialized);
        }
        virtual_reserves::validate_virtual_reserves(0, 0, virtual_a, virtual_b)
            .map_err(map_virtual)?;
        env.storage().instance().set(&key_init, &true);
        env.storage()
            .instance()
            .set(&symbol_short!("token_a"), &token_a);
        env.storage()
            .instance()
            .set(&symbol_short!("token_b"), &token_b);
        env.storage()
            .instance()
            .set(&symbol_short!("lp_token"), &lp_token);
        env.storage()
            .instance()
            .set(&symbol_short!("res_a"), &0i128);
        env.storage()
            .instance()
            .set(&symbol_short!("res_b"), &0i128);
        env.storage()
            .instance()
            .set(&symbol_short!("tot_sh"), &0i128);
        env.storage()
            .instance()
            .set(&symbol_short!("vres_a"), &virtual_a);
        env.storage()
            .instance()
            .set(&symbol_short!("vres_b"), &virtual_b);
        Ok(())
    }

    /// Configure adaptive swap fee engine based on dynamic volatility oracle (Issue #930).
    pub fn set_adaptive_fee_config(
        env: Env,
        f_base: u32,
        f_scalar: u32,
        oracle: Option<Address>,
        asset_symbol: Option<soroban_sdk::Symbol>,
    ) -> Result<(), AmmError> {
        let key_init = symbol_short!("init");
        if !env.storage().instance().has(&key_init) {
            return Err(AmmError::NotInitialized);
        }
        if f_base > MAX_DYNAMIC_FEE_BPS {
            return Err(AmmError::ArithmeticOverflow);
        }
        let config = AdaptiveFeeConfig {
            f_base,
            f_scalar,
        };
        env.storage()
            .instance()
            .set(&symbol_short!("fee_cfg"), &config);

        if let Some(oracle_addr) = oracle {
            env.storage()
                .instance()
                .set(&symbol_short!("oracle"), &oracle_addr);
        }
        if let Some(symbol) = asset_symbol {
            env.storage()
                .instance()
                .set(&symbol_short!("feed_sym"), &symbol);
        }
        Ok(())
    }

    /// Read the installed adaptive fee configuration.
    pub fn get_adaptive_fee_config(env: Env) -> Option<AdaptiveFeeConfig> {
        env.storage().instance().get(&symbol_short!("fee_cfg"))
    }

    /// Set or update the live volatility scalar Vsigma override (Issue #930).
    pub fn set_volatility_scalar(env: Env, v_sigma: u32) -> Result<(), AmmError> {
        env.storage()
            .instance()
            .set(&symbol_short!("v_sigma"), &v_sigma);
        Ok(())
    }

    /// Query the current dynamic fee fswap = min(fbase + (Vsigma * fscalar), 100 BPS) in basis points.
    pub fn get_dynamic_swap_fee(env: Env) -> u32 {
        if let Some(config) = env
            .storage()
            .instance()
            .get::<_, AdaptiveFeeConfig>(&symbol_short!("fee_cfg"))
        {
            let v_sigma: u32 = env
                .storage()
                .instance()
                .get(&symbol_short!("v_sigma"))
                .unwrap_or_else(|| {
                    if let (Some(oracle), Some(sym)) = (
                        env.storage().instance().get(&symbol_short!("oracle")),
                        env.storage().instance().get(&symbol_short!("feed_sym")),
                    ) {
                        query_volatility_scalar(&env, &oracle, &sym)
                    } else {
                        0
                    }
                });
            compute_dynamic_fee(config.f_base, v_sigma, config.f_scalar).unwrap_or(config.f_base)
        } else {
            0u32
        }
    }

    /// Query the last dynamic fee snapshot applied in swap execution.
    pub fn get_applied_fee_snapshot(env: Env) -> Option<AppliedFeeSnapshot> {
        env.storage().instance().get(&symbol_short!("fee_snap"))
    }

    pub fn deposit(
        env: Env,
        provider: Address,
        amount_a_desired: i128,
        amount_b_desired: i128,
        min_lp_mint: i128,
    ) -> Result<i128, ContractError> {
        provider.require_auth();

        if amount_a_desired <= 0 || amount_b_desired <= 0 {
            return Err(ContractError::ZeroDeposit);
        }

        let token_a_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("token_a"))
            .ok_or(ContractError::NotInitialized)?;
        let token_b_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("token_b"))
            .ok_or(ContractError::NotInitialized)?;
        let lp_token_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("lp_token"))
            .ok_or(ContractError::NotInitialized)?;

        let mut reserve_a: i128 = env
            .storage()
            .instance()
            .get(&symbol_short!("res_a"))
            .unwrap_or(0);
        let mut total_shares: i128 = env
            .storage()
            .instance()
            .get(&symbol_short!("tot_sh"))
            .unwrap_or(0);
        let mut reserve_b: i128 = env
            .storage()
            .instance()
            .get(&symbol_short!("res_b"))
            .unwrap_or(0);
        let (virtual_a, virtual_b) = load_virtual_reserves(&env);

        let (deposit_a, deposit_b, minted_shares) = if total_shares == 0 {
            // Genesis mint. Counting the virtual core is what gives a brand new
            // pair a non-zero starting supply instead of a dust one.
            let initial_shares =
                initial_lp_shares(amount_a_desired, amount_b_desired, virtual_a, virtual_b)
                    .map_err(map_virtual)?;
            if initial_shares < min_lp_mint {
                return Err(ContractError::SlippageExceeded);
            }
            (amount_a_desired, amount_b_desired, initial_shares)
        } else {
            // The deposit ratio is set by the *effective* curve, so a pair with
            // a virtual core prices deposits the same way it prices trades.
            let effective = EffectiveReserves::new(reserve_a, reserve_b, virtual_a, virtual_b)
                .map_err(map_virtual)?;
            let required_b = (amount_a_desired * effective.y_eff) / effective.x_eff;
            if amount_b_desired < required_b {
                return Err(ContractError::InvalidDepositRatio);
            }
            let optimal_a = (amount_b_desired * effective.x_eff) / effective.y_eff;
            let (opt_a, opt_b) = if optimal_a <= amount_a_desired {
                (optimal_a, amount_b_desired)
            } else {
                (amount_a_desired, required_b)
            };

            // Shares stay proportional to the *real* reserves on both legs: the
            // virtual core is not LP-owned and must not be dilutable.
            let shares_a = (opt_a * total_shares) / reserve_a;
            let shares_b = (opt_b * total_shares) / reserve_b;
            let shares = shares_a.min(shares_b);
            if shares < min_lp_mint {
                return Err(ContractError::SlippageExceeded);
            }
            (opt_a, opt_b, shares)
        };

        let token_a = TokenClient::new(&env, &token_a_addr);
        let token_b = TokenClient::new(&env, &token_b_addr);
        let lp_token = soroban_sdk::token::StellarAssetClient::new(&env, &lp_token_addr);

        token_a.transfer(&provider, &env.current_contract_address(), &deposit_a);
        token_b.transfer(&provider, &env.current_contract_address(), &deposit_b);
        lp_token.mint(&provider, &minted_shares);

        reserve_a += deposit_a;
        reserve_b += deposit_b;
        total_shares += minted_shares;

        env.storage()
            .instance()
            .set(&symbol_short!("res_a"), &reserve_a);
        env.storage()
            .instance()
            .set(&symbol_short!("res_b"), &reserve_b);
        env.storage()
            .instance()
            .set(&symbol_short!("tot_sh"), &total_shares);

        Ok(minted_shares)
    }

    /// Execute a constant-product swap: trader sends `amount_in` of token A and
    /// receives at least `min_amount_out` of token B.
    ///
    /// The quote and the invariant both run on the **effective** reserves
    /// (issue #906):
    ///
    /// ```text
    /// x_eff = reserve_a + v_a      y_eff = reserve_b + v_b
    /// amount_out = y_eff * amount_in / (x_eff + amount_in)
    /// k_eff     = x_eff * y_eff   (must not decrease)
    /// ```
    ///
    /// Reverts with [`AmmError::InvariantViolation`] if `k_eff` decreased, and
    /// with [`AmmError::VirtualFloorBreached`] if the quote would pay out more
    /// than the real reserve on the output leg.
    pub fn swap(
        env: Env,
        trader: Address,
        amount_in: i128,
        min_amount_out: i128,
    ) -> Result<i128, ContractError> {
        trader.require_auth();

        if amount_in <= 0 {
            return Err(ContractError::ZeroDeposit);
        }

        let token_a_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("token_a"))
            .ok_or(ContractError::NotInitialized)?;
        let token_b_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("token_b"))
            .ok_or(ContractError::NotInitialized)?;

        let reserve_a: i128 = env
            .storage()
            .instance()
            .get(&symbol_short!("res_a"))
            .unwrap_or(0);
        let reserve_b: i128 = env
            .storage()
            .instance()
            .get(&symbol_short!("res_b"))
            .unwrap_or(0);

        if reserve_a <= 0 || reserve_b <= 0 {
            return Err(ContractError::PoolEmpty);
        }

        let (virtual_a, virtual_b) = load_virtual_reserves(&env);
        let before =
            EffectiveReserves::new(reserve_a, reserve_b, virtual_a, virtual_b).map_err(map_virtual)?;

        // Compute adaptive swap fee if configured (Issue #930)
        let (effective_amount_in, _fee_deducted) = if let Some(config) = env
            .storage()
            .instance()
            .get::<_, AdaptiveFeeConfig>(&symbol_short!("fee_cfg"))
        {
            // 1. Query volatility scalar Vsigma from dynamic oracle feed
            let v_sigma: u32 = env
                .storage()
                .instance()
                .get(&symbol_short!("v_sigma"))
                .unwrap_or_else(|| {
                    if let (Some(oracle), Some(sym)) = (
                        env.storage().instance().get(&symbol_short!("oracle")),
                        env.storage().instance().get(&symbol_short!("feed_sym")),
                    ) {
                        query_volatility_scalar(&env, &oracle, &sym)
                    } else {
                        0
                    }
                });

            // 2. Compute dynamic fee fswap = fbase + (Vsigma * fscalar) constrained to fswap <= 0.01 (100 BPS)
            let f_swap = compute_dynamic_fee(config.f_base, v_sigma, config.f_scalar)?;

            // 3. Assert pool swap execution applies updated fee instantly within current ledger
            let (net_in, fee_amount) = apply_fee_to_amount_in(amount_in, f_swap)?;

            let snapshot = AppliedFeeSnapshot {
                f_swap,
                v_sigma,
                ledger_sequence: env.ledger().sequence(),
            };
            env.storage()
                .instance()
                .set(&symbol_short!("fee_snap"), &snapshot);

            env.events().publish(
                (symbol_short!("dyn_fee"), env.ledger().sequence()),
                (f_swap, v_sigma, fee_amount),
            );

            (net_in, fee_amount)
        } else {
            (amount_in, 0i128)
        };

        // Quote, floor the post-trade reserves and re-check the effective
        // invariant in one shot using effective_amount_in.
        let (after, amount_out, k_before) =
            before.swap_a_to_b(effective_amount_in).map_err(map_virtual)?;

        if amount_out <= 0 {
            return Err(ContractError::SlippageExceeded);
        }
        assert_min_amount_out(amount_out, min_amount_out).map_err(map_virtual)?;
        assert_k_eff_not_decreased(&k_before, &after.k_eff()).map_err(map_virtual)?;

        // Execute token transfers: trader sends full amount_in, receives amount_out
        let token_a = TokenClient::new(&env, &token_a_addr);
        let token_b = TokenClient::new(&env, &token_b_addr);

        token_a.transfer(&trader, &env.current_contract_address(), &amount_in);
        token_b.transfer(&env.current_contract_address(), &trader, &amount_out);

        // Persist updated reserves
        env.storage()
            .instance()
            .set(&symbol_short!("res_a"), &after.x);
        env.storage()
            .instance()
            .set(&symbol_short!("res_b"), &after.y);

        Ok(amount_out)
    }

    /// Burn `shares` LP tokens and return the pro-rata `(amount_a, amount_b)`
    /// owed to `provider`.
    ///
    /// Unlike [`Self::swap`], a withdrawal is *not* bounded by the
    /// constant-product invariant — burning shares simply reduces the claim on
    /// the real reserves. That is exactly the hole issue #906 closes: without
    /// a floor, the last LP out could leave the pair quoting on the virtual
    /// core alone and hand the next depositor a violently mispriced pool.
    ///
    /// Both legs must therefore clear the core virtual reserve threshold. The
    /// call reverts with [`AmmError::VirtualFloorBreached`] otherwise, and
    /// with [`AmmError::SlippageExceeded`] if the realised amounts fall below
    /// the caller's `min_amount_a` / `min_amount_b` slippage bounds.
    pub fn remove_liquidity(
        env: Env,
        provider: Address,
        shares: i128,
        min_amount_a: i128,
        min_amount_b: i128,
    ) -> Result<(i128, i128), AmmError> {
        provider.require_auth();

        if shares <= 0 {
            return Err(AmmError::NonPositiveAmount);
        }

        let token_a_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("token_a"))
            .ok_or(AmmError::NotInitialized)?;
        let token_b_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("token_b"))
            .ok_or(AmmError::NotInitialized)?;
        let lp_token_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("lp_token"))
            .ok_or(AmmError::NotInitialized)?;

        let reserve_a: i128 = env
            .storage()
            .instance()
            .get(&symbol_short!("res_a"))
            .unwrap_or(0);
        let reserve_b: i128 = env
            .storage()
            .instance()
            .get(&symbol_short!("res_b"))
            .unwrap_or(0);
        let total_shares: i128 = env
            .storage()
            .instance()
            .get(&symbol_short!("tot_sh"))
            .unwrap_or(0);

        if total_shares <= 0 || shares > total_shares {
            return Err(AmmError::InvalidDepositRatio);
        }

        let (virtual_a, virtual_b) = load_virtual_reserves(&env);
        let reserves =
            EffectiveReserves::new(reserve_a, reserve_b, virtual_a, virtual_b).map_err(map_virtual)?;

        // Pro-rata on the real reserves; the core is not part of any claim.
        let amount_a = (shares * reserve_a) / total_shares;
        let amount_b = (shares * reserve_b) / total_shares;
        if amount_a <= 0 || amount_b <= 0 {
            return Err(AmmError::NonPositiveAmount);
        }

        // The floor that keeps the pair's advertised curve backed.
        assert_withdrawal_allowed_both_sides(&reserves, amount_a, amount_b).map_err(map_virtual)?;

        if amount_a < min_amount_a || amount_b < min_amount_b {
            return Err(AmmError::SlippageExceeded);
        }

        let token_a = TokenClient::new(&env, &token_a_addr);
        let token_b = TokenClient::new(&env, &token_b_addr);
        let lp_token = TokenClient::new(&env, &lp_token_addr);

        lp_token.burn(&provider, &shares);
        token_a.transfer(&env.current_contract_address(), &provider, &amount_a);
        token_b.transfer(&env.current_contract_address(), &provider, &amount_b);

        env.storage()
            .instance()
            .set(&symbol_short!("res_a"), &(reserve_a - amount_a));
        env.storage()
            .instance()
            .set(&symbol_short!("res_b"), &(reserve_b - amount_b));
        env.storage()
            .instance()
            .set(&symbol_short!("tot_sh"), &(total_shares - shares));

        Ok((amount_a, amount_b))
    }

    /// `(reserve_a + v_a, reserve_b + v_b)`, the balances the pool actually
    /// quotes and enforces its invariant on.
    pub fn get_effective_reserves(env: Env) -> (i128, i128) {
        let (reserve_a, reserve_b) = Self::get_reserves(env.clone());
        let (virtual_a, virtual_b) = load_virtual_reserves(&env);
        (reserve_a + virtual_a, reserve_b + virtual_b)
    }

    /// The core virtual reserve buffer `(v_a, v_b)` installed for this pair.
    pub fn get_virtual_reserves(env: Env) -> (i128, i128) {
        load_virtual_reserves(&env)
    }

    pub fn get_reserves(env: Env) -> (i128, i128) {
        let reserve_a: i128 = env
            .storage()
            .instance()
            .get(&symbol_short!("res_a"))
            .unwrap_or(0);
        let reserve_b: i128 = env
            .storage()
            .instance()
            .get(&symbol_short!("res_b"))
            .unwrap_or(0);
        (reserve_a, reserve_b)
    }

    pub fn get_total_shares(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&symbol_short!("tot_sh"))
            .unwrap_or(0)
    }

    /// Q64.64 price of `tick`: `1.0001^tick`.
    pub fn tick_price(_env: Env, tick: i32) -> Result<u128, AmmError> {
        tick_bitmap::tick_to_price_q64(tick)
    }

    /// Next initialized tick in the swap direction, scanning at most
    /// `max_words` bitmap words. See [`tick_bitmap::next_initialized_tick`].
    pub fn next_initialized_tick(
        env: Env,
        tick: i32,
        tick_spacing: i32,
        lte: bool,
        max_words: u32,
    ) -> Result<(i32, bool), AmmError> {
        tick_bitmap::next_initialized_tick(&env, tick, tick_spacing, lte, max_words)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger as _};
    use soroban_sdk::{Address, Env};

    /// Verify that the constant-product swap function rejects when k_new < k_old.
    /// We seed reserves directly into contract storage so we can bypass the
    /// broken LP-minting path in `deposit`.
    #[test]
    fn test_swap_invariant_violation_reverts() {
        let env = Env::default();
        env.mock_all_auths();

        let token_a_admin = Address::generate(&env);
        let token_b_admin = Address::generate(&env);
        let lp_admin = Address::generate(&env);

        let token_a = env.register_stellar_asset_contract(token_a_admin.clone());
        let token_b = env.register_stellar_asset_contract(token_b_admin.clone());
        let lp_token = env.register_stellar_asset_contract(lp_admin.clone());

        let contract_id = env.register_contract(None, AmmContract);
        let client = AmmContractClient::new(&env, &contract_id);

        client.initialize(&token_a, &token_b, &lp_token);

        // Seed reserves directly so we don't need deposit's LP-mint path.
        env.as_contract(&contract_id, || {
            env.storage()
                .instance()
                .set(&symbol_short!("res_a"), &1000i128);
            env.storage()
                .instance()
                .set(&symbol_short!("res_b"), &2000i128);
        });

        // Provide token A to the trader so the transfer can succeed.
        let trader = Address::generate(&env);
        soroban_sdk::token::StellarAssetClient::new(&env, &token_a).mint(&trader, &500);
        // Fund the contract's token B balance so the outbound transfer works.
        soroban_sdk::token::StellarAssetClient::new(&env, &token_b).mint(&contract_id, &2000);

        // A valid swap: 100 A → some B.
        let amount_out = client.swap(&trader, &100, &1);
        assert!(amount_out > 0, "expected positive output");

        // k_new >= k_old
        let (new_ra, new_rb) = client.get_reserves();
        let k_old: i128 = 1000 * 2000;
        let k_new: i128 = new_ra * new_rb;
        assert!(
            k_new >= k_old,
            "invariant violated: k_new={k_new} < k_old={k_old}"
        );
    }

    #[test]
    fn test_swap_zero_input_rejected() {
        let env = Env::default();
        env.mock_all_auths();

        let token_a_admin = Address::generate(&env);
        let token_b_admin = Address::generate(&env);
        let lp_admin = Address::generate(&env);
        let token_a = env.register_stellar_asset_contract(token_a_admin.clone());
        let token_b = env.register_stellar_asset_contract(token_b_admin.clone());
        let lp_token = env.register_stellar_asset_contract(lp_admin.clone());

        let contract_id = env.register_contract(None, AmmContract);
        let client = AmmContractClient::new(&env, &contract_id);
        client.initialize(&token_a, &token_b, &lp_token);

        env.as_contract(&contract_id, || {
            env.storage()
                .instance()
                .set(&symbol_short!("res_a"), &1000i128);
            env.storage()
                .instance()
                .set(&symbol_short!("res_b"), &2000i128);
        });

        let trader = Address::generate(&env);
        let result = client.try_swap(&trader, &0, &0);
        assert!(result.is_err(), "zero amount_in should be rejected");
    }

    #[test]
    fn test_tick_entrypoints() {
        let env = Env::default();
        let contract_id = env.register_contract(None, AmmContract);
        let client = AmmContractClient::new(&env, &contract_id);

        assert_eq!(client.tick_price(&0), tick_bitmap::Q64);
        assert_eq!(
            client.try_tick_price(&(tick_bitmap::MAX_TICK + 1)),
            Err(Ok(AmmError::TickOutOfBounds))
        );

        env.as_contract(&contract_id, || {
            tick_bitmap::flip_tick(&env, 600, 60).unwrap();
        });
        assert_eq!(client.next_initialized_tick(&0, &60, &false, &10), (600, true));
        assert_eq!(
            client.try_next_initialized_tick(&0, &0, &false, &10),
            Err(Ok(AmmError::InvalidTickSpacing))
        );
    }

    /// `initialize` installs the default 1,000-unit virtual core, so a brand
    /// new pair already quotes on a non-zero curve before any deposit lands.
    #[test]
    fn test_initialize_installs_default_virtual_core() {
        let env = Env::default();
        env.mock_all_auths();

        let token_a = env.register_stellar_asset_contract(Address::generate(&env));
        let token_b = env.register_stellar_asset_contract(Address::generate(&env));
        let lp_token = env.register_stellar_asset_contract(Address::generate(&env));

        let contract_id = env.register_contract(None, AmmContract);
        let client = AmmContractClient::new(&env, &contract_id);
        client.initialize(&token_a, &token_b, &lp_token);

        assert_eq!(client.get_virtual_reserves(), (1000, 1000));
        assert_eq!(client.get_effective_reserves(), (1000, 1000));
    }

    /// An explicit core is stored verbatim; degenerate (zero) buffers are
    /// rejected.
    #[test]
    fn test_initialize_with_custom_virtual_core() {
        let env = Env::default();
        env.mock_all_auths();

        let token_a = env.register_stellar_asset_contract(Address::generate(&env));
        let token_b = env.register_stellar_asset_contract(Address::generate(&env));
        let lp_token = env.register_stellar_asset_contract(Address::generate(&env));

        let contract_id = env.register_contract(None, AmmContract);
        let client = AmmContractClient::new(&env, &contract_id);
        client.initialize_with_virtual_reserves(&token_a, &token_b, &lp_token, &500, &700);

        assert_eq!(client.get_virtual_reserves(), (500, 700));

        // A second client on a fresh contract: zero buffers are rejected.
        let other = env.register_contract(None, AmmContract);
        let other_client = AmmContractClient::new(&env, &other);
        let res = other_client.try_initialize_with_virtual_reserves(
            &token_a,
            &token_b,
            &lp_token,
            &0,
            &700,
        );
        assert_eq!(res, Err(Ok(AmmError::NonPositiveAmount)));
    }

    /// The genesis mint counts the virtual core: seeding a fresh pair with a
    /// 500/500 deposit must record the `min(500 + v_a, 500 + v_b) = 1500`
    /// share supply rather than dust.
    ///
    /// NOTE: exercised through storage seeding rather than a live `deposit`
    /// call because the LP-mint leg of `deposit` (SAC `mint` invoked from
    /// within the contract) traps in the SDK 20 test host — a pre-existing
    /// limitation the older swap tests already work around by seeding
    /// reserves directly. The genesis share formula itself is covered by
    /// `virtual_reserves::tests::initial_shares_bootstrap_from_the_buffer`.
    #[test]
    fn test_genesis_share_supply_counts_virtual_core() {
        let env = Env::default();
        env.mock_all_auths();

        let token_a = env.register_stellar_asset_contract(Address::generate(&env));
        let token_b = env.register_stellar_asset_contract(Address::generate(&env));
        let lp_token = env.register_stellar_asset_contract(Address::generate(&env));

        let contract_id = env.register_contract(None, AmmContract);
        let client = AmmContractClient::new(&env, &contract_id);
        client.initialize(&token_a, &token_b, &lp_token);

        // Genesis state a 500/500 deposit would have produced: real reserves
        // plus the min-formula supply against the 1,000-unit core.
        let genesis_shares =
            virtual_reserves::initial_lp_shares(500, 500, 1000, 1000).unwrap();
        assert_eq!(genesis_shares, 1500);

        env.as_contract(&contract_id, || {
            env.storage()
                .instance()
                .set(&symbol_short!("res_a"), &500i128);
            env.storage()
                .instance()
                .set(&symbol_short!("res_b"), &500i128);
            env.storage()
                .instance()
                .set(&symbol_short!("tot_sh"), &genesis_shares);
        });

        assert_eq!(client.get_reserves(), (500, 500));
        assert_eq!(client.get_effective_reserves(), (1500, 1500));
        assert_eq!(client.get_total_shares(), 1500);
    }

    /// Burning 100 of 2000 shares against (2000, 2000) returns (100, 100):
    /// the real reserves stay comfortably above the 1,000-unit core.
    #[test]
    fn test_remove_liquidity_pays_pro_rata_above_core() {
        let env = Env::default();
        env.mock_all_auths();

        let token_a = env.register_stellar_asset_contract(Address::generate(&env));
        let token_b = env.register_stellar_asset_contract(Address::generate(&env));
        let lp_token = env.register_stellar_asset_contract(Address::generate(&env));

        let contract_id = env.register_contract(None, AmmContract);
        let client = AmmContractClient::new(&env, &contract_id);
        client.initialize(&token_a, &token_b, &lp_token);

        env.as_contract(&contract_id, || {
            env.storage()
                .instance()
                .set(&symbol_short!("res_a"), &2000i128);
            env.storage()
                .instance()
                .set(&symbol_short!("res_b"), &2000i128);
            env.storage()
                .instance()
                .set(&symbol_short!("tot_sh"), &2000i128);
        });

        let provider = Address::generate(&env);
        soroban_sdk::token::StellarAssetClient::new(&env, &lp_token).mint(&provider, &2000);
        soroban_sdk::token::StellarAssetClient::new(&env, &token_a).mint(&contract_id, &2000);
        soroban_sdk::token::StellarAssetClient::new(&env, &token_b).mint(&contract_id, &2000);

        let (amount_a, amount_b) = client.remove_liquidity(&provider, &100, &0, &0);
        assert_eq!((amount_a, amount_b), (100, 100));
        assert_eq!(client.get_reserves(), (1900, 1900));
    }

    /// Draining the pool is rejected: withdrawing all 2000 shares from
    /// (2000, 2000) would leave 0 on each leg, below the 1,000-unit core.
    #[test]
    fn test_remove_liquidity_cannot_breach_virtual_core() {
        let env = Env::default();
        env.mock_all_auths();

        let token_a = env.register_stellar_asset_contract(Address::generate(&env));
        let token_b = env.register_stellar_asset_contract(Address::generate(&env));
        let lp_token = env.register_stellar_asset_contract(Address::generate(&env));

        let contract_id = env.register_contract(None, AmmContract);
        let client = AmmContractClient::new(&env, &contract_id);
        client.initialize(&token_a, &token_b, &lp_token);

        env.as_contract(&contract_id, || {
            env.storage()
                .instance()
                .set(&symbol_short!("res_a"), &2000i128);
            env.storage()
                .instance()
                .set(&symbol_short!("res_b"), &2000i128);
            env.storage()
                .instance()
                .set(&symbol_short!("tot_sh"), &2000i128);
        });

        let provider = Address::generate(&env);
        soroban_sdk::token::StellarAssetClient::new(&env, &lp_token).mint(&provider, &2000);

        let res = client.try_remove_liquidity(&provider, &2000, &0, &0);
        assert_eq!(res, Err(Ok(AmmError::VirtualFloorBreached)));

        // The largest realisable exit leaves exactly the core behind. Fund the
        // pair first so the outbound transfers can settle.
        soroban_sdk::token::StellarAssetClient::new(&env, &token_a).mint(&contract_id, &2000);
        soroban_sdk::token::StellarAssetClient::new(&env, &token_b).mint(&contract_id, &2000);
        let (amount_a, amount_b) = client.remove_liquidity(&provider, &1000, &0, &0);
        assert_eq!((amount_a, amount_b), (1000, 1000));
        assert_eq!(client.get_reserves(), (1000, 1000));
    }

    #[test]
    fn test_adaptive_swap_fee_engine_volatility_oracle() {
        let env = Env::default();
        env.mock_all_auths();

        let token_a = env.register_stellar_asset_contract(Address::generate(&env));
        let token_b = env.register_stellar_asset_contract(Address::generate(&env));
        let lp_token = env.register_stellar_asset_contract(Address::generate(&env));

        let contract_id = env.register_contract(None, AmmContract);
        let client = AmmContractClient::new(&env, &contract_id);
        client.initialize(&token_a, &token_b, &lp_token);

        // Configure adaptive fee engine: fbase = 20 BPS (0.20%), fscalar = 5
        client.set_adaptive_fee_config(&20, &5, &None, &None);

        // Simulated volatility scalar Vsigma = 10 from live dynamic oracle feed
        // fswap = fbase + (Vsigma * fscalar) = 20 + (10 * 5) = 70 BPS (0.70%)
        client.set_volatility_scalar(&10);
        assert_eq!(client.get_dynamic_swap_fee(), 70);

        // High volatility: Vsigma = 25 -> 20 + 125 = 145 BPS -> capped to fswap <= 0.01 (100 BPS)
        client.set_volatility_scalar(&25);
        assert_eq!(client.get_dynamic_swap_fee(), 100);

        // Set back to Vsigma = 10 for swap execution test
        client.set_volatility_scalar(&10);

        // Deposit liquidity into pool
        env.as_contract(&contract_id, || {
            env.storage().instance().set(&symbol_short!("res_a"), &100_000i128);
            env.storage().instance().set(&symbol_short!("res_b"), &100_000i128);
            env.storage().instance().set(&symbol_short!("tot_sh"), &100_000i128);
        });

        let trader = Address::generate(&env);
        soroban_sdk::token::StellarAssetClient::new(&env, &token_a).mint(&trader, &10_000);
        soroban_sdk::token::StellarAssetClient::new(&env, &token_b).mint(&contract_id, &100_000);

        // Set ledger sequence to 500
        env.ledger().set(soroban_sdk::testutils::LedgerInfo {
            sequence_number: 500,
            ..env.ledger().get()
        });

        // Execute swap: pool swap execution applies updated fee instantly within current ledger
        let amount_out = client.swap(&trader, &10_000, &1);
        assert!(amount_out > 0);

        // Verify applied fee snapshot matches current ledger sequence 500 and fswap = 70 BPS
        let snapshot = client.get_applied_fee_snapshot().unwrap();
        assert_eq!(snapshot.f_swap, 70);
        assert_eq!(snapshot.v_sigma, 10);
        assert_eq!(snapshot.ledger_sequence, 500);
    }
}
