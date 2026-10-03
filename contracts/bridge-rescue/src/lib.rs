#`!no_std]

//! Cross-Chain Bridge Token Reclaim Emergency Rescue Handler (issue #812).
//!
//! This contract is a minimal, self-contained companion to a cross-chain bridge.
//! It does **not** implement the bridge itself — no cross-chain messaging, no
//! relayer network, no signature-scheme verification of an off-chain proof
//! payload. It only implements the piece the issue asked for: a safe way to
//! unlock funds that a real bridge locked here, if cross-chain delivery on the
//! other side permanently fails.
//!
//## Flow
//! 1. [`BridgeRescue::initialize`] configures an M-of-N admin committee, a
//!    separate validator set used for consensus proof-of-failure, and the SAC
//#    token that gets bridged.
//! 2. [`BridgeRescue::lock_tokens`] deposits `amount` of the token into the
//!    contract on behalf of `sender`, representing a cross-chain bridge lock,
//!    and returns a `lock_id`.
//! 3. Each validator calls [`BridgeRescue::submit_failure_proof`] to attest
//#    that cross-chain delivery for `lock_id` has permanently failed. An
//!    on-chain vote from an authorized, `require_auth`'d validator address
//#    *is* the "consensus proof" this contract cares about — once distinct
//#    attestations reach `validator_threshold`, the lock's failure proof is
//!    marked confirmed.
//! 4. Each admin calls [`BridgeRescue::approve_rescue`] to approve returning
//#    the funds. Once distinct approvals reach the admin `threshold` **and**
//#    the validator failure-proof is confirmed **and** the lock is still
//!    `Locked`, the rescue executes automatically as part of that call: the
//!    full `amount` is transferred back to the original `sender`, the lock is
//!    marked `Rescued`, and a `BridgeTokensRescued` event is emitted.
//!
//! ## Execution trigger design decision
//! The last vote to cross either threshold (`submit_failure_proof` crossing
//! @validator_threshold`, or `approve_rescue` crossing `threshold`) triggers
//! execution directly inside that call — no separate "execute" transaction is
//! required in the common case. A permissionless [`BridgeRescue::execute_rescue`]
//! is also provided as a fallback/keeper entry point for the case where the
//! thresholds were already met by other means (e.g. threshold config edge
//! cases) but nothing has attempted execution yet; it re-checks every
//! condition and panics with `ContractError::ThresholdNotReached` if the lock isn't
//! actually ready, so it can never bypass the consensus gate.
//!
//## Exactly-once guarantee
//! A lock can only ever leave the `Locked` status once, transitioning
//! directly to the terminal `Rescued` status inside the same storage write
//! that performs the token transfer. Every entry point that can lead to a
//! transfer (`approve_rescue`'s auto-trigger and `execute_rescue`) re-reads
//! the lock's current status immediately before transferring and panics with
//! `ContractError::LockNotLocked` if it is not `Locked`. Because Soroban contract
//! invocations are atomic, there is no window in which two concurrent calls
//! can both observe `Locked` and both transfer — the first to run
//! `env.storage()...set(status = Rescued)` closes the door for every
//! subsequent call within the same or a later transaction.
//!
//! ### Timeout refund state machine (issue #812)
//! Every lock records the claim timestamp `t_claim` at which it was created and
//! the configured timeout window `Timeout`. The expiration instant is
//! `t_claim + Timeout`. If no validator signatures (failure proofs) have been
//! presented before that instant, anyone may call [`BridgeRescue::refund_expired`]
//! to un-escrow the underlying assets and return them to the original sender.
//! The lock is then recorded as `CancelledExpired` in state.

 use soroban_sdk {
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
pub enum ContractError {
    /// Contract has not been initialized yet.
    /// Recovery steps: Inspect the state for NotInitialized and retry with valid inputs or proper conditions.
    NotInitialized = 1,
    /// Contract has already been initialized.
    /// Recovery steps: Inspect the state for AlreadyInitialized and retry with valid inputs or proper conditions.
    AlreadyInitialized = 2,
    /// `threshold` must be > 0 and <= the size of the corresponding set.
    /// Recovery steps: Inspect the state for InvalidThreshold and retry with valid inputs or proper conditions.
    InvalidThreshold = 3,
    /// `admins` or `validators` contained a duplicate address.
    /// Recovery steps: Inspect the state for DuplicateAddress and retry with valid inputs or proper conditions.
    DuplicateAddress = 4,
    /// `amount` must be greater than zero.
    /// Recovery steps: Inspect the state for ZeroAmount and retry with valid inputs or proper conditions.
    ZeroAmount = 5,
    /// No `BridgeLock` exists for the given lock id.
    /// Recovery steps: Inspect the state for LockNotFound and retry with valid inputs or proper conditions.
    LockNotFound = 6,
    /// Caller is not a member of the admin committee.
    /// Recovery steps: Inspect the state for NotAdmin and retry with valid inputs or proper conditions.
    NotAdmin = 7,
    /// Caller is not a member of the validator set.
    /// Recovery steps: Inspect the state for NotValidator and retry with valid inputs or proper conditions.
    NotValidator = 8,
    /// This address has already voted/approved for this lock.
    /// Recovery steps: Inspect the state for DuplicateVote and retry with valid inputs or proper conditions.
    DuplicateVote = 9,
    /// The lock is not in `Locked` status (already rescued, or otherwise not open).
    /// Recovery steps: Inspect the state for LockNotLocked and retry with valid inputs or proper conditions.
    LockNotLocked = 10,
    /// Validator consensus and/or admin approval threshold has not been reached yet.
    /// Recovery steps: Inspect the state for ThresholdNotReached and retry with valid inputs or proper conditions.
    ThresholdNotReached = 11,
    /// An arithmetic operation would have overflowed.
    /// Recovery steps: Inspect the state for Overflow and retry with valid inputs or proper conditions.
    Overflow = 12,
    /// The claim timeout window has not yet expired.
    TimeoutNotExpired = 13,
    /// The claim timeout window has already expired; rescue is no longer available.
    TimeoutExpired = 14,
}

/// Emitted when tokens are locked into the bridge on behalf of `sender`.
///
/// soroban-sdk 20.x (pinned by this workspace) has no `#[contractevent] /
/// `publish_event` convenience API (that landed in a later major version) —
/// events here use the plain `#[contracttype], + `env.events().publish(topics,
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

/// Emitted when a lock expires without validator signatures and the
/// underlying assets are refunded to the original sender.
#[contracttype]
pub struct BridgeTokensRefunded {
    pub lock_id: u64,
    pub sender: Address,
    pub amount: i128,
}

#[contract]
pub struct BridgeRescue;

/// Returns `Err(ContractError::NotInitialized)` unless `initialize` has run.
///
/// Deliberately returns a `Result` (propagated via `?`) rather than
/// panicking: soroban-sdk 20.x (pinned by this workspace) contract
/// dispatch handles an `Err` return from a `#[contractimpl]` method as a
/// normal, structured failure, whereas an actual Rust panic has to survive
/// a panic/unwind round trip through the host — routine, fully-expected
/// validation failures use the former on every entrypoint below.
fn require_initialized(env: &Env) -> Result<(), ContractError> {
    if !env.storage().instance().has(&DataKey::Initialized) {
        return Err(ContractError::NotInitialized);
    }
    Ok()
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
        .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized))
}

fn get_admin_threshold(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&DataKey::AdminThreshold)
        .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized))
}

fn get_validators(env: &Env) -> Vec<Address> {
    env.storage()
        .instance()
        .get(&DataKey::Validators)
        .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized))
}

fn get_validator_threshold(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&DataKey::ValidatorThreshold)
        .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized))
}

fn get_token(env: &Env) -> Address {
    env.storage()
        .instance()
        .get(&DataKey::Token)
        .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized))
}

fn get_timeout(env: &Env) -> u64 {
    env.storage()
        .instance()
        .get(&DataKey::Timeout)
        .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized))
}

fn get_lock_checked(env: &Env, lock_id: u64) -> Result<BridgeLock, Error> {
    env.storage()
        .persistent()
        .get(&DataKey::Lock(lock_id))
        .ok_or(ContractError::LockNotFound)
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

/// Returns the expiration instant `t_claim + Timeout` for a lock.
/// Overflow-checked so a malicious or corrupt `claim_timestamp` cannot wrap
/// around to a value that never expires.
fn lock_expiration_instant(env: &Env, lock: &BridgeLock) -> Result<u64, Error> {
    let timeout = get_timeout(env);
    lock
        .claim_timestamp
        .checked_add(timeout)
        .ok_or(Error::Overflow)
}

/// Returns `true` if the lock's claim timeout window has elapsed as of the
/// current ledger timestamp.
///
/// The expiration instant is the first ledger timestamp at which the window
/// is considered closed: `now >= t_claim + Timeout`.
fn is_expired(env: &Env, lock: &BridgeLock) -> Result<bool, Error> {
    let expiration = lock_expiration_instant(env, lock)?;
    Ok(env.ledger().timestamp() >= expiration)
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
fn perform_rescue(env: &Env, mut lock: BridgeLock) -> Result<(), ContractError> {
    if lock.status != LockStatus::Locked {
        return Err(ContractError::LockNotLocked);
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

    Ok()
}

/// Performs the timeout refund: un-escrows the underlying assets and
/// returns them to the original sender, then records the lock as
/// `CancelledExpired`.
///
/// Like `perform_rescue`, this is the single choke point for the timeout
/// path: it re-checks the lock status and the expiration condition itself,
/// flips the lock to the terminal `CancelledExpired` status and persists it
/// before transferring, so a double-refund is impossible.
fn perform_refund(env: &Env, mut lock: BridgeLock) -> Result<(), Error> {
    if lock.status != LockStatus::Locked {
        return Err(Error::LockNotLocked);
    }

    if !is_expired(env, &lock)? {
        return Err(Error::TimeoutNotExpired);
    }

    lock.status = LockStatus::CancelledExpired;
    save_lock(env, &lock);

    let token_client = token::Client::new(env, &get_token(env));
    token_client.transfer(&env.current_contract_address(), &lock.sender, &lock.amount);

    env.events().publish(
        (symbol_short!("bridgeref"),),
        BridgeTokensRefunded {
            lock_id: lock.id,
            sender: lock.sender.clone(),
            amount: lock.amount,
        },
    );

    Ok()
}

#[contractimpl]
impl BridgeRescue {
    /// Initialize the contract with an M-of-N admin committee, a validator
    /// set used for consensus proof-of-failure, the SAC token that gets
    /// bridged, and the claim timeout window (issue #812). Can only be called once.
    ///
    /// Panics with `ContractError::InvalidThreshold` if either threshold is `0` or
    /// greater than the size of its corresponding set, and with
    /// `ContractError::DuplicateAddress` if `admins` or `validators` contain a
    /// repeated address.
    pub fn initialize(
        env: Env,
        admins: Vec<Address>,
        threshold: u32,
        validators: Vec<Address>,
        validator_threshold: u32,
        token: Address,
        timeout: u64,
    ) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Initialized) {
            return Err(ContractError::AlreadyInitialized);
        }

        if threshold == 0 || threshold > admins.len() {
            return Err(ContractError::InvalidThreshold);
        }
        if validator_threshold == 0 || validator_threshold > validators.len() {
            return Err(ContractError::InvalidThreshold);
        }
        if has_duplicate_addresses(&admins) || has_duplicate_addresses(&validators) {
            return Err(ContractError::DuplicateAddress);
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
        env.storage().instance().set(&DataKey::Timeout, &timeout);
        env.storage()
            .instance()
            .set(&DataKey::NextLockId, &u64);
        env.storage().instance().set(&DataKey::Initialized, &true);

        Ok()
    }

    /// Deposits `amount` of the bridged token into the contract on behalf of
    /// `sender`, representing a cross-chain bridge lock. Records the claim
    /// timestamp `t_claim` (the current ledger timestamp) so the expiration
    /// instant `t_claim + Timeout` can be derived later.
    pub fn lock_tokens(env: Env, sender: Address, amount: i128) -> Result<u64, Error> {
        require_initialized(&env)?;
        sender.require_auth();

        if amount <= 0 {
            return Err(ContractError::ZeroAmount);
        }

        let lock_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextLockId)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        let next_id = lock_id
            .checked_add(1)
            .ok_or(Error::Overflow)?;
        env.storage()
            .instance()
            .set(&DataKey::NextLockId, &next_id);

        let token_client = token::Client::new(&env, &get_token(&env));
        token_client.transfer(&sender, &env.current_contract_address(), &amount);

        let lock = BridgeLock {
            id: lock_id,
            sender: sender.clone(),
            amount,
            status: LockStatus::Locked,
            validator_confirmed: false,
            claim_timestamp: env.ledger().timestamp(),
        };
        save_lock(&env, &lock);

        env.events().publish(
            (symbol_short!("bridgelock"),),
            BridgeTokensLocked {
                lock_id,
                sender,
                amount,
            },
        );

        Ok(lock_id)
    }

    /// Submits a failure-proof attestation from a validator for `lock_id`.
    ///
    /// Once distinct attestations reach `validator_threshold`, the lock's
    /// failure proof is marked confirmed. If admin approvals are already
    /// sufficient, execution is triggered in this call.
    ///
    /// A failure proof can no longer be submitted once the claim timeout
    /// window has expired — the timeout refund path is the only remaining
    /// recourse for an expired lock.
    pub fn submit_failure_proof(env: Env, validator: Address, lock_id: u64) -> Result<(), Error> {
        require_initialized(&env)?;
        validator.require_auth();

        let validators = get_validators(&env);
        if !validators.contains(&validator) {
            return Err(Error::NotValidator);
        }

        let mut lock = get_lock_checked(&env, lock_id)?;
        if lock.status != LockStatus::Locked {
            return Err(ContractError::LockNotLocked);
        }

        if is_expired(&env, &lock)? {
            return Err(Error::TimeoutExpired);
        }

        let vote_key = DataKey::ValidatorVote(lock_id, validator.clone());
        if env.storage().persistent().has(&vote_key) {
            return Err(ContractError::DuplicateVote);
        }
        env.storage().persistent().set(&vote_key, &true);

        let count = validator_vote_count(&env, lock_id)
            .checked_add(1)
            .ok_or(ContractError::Overflow)?;
        env.storage()
            .persistent()
            .set(&DataKey::ValidatorVoteCount(lock_id), &count);

        env.events().publish(
            (symbol_short!("bridgevote"),),
            FailureProofSubmitted {
                lock_id,
                validator,
                vote_count: count,
            },
        );

        if count >= get_validator_threshold(&env) {
            lock.validator_confirmed = true;
            save_lock(&env, &lock);
        }

        if is_ready_for_rescue(&env, &lock) {
            perform_rescue(&env, lock)?;
        }

        Ok()
    }

    /// Approves returning the funds for `lock_id`. Once distinct approvals
    /// reach the admin threshold and the validator failure-proof is confirmed,
    /// the rescue executes automatically as part of this call.
    pub fn approve_rescue(env: Env, admin: Address, lock_id: u64) -> Result<(), Error> {
        require_initialized(&env)?;
        admin.require_auth();

        let admins = get_admins(&env);
        if !admins.contains(&admin) {
            return Err(Error::NotAdmin);
        }

        let mut lock = get_lock_checked(&env, lock_id)?;
        if lock.status != LockStatus::Locked {
            return Err(ContractError::LockNotLocked);
        }

        let approval_key = DataKey::AdminApproval(lock_id, admin.clone());
        if env.storage().persistent().has(&approval_key) {
            return Err(ContractError::DuplicateVote);
        }
        env.storage().persistent().set(&approval_key, &true);

        let count = admin_approval_count(&env, lock_id)
            .checked_add(1)
            .ok_or(ContractError::Overflow)?;
        env.storage()
            .persistent()
            .set(&DataKey::AdminApprovalCount(lock_id), &count);

        env.events().publish(
            (symbol_short!("bridgeappr"),),
            RescueApproved {
                lock_id,
                admin,
                approval_count: count,
            },
        );

        if is_ready_for_rescue(&env, &lock) {
            perform_rescue(&env, lock)?;
        }

        Ok()
    }

    /// Permissionless keeper entry point that executes a rescue whose
    /// thresholds were already met but which has not yet been executed.
    ///
    /// Reverts with `Error::ThresholdNotReached` if the lock is not
    /// actually ready, so it can never bypass the consensus gate.
    pub fn execute_rescue(env: Env, lock_id: u64) -> Result<(), Error> {
        require_initialized(&env)?;

        let lock = get_lock_checked(&env, lock_id)?;
        if !is_ready_for_rescue(&env, &lock) {
            return Err(ContractError::ThresholdNotReached);
        }

        perform_rescue(&env, lock)
    }

    /// Permissionless timeout refund entry point (issue #812).
    ///
    /// Un-escrows the underlying assets and returns them to the original
    /// sender if the claim timeout window `t_claim + Timeout` has elapsed and
    /// no validator signatures (validator failure-proof confirmation) have
    /// been presented. The lock is then recorded as `CancelledExpired`.
    ///
    /// Reverts with `Error::TimeoutNotExpired` if the window has not yet
    /// elapsed, and with `Error::LockNotLocked` if the lock is not open.
    pub fn refund_expired(env: Env, lock_id: u64) -> Result<(), Error> {
        require_initialized(&env)?;

        let lock = get_lock_checked(&env, lock_id)?;
        if lock.status != LockStatus::Locked {
            return Err(Error::LockNotLocked);
        }

        // If validator signatures have already been presented and the
        // failure proof is confirmed, the rescue path is the correct one.
        if lock.validator_confirmed {
            return Err(Error::ThresholdNotReached);
        }

        perform_refund(&env, lock)
    }

    /// Returns the expiration instant `t_claim + Timeout` for `lock_id`.
    /// Useful for off-chain monitoring and for tests.
    pub fn get_expiration(env: Env, lock_id: u64) -> Result<u64, Error> {
        require_initialized(&env)?;
        let lock = get_lock_checked(&env, lock_id)?;
        lock_expiration_instant(&env, &lock)
    }

    /// Returns the current status of a lock.
    pub fn get_lock(env: Env, lock_id: u64) -> Result<BridgeLock, Error> {
        require_initialized(&env)?;
        get_lock_checked(&env, lock_id)
    }
}
