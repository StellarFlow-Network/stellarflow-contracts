#![no_std]

//! Cross-Chain Bridge Token Reclaim Emergency Rescue Handler (issue #812).
//!
//! This contract is a minimal, self-contained companion to a cross-chain bridge.
//! It does **not** implement the bridge itself — no cross-chain messaging, no
//! relayer network, no signature-scheme verification of an off-chain proof
//! payload. It only implements the piece the issue asked for: a safe way to
//! unlock funds that a real bridge locked here, if cross-chain delivery on the
//! other side permanently fails.
//!
//! ## Flow
//! 1. [`BridgeRescue::initialize`] configures an M-of-N admin committee, a
//!    separate validator set used for consensus proof-of-failure, and the SAC
//!    token that gets bridged.
//! 2. [`BridgeRescue::lock_tokens`] deposits `amount` of the token into the
//!    contract on behalf of `sender`, representing a cross-chain bridge lock,
//!    and returns a `lock_id`.
//! 3. Each validator calls [`BridgeRescue::submit_failure_proof`] to attest
//!    that cross-chain delivery for `lock_id` has permanently failed. An
//!    on-chain vote from an authorized, `require_auth`'d validator address
//!    *is* the "consensus proof" this contract cares about — once distinct
//!    attestations reach `validator_threshold`, the lock's failure proof is
//!    marked confirmed.
//! 4. Each admin calls [`BridgeRescue::approve_rescue`] to approve returning
//!    the funds. Once distinct approvals reach the admin `threshold` **and**
//!    the validator failure-proof is confirmed **and** the lock is still
//!    `Locked`, the rescue executes automatically as part of that call: the
//!    full `amount` is transferred back to the original `sender`, the lock is
//!    marked `Rescued`, and a `BridgeTokensRescued` event is emitted.
//!
//! ## Execution trigger design decision
//! The last vote to cross either threshold (`submit_failure_proof` crossing
//! `validator_threshold`, or `approve_rescue` crossing `threshold`) triggers
//! execution directly inside that call — no separate "execute" transaction is
//! required in the common case. A permissionless [`BridgeRescue::execute_rescue`]
//! is also provided as a fallback/keeper entry point for the case where the
//! thresholds were already met by other means (e.g. threshold config edge
//! cases) but nothing has attempted execution yet; it re-checks every
//! condition and panics with `Error::ThresholdNotReached` if the lock isn't
//! actually ready, so it can never bypass the consensus gate.
//!
//! ## Exactly-once guarantee
//! A lock can only ever leave the `Locked` status once, transitioning
//! directly to the terminal `Rescued` status inside the same storage write
//! that performs the token transfer. Every entry point that can lead to a
//! transfer (`approve_rescue`'s auto-trigger and `execute_rescue`) re-reads
//! the lock's current status immediately before transferring and panics with
//! `Error::LockNotLocked` if it is not `Locked`. Because Soroban contract
//! invocations are atomic, there is no window in which two concurrent calls
//! can both observe `Locked` and both transfer — the first to run
//! `env.storage()...set(status = Rescued)` closes the door for every
//! subsequent call within the same or a later transaction.

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, token,
    Address, Env, String, Vec,
};

use crate::types::{BridgeLock, DataKey, LockStatus};

pub mod types;

#[cfg(test)]
mod test;

/// Error types for the bridge rescue contract.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    /// Contract has not been initialized yet.
    NotInitialized = 1,
    /// Contract has already been initialized.
    AlreadyInitialized = 2,
    /// `threshold` must be > 0 and <= the size of the corresponding set.
    InvalidThreshold = 3,
    /// `admins` or `validators` contained a duplicate address.
    DuplicateAddress = 4,
    /// `amount` must be greater than zero.
    ZeroAmount = 5,
    /// No `BridgeLock` exists for the given lock id.
    LockNotFound = 6,
    /// Caller is not a member of the admin committee.
    NotAdmin = 7,
    /// Caller is not a member of the validator set.
    NotValidator = 8,
    /// This address has already voted/approved for this lock.
    DuplicateVote = 9,
    /// The lock is not in `Locked` status (already rescued, or otherwise not open).
    LockNotLocked = 10,
    /// Validator consensus and/or admin approval threshold has not been reached yet.
    ThresholdNotReached = 11,
    /// An arithmetic operation would have overflowed.
    Overflow = 12,
}

/// Emitted when tokens are locked into the bridge on behalf of `sender`.
///
/// soroban-sdk 20.x (pinned by this workspace) has no `#[contractevent]` /
/// `publish_event` convenience API (that landed in a later major version) —
/// events here use the plain `#[contracttype]` + `env.events().publish(topics,
/// data)` form instead, matching the pattern already used elsewhere in this
/// workspace (see `price-oracle/src/event_topics.rs`).
#[contracttype]
pub struct BridgeTokensLocked {
    pub lock_id: u64,
    pub sender: Address,
    pub amount: i128,
}

/// Emitted each time a validator submits a failure-proof attestation for a lock.
#[contracttype]
pub struct FailureProofSubmitted {
    pub lock_id: u64,
    pub validator: Address,
    pub vote_count: u32,
}

/// Emitted each time an admin approves the rescue of a lock.
#[contracttype]
pub struct RescueApproved {
    pub lock_id: u64,
    pub admin: Address,
    pub approval_count: u32,
}

/// Emitted when a lock is successfully rescued: funds are unlocked back to
/// the original sender. This is the deliverable event required by issue #812.
#[contracttype]
pub struct BridgeTokensRescued {
    pub lock_id: u64,
    pub sender: Address,
    pub amount: i128,
}

#[contract]
pub struct BridgeRescue;

/// Returns `Err(Error::NotInitialized)` unless `initialize` has run.
///
/// Deliberately returns a `Result` (propagated via `?`) rather than
/// panicking: soroban-sdk 20.x (pinned by this workspace) contract
/// dispatch handles an `Err` return from a `#[contractimpl]` method as a
/// normal, structured failure, whereas an actual Rust panic has to survive
/// a panic/unwind round trip through the host — routine, fully-expected
/// validation failures use the former on every entrypoint below.
fn require_initialized(env: &Env) -> Result<(), Error> {
    if !env.storage().instance().has(&DataKey::Initialized) {
        return Err(Error::NotInitialized);
    }
    Ok(())
}

fn has_duplicate_addresses(addrs: &Vec<Address>) -> bool {
    let len = addrs.len();
    for i in 0..len {
        let a = addrs.get(i).unwrap();
        for j in (i + 1)..len {
            let b = addrs.get(j).unwrap();
            if a == b {
                return true;
            }
        }
    }
    false
}

fn get_admins(env: &Env) -> Vec<Address> {
    env.storage()
        .instance()
        .get(&DataKey::Admins)
        .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized))
}

fn get_admin_threshold(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&DataKey::AdminThreshold)
        .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized))
}

fn get_validators(env: &Env) -> Vec<Address> {
    env.storage()
        .instance()
        .get(&DataKey::Validators)
        .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized))
}

fn get_validator_threshold(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&DataKey::ValidatorThreshold)
        .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized))
}

fn get_token(env: &Env) -> Address {
    env.storage()
        .instance()
        .get(&DataKey::Token)
        .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized))
}

fn get_lock_checked(env: &Env, lock_id: u64) -> Result<BridgeLock, Error> {
    env.storage()
        .persistent()
        .get(&DataKey::Lock(lock_id))
        .ok_or(Error::LockNotFound)
}

fn save_lock(env: &Env, lock: &BridgeLock) {
    env.storage().persistent().set(&DataKey::Lock(lock.id), lock);
}

fn validator_vote_count(env: &Env, lock_id: u64) -> u32 {
    env.storage()
        .persistent()
        .get(&DataKey::ValidatorVoteCount(lock_id))
        .unwrap_or(0)
}

fn admin_approval_count(env: &Env, lock_id: u64) -> u32 {
    env.storage()
        .persistent()
        .get(&DataKey::AdminApprovalCount(lock_id))
        .unwrap_or(0)
}

/// Returns `true` if `lock` currently satisfies every condition required to
/// execute the rescue: validator failure-proof confirmed, admin approval
/// threshold met, and the lock is still `Locked`.
fn is_ready_for_rescue(env: &Env, lock: &BridgeLock) -> bool {
    if lock.status != LockStatus::Locked {
        return false;
    }
    if !lock.validator_confirmed {
        return false;
    }
    let threshold = get_admin_threshold(env);
    admin_approval_count(env, lock.id) >= threshold
}

/// Performs the actual asset transfer and terminal state transition.
///
/// Callers must have already verified `is_ready_for_rescue`. This function
/// re-checks the lock's status itself immediately before transferring, so it
/// is the single choke point that makes a double-rescue structurally
/// impossible: the very first thing it does after the status check is flip
/// the lock to `Rescued` and persist it, before any further logic runs.
fn perform_rescue(env: &Env, mut lock: BridgeLock) -> Result<(), Error> {
    if lock.status != LockStatus::Locked {
        return Err(Error::LockNotLocked);
    }

    lock.status = LockStatus::Rescued;
    save_lock(env, &lock);

    let token_client = token::Client::new(env, &get_token(env));
    token_client.transfer(&env.current_contract_address(), &lock.sender, &lock.amount);

    env.events().publish(
        (symbol_short!("bridgersc"),),
        BridgeTokensRescued {
            lock_id: lock.id,
            sender: lock.sender.clone(),
            amount: lock.amount,
        },
    );

    Ok(())
}

#[contractimpl]
impl BridgeRescue {
    /// Initialize the contract with an M-of-N admin committee, a validator
    /// set used for consensus proof-of-failure, and the SAC token that gets
    /// bridged. Can only be called once.
    ///
    /// Panics with `Error::InvalidThreshold` if either threshold is `0` or
    /// greater than the size of its corresponding set, and with
    /// `Error::DuplicateAddress` if `admins` or `validators` contain a
    /// repeated address.
    pub fn initialize(
        env: Env,
        admins: Vec<Address>,
        threshold: u32,
        validators: Vec<Address>,
        validator_threshold: u32,
        token: Address,
    ) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Initialized) {
            return Err(Error::AlreadyInitialized);
        }

        if threshold == 0 || threshold > admins.len() {
            return Err(Error::InvalidThreshold);
        }
        if validator_threshold == 0 || validator_threshold > validators.len() {
            return Err(Error::InvalidThreshold);
        }
        if has_duplicate_addresses(&admins) || has_duplicate_addresses(&validators) {
            return Err(Error::DuplicateAddress);
        }

        env.storage().instance().set(&DataKey::Admins, &admins);
        env.storage()
            .instance()
            .set(&DataKey::AdminThreshold, &threshold);
        env.storage()
            .instance()
            .set(&DataKey::Validators, &validators);
        env.storage()
            .instance()
            .set(&DataKey::ValidatorThreshold, &validator_threshold);
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage()
            .instance()
            .set(&DataKey::NextLockId, &0u64);
        env.storage().instance().set(&DataKey::Initialized, &true);

        Ok(())
    }

    /// Deposit `amount` of the bridge token into the contract on behalf of
    /// `sender`, representing a cross-chain bridge lock. Requires `sender`'s
    /// authorization and transfers the tokens from `sender` into the
    /// contract's custody. Returns the newly created lock id.
    pub fn lock_tokens(
        env: Env,
        sender: Address,
        amount: i128,
        dest_chain_ref: String,
    ) -> Result<u64, Error> {
        require_initialized(&env)?;
        sender.require_auth();

        if amount <= 0 {
            return Err(Error::ZeroAmount);
        }

        let token_client = token::Client::new(&env, &get_token(&env));
        token_client.transfer(&sender, &env.current_contract_address(), &amount);

        let next_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextLockId)
            .unwrap_or(0);
        let lock_id = next_id;
        let new_next_id = next_id.checked_add(1).ok_or(Error::Overflow)?;
        env.storage()
            .instance()
            .set(&DataKey::NextLockId, &new_next_id);

        let lock = BridgeLock {
            id: lock_id,
            sender: sender.clone(),
            amount,
            status: LockStatus::Locked,
            dest_chain_ref,
            validator_confirmed: false,
        };
        save_lock(&env, &lock);

        env.events().publish(
            (symbol_short!("bridgelck"),),
            BridgeTokensLocked {
                lock_id,
                sender,
                amount,
            },
        );

        Ok(lock_id)
    }

    /// Validator attestation that cross-chain delivery for `lock_id` has
    /// permanently failed. Requires `validator`'s authorization and that
    /// `validator` is a member of the configured validator set. Each
    /// validator may vote at most once per lock.
    ///
    /// An on-chain vote from an authorized validator address *is* the
    /// consensus proof for this contract's purposes — no off-chain signature
    /// payload is verified here. Once distinct attestations reach
    /// `validator_threshold`, the lock's failure-proof is marked confirmed.
    /// If, at that point, the admin approval threshold has already been
    /// reached too, the rescue executes immediately as part of this call.
    pub fn submit_failure_proof(
        env: Env,
        validator: Address,
        lock_id: u64,
        _sig_or_attestation: String,
    ) -> Result<(), Error> {
        require_initialized(&env)?;
        validator.require_auth();

        if !get_validators(&env).contains(&validator) {
            return Err(Error::NotValidator);
        }

        let mut lock = get_lock_checked(&env, lock_id)?;
        if lock.status != LockStatus::Locked {
            return Err(Error::LockNotLocked);
        }

        let vote_key = DataKey::ValidatorVote(lock_id, validator.clone());
        if env.storage().persistent().has(&vote_key) {
            return Err(Error::DuplicateVote);
        }
        env.storage().persistent().set(&vote_key, &true);

        let count = validator_vote_count(&env, lock_id)
            .checked_add(1)
            .ok_or(Error::Overflow)?;
        env.storage()
            .persistent()
            .set(&DataKey::ValidatorVoteCount(lock_id), &count);

        env.events().publish(
            (symbol_short!("failproof"),),
            FailureProofSubmitted {
                lock_id,
                validator,
                vote_count: count,
            },
        );

        if !lock.validator_confirmed && count >= get_validator_threshold(&env) {
            lock.validator_confirmed = true;
            save_lock(&env, &lock);
        }

        if is_ready_for_rescue(&env, &lock) {
            perform_rescue(&env, lock)?;
        }

        Ok(())
    }

    /// Admin approval to rescue `lock_id`. Requires `admin`'s authorization
    /// and that `admin` is a member of the configured admin committee. Each
    /// admin may approve at most once per lock.
    ///
    /// Once distinct approvals reach the admin `threshold` **and** the
    /// validator failure-proof is confirmed **and** the lock is still
    /// `Locked`, the rescue executes immediately as part of this call: the
    /// locked `amount` is transferred back to the original `sender`, the
    /// lock is marked `Rescued`, and a `BridgeTokensRescued` event is
    /// emitted.
    pub fn approve_rescue(env: Env, admin: Address, lock_id: u64) -> Result<(), Error> {
        require_initialized(&env)?;
        admin.require_auth();

        if !get_admins(&env).contains(&admin) {
            return Err(Error::NotAdmin);
        }

        let lock = get_lock_checked(&env, lock_id)?;
        if lock.status != LockStatus::Locked {
            return Err(Error::LockNotLocked);
        }

        let approval_key = DataKey::AdminApproval(lock_id, admin.clone());
        if env.storage().persistent().has(&approval_key) {
            return Err(Error::DuplicateVote);
        }
        env.storage().persistent().set(&approval_key, &true);

        let count = admin_approval_count(&env, lock_id)
            .checked_add(1)
            .ok_or(Error::Overflow)?;
        env.storage()
            .persistent()
            .set(&DataKey::AdminApprovalCount(lock_id), &count);

        env.events().publish(
            (symbol_short!("rescappr"),),
            RescueApproved {
                lock_id,
                admin,
                approval_count: count,
            },
        );

        if is_ready_for_rescue(&env, &lock) {
            perform_rescue(&env, lock)?;
        }

        Ok(())
    }

    /// Permissionless fallback/keeper entry point: executes the rescue for
    /// `lock_id` if every condition is already satisfied (validator
    /// consensus confirmed, admin threshold met, lock still `Locked`).
    ///
    /// In the normal flow the last `submit_failure_proof` or `approve_rescue`
    /// call that crosses its respective threshold triggers execution
    /// automatically, so this entry point is not required for the happy
    /// path. It exists purely so a stuck lock that somehow met every
    /// condition without triggering execution can still be swept, and it
    /// re-validates every condition itself — it can never bypass consensus.
    ///
    /// Panics with `Error::ThresholdNotReached` if the lock is not yet ready,
    /// and with `Error::LockNotLocked` if it has already been rescued.
    pub fn execute_rescue(env: Env, lock_id: u64) -> Result<(), Error> {
        require_initialized(&env)?;

        let lock = get_lock_checked(&env, lock_id)?;
        if lock.status != LockStatus::Locked {
            return Err(Error::LockNotLocked);
        }
        if !is_ready_for_rescue(&env, &lock) {
            return Err(Error::ThresholdNotReached);
        }

        perform_rescue(&env, lock)
    }

    /// Get the full details/status of a lock, or `None` if it does not exist.
    pub fn get_lock(env: Env, lock_id: u64) -> Option<BridgeLock> {
        env.storage().persistent().get(&DataKey::Lock(lock_id))
    }

    /// Number of distinct validator attestations recorded so far for `lock_id`.
    pub fn get_validator_vote_count(env: Env, lock_id: u64) -> u32 {
        validator_vote_count(&env, lock_id)
    }

    /// Whether `validator` has already submitted a failure-proof attestation
    /// for `lock_id`.
    pub fn has_validator_voted(env: Env, lock_id: u64, validator: Address) -> bool {
        env.storage()
            .persistent()
            .has(&DataKey::ValidatorVote(lock_id, validator))
    }

    /// Number of distinct admin approvals recorded so far for `lock_id`.
    pub fn get_admin_approval_count(env: Env, lock_id: u64) -> u32 {
        admin_approval_count(&env, lock_id)
    }

    /// Whether `admin` has already approved the rescue for `lock_id`.
    pub fn has_admin_approved(env: Env, lock_id: u64, admin: Address) -> bool {
        env.storage()
            .persistent()
            .has(&DataKey::AdminApproval(lock_id, admin))
    }

    /// Returns the configured admin committee.
    pub fn get_admins(env: Env) -> Vec<Address> {
        get_admins(&env)
    }

    /// Returns the configured admin approval threshold.
    pub fn get_admin_threshold(env: Env) -> u32 {
        get_admin_threshold(&env)
    }

    /// Returns the configured validator set.
    pub fn get_validators(env: Env) -> Vec<Address> {
        get_validators(&env)
    }

    /// Returns the configured validator consensus threshold.
    pub fn get_validator_threshold(env: Env) -> u32 {
        get_validator_threshold(&env)
    }

    /// Returns the configured bridge token address.
    pub fn get_token(env: Env) -> Address {
        get_token(&env)
    }
}
