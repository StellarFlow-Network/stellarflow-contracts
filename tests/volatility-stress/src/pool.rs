//! Drives the **real** `amm-contract` pool inside a Soroban host environment.
//!
//! # Why the pool is called directly instead of through a client
//!
//! The obvious way to exercise `amm-contract` from another crate is
//! `env.register_contract(None, AmmContract)` followed by
//! `AmmContractClient::swap(..)`, which is exactly what `contracts/amm`'s own
//! test module does. That is not available from an external crate:
//!
//! ```text
//! error[E0277]: the trait bound `AmmContract: ContractFunctionSet` is not satisfied
//! ```
//!
//! `#[contractimpl]` only emits the `ContractFunctionSet` impl when the
//! *defining* crate is compiled with its own `test` cfg or a `testutils`
//! feature. `amm-contract` has neither, and adding one would mean editing
//! `contracts/amm/Cargo.toml`, which is outside the scope of this issue.
//!
//! So the harness calls the production entry points directly, inside
//! `env.as_contract`, against a registered contract address:
//!
//! ```no_run
//! # use amm_contract::AmmContract;
//! # use soroban_sdk::{Env, Address};
//! # use soroban_sdk::testutils::MockAuthContract;
//! # fn f(env: Env, id: Address, trader: Address) {
//! env.as_contract(&id, || {
//!     AmmContract::swap(env.clone(), trader, 1_000, 0)
//! });
//! # }
//! ```
//!
//! This runs the exact production function body — the same `AmmContract::swap`,
//! the same `AmmError` surface, the same `EffectiveReserves` arithmetic, real
//! instance storage, and real Stellar Asset Contract token transfers through the
//! host. Nothing about the pool's behaviour is re-implemented or substituted
//! here. What the dispatch trampoline would otherwise supply is instead provided
//! explicitly:
//!
//! * `env.as_contract(&id, ..)` makes `id` the current contract, which is what
//!   the trampoline does and what `env.current_contract_address()` reads inside
//!   the token transfers.
//! * `Env::mock_all_auths_allowing_non_root_auth` satisfies the
//!   `require_auth` calls, which is required because a direct call is not a
//!   root host invocation and recording auth otherwise rejects non-root
//!   authorizations.
//! * `MockAuthContract` is only a registerable host contract used to obtain a
//!   contract address with instance storage. It carries no pool or risk-control
//!   logic and is never invoked.

use amm_contract::virtual_reserves::{EffectiveReserves, VirtualReserveError, U256};
use amm_contract::{AmmContract, AmmError};
use soroban_sdk::testutils::{Address as _, MockAuthContract};
use soroban_sdk::token::{StellarAssetClient, TokenClient};
use soroban_sdk::{Address, Env};

/// Fixed-point scale for spot prices: `1e9`, i.e. nine decimal places.
///
/// Chosen so a 15% move on a pool whose reserves are in the millions is
/// represented exactly enough that consecutive prices never collapse onto the
/// same integer.
pub const SPOT_SCALE: i128 = 1_000_000_000;

/// The protocol default virtual core installed by `AmmContract::initialize`.
pub const DEFAULT_VIRTUAL_RESERVE: i128 = 1_000;

/// A completed deposit, including the amounts the pool actually took.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Deposit {
    /// Token A actually transferred in.
    pub amount_a: i128,
    /// Token B actually transferred in.
    pub amount_b: i128,
    /// LP shares minted.
    pub shares: i128,
}

/// A full pool instance plus the addresses needed to inspect and fund it.
pub struct PoolHarness {
    env: Env,
    id: Address,
    token_a: Address,
    token_b: Address,
    lp_token: Address,
    virtual_a: i128,
    virtual_b: i128,
}

impl PoolHarness {
    /// Initialise a pair with the protocol default virtual core of 1,000 per leg.
    pub fn new() -> Self {
        Self::with_virtual_reserves(DEFAULT_VIRTUAL_RESERVE, DEFAULT_VIRTUAL_RESERVE)
    }

    /// Initialise a pair with an explicit virtual reserve core.
    pub fn with_virtual_reserves(virtual_a: i128, virtual_b: i128) -> Self {
        let env = Env::default();
        // A direct call is not a root host invocation, so recording auth would
        // reject every `require_auth` in the pool. Allowing non-root auth is
        // what makes the production entry points callable here at all.
        env.mock_all_auths_allowing_non_root_auth();
        // Long runs are about state transitions, not about metering, and the
        // default budget is sized for a single transaction.
        env.budget().reset_unlimited();

        let token_a = env.register_stellar_asset_contract(Address::generate(&env));
        let token_b = env.register_stellar_asset_contract(Address::generate(&env));
        let lp_token = env.register_stellar_asset_contract(Address::generate(&env));
        let id = env.register_contract(None, MockAuthContract);

        let init_env = env.clone();
        env.as_contract(&id, || {
            AmmContract::initialize_with_virtual_reserves(
                init_env.clone(),
                token_a.clone(),
                token_b.clone(),
                lp_token.clone(),
                virtual_a,
                virtual_b,
            )
            .unwrap();
        });

        Self {
            env,
            id,
            token_a,
            token_b,
            lp_token,
            virtual_a,
            virtual_b,
        }
    }

    /// The host environment. Shared with callers that need to advance ledger
    /// time or inspect auth trees.
    pub fn env(&self) -> &Env {
        &self.env
    }

    /// The pool's own address, i.e. the account that custodies the tokens.
    pub fn id(&self) -> &Address {
        &self.id
    }

    pub fn token_a(&self) -> &Address {
        &self.token_a
    }

    pub fn token_b(&self) -> &Address {
        &self.token_b
    }

    pub fn lp_token(&self) -> &Address {
        &self.lp_token
    }

    /// The pair's virtual core `(v_a, v_b)`.
    pub fn virtual_reserves(&self) -> (i128, i128) {
        (self.virtual_a, self.virtual_b)
    }

    /// Mint test liquidity. Bypasses the pool; only ever used to fund accounts
    /// before they interact with it.
    pub fn fund(&self, who: &Address, amount_a: i128, amount_b: i128) {
        if amount_a > 0 {
            StellarAssetClient::new(&self.env, &self.token_a).mint(who, &amount_a);
        }
        if amount_b > 0 {
            StellarAssetClient::new(&self.env, &self.token_b).mint(who, &amount_b);
        }
    }

    /// Create a fresh, funded account.
    pub fn new_account(&self, amount_a: i128, amount_b: i128) -> Address {
        let who = Address::generate(&self.env);
        self.fund(&who, amount_a, amount_b);
        who
    }

    /// Top `who` up so it holds at least `minimum_a` / `minimum_b`.
    ///
    /// Long stress runs spend more than any fixed up-front grant covers, and
    /// running out of balance mid-run would abort the sequence on a
    /// `TokenClient` transfer rather than on a pool property. Refilling here
    /// keeps every assertion about the pool rather than about test bookkeeping.
    pub fn ensure_funded(&self, who: &Address, minimum_a: i128, minimum_b: i128) {
        let short_a = (minimum_a - self.balance_a(who)).max(0);
        let short_b = (minimum_b - self.balance_b(who)).max(0);
        self.fund(who, short_a, short_b);
    }

    // --- Production entry points -----------------------------------------

    /// `AmmContract::deposit`, reporting the amounts the pool actually took.
    ///
    /// A caller cannot assume it received the amounts it asked for: the contract
    /// prices the deposit against the effective curve and takes the smaller of
    /// `amount_a_desired` and `amount_b_desired * x_eff / y_eff`, which lands a
    /// few units short because of floor division. The contract returns only the
    /// minted shares, so the amounts are recovered from the reserve deltas —
    /// which is precisely what the pool persisted.
    pub fn deposit_ex(
        &self,
        provider: &Address,
        amount_a: i128,
        amount_b: i128,
        min_lp_mint: i128,
    ) -> Result<Deposit, AmmError> {
        let before = self.reserves();
        let shares = {
            let env = self.env.clone();
            self.env.as_contract(&self.id, || {
                AmmContract::deposit(
                    env.clone(),
                    provider.clone(),
                    amount_a,
                    amount_b,
                    min_lp_mint,
                )
            })?
        };
        let after = self.reserves();
        Ok(Deposit {
            amount_a: after.0 - before.0,
            amount_b: after.1 - before.1,
            shares,
        })
    }

    /// `AmmContract::deposit`.
    pub fn deposit(
        &self,
        provider: &Address,
        amount_a: i128,
        amount_b: i128,
        min_lp_mint: i128,
    ) -> Result<i128, AmmError> {
        self.deposit_ex(provider, amount_a, amount_b, min_lp_mint)
            .map(|d| d.shares)
    }

    /// `AmmContract::swap` (token A in, token B out).
    pub fn swap_a_to_b(
        &self,
        trader: &Address,
        amount_in: i128,
        min_amount_out: i128,
    ) -> Result<i128, AmmError> {
        let env = self.env.clone();
        self.env.as_contract(&self.id, || {
            AmmContract::swap(env.clone(), trader.clone(), amount_in, min_amount_out)
        })
    }

    /// `AmmContract::remove_liquidity`.
    pub fn remove_liquidity(
        &self,
        provider: &Address,
        shares: i128,
        min_amount_a: i128,
        min_amount_b: i128,
    ) -> Result<(i128, i128), AmmError> {
        let env = self.env.clone();
        self.env.as_contract(&self.id, || {
            AmmContract::remove_liquidity(
                env.clone(),
                provider.clone(),
                shares,
                min_amount_a,
                min_amount_b,
            )
        })
    }

    // --- Read-only views -------------------------------------------------

    /// `AmmContract::get_reserves`: the real `(reserve_a, reserve_b)`.
    pub fn reserves(&self) -> (i128, i128) {
        let env = self.env.clone();
        self.env
            .as_contract(&self.id, || AmmContract::get_reserves(env.clone()))
    }

    /// `AmmContract::get_effective_reserves`.
    pub fn effective_reserves(&self) -> (i128, i128) {
        let env = self.env.clone();
        self.env.as_contract(&self.id, || {
            AmmContract::get_effective_reserves(env.clone())
        })
    }

    /// `AmmContract::get_total_shares`.
    pub fn total_shares(&self) -> i128 {
        let env = self.env.clone();
        self.env
            .as_contract(&self.id, || AmmContract::get_total_shares(env.clone()))
    }

    // --- Derived state ---------------------------------------------------

    /// The effective-reserve snapshot the pool enforces its invariant on.
    pub fn effective(&self) -> EffectiveReserves {
        let (x, y) = self.reserves();
        EffectiveReserves::new(x, y, self.virtual_a, self.virtual_b)
            .expect("real reserves stay non-negative by construction")
    }

    /// `k_eff = x_eff * y_eff`, at 256-bit precision.
    pub fn k_eff(&self) -> U256 {
        self.effective().k_eff()
    }

    /// Spot price of A denominated in B, scaled by [`SPOT_SCALE`].
    ///
    /// This is `y_eff / x_eff`, i.e. what a unit of A is worth in B right now.
    /// Selling A into the pool can only lower it.
    pub fn spot_price_b_per_a(&self) -> i128 {
        let (x_eff, y_eff) = self.effective_reserves();
        (y_eff * SPOT_SCALE) / x_eff
    }

    /// `amount_b` the production quote pays for `amount_a`, evaluated against
    /// the *current* state without mutating it.
    ///
    /// This calls `EffectiveReserves::swap_a_to_b` directly, the same function
    /// `AmmContract::swap` quotes with, so a mismatch between this and the
    /// realised output would be a real defect in the contract rather than an
    /// artefact of the harness.
    pub fn quote_a_to_b(&self, amount_in: i128) -> Result<i128, VirtualReserveError> {
        self.effective()
            .swap_a_to_b(amount_in)
            .map(|(_, out, _)| out)
    }

    /// The `(amount_a, amount_b)` the production deposit logic demands to keep
    /// the deposit at the current curve, for a desired `amount_a`.
    pub fn required_b_for_a(&self, amount_a: i128) -> i128 {
        let (x_eff, y_eff) = self.effective_reserves();
        (amount_a * y_eff) / x_eff
    }

    /// Token A balance of `who`.
    pub fn balance_a(&self, who: &Address) -> i128 {
        TokenClient::new(&self.env, &self.token_a).balance(who)
    }

    /// Token B balance of `who`.
    pub fn balance_b(&self, who: &Address) -> i128 {
        TokenClient::new(&self.env, &self.token_b).balance(who)
    }

    /// LP token balance of `who`.
    pub fn balance_lp(&self, who: &Address) -> i128 {
        TokenClient::new(&self.env, &self.lp_token).balance(who)
    }
}

impl Default for PoolHarness {
    fn default() -> Self {
        Self::new()
    }
}
