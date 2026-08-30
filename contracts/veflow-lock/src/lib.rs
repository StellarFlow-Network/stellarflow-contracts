#![no_std]

//! Vote-Escrowed FLOW (`veFLOW`) lock contract.
//!
//! Users lock FLOW tokens for a variable time duration (from one week up to
//! four years) and receive a non-transferable voting weight proportional to
//! both the locked amount and the lock duration:
//!
//! `Weight = Amount * (LockDuration / MaxDuration)`
//!
//! Tokens cannot be unlocked before the designated expiration ledger sequence.
//! A lock can be extended and topped up with additional FLOW at any time while
//! it is still active.
//!
//! The contract exposes the `get_voting_power(user, proposed_at)` view that the
//! `StellarFlow` governance oracle contract invokes cross-contract when weight
//! is required for `veFLOW` proposals.

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, token, Address, Env,
};

#[cfg(test)]
mod test_support;

/// Number of seconds assumed per ledger when translating lock durations into
/// absolute expiration timestamps (Stellar ledgers target ~5s, mirrored here as
/// a conservative upper bound of 60s).
const SECONDS_PER_LEDGER: u64 = 60;

/// Minimum lock duration: one week, expressed in ledgers.
pub const MIN_LOCK_DURATION_LEDGERS: u64 = 10_080;

/// Maximum lock duration: four years, expressed in ledgers.
pub const MAX_LOCK_DURATION_LEDGERS: u64 = 2_102_400;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum VeflowLockError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    NotAdmin = 3,
    NoLock = 4,
    LockNotExpired = 5,
    InvalidAmount = 6,
    InvalidDuration = 7,
    LockExpired = 8,
    DurationNotExtended = 9,
    WeightOverflow = 10,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Lock {
    /// FLOW amount currently locked, in token stroops.
    pub amount: i128,
    /// Ledger sequence at which the current lock (re)started.
    pub start_ledger: u32,
    /// Ledger sequence after which the lock may be withdrawn.
    pub expiry_ledger: u32,
    /// Ledger timestamp at which the current lock (re)started.
    pub start_timestamp: u64,
    /// Ledger timestamp after which the lock is considered expired for voting.
    pub expiry_timestamp: u64,
    /// Total locked duration in ledgers (used for voting weight).
    pub duration: u64,
}

#[contracttype]
pub enum DataKey {
    Admin,
    Token,
    MaxDuration,
    Lock(Address),
}

#[contract]
pub struct VeflowLock;

#[contractimpl]
impl VeflowLock {
    /// Initialize the contract with an admin and the FLOW token address.
    ///
    /// `max_duration` is the maximum lock duration (in ledgers) against which
    /// voting weight is normalised. It defaults to 4 years when zero.
    pub fn initialize(
        env: Env,
        admin: Address,
        token: Address,
        max_duration: u64,
    ) -> Result<(), VeflowLockError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(VeflowLockError::AlreadyInitialized);
        }
        admin.require_auth();
        let max_duration = if max_duration == 0 {
            MAX_LOCK_DURATION_LEDGERS
        } else {
            max_duration
        };
        if max_duration < MIN_LOCK_DURATION_LEDGERS {
            return Err(VeflowLockError::InvalidDuration);
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage().instance().set(&DataKey::MaxDuration, &max_duration);
        Ok(())
    }

    /// Create (or top up) a vote-escrowed lock.
    ///
    /// Transfers `amount` FLOW from `user` into the contract and, if the user
    /// has no active lock, opens one with the given `duration` (in ledgers).
    /// If the user already holds an active lock, the amount is simply added to
    /// it (see [`Self::add_amount`]).
    pub fn lock(
        env: Env,
        user: Address,
        amount: i128,
        duration: u64,
    ) -> Result<Lock, VeflowLockError> {
        user.require_auth();
        Self::check_initialized(&env)?;
        if amount <= 0 {
            return Err(VeflowLockError::InvalidAmount);
        }
        let current_ledger = env.ledger().sequence();
        let current_timestamp = env.ledger().timestamp();

        let existing = Self::get_lock(env.clone(), user.clone());
        if let Some(lock) = existing {
            if current_ledger >= lock.expiry_ledger {
                return Err(VeflowLockError::LockExpired);
            }
            Self::transfer_in(&env, &user, amount)?;
            Self::store_lock(
                &env,
                &user,
                Lock {
                    amount: lock
                        .amount
                        .checked_add(amount)
                        .ok_or(VeflowLockError::WeightOverflow)?,
                    ..lock
                },
            );
            env.events().publish(
                (symbol_short!("lock_add"),),
                (user.clone(), amount),
            );
            return Self::get_lock(env, user).ok_or(VeflowLockError::NoLock);
        }

        let max_duration = Self::max_duration(&env)?;
        Self::validate_duration(duration, max_duration)?;
        Self::transfer_in(&env, &user, amount)?;
        let expiry_ledger = current_ledger
            .checked_add(duration as u32)
            .ok_or(VeflowLockError::InvalidDuration)?;
        let expiry_timestamp = current_timestamp
            .checked_add(duration.saturating_mul(SECONDS_PER_LEDGER))
            .ok_or(VeflowLockError::InvalidDuration)?;
        let lock = Lock {
            amount,
            start_ledger: current_ledger,
            expiry_ledger,
            start_timestamp: current_timestamp,
            expiry_timestamp,
            duration,
        };
        Self::store_lock(&env, &user, lock.clone());
        env.events().publish((symbol_short!("lock_new"),), (user, amount, duration));
        Ok(lock)
    }

    /// Extend the duration of an active lock.
    ///
    /// `new_duration` is the new total lock duration in ledgers. It must be
    /// strictly longer than the current duration and no longer than the
    /// maximum. The expiration is pushed out by the delta.
    pub fn extend_lock(env: Env, user: Address, new_duration: u64) -> Result<Lock, VeflowLockError> {
        user.require_auth();
        Self::check_initialized(&env)?;
        let max_duration = Self::max_duration(&env)?;
        Self::validate_duration(new_duration, max_duration)?;
        let current_ledger = env.ledger().sequence();
        let current_timestamp = env.ledger().timestamp();
        let mut lock = Self::get_lock(env.clone(), user.clone()).ok_or(VeflowLockError::NoLock)?;
        if current_ledger >= lock.expiry_ledger {
            return Err(VeflowLockError::LockExpired);
        }
        if new_duration <= lock.duration {
            return Err(VeflowLockError::DurationNotExtended);
        }
        let delta: u64 = new_duration - lock.duration;
        lock.duration = new_duration;
        lock.expiry_ledger = lock
            .expiry_ledger
            .checked_add(delta as u32)
            .ok_or(VeflowLockError::InvalidDuration)?;
        lock.expiry_timestamp = lock
            .expiry_timestamp
            .checked_add(delta.saturating_mul(SECONDS_PER_LEDGER))
            .ok_or(VeflowLockError::InvalidDuration)?;
        lock.start_ledger = current_ledger;
        lock.start_timestamp = current_timestamp;
        Self::store_lock(&env, &user, lock.clone());
        env.events().publish(
            (symbol_short!("lock_ext"),),
            (user, lock.duration, lock.expiry_ledger),
        );
        Ok(lock)
    }

    /// Add more FLOW to an active lock without changing its expiration.
    pub fn add_amount(env: Env, user: Address, amount: i128) -> Result<Lock, VeflowLockError> {
        user.require_auth();
        Self::check_initialized(&env)?;
        if amount <= 0 {
            return Err(VeflowLockError::InvalidAmount);
        }
        let current_ledger = env.ledger().sequence();
        let mut lock = Self::get_lock(env.clone(), user.clone()).ok_or(VeflowLockError::NoLock)?;
        if current_ledger >= lock.expiry_ledger {
            return Err(VeflowLockError::LockExpired);
        }
        Self::transfer_in(&env, &user, amount)?;
        lock.amount = lock
            .amount
            .checked_add(amount)
            .ok_or(VeflowLockError::WeightOverflow)?;
        Self::store_lock(&env, &user, lock.clone());
        env.events().publish((symbol_short!("lock_add"),), (user, amount));
        Ok(lock)
    }

    /// Withdraw FLOW once the lock has reached its expiration ledger sequence.
    ///
    /// Early unlocks are rejected with [`VeflowLockError::LockNotExpired`] until
    /// `env.ledger().sequence() >= expiry_ledger`.
    pub fn withdraw(env: Env, user: Address) -> Result<i128, VeflowLockError> {
        user.require_auth();
        Self::check_initialized(&env)?;
        let lock = Self::get_lock(env.clone(), user.clone()).ok_or(VeflowLockError::NoLock)?;
        if env.ledger().sequence() < lock.expiry_ledger {
            return Err(VeflowLockError::LockNotExpired);
        }
        let token = Self::token(&env)?;
        token::Client::new(&env, &token).transfer(
            &env.current_contract_address(),
            &user,
            &lock.amount,
        );
        env.storage().persistent().remove(&DataKey::Lock(user.clone()));
        env.events().publish((symbol_short!("lock_wd"),), (user, lock.amount));
        Ok(lock.amount)
    }

    /// Voting weight of `voter` at proposal time `proposed_at`.
    ///
    /// Implements the linear formula
    /// `Weight = Amount * (LockDuration / MaxDuration)` and returns 0 once the
    /// lock has expired by `proposed_at`. This is the cross-contract view the
    /// StellarFlow governance oracle expects from the veFLOW lock contract.
    pub fn get_voting_power(env: Env, voter: Address, proposed_at: u64) -> i128 {
        let Some(lock) = Self::get_lock(env.clone(), voter) else {
            return 0;
        };
        if proposed_at >= lock.expiry_timestamp {
            return 0;
        }
        let max_duration = match Self::max_duration(&env) {
            Ok(m) => m,
            Err(_) => return 0,
        };
        if lock.duration >= max_duration {
            return lock.amount;
        }
        let numerator = lock
            .amount
            .checked_mul(lock.duration as i128);
        match numerator {
            Some(n) => n / (max_duration as i128),
            None => 0,
        }
    }

    /// View the current lock for a user.
    pub fn get_lock(env: Env, user: Address) -> Option<Lock> {
        env.storage()
            .persistent()
            .get(&DataKey::Lock(user))
    }

    /// View the admin address.
    pub fn get_admin(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Admin)
    }

    /// View the FLOW token address.
    pub fn get_token(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Token)
    }

    /// View the configured maximum lock duration (in ledgers).
    pub fn get_max_duration(env: Env) -> Option<u64> {
        env.storage().instance().get(&DataKey::MaxDuration)
    }

    fn check_initialized(env: &Env) -> Result<(), VeflowLockError> {
        if env.storage().instance().has(&DataKey::Admin) {
            Ok(())
        } else {
            Err(VeflowLockError::NotInitialized)
        }
    }

    fn token(env: &Env) -> Result<Address, VeflowLockError> {
        env.storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(VeflowLockError::NotInitialized)
    }

    fn max_duration(env: &Env) -> Result<u64, VeflowLockError> {
        env.storage()
            .instance()
            .get(&DataKey::MaxDuration)
            .ok_or(VeflowLockError::NotInitialized)
    }

    fn validate_duration(
        duration: u64,
        max_duration: u64,
    ) -> Result<(), VeflowLockError> {
        if duration < MIN_LOCK_DURATION_LEDGERS || duration > max_duration {
            return Err(VeflowLockError::InvalidDuration);
        }
        Ok(())
    }

    fn transfer_in(env: &Env, from: &Address, amount: i128) -> Result<(), VeflowLockError> {
        let token = Self::token(env)?;
        token::Client::new(env, &token).transfer(from, &env.current_contract_address(), &amount);
        Ok(())
    }

    fn store_lock(env: &Env, user: &Address, lock: Lock) {
        env.storage().persistent().set(&DataKey::Lock(user.clone()), &lock);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{MockToken, MockTokenClient};
    use soroban_sdk::testutils::{Address as _, Ledger, LedgerInfo};
    use soroban_sdk::Env;

    fn setup() -> (Env, VeflowLockClient<'static>, Address, MockTokenClient<'static>, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let token_id = env.register_contract(None, MockToken);
        let stellar = MockTokenClient::new(&env, &token_id);

        let contract_id = env.register_contract(None, VeflowLock);
        let client = VeflowLockClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        client.initialize(&admin, &token_id, &MAX_LOCK_DURATION_LEDGERS);
        (env, client, token_id, stellar, admin)
    }

    fn advance(env: &Env, ledgers: u32) {
        let info = env.ledger().get();
        env.ledger().set(LedgerInfo {
            protocol_version: info.protocol_version,
            sequence_number: info.sequence_number + ledgers,
            timestamp: info.timestamp + (ledgers as u64 * SECONDS_PER_LEDGER),
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 0,
            min_persistent_entry_ttl: 0,
            max_entry_ttl: u32::MAX,
        });
    }

    #[test]
    fn test_initialize() {
        let (_env, client, token_id, _stellar, admin) = setup();
        assert_eq!(client.get_admin(), Some(admin));
        assert_eq!(client.get_token(), Some(token_id));
        assert_eq!(client.get_max_duration(), Some(MAX_LOCK_DURATION_LEDGERS));
    }

    #[test]
    fn test_lock_creates_lock_and_full_weight() {
        let (env, client, token_id, stellar, _admin) = setup();
        let user = Address::generate(&env);
        stellar.mint(&user, &1_000);

        let lock = client.lock(&user, &1_000, &MAX_LOCK_DURATION_LEDGERS);
        assert_eq!(lock.amount, 1_000);
        assert_eq!(lock.duration, MAX_LOCK_DURATION_LEDGERS);
        assert_eq!(client.get_voting_power(&user, &0), 1_000);

        let token = MockTokenClient::new(&env, &token_id);
        assert_eq!(token.balance(&client.address), 1_000);
    }

    #[test]
    fn test_voting_power_scales_linear_with_duration() {
        let env = Env::default();
        env.mock_all_auths();
        let token_id = env.register_contract(None, MockToken);
        let stellar = MockTokenClient::new(&env, &token_id);
        let contract_id = env.register_contract(None, VeflowLock);
        let client = VeflowLockClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        client.initialize(&admin, &token_id, &MAX_LOCK_DURATION_LEDGERS);

        let user = Address::generate(&env);
        stellar.mint(&user, &2_000);
        // Half of the max duration -> half of the voting weight.
        client.lock(&user, &2_000, &(MAX_LOCK_DURATION_LEDGERS / 2));
        assert_eq!(client.get_voting_power(&user, &0), 1_000);
    }

    #[test]
    fn test_lock_rejects_duration_below_minimum() {
        let (_env, client, _token_id, stellar, _admin) = setup();
        let user = Address::generate(&_env);
        stellar.mint(&user, &1_000);
        let result = client.try_lock(&user, &1_000, &(MIN_LOCK_DURATION_LEDGERS - 1));
        assert_eq!(result, Err(Ok(VeflowLockError::InvalidDuration)));
    }

    #[test]
    fn test_lock_rejects_duration_above_maximum() {
        let (_env, client, _token_id, stellar, _admin) = setup();
        let user = Address::generate(&_env);
        stellar.mint(&user, &1_000);
        let result = client.try_lock(&user, &1_000, &(MAX_LOCK_DURATION_LEDGERS + 1));
        assert_eq!(result, Err(Ok(VeflowLockError::InvalidDuration)));
    }

    #[test]
    fn test_lock_adds_to_existing_lock() {
        let (_env, client, _token_id, stellar, _admin) = setup();
        let user = Address::generate(&_env);
        stellar.mint(&user, &3_000);
        client.lock(&user, &1_000, &MAX_LOCK_DURATION_LEDGERS);
        let lock = client.lock(&user, &2_000, &MAX_LOCK_DURATION_LEDGERS);
        assert_eq!(lock.amount, 3_000);
        assert_eq!(client.get_voting_power(&user, &0), 3_000);
    }

    #[test]
    fn test_extend_lock_increases_expiry_and_weight() {
        let (_env, client, _token_id, stellar, _admin) = setup();
        let user = Address::generate(&_env);
        stellar.mint(&user, &2_000);
        client.lock(&user, &2_000, &(MAX_LOCK_DURATION_LEDGERS / 2));
        assert_eq!(client.get_voting_power(&user, &0), 1_000);

        let extended = client.extend_lock(&user, &MAX_LOCK_DURATION_LEDGERS);
        assert_eq!(extended.duration, MAX_LOCK_DURATION_LEDGERS);
        assert_eq!(client.get_voting_power(&user, &0), 2_000);
    }

    #[test]
    fn test_extend_lock_rejects_shorter_duration() {
        let (_env, client, _token_id, stellar, _admin) = setup();
        let user = Address::generate(&_env);
        stellar.mint(&user, &1_000);
        client.lock(&user, &1_000, &MAX_LOCK_DURATION_LEDGERS);
        let result = client.try_extend_lock(&user, &(MAX_LOCK_DURATION_LEDGERS - 1));
        assert_eq!(result, Err(Ok(VeflowLockError::DurationNotExtended)));
    }

    #[test]
    fn test_add_amount_increases_lock_without_changing_expiry() {
        let (_env, client, _token_id, stellar, _admin) = setup();
        let user = Address::generate(&_env);
        stellar.mint(&user, &2_000);
        let original = client.lock(&user, &1_000, &MAX_LOCK_DURATION_LEDGERS);
        let expiry_before = original.expiry_ledger;
        let updated = client.add_amount(&user, &1_000);
        assert_eq!(updated.amount, 2_000);
        assert_eq!(updated.expiry_ledger, expiry_before);
        assert_eq!(client.get_voting_power(&user, &0), 2_000);
    }

    #[test]
    fn test_withdraw_before_expiry_rejected() {
        let (env, client, _token_id, stellar, _admin) = setup();
        let user = Address::generate(&env);
        stellar.mint(&user, &1_000);
        client.lock(&user, &1_000, &MAX_LOCK_DURATION_LEDGERS);
        // Advance fewer ledgers than the lock duration.
        advance(&env, 100);
        let result = client.try_withdraw(&user);
        assert_eq!(result, Err(Ok(VeflowLockError::LockNotExpired)));
        assert_eq!(client.get_lock(&user).unwrap().amount, 1_000);
    }

    #[test]
    fn test_withdraw_after_expiry_returns_locked_amount() {
        let (env, client, token_id, stellar, _admin) = setup();
        let user = Address::generate(&env);
        stellar.mint(&user, &1_000);
        client.lock(&user, &1_000, &MIN_LOCK_DURATION_LEDGERS);
        // Advance past the expiry ledger sequence.
        advance(&env, MIN_LOCK_DURATION_LEDGERS as u32 + 1);
        let withdrawn = client.withdraw(&user);
        assert_eq!(withdrawn, 1_000);
        assert!(client.get_lock(&user).is_none());

        let token = MockTokenClient::new(&env, &token_id);
        assert_eq!(token.balance(&user), 1_000);
        assert_eq!(token.balance(&client.address), 0);
    }

    #[test]
    fn test_voting_power_is_zero_without_lock() {
        let (_env, client, _token_id, _stellar, _admin) = setup();
        let user = Address::generate(&_env);
        assert_eq!(client.get_voting_power(&user, &0), 0);
    }

    #[test]
    fn test_voting_power_zero_after_expiry_time() {
        let (env, client, _token_id, stellar, _admin) = setup();
        let user = Address::generate(&env);
        stellar.mint(&user, &1_000);
        client.lock(&user, &1_000, &MIN_LOCK_DURATION_LEDGERS);
        let expiry = client.get_lock(&user).unwrap().expiry_timestamp;
        advance(&env, MIN_LOCK_DURATION_LEDGERS as u32 + 1);
        // A proposed_at past the expiry timestamp yields zero voting weight.
        assert_eq!(client.get_voting_power(&user, &(expiry + 1)), 0);
    }
}