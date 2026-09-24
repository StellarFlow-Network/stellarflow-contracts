#![no_std]

//! A minimal constant-product (x*y=k) two-asset liquidity pool.
//!
//! This crate is intentionally scoped down (see issue #768): it implements only
//! `initialize`, `deposit`, `withdraw`, `swap`, and a handful of read-only
//! getters — just enough surface area to meaningfully exercise the storage TTL
//! extension requirement. There are no fee tiers, no separate LP token
//! contract, and no oracle integration. Balances are tracked with plain
//! internal accounting (`i128` amounts recorded per pool / per user) rather
//! than real SEP-41 token transfers, keeping the example self-contained and
//! test-friendly while still exercising the same storage-access patterns a
//! full pool would use.
//!
//! ## TTL extension (the actual deliverable of #768)
//!
//! Soroban persistent storage entries are subject to expiration: if an entry's
//! time-to-live (TTL) is not periodically extended, it can enter the
//! "archived" state and become unreadable/unwritable without a restore. For a
//! pool that is actively being traded against, that would be a serious
//! liveness bug — an idle-looking pool (no state-changing calls for a long
//! stretch) could silently expire and then reject its next `swap`/`deposit`/
//! `withdraw` outright.
//!
//! To prevent that, every one of `swap`, `deposit`, and `withdraw` calls
//! [`bump_pool_ttl`] and [`bump_user_ttl`] *before* reading any pool or user
//! state, via the shared helpers below. Both helpers are no-ops when the
//! remaining TTL is still comfortably above [`BUMP_THRESHOLD`], so healthy
//! pools pay no extra cost; only entries approaching expiration are extended,
//! out to [`BUMP_AMOUNT`] ledgers.

use soroban_sdk::{contract, contracterror, contractimpl, panic_with_error, Address, Env};

mod types;
pub use types::{DataKey, PoolReserves};

#[cfg(test)]
mod test;

/// Number of ledgers below which a persistent entry's TTL is proactively
/// extended. At roughly 5 seconds/ledger, `518_400` ledgers is ~30 days —
/// comfortably inside Soroban's minimum persistent TTL window, so any pool or
/// user record that is touched at least once a month never approaches
/// archival.
pub const BUMP_THRESHOLD: u32 = 518_400;

/// Number of ledgers a bumped entry's TTL is extended *to* (from the current
/// ledger), when the remaining TTL falls below [`BUMP_THRESHOLD`]. At ~5
/// seconds/ledger, `1_036_800` ledgers is ~60 days, giving a wide safety
/// margin before the next bump is strictly required.
pub const BUMP_AMOUNT: u32 = 1_036_800;

/// Error types for the LP pool contract.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    /// The contract has not been initialized yet.
    NotInitialized = 1,
    /// The contract has already been initialized.
    AlreadyInitialized = 2,
    /// An amount argument was zero or negative where a positive value is required.
    ZeroAmount = 3,
    /// The caller does not hold enough LP shares for this withdrawal.
    InsufficientShares = 4,
    /// The pool does not hold enough reserves to satisfy this operation.
    InsufficientLiquidity = 5,
    /// `token_in` does not match either pooled asset.
    InvalidToken = 6,
    /// Resulting swap output is below the caller's `min_amount_out` (slippage guard).
    SlippageExceeded = 7,
    /// An arithmetic operation would overflow or underflow.
    Overflow = 8,
}

/// Guard: `Err(Error::NotInitialized)` unless `initialize` has run.
///
/// Returns a `Result` (rather than panicking) so that entrypoints calling
/// this via `?` surface the failure through the SDK's normal
/// `Result<T, Error>` dispatch path instead of an actual Rust panic —
/// avoiding a panic/unwind round trip through the host for a routine,
/// fully-expected validation failure.
fn require_initialized(env: &Env) -> Result<(), Error> {
    if !env.storage().persistent().has(&DataKey::Initialized) {
        return Err(Error::NotInitialized);
    }
    Ok(())
}

/// Extend the pool reserves entry's TTL if it is running low.
///
/// Called at the top of every state-changing entrypoint, before any pool
/// state is read, so an actively-used pool's reserves record never drifts
/// into the archived state. A no-op if the entry does not exist yet (i.e.
/// before `initialize`) or if its remaining TTL is still above
/// [`BUMP_THRESHOLD`].
fn bump_pool_ttl(env: &Env) {
    let storage = env.storage().persistent();
    if storage.has(&DataKey::Reserves) {
        storage.extend_ttl(&DataKey::Reserves, BUMP_THRESHOLD, BUMP_AMOUNT);
    }
}

/// Extend a user's share-record TTL if it is running low.
///
/// Called alongside [`bump_pool_ttl`] at the top of every state-changing
/// entrypoint. A no-op if the user has no share record yet (e.g. their first
/// ever `deposit`) or if its remaining TTL is still above
/// [`BUMP_THRESHOLD`].
fn bump_user_ttl(env: &Env, user: &Address) {
    let storage = env.storage().persistent();
    let key = DataKey::UserShares(user.clone());
    if storage.has(&key) {
        storage.extend_ttl(&key, BUMP_THRESHOLD, BUMP_AMOUNT);
    }
}

/// Read the current pool reserves, panicking if the pool has not been initialized.
fn read_reserves(env: &Env) -> PoolReserves {
    env.storage()
        .persistent()
        .get(&DataKey::Reserves)
        .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized))
}

fn write_reserves(env: &Env, reserves: &PoolReserves) {
    env.storage()
        .persistent()
        .set(&DataKey::Reserves, reserves);
}

/// Read a user's LP share balance, defaulting to zero if they have never deposited.
fn read_user_shares(env: &Env, user: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::UserShares(user.clone()))
        .unwrap_or(0)
}

fn write_user_shares(env: &Env, user: &Address, shares: i128) {
    env.storage()
        .persistent()
        .set(&DataKey::UserShares(user.clone()), &shares);
}

/// Integer square root (floor) via Newton's method, for `i128` values >= 0.
fn isqrt(value: i128) -> i128 {
    if value < 2 {
        return value.max(0);
    }
    let mut x = value;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + value / x) / 2;
    }
    x
}

#[contract]
pub struct LpPool;

#[contractimpl]
impl LpPool {
    /// Initialize the pool with the two pooled asset addresses.
    ///
    /// Can only be called once. Sets both reserves and total shares to zero.
    pub fn initialize(env: Env, token_a: Address, token_b: Address) -> Result<(), Error> {
        if env.storage().persistent().has(&DataKey::Initialized) {
            return Err(Error::AlreadyInitialized);
        }

        env.storage().persistent().set(&DataKey::TokenA, &token_a);
        env.storage().persistent().set(&DataKey::TokenB, &token_b);
        env.storage().persistent().set(
            &DataKey::Reserves,
            &PoolReserves {
                reserve_a: 0,
                reserve_b: 0,
                total_shares: 0,
            },
        );
        env.storage().persistent().set(&DataKey::Initialized, &true);

        Ok(())
    }

    /// Deposit `amount_a` of token A and `amount_b` of token B into the pool,
    /// minting LP shares to `user` in return.
    ///
    /// The first deposit sets the pool's initial price and mints
    /// `isqrt(amount_a * amount_b)` shares. Subsequent deposits mint shares
    /// proportional to the smaller of the two contributed ratios, so
    /// depositing off the current ratio never lets a caller mint more than
    /// their fair share.
    ///
    /// Requires `user.require_auth()`.
    pub fn deposit(env: Env, user: Address, amount_a: i128, amount_b: i128) -> Result<i128, Error> {
        // TTL extension MUST happen before any pool/user state is read.
        bump_pool_ttl(&env);
        bump_user_ttl(&env, &user);

        user.require_auth();

        if amount_a <= 0 || amount_b <= 0 {
            return Err(Error::ZeroAmount);
        }

        require_initialized(&env)?;
        let mut reserves = read_reserves(&env);

        let minted_shares = if reserves.total_shares == 0 {
            let product = amount_a.checked_mul(amount_b).ok_or(Error::Overflow)?;
            isqrt(product)
        } else {
            let shares_from_a = amount_a
                .checked_mul(reserves.total_shares)
                .ok_or(Error::Overflow)?
                .checked_div(reserves.reserve_a)
                .ok_or(Error::Overflow)?;
            let shares_from_b = amount_b
                .checked_mul(reserves.total_shares)
                .ok_or(Error::Overflow)?
                .checked_div(reserves.reserve_b)
                .ok_or(Error::Overflow)?;
            shares_from_a.min(shares_from_b)
        };

        if minted_shares <= 0 {
            return Err(Error::ZeroAmount);
        }

        reserves.reserve_a = reserves
            .reserve_a
            .checked_add(amount_a)
            .ok_or(Error::Overflow)?;
        reserves.reserve_b = reserves
            .reserve_b
            .checked_add(amount_b)
            .ok_or(Error::Overflow)?;
        reserves.total_shares = reserves
            .total_shares
            .checked_add(minted_shares)
            .ok_or(Error::Overflow)?;
        write_reserves(&env, &reserves);

        let prior_shares = read_user_shares(&env, &user);
        let new_shares = prior_shares
            .checked_add(minted_shares)
            .ok_or(Error::Overflow)?;
        write_user_shares(&env, &user, new_shares);

        Ok(minted_shares)
    }

    /// Burn `shares` of `user`'s LP shares and return their proportional
    /// share of both reserves.
    ///
    /// Requires `user.require_auth()`.
    pub fn withdraw(env: Env, user: Address, shares: i128) -> Result<(i128, i128), Error> {
        // TTL extension MUST happen before any pool/user state is read.
        bump_pool_ttl(&env);
        bump_user_ttl(&env, &user);

        user.require_auth();

        if shares <= 0 {
            return Err(Error::ZeroAmount);
        }

        require_initialized(&env)?;
        let mut reserves = read_reserves(&env);

        let user_shares = read_user_shares(&env, &user);
        if shares > user_shares {
            return Err(Error::InsufficientShares);
        }

        let amount_a = reserves
            .reserve_a
            .checked_mul(shares)
            .ok_or(Error::Overflow)?
            .checked_div(reserves.total_shares)
            .ok_or(Error::Overflow)?;
        let amount_b = reserves
            .reserve_b
            .checked_mul(shares)
            .ok_or(Error::Overflow)?
            .checked_div(reserves.total_shares)
            .ok_or(Error::Overflow)?;

        if amount_a <= 0 || amount_b <= 0 {
            return Err(Error::ZeroAmount);
        }
        if amount_a > reserves.reserve_a || amount_b > reserves.reserve_b {
            return Err(Error::InsufficientLiquidity);
        }

        reserves.reserve_a = reserves
            .reserve_a
            .checked_sub(amount_a)
            .ok_or(Error::Overflow)?;
        reserves.reserve_b = reserves
            .reserve_b
            .checked_sub(amount_b)
            .ok_or(Error::Overflow)?;
        reserves.total_shares = reserves
            .total_shares
            .checked_sub(shares)
            .ok_or(Error::Overflow)?;
        write_reserves(&env, &reserves);

        let remaining_shares = user_shares.checked_sub(shares).ok_or(Error::Overflow)?;
        write_user_shares(&env, &user, remaining_shares);

        Ok((amount_a, amount_b))
    }

    /// Swap `amount_in` of `token_in` (must be either pooled asset) for the
    /// other asset, using the constant-product `x*y=k` formula with no fee.
    ///
    /// Reverts with `Error::SlippageExceeded` if the computed output is below
    /// `min_amount_out`.
    ///
    /// Requires `user.require_auth()`.
    pub fn swap(
        env: Env,
        user: Address,
        token_in: Address,
        amount_in: i128,
        min_amount_out: i128,
    ) -> Result<i128, Error> {
        // TTL extension MUST happen before any pool/user state is read.
        bump_pool_ttl(&env);
        bump_user_ttl(&env, &user);

        user.require_auth();

        if amount_in <= 0 {
            return Err(Error::ZeroAmount);
        }

        require_initialized(&env)?;
        let mut reserves = read_reserves(&env);

        let token_a: Address = env
            .storage()
            .persistent()
            .get(&DataKey::TokenA)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        let token_b: Address = env
            .storage()
            .persistent()
            .get(&DataKey::TokenB)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));

        let (reserve_in, reserve_out, a_in) = if token_in == token_a {
            (reserves.reserve_a, reserves.reserve_b, true)
        } else if token_in == token_b {
            (reserves.reserve_b, reserves.reserve_a, false)
        } else {
            return Err(Error::InvalidToken);
        };

        if reserve_in <= 0 || reserve_out <= 0 {
            return Err(Error::InsufficientLiquidity);
        }

        // amount_out = reserve_out * amount_in / (reserve_in + amount_in)
        // Integer division always rounds the output down, so the pool's
        // constant product k can only stay the same or grow — it never
        // decreases from a swap.
        let new_reserve_in = reserve_in.checked_add(amount_in).ok_or(Error::Overflow)?;
        let numerator = reserve_out.checked_mul(amount_in).ok_or(Error::Overflow)?;
        let amount_out = numerator
            .checked_div(new_reserve_in)
            .ok_or(Error::Overflow)?;

        if amount_out <= 0 {
            return Err(Error::ZeroAmount);
        }
        if amount_out < min_amount_out {
            return Err(Error::SlippageExceeded);
        }
        if amount_out > reserve_out {
            return Err(Error::InsufficientLiquidity);
        }

        if a_in {
            reserves.reserve_a = new_reserve_in;
            reserves.reserve_b = reserves
                .reserve_b
                .checked_sub(amount_out)
                .ok_or(Error::Overflow)?;
        } else {
            reserves.reserve_b = new_reserve_in;
            reserves.reserve_a = reserves
                .reserve_a
                .checked_sub(amount_out)
                .ok_or(Error::Overflow)?;
        }
        write_reserves(&env, &reserves);

        Ok(amount_out)
    }

    /// Return the current `(reserve_a, reserve_b)` balances.
    pub fn get_reserves(env: Env) -> (i128, i128) {
        let reserves = read_reserves(&env);
        (reserves.reserve_a, reserves.reserve_b)
    }

    /// Return the total number of LP shares currently outstanding.
    pub fn get_total_shares(env: Env) -> i128 {
        read_reserves(&env).total_shares
    }

    /// Return `user`'s current LP share balance (zero if they never deposited).
    pub fn get_shares(env: Env, user: Address) -> i128 {
        read_user_shares(&env, &user)
    }

    /// Return the pooled token addresses as `(token_a, token_b)`.
    pub fn get_tokens(env: Env) -> (Address, Address) {
        let token_a = env
            .storage()
            .persistent()
            .get(&DataKey::TokenA)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        let token_b = env
            .storage()
            .persistent()
            .get(&DataKey::TokenB)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        (token_a, token_b)
    }
}
