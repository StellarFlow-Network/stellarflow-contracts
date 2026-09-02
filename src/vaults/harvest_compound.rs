//! Yield-farm harvest → swap → re-stake auto-router (Issue #798).
//!
//! [`harvest_and_compound`] is a single atomic vault entrypoint that turns a
//! farmer's *accrued but unstaked* reward tokens into *additional staked LP*,
//! in one transaction:
//!
//! 1. **Claim** — settle and withdraw the caller's pending emissions from the
//!    [`crate::vaults::lp_farming`] pool (`reward_token`).
//! 2. **Swap** — route that reward amount through an external DEX router along
//!    a caller-supplied `path` that ends at the farm's `lp_token`.
//! 3. **Re-stake** — deposit every LP token the swap produced back into the
//!    same farm, raising the caller's share balance and therefore their
//!    forward emission rate. That compounding is the whole point: rewards that
//!    would have sat idle now earn emissions themselves.
//!
//! ## Why the route is caller-supplied
//!
//! There is no on-chain AMM in this contract, and the best venue for a
//! `reward_token → lp_token` conversion changes block to block. Rather than
//! pin a router into storage, every call names its own `router` and `path`, so
//! an off-chain keeper can quote the market and hand in the best route it
//! found — the "auto-router" of the issue title. Nothing about that is trusted:
//! see the safety notes below.
//!
//! ## Router integration contract
//!
//! `router` must expose:
//!
//! ```text
//! swap_exact_tokens_for_tokens(
//!     amount_in: i128, amount_out_min: i128, path: Vec<Address>, to: Address,
//! )
//! ```
//!
//! This is the Uniswap-V2 / Soroswap signature. The vault transfers `amount_in`
//! of `path[0]` to the router *before* the call (transfer-then-call), and the
//! router must deliver the output of `path[last]` to `to`. Any return value is
//! ignored.
//!
//! ## Safety
//!
//! The router is arbitrary caller-supplied code, so it is treated as hostile:
//!
//! * **Output is measured, never reported.** `lp_acquired` is the vault's own
//!   `balance()` delta across the swap, not the router's return value. A router
//!   that claims to have paid out cannot make the vault credit LP it did not
//!   actually receive.
//! * **Slippage is enforced locally.** `amount_out_min` is passed to the router
//!   as `0` and the real check runs here against the measured delta, so a
//!   router cannot satisfy the bound by lying about it. The caller's
//!   `min_lp_out` is the only bound that binds.
//! * **Re-entry is blocked.** The wrapper in `lib.rs` holds the contract-wide
//!   [`crate::security::reentrancy::ReentrancyGuard`] for the duration, so a
//!   router that calls back into the vault mid-swap aborts the transaction.
//! * **Failure is atomic.** If the swap under-delivers, the whole transaction
//!   reverts — including step 1 — so the caller's rewards are never consumed
//!   without LP coming back. They stay claimable and the call can be retried
//!   with a different route.
//!
//! ## Authorization
//!
//! This is not a permissionless keeper entry point: it moves `user`'s rewards
//! and stakes on their behalf, so `user` must authorize the call.
//!
//! That authorization is asserted exactly once. Soroban permits a single
//! `require_auth` per address per invocation frame — a second one aborts the
//! host rather than returning an error — and this function runs
//! [`lp_farming::claim_rewards`](crate::vaults::lp_farming::claim_rewards),
//! which already requires it. Everything downstream therefore uses the
//! pre-authorized variants (see
//! [`lp_farming::stake_preauthorized`](crate::vaults::lp_farming::stake_preauthorized)).

use soroban_sdk::{contracttype, token, Address, Env, IntoVal, Symbol, Val, Vec};

use crate::{amm, vaults, ContractError};

/// A swap path must name at least an input and an output token.
pub const MIN_SWAP_PATH_LEN: u32 = 2;

/// Upper bound on hops in a caller-supplied route. Routes longer than this are
/// rejected outright rather than forwarded to the router, which keeps the
/// unbounded `Vec` argument from being used to inflate the call's footprint.
pub const MAX_SWAP_PATH_LEN: u32 = 8;

/// Outcome of one [`harvest_and_compound`] round trip.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HarvestCompoundResult {
    /// Reward tokens claimed from the farm and fed into the swap.
    pub reward_claimed: i128,
    /// LP tokens the swap actually delivered, measured as a balance delta.
    pub lp_acquired: i128,
    /// The caller's farm share balance after the newly acquired LP was staked.
    pub new_share_balance: i128,
}

/// Claim, swap, and re-stake in one atomic step. See the module docs for the
/// router integration contract and the trust model.
///
/// # Errors
/// * [`ContractError::VaultPaused`] / [`ContractError::ContractPaused`] — vault frozen.
/// * [`ContractError::VaultNotInitialized`] — no yield farm configured.
/// * [`ContractError::HarvestInvalidPath`] — `path` does not run `reward_token → lp_token`,
///   or its length is outside [`MIN_SWAP_PATH_LEN`]..=[`MAX_SWAP_PATH_LEN`].
/// * [`ContractError::HarvestInvalidMinOut`] — `min_lp_out` is negative.
/// * [`ContractError::HarvestNothingToCompound`] — no rewards have accrued yet.
/// * [`ContractError::HarvestSwapFailed`] — the router delivered no LP at all.
/// * [`ContractError::HarvestSlippageExceeded`] — LP delivered was below `min_lp_out`.
pub fn harvest_and_compound(
    env: &Env,
    user: Address,
    router: Address,
    path: Vec<Address>,
    min_lp_out: i128,
) -> Result<HarvestCompoundResult, ContractError> {
    // Deliberately no `user.require_auth()` here: `lp_farming::claim_rewards`
    // (step 1) already demands it, and calling `require_auth` twice for the
    // same address within a single invocation frame aborts the host rather
    // than returning an error. The caller's authorization is still mandatory —
    // it is just asserted exactly once, by the first delegate that needs it.
    vaults::pause_guard::require_vault_operational(env)?;

    if min_lp_out < 0 {
        return Err(ContractError::HarvestInvalidMinOut);
    }

    let farm = vaults::lp_farming::get_config(env).ok_or(ContractError::VaultNotInitialized)?;
    validate_path(&path, &farm)?;

    // ── 1. Claim ────────────────────────────────────────────────────────────
    // `claim_rewards` settles the pool and pays the caller directly, so the
    // reward lands in `user`'s balance and is pulled back into vault custody
    // below. Routing it through the caller keeps `lp_farming` untouched.
    let reward_claimed = vaults::lp_farming::claim_rewards(env, user.clone())?;
    if reward_claimed <= 0 {
        return Err(ContractError::HarvestNothingToCompound);
    }

    let vault = env.current_contract_address();
    let reward_client = token::Client::new(env, &farm.reward_token);
    let lp_client = token::Client::new(env, &farm.lp_token);

    // ── 2. Swap ─────────────────────────────────────────────────────────────
    // Transfer-then-call: the router is funded first, then invoked. Measure the
    // LP balance *after* funding so the delta captures the swap alone and is
    // unaffected by LP already staked in the vault by other farmers.
    reward_client.transfer(&user, &vault, &reward_claimed);
    reward_client.transfer(&vault, &router, &reward_claimed);

    let lp_before = lp_client.balance(&vault);
    let _: Val = env.invoke_contract(
        &router,
        &Symbol::new(env, "swap_exact_tokens_for_tokens"),
        soroban_sdk::vec![
            env,
            reward_claimed.into_val(env),
            // Deliberately 0: the binding slippage check is the measured one
            // below, which a hostile router cannot talk its way past.
            0i128.into_val(env),
            path.into_val(env),
            vault.clone().into_val(env),
        ],
    );
    let lp_after = lp_client.balance(&vault);

    let lp_acquired = lp_after
        .checked_sub(lp_before)
        .ok_or(ContractError::MathOverflow)?;
    if lp_acquired <= 0 {
        return Err(ContractError::HarvestSwapFailed);
    }
    // `min_lp_out` is non-negative (checked above) and `lp_acquired` is
    // positive, so both casts are lossless.
    amm::slippage::enforce_slippage(lp_acquired as u128, min_lp_out as u128)
        .map_err(|_| ContractError::HarvestSlippageExceeded)?;

    // ── 3. Re-stake ─────────────────────────────────────────────────────────
    // `lp_farming::stake` pulls LP from the staker, so hand the swap proceeds
    // to `user` first and let the farm draw them straight back in.
    lp_client.transfer(&vault, &user, &lp_acquired);
    // `stake_preauthorized`, not `stake`: step 1's `claim_rewards` already
    // asserted `user`'s authorization for this frame.
    vaults::lp_farming::stake_preauthorized(env, user.clone(), lp_acquired)?;
    let new_share_balance = vaults::lp_farming::get_share_balance(env, user.clone());

    env.events().publish(
        (soroban_sdk::symbol_short!("hrvcmpnd"), user),
        (reward_claimed, lp_acquired, new_share_balance),
    );

    Ok(HarvestCompoundResult {
        reward_claimed,
        lp_acquired,
        new_share_balance,
    })
}

/// A route is only usable if it converts exactly the farm's reward token into
/// exactly the farm's LP token; anything else would swap the wrong asset or
/// deliver something the farm cannot stake.
fn validate_path(
    path: &Vec<Address>,
    farm: &vaults::lp_farming::FarmingConfig,
) -> Result<(), ContractError> {
    let len = path.len();
    if !(MIN_SWAP_PATH_LEN..=MAX_SWAP_PATH_LEN).contains(&len) {
        return Err(ContractError::HarvestInvalidPath);
    }
    if path.get(0) != Some(farm.reward_token.clone())
        || path.get(len - 1) != Some(farm.lp_token.clone())
    {
        return Err(ContractError::HarvestInvalidPath);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Events, Ledger};
    use soroban_sdk::{contract, contractimpl, symbol_short, IntoVal, TryFromVal};

    // ── Test doubles for the external DEX router ────────────────────────────

    const OUT_TOKEN: soroban_sdk::Symbol = symbol_short!("OUT");
    const RATE_NUM: soroban_sdk::Symbol = symbol_short!("NUM");
    const RATE_DEN: soroban_sdk::Symbol = symbol_short!("DEN");

    /// Stand-in for the external DEX router: pays out `amount_in * num / den`
    /// of `out_token` from its own float, then *lies* about the amount by
    /// reporting `i128::MAX`. Every assertion on `lp_acquired` therefore proves
    /// the vault trusted its measured balance delta and not this number.
    #[contract]
    pub struct MockRouter;

    #[contractimpl]
    impl MockRouter {
        pub fn configure(env: Env, out_token: Address, num: i128, den: i128) {
            env.storage().instance().set(&OUT_TOKEN, &out_token);
            env.storage().instance().set(&RATE_NUM, &num);
            env.storage().instance().set(&RATE_DEN, &den);
        }

        pub fn swap_exact_tokens_for_tokens(
            env: Env,
            amount_in: i128,
            _amount_out_min: i128,
            _path: Vec<Address>,
            to: Address,
        ) -> Vec<i128> {
            let out: Address = env.storage().instance().get(&OUT_TOKEN).unwrap();
            let num: i128 = env.storage().instance().get(&RATE_NUM).unwrap();
            let den: i128 = env.storage().instance().get(&RATE_DEN).unwrap();
            let amount_out = amount_in * num / den;
            if amount_out > 0 {
                token::Client::new(&env, &out).transfer(
                    &env.current_contract_address(),
                    &to,
                    &amount_out,
                );
            }
            soroban_sdk::vec![&env, amount_in, i128::MAX]
        }
    }

    // ── Fixture ─────────────────────────────────────────────────────────────

    /// Staked LP, chosen so `acc_reward_per_share` math stays exact.
    const STAKE: i128 = 1_000_000;
    /// Farm emission per ledger at the default 1x multiplier.
    const EMISSION: i128 = 100;
    /// Ledgers advanced before harvesting: `EMISSION * LEDGERS` rewards accrue.
    const LEDGERS: u32 = 10;
    /// Rewards the single staker is owed after `LEDGERS` ledgers.
    const EXPECTED_REWARD: i128 = EMISSION * LEDGERS as i128; // 1_000

    struct Fixture {
        env: Env,
        client: crate::TimeLockedUpgradeContractClient<'static>,
        contract_id: Address,
        admin: Address,
        user: Address,
        lp_token: Address,
        reward_token: Address,
    }

    impl Fixture {
        /// Route that actually converts the farm's reward token into its LP
        /// token: the only shape `validate_path` accepts.
        fn path(&self) -> Vec<Address> {
            soroban_sdk::vec![
                &self.env,
                self.reward_token.clone(),
                self.lp_token.clone()
            ]
        }

        /// Register a [`MockRouter`] paying `num/den`, pre-funded with enough
        /// LP to settle the swap.
        fn router(&self, num: i128, den: i128) -> Address {
            let router = self.env.register_contract(None, MockRouter);
            MockRouterClient::new(&self.env, &router).configure(&self.lp_token, &num, &den);
            mint(&self.env, &self.lp_token, &router, STAKE);
            router
        }
    }

    fn mint(env: &Env, asset: &Address, to: &Address, amount: i128) {
        token::StellarAssetClient::new(env, asset).mint(to, &amount);
    }

    /// Advance the ledger by `count` sequence numbers.
    ///
    /// Reads the current `LedgerInfo` and edits it rather than constructing a
    /// fresh one: a literal `LedgerInfo { .. }` also resets the entry-TTL
    /// fields, and the mismatch against entries already written (the
    /// Stellar-asset contracts registered during setup) makes every later
    /// token call fail with `Error(Context, InternalError)` in this pinned
    /// soroban-sdk 20.0.0 test harness.
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
        let reward_token = env.register_stellar_asset_contract(issuer);

        client.init_yield_farming(&admin, &lp_token, &reward_token, &EMISSION);

        let user = Address::generate(&env);
        mint(&env, &lp_token, &user, STAKE);
        client.stake_lp(&user, &STAKE);

        advance_ledgers(&env, LEDGERS);

        // Fund the reward pot so `claim_rewards` has something to pay out.
        let funder = Address::generate(&env);
        mint(&env, &reward_token, &funder, EXPECTED_REWARD * 10);
        client.fund_yield_rewards(&funder, &(EXPECTED_REWARD * 10));

        Fixture {
            env,
            client,
            contract_id,
            admin,
            user,
            lp_token,
            reward_token,
        }
    }

    // ── Happy path ──────────────────────────────────────────────────────────

    #[test]
    fn compounds_rewards_into_additional_staked_lp() {
        let f = setup();
        assert_eq!(
            f.client.pending_yield_rewards(&f.user),
            EXPECTED_REWARD,
            "fixture should have accrued exactly EXPECTED_REWARD"
        );

        // 1 reward token buys 2 LP tokens.
        let router = f.router(2, 1);
        let result = f
            .client
            .harvest_and_compound(&f.user, &router, &f.path(), &(EXPECTED_REWARD * 2));

        assert_eq!(result.reward_claimed, EXPECTED_REWARD);
        // Measured, not the `i128::MAX` the router reported.
        assert_eq!(result.lp_acquired, EXPECTED_REWARD * 2);
        assert_eq!(result.new_share_balance, STAKE + EXPECTED_REWARD * 2);
        assert_eq!(
            f.client.yield_farming_share_balance(&f.user),
            STAKE + EXPECTED_REWARD * 2,
            "the swap proceeds must end up staked, not sitting with the user"
        );
    }

    #[test]
    fn compound_leaves_no_tokens_stranded_with_the_user() {
        let f = setup();
        let router = f.router(1, 1);
        f.client
            .harvest_and_compound(&f.user, &router, &f.path(), &0);

        // Rewards were swapped, LP was re-staked: the caller nets zero of both.
        assert_eq!(
            token::Client::new(&f.env, &f.reward_token).balance(&f.user),
            0
        );
        assert_eq!(token::Client::new(&f.env, &f.lp_token).balance(&f.user), 0);
    }

    #[test]
    fn compound_emits_event_with_claim_swap_and_stake_amounts() {
        let f = setup();
        let router = f.router(1, 1);
        let result = f
            .client
            .harvest_and_compound(&f.user, &router, &f.path(), &0);

        let expected_topics: Vec<Val> = soroban_sdk::vec![
            &f.env,
            symbol_short!("hrvcmpnd").into_val(&f.env),
            f.user.clone().into_val(&f.env),
        ];
        let event = f
            .env
            .events()
            .all()
            .iter()
            .find(|e| e.0 == f.contract_id && e.1 == expected_topics)
            .expect("expected an hrvcmpnd event addressed to the caller");
        let payload = <(i128, i128, i128)>::try_from_val(&f.env, &event.2).unwrap();
        assert_eq!(
            payload,
            (
                result.reward_claimed,
                result.lp_acquired,
                result.new_share_balance
            )
        );
    }

    #[test]
    fn compounding_raises_the_next_harvests_reward() {
        let f = setup();
        let router = f.router(1, 1);
        f.client
            .harvest_and_compound(&f.user, &router, &f.path(), &0);
        let staked_after = f.client.yield_farming_share_balance(&f.user);

        // A second staker splits emissions by share weight, so the compounded
        // position must now draw the larger slice — the point of the feature.
        let rival = Address::generate(&f.env);
        mint(&f.env, &f.lp_token, &rival, STAKE);
        f.client.stake_lp(&rival, &STAKE);
        advance_ledgers(&f.env, LEDGERS);

        let user_pending = f.client.pending_yield_rewards(&f.user);
        let rival_pending = f.client.pending_yield_rewards(&rival);
        assert!(staked_after > STAKE);
        assert!(
            user_pending > rival_pending,
            "compounded position ({staked_after}) should out-earn the flat one: \
             user={user_pending} rival={rival_pending}"
        );
    }

    // ── Rejections ──────────────────────────────────────────────────────────

    #[test]
    fn rejects_when_no_rewards_have_accrued() {
        let f = setup();
        let router = f.router(1, 1);
        // Drain the pending balance first so the second call has nothing left.
        f.client.claim_rewards(&f.user);

        let result = f
            .client
            .try_harvest_and_compound(&f.user, &router, &f.path(), &0);
        assert_eq!(
            result,
            Err(Ok(ContractError::HarvestNothingToCompound)),
            "compounding nothing must fail rather than call the router"
        );
    }

    #[test]
    fn rejects_negative_min_lp_out() {
        let f = setup();
        let router = f.router(1, 1);
        let result = f
            .client
            .try_harvest_and_compound(&f.user, &router, &f.path(), &-1);
        assert_eq!(result, Err(Ok(ContractError::HarvestInvalidMinOut)));
    }

    #[test]
    fn rejects_path_not_starting_at_reward_token() {
        let f = setup();
        let router = f.router(1, 1);
        let bogus = Address::generate(&f.env);
        let path = soroban_sdk::vec![&f.env, bogus, f.lp_token.clone()];

        let result = f
            .client
            .try_harvest_and_compound(&f.user, &router, &path, &0);
        assert_eq!(result, Err(Ok(ContractError::HarvestInvalidPath)));
    }

    #[test]
    fn rejects_path_not_ending_at_lp_token() {
        let f = setup();
        let router = f.router(1, 1);
        let bogus = Address::generate(&f.env);
        let path = soroban_sdk::vec![&f.env, f.reward_token.clone(), bogus];

        let result = f
            .client
            .try_harvest_and_compound(&f.user, &router, &path, &0);
        assert_eq!(result, Err(Ok(ContractError::HarvestInvalidPath)));
    }

    #[test]
    fn rejects_single_hop_path() {
        let f = setup();
        let router = f.router(1, 1);
        let path = soroban_sdk::vec![&f.env, f.reward_token.clone()];

        let result = f
            .client
            .try_harvest_and_compound(&f.user, &router, &path, &0);
        assert_eq!(result, Err(Ok(ContractError::HarvestInvalidPath)));
    }

    #[test]
    fn rejects_path_longer_than_the_hop_cap() {
        let f = setup();
        let router = f.router(1, 1);
        let mut path = soroban_sdk::vec![&f.env, f.reward_token.clone()];
        while path.len() < MAX_SWAP_PATH_LEN {
            path.push_back(Address::generate(&f.env));
        }
        path.push_back(f.lp_token.clone());
        assert_eq!(path.len(), MAX_SWAP_PATH_LEN + 1);

        let result = f
            .client
            .try_harvest_and_compound(&f.user, &router, &path, &0);
        assert_eq!(result, Err(Ok(ContractError::HarvestInvalidPath)));
    }

    #[test]
    fn rejects_when_the_router_delivers_nothing() {
        let f = setup();
        let router = f.router(0, 1);
        let result = f
            .client
            .try_harvest_and_compound(&f.user, &router, &f.path(), &0);
        assert_eq!(result, Err(Ok(ContractError::HarvestSwapFailed)));
    }

    #[test]
    fn rejects_when_the_router_underdelivers_against_min_lp_out() {
        let f = setup();
        // Half the requested output.
        let router = f.router(1, 2);
        let result = f
            .client
            .try_harvest_and_compound(&f.user, &router, &f.path(), &EXPECTED_REWARD);
        assert_eq!(result, Err(Ok(ContractError::HarvestSlippageExceeded)));
    }

    #[test]
    fn rejects_while_the_vault_is_paused() {
        let f = setup();
        let router = f.router(1, 1);
        f.client.pause_vault(&f.admin);

        let result = f
            .client
            .try_harvest_and_compound(&f.user, &router, &f.path(), &0);
        assert_eq!(result, Err(Ok(ContractError::VaultPaused)));
    }

    /// The untrusted router is called mid-harvest, so the contract-wide
    /// reentrancy lock must already be held by then and must refuse a second
    /// entry.
    ///
    /// The lock is taken directly rather than by driving a router that calls
    /// back in: in this soroban-sdk 20.0.0 native test harness a *sub*
    /// invocation that returns `Err` escalates to a non-unwinding panic and
    /// aborts the whole test process, so a live re-entry cannot be observed.
    /// Holding the lock reproduces the exact state a re-entrant call would
    /// meet, which is what the guard actually keys off.
    #[test]
    fn rejects_entry_while_the_reentrancy_lock_is_held() {
        let f = setup();
        let router = f.router(1, 1);
        f.env.as_contract(&f.contract_id, || {
            crate::security::reentrancy::lock(&f.env).expect("lock should be free");
        });

        let result = f
            .client
            .try_harvest_and_compound(&f.user, &router, &f.path(), &0);
        assert_eq!(result, Err(Ok(ContractError::ReentrancyDetected)));

        // Nothing was consumed: the reward is still there to harvest.
        assert_eq!(f.client.pending_yield_rewards(&f.user), EXPECTED_REWARD);
    }

    // ── Atomicity ───────────────────────────────────────────────────────────

    #[test]
    fn failed_swap_leaves_the_reward_claimable() {
        let f = setup();
        let router = f.router(0, 1);
        let lp_before = f.client.yield_farming_share_balance(&f.user);

        assert!(f
            .client
            .try_harvest_and_compound(&f.user, &router, &f.path(), &0)
            .is_err());

        // The whole transaction rolled back, claim included: nothing was burned.
        assert_eq!(f.client.pending_yield_rewards(&f.user), EXPECTED_REWARD);
        assert_eq!(f.client.yield_farming_share_balance(&f.user), lp_before);
        assert_eq!(
            token::Client::new(&f.env, &f.reward_token).balance(&f.user),
            0
        );
    }

    #[test]
    fn retry_after_a_failed_route_succeeds() {
        let f = setup();
        let dud = f.router(0, 1);
        assert!(f
            .client
            .try_harvest_and_compound(&f.user, &dud, &f.path(), &0)
            .is_err());

        // Same rewards, better venue.
        let good = f.router(1, 1);
        let result = f
            .client
            .harvest_and_compound(&f.user, &good, &f.path(), &EXPECTED_REWARD);
        assert_eq!(result.reward_claimed, EXPECTED_REWARD);
        assert_eq!(result.lp_acquired, EXPECTED_REWARD);
    }
}
