//! Auto-compounding yield farm strategy entrypoint (Issue #911).
//!
//! [`compound_strategy`] is a single atomic vault entrypoint that turns a
//! farmer's accrued reward tokens back into staked pool liquidity:
//!
//! 1. **Claim** — settle and withdraw the caller's pending emissions from the
//!    [`crate::vaults::lp_farming`] pool.
//! 2. **Single-sided swap** — swap the *optimal* fraction of the reward into
//!    the pool's other token (`pair_token`), so the post-swap holdings match
//!    the pool's post-swap reserve ratio (see [`optimal_swap_amount`]).
//! 3. **Add liquidity** — deposit both legs into the pool, receiving LP.
//! 4. **Re-stake** — stake every LP token received back into the farm.
//! 5. **Invariant** — assert the compounding yield on the caller's position
//!    is strictly positive (see [`assert_positive_compound_yield`]).
//!
//! Unlike [`crate::vaults::harvest_compound`], which relies on the router to
//! convert rewards straight into LP along a caller-supplied path, this
//! strategy performs the zap itself: the swap split is computed on-chain from
//! the pool's live reserves, so only the unavoidable rounding dust is left
//! unpaired — and that dust is returned to the caller, never kept.
//!
//! ## Router integration contract
//!
//! `router` fronts the `reward_token`/`pair_token` constant-product pool whose
//! LP token is the farm's `lp_token`, and must expose:
//!
//! ```text
//! get_reserves(token_a: Address, token_b: Address) -> (i128, i128)
//! swap_exact_tokens_for_tokens(
//!     amount_in: i128, amount_out_min: i128, path: Vec<Address>, to: Address,
//! )
//! add_liquidity(
//!     token_a: Address, token_b: Address, amount_a: i128, amount_b: i128, to: Address,
//! )
//! ```
//!
//! Both mutating calls are transfer-then-call: the vault funds the router
//! *before* invoking it. `add_liquidity` must mint LP to `to` and return any
//! amount it did not deposit to `to`. Return values are ignored.
//!
//! ## Safety
//!
//! The router is caller-supplied and treated as hostile, exactly as in
//! [`crate::vaults::harvest_compound`]:
//!
//! * **Output is measured, never reported.** Every amount the router delivers
//!   is the vault's own `balance()` delta across the call.
//! * **Slippage is enforced locally** against the measured LP delta; the
//!   router is told `amount_out_min = 0`.
//! * **Reserves only steer the split.** A router lying about its reserves can
//!   only make the swap suboptimal, which surfaces as less LP and trips the
//!   caller's `min_lp_out` bound.
//! * **Re-entry is blocked** by the contract-wide reentrancy guard held by the
//!   `lib.rs` wrapper.
//! * **Failure is atomic.** Any error reverts the whole transaction, claim
//!   included, so rewards are never consumed without LP being staked.
//!
//! ## Authorization
//!
//! `user` must authorize the call. It is asserted exactly once, by
//! [`lp_farming::claim_rewards`](crate::vaults::lp_farming::claim_rewards);
//! the re-stake therefore uses
//! [`lp_farming::stake_preauthorized`](crate::vaults::lp_farming::stake_preauthorized).

use soroban_sdk::{contracttype, token, Address, Env, IntoVal, Symbol, Val, U256};

use crate::{amm, vaults, ContractError};

/// Swap fee of the target pool, in basis points (Uniswap-V2 / Soroswap 0.30%).
pub const POOL_FEE_BPS: u64 = amm::invariant::FEE_TIER_0_30_BPS;

/// Upper bound on the reserve and amount fed to [`optimal_swap_amount`].
/// At `2^96` every intermediate term stays below `2^224`, well inside `U256`.
pub const MAX_ZAP_AMOUNT: i128 = 1 << 96;

/// Fixed-point scale of [`StrategyCompoundResult::compound_yield`]:
/// `1_000_000_000` is a 100% gain on the position for this compounding round.
pub const COMPOUND_YIELD_PRECISION: i128 = 1_000_000_000;

/// Action emitted, under [`vaults::autocompound::EVENT_PROTOCOL_TOPIC`], when
/// a strategy round compounds rewards back into staked liquidity.
pub const EVENT_ACTION_STRATEGY_COMPOUND: Symbol = soroban_sdk::symbol_short!("zapcmpnd");

const BPS_DENOMINATOR: u64 = 10_000;

/// Outcome of one [`compound_strategy`] round.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StrategyCompoundResult {
    /// Reward tokens claimed from the farm.
    pub reward_claimed: i128,
    /// Part of the claimed reward swapped into `pair_token`.
    pub reward_swapped: i128,
    /// LP tokens minted by the pool, measured as a balance delta, and staked.
    pub lp_acquired: i128,
    /// The caller's farm share balance after re-staking.
    pub new_share_balance: i128,
    /// Share growth of the caller's position this round, scaled by
    /// [`COMPOUND_YIELD_PRECISION`]. Always strictly positive.
    pub compound_yield: i128,
}

/// Claim, zap, add liquidity, and re-stake in one atomic step. See the module
/// docs for the router integration contract and the trust model.
///
/// # Errors
/// * [`ContractError::VaultPaused`] / [`ContractError::ContractPaused`] — vault frozen.
/// * [`ContractError::VaultNotInitialized`] — no yield farm configured.
/// * [`ContractError::HarvestInvalidMinOut`] — `min_lp_out` is negative.
/// * [`ContractError::StrategyInvalidPairToken`] — `pair_token` is the farm's
///   reward or LP token.
/// * [`ContractError::HarvestNothingToCompound`] — no rewards accrued, or too
///   few to fund both sides of the deposit.
/// * [`ContractError::StrategyInvalidReserves`] — the pool reports an empty reserve.
/// * [`ContractError::HarvestSwapFailed`] — the swap or deposit delivered nothing.
/// * [`ContractError::HarvestSlippageExceeded`] — LP delivered was below `min_lp_out`.
/// * [`ContractError::StrategyYieldNotPositive`] — compounding did not grow the position.
pub fn compound_strategy(
    env: &Env,
    user: Address,
    router: Address,
    pair_token: Address,
    min_lp_out: i128,
) -> Result<StrategyCompoundResult, ContractError> {
    vaults::pause_guard::require_vault_operational(env)?;

    if min_lp_out < 0 {
        return Err(ContractError::HarvestInvalidMinOut);
    }

    let farm = vaults::lp_farming::get_config(env).ok_or(ContractError::VaultNotInitialized)?;
    if pair_token == farm.reward_token || pair_token == farm.lp_token {
        return Err(ContractError::StrategyInvalidPairToken);
    }

    let shares_before = vaults::lp_farming::get_share_balance(env, user.clone());

    // ── 1. Claim ────────────────────────────────────────────────────────────
    // `claim_rewards` asserts `user`'s authorization for this frame and pays
    // the reward to `user`; it is pulled into vault custody right after.
    let reward_claimed = vaults::lp_farming::claim_rewards(env, user.clone())?;
    if reward_claimed <= 0 {
        return Err(ContractError::HarvestNothingToCompound);
    }

    let vault = env.current_contract_address();
    let reward_client = token::Client::new(env, &farm.reward_token);
    let pair_client = token::Client::new(env, &pair_token);
    let lp_client = token::Client::new(env, &farm.lp_token);
    reward_client.transfer(&user, &vault, &reward_claimed);

    // ── 2. Single-sided swap ────────────────────────────────────────────────
    let (reward_reserve, pair_reserve): (i128, i128) = env.invoke_contract(
        &router,
        &Symbol::new(env, "get_reserves"),
        soroban_sdk::vec![
            env,
            farm.reward_token.into_val(env),
            pair_token.into_val(env),
        ],
    );
    if reward_reserve <= 0 || pair_reserve <= 0 {
        return Err(ContractError::StrategyInvalidReserves);
    }

    let reward_swapped = optimal_swap_amount(env, reward_reserve, reward_claimed, POOL_FEE_BPS)?;
    let reward_paired = reward_claimed
        .checked_sub(reward_swapped)
        .ok_or(ContractError::MathOverflow)?;
    if reward_swapped <= 0 || reward_paired <= 0 {
        return Err(ContractError::HarvestNothingToCompound);
    }

    reward_client.transfer(&vault, &router, &reward_swapped);
    let pair_before = pair_client.balance(&vault);
    let _: Val = env.invoke_contract(
        &router,
        &Symbol::new(env, "swap_exact_tokens_for_tokens"),
        soroban_sdk::vec![
            env,
            reward_swapped.into_val(env),
            // Deliberately 0: the binding slippage check is the measured LP
            // delta below, which a hostile router cannot talk its way past.
            0i128.into_val(env),
            soroban_sdk::vec![env, farm.reward_token.clone(), pair_token.clone()].into_val(env),
            vault.clone().into_val(env),
        ],
    );
    let pair_acquired = balance_gain(&pair_client, &vault, pair_before)?;
    if pair_acquired <= 0 {
        return Err(ContractError::HarvestSwapFailed);
    }

    // ── 3. Add liquidity ────────────────────────────────────────────────────
    reward_client.transfer(&vault, &router, &reward_paired);
    pair_client.transfer(&vault, &router, &pair_acquired);
    let reward_before = reward_client.balance(&vault);
    let pair_before = pair_client.balance(&vault);
    let lp_before = lp_client.balance(&vault);
    let _: Val = env.invoke_contract(
        &router,
        &Symbol::new(env, "add_liquidity"),
        soroban_sdk::vec![
            env,
            farm.reward_token.into_val(env),
            pair_token.into_val(env),
            reward_paired.into_val(env),
            pair_acquired.into_val(env),
            vault.clone().into_val(env),
        ],
    );
    let lp_acquired = balance_gain(&lp_client, &vault, lp_before)?;
    if lp_acquired <= 0 {
        return Err(ContractError::HarvestSwapFailed);
    }
    // `min_lp_out` is non-negative (checked above) and `lp_acquired` is
    // positive, so both casts are lossless.
    amm::slippage::enforce_slippage(lp_acquired as u128, min_lp_out as u128)
        .map_err(|_| ContractError::HarvestSlippageExceeded)?;

    // Whatever the pool did not deposit belongs to `user`. Returning it keeps
    // the vault's reward balance equal to the farm's reward pot.
    let reward_dust = balance_gain(&reward_client, &vault, reward_before)?;
    if reward_dust > 0 {
        reward_client.transfer(&vault, &user, &reward_dust);
    }
    let pair_dust = balance_gain(&pair_client, &vault, pair_before)?;
    if pair_dust > 0 {
        pair_client.transfer(&vault, &user, &pair_dust);
    }

    // ── 4. Re-stake ─────────────────────────────────────────────────────────
    lp_client.transfer(&vault, &user, &lp_acquired);
    vaults::lp_farming::stake_preauthorized(env, user.clone(), lp_acquired)?;
    let new_share_balance = vaults::lp_farming::get_share_balance(env, user.clone());

    // ── 5. Yield invariant ──────────────────────────────────────────────────
    let compound_yield = assert_positive_compound_yield(shares_before, new_share_balance)?;

    env.events().publish(
        (
            vaults::autocompound::EVENT_PROTOCOL_TOPIC,
            EVENT_ACTION_STRATEGY_COMPOUND,
        ),
        (
            user,
            reward_claimed,
            reward_swapped,
            lp_acquired,
            new_share_balance,
            compound_yield,
        ),
    );

    Ok(StrategyCompoundResult {
        reward_claimed,
        reward_swapped,
        lp_acquired,
        new_share_balance,
        compound_yield,
    })
}

/// Amount of a single-sided `amount_in` to swap through a constant-product
/// pool holding `reserve_in` of the input token, so that the swap output and
/// the unswapped remainder match the pool's post-swap reserve ratio and can be
/// deposited with no leftover.
///
/// With fee factor `g = 1 - fee`, the closed-form solution is
///
/// ```text
/// s = (sqrt(R²(1 + g)² + 4gRa) - R(1 + g)) / 2g
/// ```
///
/// evaluated here in basis points and 256-bit integers. The result is rounded
/// up: the pool rounds its swap output down, so erring towards the larger
/// swap keeps the paired leg from coming up short.
///
/// # Errors
/// * [`ContractError::InvalidInput`] — non-positive reserve or amount, or a
///   fee of 100% or more.
/// * [`ContractError::MathOverflow`] — reserve or amount above [`MAX_ZAP_AMOUNT`].
pub fn optimal_swap_amount(
    env: &Env,
    reserve_in: i128,
    amount_in: i128,
    fee_bps: u64,
) -> Result<i128, ContractError> {
    if reserve_in <= 0 || amount_in <= 0 || fee_bps >= BPS_DENOMINATOR {
        return Err(ContractError::InvalidInput);
    }
    if reserve_in > MAX_ZAP_AMOUNT || amount_in > MAX_ZAP_AMOUNT {
        return Err(ContractError::MathOverflow);
    }

    let bps = u128::from(BPS_DENOMINATOR);
    let gamma = bps - u128::from(fee_bps);
    let reserve = U256::from_u128(env, reserve_in as u128);
    let amount = U256::from_u128(env, amount_in as u128);

    // b = R(1 + g), x = 4gRa — both scaled by `bps` (squared, for x).
    let b = reserve.mul(&U256::from_u128(env, bps + gamma));
    let x = U256::from_u128(env, 4 * bps * gamma)
        .mul(&reserve)
        .mul(&amount);
    let root = isqrt_above(env, &b.mul(&b).add(&x), &b, &x);
    let denominator = 2 * gamma;

    root.sub(&b)
        .add(&U256::from_u128(env, denominator - 1))
        .div(&U256::from_u128(env, denominator))
        .to_u128()
        .and_then(|s| i128::try_from(s).ok())
        .ok_or(ContractError::MathOverflow)
}

/// Floor square root of `n = b² + x` by Newton's method, seeded from the upper
/// bound `sqrt(b² + x) <= b + x / 2b`, which converges in a handful of steps.
/// `b` must be non-zero.
fn isqrt_above(env: &Env, n: &U256, b: &U256, x: &U256) -> U256 {
    let mut root = b.add(&x.div(&b.shl(1))).add(&U256::from_u32(env, 1));
    loop {
        let next = root.add(&n.div(&root)).shr(1);
        if next >= root {
            return root;
        }
        root = next;
    }
}

/// The APY compounding invariant.
///
/// A compounding round's annualized yield is `(1 + y)^n - 1` for the round's
/// share growth `y` and a positive number of rounds `n`, so it is strictly
/// positive exactly when `y` is. Returns `y` scaled by
/// [`COMPOUND_YIELD_PRECISION`].
///
/// # Errors
/// * [`ContractError::StrategyYieldNotPositive`] — the position did not grow,
///   or grew by less than one unit of precision, or there was no prior
///   position to measure a yield against.
pub fn assert_positive_compound_yield(
    shares_before: i128,
    shares_after: i128,
) -> Result<i128, ContractError> {
    if shares_before <= 0 {
        return Err(ContractError::StrategyYieldNotPositive);
    }
    let compound_yield = shares_after
        .checked_sub(shares_before)
        .ok_or(ContractError::MathOverflow)?
        .checked_mul(COMPOUND_YIELD_PRECISION)
        .ok_or(ContractError::MathOverflow)?
        / shares_before;
    if compound_yield <= 0 {
        return Err(ContractError::StrategyYieldNotPositive);
    }
    Ok(compound_yield)
}

/// Tokens `holder` gained since its balance was `before`.
fn balance_gain(
    client: &token::Client,
    holder: &Address,
    before: i128,
) -> Result<i128, ContractError> {
    client
        .balance(holder)
        .checked_sub(before)
        .ok_or(ContractError::MathOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Events, Ledger};
    use soroban_sdk::{contract, contractimpl, symbol_short, TryFromVal, Vec};

    // ── Test double for the external pool router ────────────────────────────

    const REWARD: Symbol = symbol_short!("REWARD");
    const PAIR: Symbol = symbol_short!("PAIR");
    const LP: Symbol = symbol_short!("LP");
    const RES_RWD: Symbol = symbol_short!("RES_RWD");
    const RES_PAIR: Symbol = symbol_short!("RES_PAIR");
    const SUPPLY: Symbol = symbol_short!("SUPPLY");

    /// Minimal Uniswap-V2-style `reward`/`pair` pool: constant-product swaps at
    /// [`POOL_FEE_BPS`], ratio-matched deposits, and refunds of whatever a
    /// deposit could not use. It pays out of floats minted to it in setup.
    #[contract]
    pub struct MockPool;

    fn read(env: &Env, key: &Symbol) -> i128 {
        env.storage().instance().get(key).unwrap()
    }

    #[contractimpl]
    impl MockPool {
        pub fn configure(
            env: Env,
            reward: Address,
            pair: Address,
            lp: Address,
            reward_reserve: i128,
            pair_reserve: i128,
            lp_supply: i128,
        ) {
            let s = env.storage().instance();
            s.set(&REWARD, &reward);
            s.set(&PAIR, &pair);
            s.set(&LP, &lp);
            s.set(&RES_RWD, &reward_reserve);
            s.set(&RES_PAIR, &pair_reserve);
            s.set(&SUPPLY, &lp_supply);
        }

        pub fn get_reserves(env: Env, token_a: Address, _token_b: Address) -> (i128, i128) {
            let (rr, pr) = (read(&env, &RES_RWD), read(&env, &RES_PAIR));
            let reward: Address = env.storage().instance().get(&REWARD).unwrap();
            if token_a == reward {
                (rr, pr)
            } else {
                (pr, rr)
            }
        }

        /// Swaps `reward` for `pair` only — the direction the strategy uses.
        pub fn swap_exact_tokens_for_tokens(
            env: Env,
            amount_in: i128,
            _amount_out_min: i128,
            _path: Vec<Address>,
            to: Address,
        ) -> Vec<i128> {
            let (rr, pr) = (read(&env, &RES_RWD), read(&env, &RES_PAIR));
            let out = swap_out(amount_in, rr, pr);
            env.storage().instance().set(&RES_RWD, &(rr + amount_in));
            env.storage().instance().set(&RES_PAIR, &(pr - out));
            let pair: Address = env.storage().instance().get(&PAIR).unwrap();
            token::Client::new(&env, &pair).transfer(&env.current_contract_address(), &to, &out);
            soroban_sdk::vec![&env, amount_in, out]
        }

        /// Pre-funded deposit: uses the ratio-matched part of both amounts,
        /// mints LP pro rata, and refunds the rest to `to`.
        pub fn add_liquidity(
            env: Env,
            _token_a: Address,
            _token_b: Address,
            amount_a: i128,
            amount_b: i128,
            to: Address,
        ) -> i128 {
            let (rr, pr, supply) = (
                read(&env, &RES_RWD),
                read(&env, &RES_PAIR),
                read(&env, &SUPPLY),
            );
            let (used_a, used_b, minted) = deposit(amount_a, amount_b, rr, pr, supply);
            let s = env.storage().instance();
            s.set(&RES_RWD, &(rr + used_a));
            s.set(&RES_PAIR, &(pr + used_b));
            s.set(&SUPPLY, &(supply + minted));

            let me = env.current_contract_address();
            let lp: Address = s.get(&LP).unwrap();
            let reward: Address = s.get(&REWARD).unwrap();
            let pair: Address = s.get(&PAIR).unwrap();
            token::Client::new(&env, &lp).transfer(&me, &to, &minted);
            if amount_a > used_a {
                token::Client::new(&env, &reward).transfer(&me, &to, &(amount_a - used_a));
            }
            if amount_b > used_b {
                token::Client::new(&env, &pair).transfer(&me, &to, &(amount_b - used_b));
            }
            minted
        }
    }

    /// Output of a constant-product swap at [`POOL_FEE_BPS`].
    fn swap_out(amount_in: i128, reserve_in: i128, reserve_out: i128) -> i128 {
        let in_after_fee = amount_in * (BPS_DENOMINATOR - POOL_FEE_BPS) as i128;
        in_after_fee * reserve_out / (reserve_in * BPS_DENOMINATOR as i128 + in_after_fee)
    }

    /// Ratio-matched pool deposit: returns `(used_a, used_b, lp_minted)`.
    fn deposit(amount_a: i128, amount_b: i128, ra: i128, rb: i128, supply: i128) -> (i128, i128, i128) {
        let b_matched = amount_a * rb / ra;
        let (used_a, used_b) = if b_matched <= amount_b {
            (amount_a, b_matched)
        } else {
            (amount_b * ra / rb, amount_b)
        };
        (used_a, used_b, (used_a * supply / ra).min(used_b * supply / rb))
    }

    /// LP minted by zapping `amount` of the reward into a balanced pool of
    /// `depth` per side, swapping `swapped` of it first.
    fn zap_lp(swapped: i128, amount: i128, depth: i128) -> i128 {
        let out = swap_out(swapped, depth, depth);
        deposit(amount - swapped, out, depth + swapped, depth - out, depth).2
    }

    // ── Fixture ─────────────────────────────────────────────────────────────

    /// Staked LP, chosen so `acc_reward_per_share` math stays exact.
    const STAKE: i128 = 1_000_000;
    /// Farm emission per ledger at the default 1x multiplier.
    const EMISSION: i128 = 100;
    /// Ledgers advanced before compounding: `EMISSION * LEDGERS` rewards accrue.
    const LEDGERS: u32 = 10;
    /// Rewards the single staker is owed after `LEDGERS` ledgers.
    const EXPECTED_REWARD: i128 = EMISSION * LEDGERS as i128; // 1_000
    /// Reward tokens held by the farm's reward pot.
    const REWARD_POT: i128 = EXPECTED_REWARD * 10;
    /// Depth of each side of the mock pool, and its LP supply.
    const POOL_DEPTH: i128 = 1_000_000;

    struct Fixture {
        env: Env,
        client: crate::TimeLockedUpgradeContractClient<'static>,
        contract_id: Address,
        admin: Address,
        user: Address,
        lp_token: Address,
        reward_token: Address,
        pair_token: Address,
    }

    impl Fixture {
        /// Register a [`MockPool`] with the given reserves, funded with enough
        /// of the pair and LP tokens to settle any swap or deposit.
        fn pool(&self, reward_reserve: i128, pair_reserve: i128) -> Address {
            let pool = self.env.register_contract(None, MockPool);
            MockPoolClient::new(&self.env, &pool).configure(
                &self.reward_token,
                &self.pair_token,
                &self.lp_token,
                &reward_reserve,
                &pair_reserve,
                &POOL_DEPTH,
            );
            mint(&self.env, &self.pair_token, &pool, POOL_DEPTH);
            mint(&self.env, &self.lp_token, &pool, POOL_DEPTH);
            pool
        }

        fn balance(&self, asset: &Address, holder: &Address) -> i128 {
            token::Client::new(&self.env, asset).balance(holder)
        }
    }

    fn mint(env: &Env, asset: &Address, to: &Address, amount: i128) {
        token::StellarAssetClient::new(env, asset).mint(to, &amount);
    }

    /// Edits the live `LedgerInfo` rather than building a fresh one, which
    /// would reset the entry-TTL fields and break later token calls on the
    /// pinned soroban-sdk 20.0.0 test harness.
    fn advance_ledgers(env: &Env, count: u32) {
        let mut info = env.ledger().get();
        info.sequence_number += count;
        info.timestamp += 5 * count as u64;
        env.ledger().set(info);
    }

    /// Farm initialized, `user` fully staked, `LEDGERS` elapsed so exactly
    /// `EXPECTED_REWARD` is claimable, and the reward pot funded.
    fn setup() -> Fixture {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let client = crate::TimeLockedUpgradeContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        client.initialize(&admin, &treasury);

        let issuer = Address::generate(&env);
        let lp_token = env.register_stellar_asset_contract(issuer.clone());
        let reward_token = env.register_stellar_asset_contract(issuer.clone());
        let pair_token = env.register_stellar_asset_contract(issuer);

        client.init_yield_farming(&admin, &lp_token, &reward_token, &EMISSION);

        let user = Address::generate(&env);
        mint(&env, &lp_token, &user, STAKE);
        client.stake_lp(&user, &STAKE);

        advance_ledgers(&env, LEDGERS);

        let funder = Address::generate(&env);
        mint(&env, &reward_token, &funder, REWARD_POT);
        client.fund_yield_rewards(&funder, &REWARD_POT);

        Fixture {
            env,
            client,
            contract_id,
            admin,
            user,
            lp_token,
            reward_token,
            pair_token,
        }
    }

    // ── Happy path ──────────────────────────────────────────────────────────

    #[test]
    fn compounds_rewards_into_additional_staked_lp() {
        let f = setup();
        let pool = f.pool(POOL_DEPTH, POOL_DEPTH);

        let result = f
            .client
            .compound_yield_strategy(&f.user, &pool, &f.pair_token, &0);

        assert_eq!(result.reward_claimed, EXPECTED_REWARD);
        assert_eq!(
            result.reward_swapped,
            optimal_swap_amount(&f.env, POOL_DEPTH, EXPECTED_REWARD, POOL_FEE_BPS).unwrap()
        );
        assert!(result.lp_acquired > 0);
        assert_eq!(result.new_share_balance, STAKE + result.lp_acquired);
        assert_eq!(
            f.client.yield_farming_share_balance(&f.user),
            result.new_share_balance
        );
        assert_eq!(
            result.compound_yield,
            result.lp_acquired * COMPOUND_YIELD_PRECISION / STAKE
        );
        assert!(result.compound_yield > 0);
    }

    #[test]
    fn optimal_split_leaves_at_most_rounding_dust_unpaired() {
        let f = setup();
        let pool = f.pool(POOL_DEPTH, POOL_DEPTH);
        f.client
            .compound_yield_strategy(&f.user, &pool, &f.pair_token, &0);

        // Unpaired leftovers go back to the caller; with the optimal split
        // they are bounded by integer rounding, not by a lopsided swap.
        assert!(f.balance(&f.reward_token, &f.user) <= 2);
        assert!(f.balance(&f.pair_token, &f.user) <= 2);
        assert_eq!(f.balance(&f.lp_token, &f.user), 0);
    }

    #[test]
    fn vault_keeps_nothing_beyond_the_remaining_reward_pot() {
        let f = setup();
        let pool = f.pool(POOL_DEPTH, POOL_DEPTH);
        f.client
            .compound_yield_strategy(&f.user, &pool, &f.pair_token, &0);

        assert_eq!(
            f.balance(&f.reward_token, &f.contract_id),
            REWARD_POT - EXPECTED_REWARD
        );
        assert_eq!(f.balance(&f.pair_token, &f.contract_id), 0);
        assert_eq!(
            f.balance(&f.lp_token, &f.contract_id),
            f.client.yield_farming_share_balance(&f.user)
        );
    }

    #[test]
    fn compound_emits_strategy_event() {
        let f = setup();
        let pool = f.pool(POOL_DEPTH, POOL_DEPTH);
        let result = f
            .client
            .compound_yield_strategy(&f.user, &pool, &f.pair_token, &0);

        let expected_topics: Vec<Val> = soroban_sdk::vec![
            &f.env,
            vaults::autocompound::EVENT_PROTOCOL_TOPIC.into_val(&f.env),
            EVENT_ACTION_STRATEGY_COMPOUND.into_val(&f.env),
        ];
        let event = f
            .env
            .events()
            .all()
            .iter()
            .find(|e| e.0 == f.contract_id && e.1 == expected_topics)
            .expect("expected a strategy compound event");
        let payload =
            <(Address, i128, i128, i128, i128, i128)>::try_from_val(&f.env, &event.2).unwrap();
        assert_eq!(
            payload,
            (
                f.user.clone(),
                result.reward_claimed,
                result.reward_swapped,
                result.lp_acquired,
                result.new_share_balance,
                result.compound_yield,
            )
        );
    }

    // ── Rejections ──────────────────────────────────────────────────────────

    #[test]
    fn rejects_negative_min_lp_out() {
        let f = setup();
        let pool = f.pool(POOL_DEPTH, POOL_DEPTH);
        let result = f
            .client
            .try_compound_yield_strategy(&f.user, &pool, &f.pair_token, &-1);
        assert_eq!(result, Err(Ok(ContractError::HarvestInvalidMinOut)));
    }

    #[test]
    fn rejects_pair_token_equal_to_a_farm_token() {
        let f = setup();
        let pool = f.pool(POOL_DEPTH, POOL_DEPTH);
        for pair in [f.reward_token.clone(), f.lp_token.clone()] {
            let result = f
                .client
                .try_compound_yield_strategy(&f.user, &pool, &pair, &0);
            assert_eq!(result, Err(Ok(ContractError::StrategyInvalidPairToken)));
        }
    }

    #[test]
    fn rejects_when_no_rewards_have_accrued() {
        let f = setup();
        let pool = f.pool(POOL_DEPTH, POOL_DEPTH);
        f.client.claim_rewards(&f.user);

        let result = f
            .client
            .try_compound_yield_strategy(&f.user, &pool, &f.pair_token, &0);
        assert_eq!(result, Err(Ok(ContractError::HarvestNothingToCompound)));
    }

    #[test]
    fn rejects_an_empty_pool() {
        let f = setup();
        let pool = f.pool(0, POOL_DEPTH);
        let result = f
            .client
            .try_compound_yield_strategy(&f.user, &pool, &f.pair_token, &0);
        assert_eq!(result, Err(Ok(ContractError::StrategyInvalidReserves)));
    }

    #[test]
    fn rejects_lp_output_below_min_lp_out() {
        let f = setup();
        let pool = f.pool(POOL_DEPTH, POOL_DEPTH);
        let result = f
            .client
            .try_compound_yield_strategy(&f.user, &pool, &f.pair_token, &EXPECTED_REWARD);
        assert_eq!(result, Err(Ok(ContractError::HarvestSlippageExceeded)));
    }

    #[test]
    fn rejects_while_the_vault_is_paused() {
        let f = setup();
        let pool = f.pool(POOL_DEPTH, POOL_DEPTH);
        f.client.pause_vault(&f.admin);

        let result = f
            .client
            .try_compound_yield_strategy(&f.user, &pool, &f.pair_token, &0);
        assert_eq!(result, Err(Ok(ContractError::VaultPaused)));
    }

    /// The lock is taken directly rather than by a re-entering router: on the
    /// soroban-sdk 20.0.0 native test harness a sub-invocation returning `Err`
    /// aborts the test process, so a live re-entry cannot be observed.
    #[test]
    fn rejects_entry_while_the_reentrancy_lock_is_held() {
        let f = setup();
        let pool = f.pool(POOL_DEPTH, POOL_DEPTH);
        f.env.as_contract(&f.contract_id, || {
            crate::security::reentrancy::lock(&f.env).expect("lock should be free");
        });

        let result = f
            .client
            .try_compound_yield_strategy(&f.user, &pool, &f.pair_token, &0);
        assert_eq!(result, Err(Ok(ContractError::ReentrancyDetected)));
    }

    // ── Atomicity ───────────────────────────────────────────────────────────

    #[test]
    fn failed_compound_leaves_the_reward_claimable() {
        let f = setup();
        let pool = f.pool(POOL_DEPTH, POOL_DEPTH);
        assert!(f
            .client
            .try_compound_yield_strategy(&f.user, &pool, &f.pair_token, &EXPECTED_REWARD)
            .is_err());

        assert_eq!(f.client.pending_yield_rewards(&f.user), EXPECTED_REWARD);
        assert_eq!(f.client.yield_farming_share_balance(&f.user), STAKE);
        assert_eq!(f.balance(&f.reward_token, &f.user), 0);
    }

    // ── Optimal swap math ───────────────────────────────────────────────────

    /// Brute force: the computed split mints the most LP of any split.
    #[test]
    fn optimal_swap_mints_the_most_lp_of_any_split() {
        let env = Env::default();
        for (depth, amount) in [(1_000_000i128, 1_000i128), (1_000_000, 500_000)] {
            let s = optimal_swap_amount(&env, depth, amount, POOL_FEE_BPS).unwrap();
            let best = (1..amount).map(|x| zap_lp(x, amount, depth)).max().unwrap();
            assert_eq!(zap_lp(s, amount, depth), best, "depth={depth} amount={amount} s={s}");
        }
    }

    /// A zap twenty times the pool's depth hits integer-rounding plateaus in
    /// the pool itself, yet still lands within 0.02% of the best split.
    #[test]
    fn optimal_swap_stays_near_best_for_zaps_dwarfing_the_pool() {
        let env = Env::default();
        let (depth, amount) = (5_000i128, 100_000i128);
        let s = optimal_swap_amount(&env, depth, amount, POOL_FEE_BPS).unwrap();
        let best = (1..amount).map(|x| zap_lp(x, amount, depth)).max().unwrap();
        let got = zap_lp(s, amount, depth);
        assert!(got * 10_000 >= best * 9_998, "s={s}: minted {got}, best {best}");
    }

    #[test]
    fn optimal_swap_tends_to_half_for_small_amounts() {
        let env = Env::default();
        // The fee pushes a small zap's swap just above half (≈ 500.6)...
        assert_eq!(optimal_swap_amount(&env, 1_000_000, 1_000, POOL_FEE_BPS), Ok(501));
        // ...while price impact on a pool-sized zap pulls it well below, towards
        // R(√2 − 1) ≈ 0.414·R.
        let s = optimal_swap_amount(&env, 1_000_000, 1_000_000, POOL_FEE_BPS).unwrap();
        assert!((414_000..416_000).contains(&s), "s={s}");
        // Fee-free against a near-infinite pool it is exactly half.
        assert_eq!(optimal_swap_amount(&env, MAX_ZAP_AMOUNT, 1_000, 0), Ok(500));
    }

    #[test]
    fn optimal_swap_handles_the_largest_supported_amounts() {
        let env = Env::default();
        let s = optimal_swap_amount(&env, MAX_ZAP_AMOUNT, MAX_ZAP_AMOUNT, POOL_FEE_BPS).unwrap();
        // ≈ (√2 − 1)·a, slightly more to cover the fee.
        assert!(s > MAX_ZAP_AMOUNT / 1_000 * 414 && s < MAX_ZAP_AMOUNT / 1_000 * 416, "s={s}");
    }

    #[test]
    fn optimal_swap_rejects_invalid_inputs() {
        let env = Env::default();
        let invalid = Err(ContractError::InvalidInput);
        assert_eq!(optimal_swap_amount(&env, 0, 1, POOL_FEE_BPS), invalid);
        assert_eq!(optimal_swap_amount(&env, 1, 0, POOL_FEE_BPS), invalid);
        assert_eq!(optimal_swap_amount(&env, 1, 1, BPS_DENOMINATOR), invalid);
        assert_eq!(
            optimal_swap_amount(&env, MAX_ZAP_AMOUNT + 1, 1, POOL_FEE_BPS),
            Err(ContractError::MathOverflow)
        );
    }

    // ── Yield invariant ─────────────────────────────────────────────────────

    #[test]
    fn compound_yield_is_the_scaled_share_growth() {
        assert_eq!(
            assert_positive_compound_yield(1_000_000, 1_000_500),
            Ok(COMPOUND_YIELD_PRECISION / 2_000)
        );
    }

    #[test]
    fn compound_yield_rejects_flat_or_shrinking_positions() {
        let not_positive = Err(ContractError::StrategyYieldNotPositive);
        assert_eq!(assert_positive_compound_yield(1_000, 1_000), not_positive);
        assert_eq!(assert_positive_compound_yield(1_000, 999), not_positive);
        assert_eq!(assert_positive_compound_yield(0, 1_000), not_positive);
        // Growth below one unit of precision rounds to zero and is rejected.
        assert_eq!(
            assert_positive_compound_yield(
                COMPOUND_YIELD_PRECISION * 2,
                COMPOUND_YIELD_PRECISION * 2 + 1
            ),
            not_positive
        );
    }
}
