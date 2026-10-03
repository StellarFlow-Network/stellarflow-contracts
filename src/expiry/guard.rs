//! Contract entrypoints for the deposit expiry guard.
//!
//! Every function here is a thin wrapper around the two sibling files in this
//! module: `super::tree` for the commitment tree, `super::{DepositRecord,
//! ExpiryError, ...}` for the record and error types. The guard logic (who may
//! do what, at what time) lives in this file.
//!
//! # Self-containment
//!
//! This file does not import from any other module in the crate. It defines
//! its own error and event types and its own storage-key family. See `mod.rs`
//! for the rationale.

use soroban_sdk::{token::Client as TokenClient, Address, BytesN, Env, Vec};

use super::tree;
use super::{
    events, DepositRecord, ExpiryConfig, ExpiryError, ExpiryStorageKey, DEPOSIT_EXPIRY_SECS,
};

fn load_config(env: &Env) -> ExpiryConfig {
    env.storage()
        .instance()
        .get::<_, ExpiryConfig>(&ExpiryStorageKey::Config)
        .unwrap_or_default()
}

fn load_admin(env: &Env) -> Option<Address> {
    env.storage()
        .instance()
        .get::<_, Address>(&ExpiryStorageKey::Admin)
}

/// Read the per-deposit lifetime in seconds.
fn expiry_secs(env: &Env) -> Result<u64, ExpiryError> {
    let cfg = load_config(env);
    if !cfg.expiry_enabled {
        return Err(ExpiryError::ExpiryDisabled);
    }
    Ok(cfg.expiry_secs_override.unwrap_or(DEPOSIT_EXPIRY_SECS))
}

/// Expiry boundary: strictly after (`now > expires_at`), matching the
/// convention used in `crate::zk::merkle::validate_root`
/// (`current_time > expires_at`) and `crate::bridge::escrow`
/// (`timestamp() <= escrow.expires_at` rejects cancellation). A deposit is
/// still withdrawable on its last second and refundable from the next.
fn is_expired(deposit: &DepositRecord, now: u64) -> bool {
    now > deposit.expires_at
}

// ---------------------------------------------------------------------
// Admin
// ---------------------------------------------------------------------

/// One-time admin set. Caller becomes the module admin. Idempotent —
/// re-calling with the same address succeeds, with a different address
/// fails with `NotAdmin`.
pub fn set_admin(env: &Env, admin: Address) -> Result<(), ExpiryError> {
    match load_admin(env) {
        Some(existing) if existing == admin => Ok(()),
        Some(_) => Err(ExpiryError::NotAdmin),
        None => {
            admin.require_auth();
            env.storage()
                .instance()
                .set(&ExpiryStorageKey::Admin, &admin);
            Ok(())
        }
    }
}

pub fn get_admin(env: &Env) -> Option<Address> {
    load_admin(env)
}

/// Update the expiry guard configuration. Admin-only.
pub fn set_expiry_config(
    env: &Env,
    caller: Address,
    expiry_enabled: bool,
    expiry_secs_override: Option<u64>,
) -> Result<(), ExpiryError> {
    let admin = load_admin(env).ok_or(ExpiryError::AdminNotSet)?;
    caller.require_auth();
    if caller != admin {
        return Err(ExpiryError::NotAdmin);
    }
    if let Some(secs) = expiry_secs_override {
        if secs == 0 {
            return Err(ExpiryError::InvalidExpiryDuration);
        }
    }
    let cfg = ExpiryConfig {
        expiry_enabled,
        expiry_secs_override,
    };
    env.storage()
        .instance()
        .set(&ExpiryStorageKey::Config, &cfg);
    events::emit_config(env, &caller, expiry_enabled, expiry_secs_override);
    Ok(())
}

pub fn get_expiry_config(env: &Env) -> ExpiryConfig {
    load_config(env)
}

// ---------------------------------------------------------------------
// Deposit
// ---------------------------------------------------------------------

/// Count of deposits created so far. The tree's own `next_index` is
/// authoritative for allocation; this is a convenience read for indexers.
pub fn deposit_count(env: &Env) -> u64 {
    env.storage()
        .instance()
        .get::<_, u64>(&ExpiryStorageKey::DepositCount)
        .unwrap_or(0)
}

/// Create a deposit: pull `amount` of `token` from `depositor`, register the
/// record, and insert the commitment (with `t_deposit`) into the Merkle tree.
///
/// `commitment` is expected to be `H(salt || recipient || amount || token ||
/// ...)` computed off-chain. The contract stores the preimage fields in the
/// clear in `DepositRecord` — see the PR description for why.
pub fn create_deposit(
    env: &Env,
    depositor: Address,
    recipient: Address,
    token: Address,
    amount: i128,
    commitment: BytesN<32>,
) -> Result<(u64, BytesN<32>), ExpiryError> {
    depositor.require_auth();
    if amount <= 0 {
        return Err(ExpiryError::InvalidAmount);
    }

    let t_deposit = env.ledger().timestamp();
    let secs = expiry_secs(env)?;

    // Pull funds in first (fail-closed: if the transfer fails we never
    // create the record).
    TokenClient::new(env, &token)
        .transfer(&depositor, &env.current_contract_address(), &amount)
        .map_err(|_| ExpiryError::TransferInFailed)?;

    let mut record = DepositRecord {
        index: 0, // filled in by tree::insert
        commitment: BytesN::from_array(env, &[0u8; 32]),
        depositor: depositor.clone(),
        recipient: recipient.clone(),
        token: token.clone(),
        amount,
        deposited_at: 0,
        expires_at: 0,
        settled: false,
        refunded: false,
    };

    let (index, root) = tree::insert(env, &mut record, commitment, t_deposit, secs)?;

    let count = deposit_count(env);
    env.storage()
        .instance()
        .set(&ExpiryStorageKey::DepositCount, &(count + 1));

    let dep_key = ExpiryStorageKey::Deposit(index);
    env.storage().persistent().set(&dep_key, &record);
    env.storage()
        .persistent()
        .extend_ttl(&dep_key, 5_000, 100_000);

    events::emit_create(
        env,
        index,
        &record.commitment,
        &depositor,
        &record.recipient,
        &token,
        amount,
        t_deposit,
        record.expires_at,
    );

    Ok((index, root))
}

/// Look up a deposit record by leaf index.
pub fn get_deposit(env: &Env, index: u64) -> Option<DepositRecord> {
    env.storage()
        .persistent()
        .get(&ExpiryStorageKey::Deposit(index))
}

fn get_deposit_or_err(env: &Env, index: u64) -> Result<DepositRecord, ExpiryError> {
    get_deposit(env, index).ok_or(ExpiryError::DepositNotFound)
}

/// Settled-state check shared by both exits.
fn ensure_open(deposit: &DepositRecord) -> Result<(), ExpiryError> {
    if deposit.settled || deposit.refunded {
        return Err(ExpiryError::AlreadySettled);
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Withdrawal (third-party / recipient path)
// ---------------------------------------------------------------------

/// Withdraw a deposit before expiry. Open to anyone who can present the
/// commitment preimage knowledge — in practice the recipient, since the
/// contract's only on-chain identity check is "not settled, not expired".
///
/// Rejected outright once `now > expires_at`: the whole point of the guard.
pub fn withdraw(env: &Env, index: u64, caller: Address) -> Result<(), ExpiryError> {
    caller.require_auth();
    let deposit = get_deposit_or_err(env, index)?;
    ensure_open(&deposit)?;

    let now = env.ledger().timestamp();
    if is_expired(&deposit, now) {
        return Err(ExpiryError::DepositExpired);
    }

    let mut updated = deposit;
    updated.settled = true;
    let dep_key = ExpiryStorageKey::Deposit(index);
    env.storage().persistent().set(&dep_key, &updated);
    env.storage()
        .persistent()
        .extend_ttl(&dep_key, 5_000, 100_000);

    TokenClient::new(env, &updated.token)
        .transfer(
            &env.current_contract_address(),
            &updated.recipient,
            &updated.amount,
        )
        .map_err(|_| ExpiryError::TransferOutFailed)?;

    events::emit_withdraw(env, index, &updated.recipient, updated.amount);
    Ok(())
}

// ---------------------------------------------------------------------
// Emergency refund (depositor path, post-expiry)
// ---------------------------------------------------------------------

/// Emergency refund of an expired deposit to its depositor. Only the
/// depositor may call this, and only after `now > expires_at`. Funds always
/// go to `deposit.depositor` regardless of who the caller is — the
/// `caller != depositor` check makes that unreachable, but the transfer
/// target is the stored depositor either way.
pub fn refund_expired(env: &Env, index: u64, caller: Address) -> Result<(), ExpiryError> {
    caller.require_auth();
    let deposit = get_deposit_or_err(env, index)?;
    ensure_open(&deposit)?;

    if caller != deposit.depositor {
        return Err(ExpiryError::NotDepositor);
    }
    let now = env.ledger().timestamp();
    if !is_expired(&deposit, now) {
        return Err(ExpiryError::NotYetExpired);
    }

    let mut updated = deposit;
    updated.settled = true;
    updated.refunded = true;
    let dep_key = ExpiryStorageKey::Deposit(index);
    env.storage().persistent().set(&dep_key, &updated);
    env.storage()
        .persistent()
        .extend_ttl(&dep_key, 5_000, 100_000);

    TokenClient::new(env, &updated.token)
        .transfer(
            &env.current_contract_address(),
            &updated.depositor,
            &updated.amount,
        )
        .map_err(|_| ExpiryError::TransferOutFailed)?;

    events::emit_refund(env, index, &updated.depositor, updated.amount);
    Ok(())
}

// ---------------------------------------------------------------------
// Read-only helpers
// ---------------------------------------------------------------------

pub fn deposit_root(env: &Env) -> BytesN<32> {
    tree::current_root(env)
}

pub fn deposit_next_index(env: &Env) -> u64 {
    tree::next_index(env)
}

pub fn is_known_deposit_root(env: &Env, root: BytesN<32>) -> bool {
    tree::is_known_root(env, root)
}

pub fn deposit_root_history(env: &Env) -> Vec<BytesN<32>> {
    tree::root_history(env)
}

/// Convenience read: has a deposit's expiry passed as of the current ledger?
pub fn is_deposit_expired(env: &Env, index: u64) -> Result<bool, ExpiryError> {
    let deposit = get_deposit_or_err(env, index)?;
    Ok(is_expired(&deposit, env.ledger().timestamp()))
}
