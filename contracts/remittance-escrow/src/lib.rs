#![no_std]

//! # Remittance Escrow — Anchor Cross-Border Payout Timeout Dispute Handler
//!
//! Minimal escrow contract for cross-border remittances that routes payouts
//! through an off-chain "anchor". If the anchor fails to prove it delivered
//! the payout before its deadline, the sender can open a dispute once a
//! 24-hour grace window has fully elapsed. Opening a dispute:
//!
//! 1. Seizes (locks) collateral the anchor staked with the contract, up to
//!    the remittance amount (or the anchor's full available collateral if
//!    that is less — see the module-level "Design decisions" note below).
//! 2. Auto-refunds the original remittance amount to the sender from the
//!    funds the contract already holds in custody.
//! 3. Marks the remittance `Refunded` so it can never be resolved twice.
//!
//! ## Design decisions
//!
//! - **Deadline handling**: `create_remittance` takes `deadline_secs`, a
//!   duration relative to the current ledger time. The stored `deadline` is
//!   `env.ledger().timestamp() + deadline_secs`, computed once at creation
//!   time. This avoids callers having to agree on an absolute timestamp
//!   up front and keeps the ledger-time semantics explicit.
//! - **Dispute window boundary**: the 24h window is measured from the
//!   remittance `deadline`, not from `create_remittance` time. A dispute is
//!   only accepted once `env.ledger().timestamp() >= deadline + 86_400`
//!   (strict, so the window must *fully* elapse — the boundary instant
//!   itself is allowed).
//! - **Collateral shortfall**: `open_dispute` never blocks the sender's
//!   refund on the anchor's collateral being sufficient. Instead it locks
//!   `min(remittance.amount, anchor_collateral_balance)` — i.e. it seizes
//!   whatever the anchor has staked, up to the remittance amount, and never
//!   panics for an under-collateralized anchor. The sender is always made
//!   whole because the funds being refunded were already escrowed in the
//!   contract by `create_remittance`; collateral seizure is a separate,
//!   best-effort penalty against the anchor.
//! - **Proof vs. dispute race**: `submit_payout_proof` is accepted any time
//!   the remittance is still `Pending`, including after the 24h window has
//!   elapsed, as long as nobody has opened a dispute yet. Soroban
//!   transactions execute one at a time, so this is a simple
//!   first-to-land-wins rule rather than a real race condition. Once either
//!   `submit_payout_proof` or `open_dispute` succeeds, the remittance is no
//!   longer `Pending` and the other call is rejected with
//!   `Error::AlreadyResolved`.

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, token,
    Address, Bytes, Env,
};

pub mod types;

pub use types::{DataKey, Remittance, RemittanceStatus};

/// Length of the dispute window, in seconds, measured from a remittance's
/// deadline. Uses ledger time (`env.ledger().timestamp()`), not ledger
/// sequence numbers.
pub const DISPUTE_WINDOW_SECS: u64 = 86_400;

/// Error types for the remittance escrow contract.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    /// Contract has not been initialized yet.
    NotInitialized = 1,
    /// Contract has already been initialized.
    AlreadyInitialized = 2,
    /// Caller is not authorized to perform this action.
    Unauthorized = 3,
    /// Amount must be greater than zero.
    ZeroAmount = 4,
    /// No remittance exists with the given id.
    RemittanceNotFound = 5,
    /// The remittance is no longer `Pending` (already `Completed` or `Refunded`).
    AlreadyResolved = 6,
    /// The 24-hour dispute window has not fully elapsed past the deadline yet.
    TooEarlyToDispute = 7,
    /// A checked arithmetic operation would have overflowed.
    ArithmeticOverflow = 8,
}

#[contract]
pub struct RemittanceEscrow;

/// Emitted once, when the contract is initialized.
#[contracttype]
pub struct ContractInitializedEvent {
    pub admin: Address,
    pub token: Address,
}

/// Emitted when a sender escrows a new remittance.
#[contracttype]
pub struct RemittanceCreatedEvent {
    pub id: u64,
    pub sender: Address,
    pub anchor: Address,
    pub amount: i128,
    pub deadline: u64,
}

/// Emitted when an anchor submits payout proof and the remittance completes.
#[contracttype]
pub struct PayoutCompletedEvent {
    pub id: u64,
    pub anchor: Address,
}

/// Emitted when an anchor deposits collateral.
#[contracttype]
pub struct CollateralDepositedEvent {
    pub anchor: Address,
    pub amount: i128,
    pub total: i128,
}

/// Emitted when a sender successfully opens a dispute on a timed-out payout.
#[contracttype]
pub struct PayoutDisputedEvent {
    pub id: u64,
    pub sender: Address,
    pub anchor: Address,
    pub locked_collateral: i128,
}

/// Emitted alongside `PayoutDisputedEvent` when the sender is auto-refunded.
#[contracttype]
pub struct RemittanceRefundedEvent {
    pub id: u64,
    pub sender: Address,
    pub amount: i128,
}

/// Returns `Err(Error::NotInitialized)` unless `initialize` has run.
///
/// Deliberately returns a `Result` (propagated via `?`) rather than
/// panicking, like every other fallible helper below: soroban-sdk 20.x
/// (pinned by this workspace) contract dispatch handles an `Err` return
/// from a `#[contractimpl]` method as a normal, structured failure, whereas
/// an actual Rust panic has to survive a panic/unwind round trip through
/// the host.
fn require_initialized(env: &Env) -> Result<(), Error> {
    if !env.storage().instance().has(&DataKey::Initialized) {
        return Err(Error::NotInitialized);
    }
    Ok(())
}

fn get_token(env: &Env) -> Result<Address, Error> {
    env.storage()
        .instance()
        .get(&DataKey::Token)
        .ok_or(Error::NotInitialized)
}

fn get_remittance(env: &Env, id: u64) -> Result<Remittance, Error> {
    env.storage()
        .persistent()
        .get(&DataKey::Remittance(id))
        .ok_or(Error::RemittanceNotFound)
}

fn set_remittance(env: &Env, remittance: &Remittance) {
    env.storage()
        .persistent()
        .set(&DataKey::Remittance(remittance.id), remittance);
}

fn get_collateral_balance(env: &Env, anchor: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::Collateral(anchor.clone()))
        .unwrap_or(0)
}

fn set_collateral_balance(env: &Env, anchor: &Address, balance: i128) {
    env.storage()
        .persistent()
        .set(&DataKey::Collateral(anchor.clone()), &balance);
}

fn checked_add(a: i128, b: i128) -> Result<i128, Error> {
    a.checked_add(b).ok_or(Error::ArithmeticOverflow)
}

fn checked_sub(a: i128, b: i128) -> Result<i128, Error> {
    a.checked_sub(b).ok_or(Error::ArithmeticOverflow)
}

#[contractimpl]
impl RemittanceEscrow {
    /// Initialize the contract with an admin and the SAC/SEP-41 token used
    /// for both remittance amounts and anchor collateral. Can only be called once.
    pub fn initialize(env: Env, admin: Address, token: Address) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Initialized) {
            return Err(Error::AlreadyInitialized);
        }

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage()
            .instance()
            .set(&DataKey::NextRemittanceId, &0u64);
        env.storage().instance().set(&DataKey::Initialized, &true);

        env.events().publish(
            (symbol_short!("cinit"),),
            ContractInitializedEvent { admin, token },
        );

        Ok(())
    }

    /// Sender escrows `amount` of the configured token for a remittance to be
    /// paid out (off-chain) by `anchor` before `deadline_secs` seconds from now.
    ///
    /// Transfers `amount` from `sender` into the contract's custody. Returns
    /// the newly allocated remittance id.
    pub fn create_remittance(
        env: Env,
        sender: Address,
        anchor: Address,
        amount: i128,
        deadline_secs: u64,
    ) -> Result<u64, Error> {
        require_initialized(&env)?;
        sender.require_auth();

        if amount <= 0 {
            return Err(Error::ZeroAmount);
        }

        let now = env.ledger().timestamp();
        let deadline = now.checked_add(deadline_secs).ok_or(Error::ArithmeticOverflow)?;

        let token_client = token::Client::new(&env, &get_token(&env)?);
        token_client.transfer(&sender, &env.current_contract_address(), &amount);

        let id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextRemittanceId)
            .unwrap_or(0);
        let next_id = id.checked_add(1).ok_or(Error::ArithmeticOverflow)?;
        env.storage()
            .instance()
            .set(&DataKey::NextRemittanceId, &next_id);

        let remittance = Remittance {
            id,
            sender: sender.clone(),
            anchor: anchor.clone(),
            amount,
            deadline,
            status: RemittanceStatus::Pending,
            proof: Bytes::new(&env),
        };
        set_remittance(&env, &remittance);

        env.events().publish(
            (symbol_short!("remcreat"),),
            RemittanceCreatedEvent {
                id,
                sender,
                anchor,
                amount,
                deadline,
            },
        );

        Ok(id)
    }

    /// The recorded anchor submits proof that the off-chain payout happened,
    /// marking the remittance `Completed`. Only callable while the remittance
    /// is still `Pending` (see module docs for the proof-vs-dispute race rule).
    pub fn submit_payout_proof(
        env: Env,
        anchor: Address,
        remittance_id: u64,
        proof: Bytes,
    ) -> Result<(), Error> {
        require_initialized(&env)?;
        anchor.require_auth();

        let mut remittance = get_remittance(&env, remittance_id)?;

        if remittance.anchor != anchor {
            return Err(Error::Unauthorized);
        }
        if remittance.status != RemittanceStatus::Pending {
            return Err(Error::AlreadyResolved);
        }

        remittance.status = RemittanceStatus::Completed;
        remittance.proof = proof;
        set_remittance(&env, &remittance);

        env.events().publish(
            (symbol_short!("paycomp"),),
            PayoutCompletedEvent {
                id: remittance_id,
                anchor,
            },
        );

        Ok(())
    }

    /// An anchor stakes `amount` of collateral with the contract. Transfers
    /// `amount` from `anchor` into the contract's custody and credits it to
    /// the anchor's on-chain collateral balance.
    pub fn deposit_collateral(env: Env, anchor: Address, amount: i128) -> Result<(), Error> {
        require_initialized(&env)?;
        anchor.require_auth();

        if amount <= 0 {
            return Err(Error::ZeroAmount);
        }

        let token_client = token::Client::new(&env, &get_token(&env)?);
        token_client.transfer(&anchor, &env.current_contract_address(), &amount);

        let current = get_collateral_balance(&env, &anchor);
        let total = checked_add(current, amount)?;
        set_collateral_balance(&env, &anchor, total);

        env.events().publish(
            (symbol_short!("coldep"),),
            CollateralDepositedEvent {
                anchor,
                amount,
                total,
            },
        );

        Ok(())
    }

    /// The sender opens a dispute on a remittance whose anchor missed its
    /// deadline and the subsequent 24-hour grace window. Only callable by the
    /// original sender, only once the window has fully elapsed, and only
    /// while the remittance is still `Pending`.
    ///
    /// On success: seizes up to `remittance.amount` of the anchor's staked
    /// collateral (or its full available balance if less — see module docs),
    /// refunds `remittance.amount` to the sender from the contract's held
    /// funds, and marks the remittance `Refunded`.
    pub fn open_dispute(env: Env, sender: Address, remittance_id: u64) -> Result<(), Error> {
        require_initialized(&env)?;
        sender.require_auth();

        let mut remittance = get_remittance(&env, remittance_id)?;

        if remittance.sender != sender {
            return Err(Error::Unauthorized);
        }
        if remittance.status != RemittanceStatus::Pending {
            return Err(Error::AlreadyResolved);
        }

        let now = env.ledger().timestamp();
        let dispute_open_at = remittance
            .deadline
            .checked_add(DISPUTE_WINDOW_SECS)
            .ok_or(Error::ArithmeticOverflow)?;

        if now < dispute_open_at {
            return Err(Error::TooEarlyToDispute);
        }

        // Lock up to `amount` of the anchor's available collateral. An
        // under-collateralized anchor never blocks the sender's refund; see
        // the "Collateral shortfall" note in the module docs.
        let available = get_collateral_balance(&env, &remittance.anchor);
        let locked = if available < remittance.amount {
            available
        } else {
            remittance.amount
        };
        if locked > 0 {
            let remaining = checked_sub(available, locked)?;
            set_collateral_balance(&env, &remittance.anchor, remaining);
        }

        let token_client = token::Client::new(&env, &get_token(&env)?);
        token_client.transfer(
            &env.current_contract_address(),
            &sender,
            &remittance.amount,
        );

        remittance.status = RemittanceStatus::Refunded;
        set_remittance(&env, &remittance);

        env.events().publish(
            (symbol_short!("paydisp"),),
            PayoutDisputedEvent {
                id: remittance_id,
                sender: sender.clone(),
                anchor: remittance.anchor.clone(),
                locked_collateral: locked,
            },
        );
        env.events().publish(
            (symbol_short!("remrefnd"),),
            RemittanceRefundedEvent {
                id: remittance_id,
                sender,
                amount: remittance.amount,
            },
        );

        Ok(())
    }

    /// Returns the full record for a remittance, or `Error::RemittanceNotFound`
    /// if it does not exist.
    pub fn get_remittance(env: Env, remittance_id: u64) -> Result<Remittance, Error> {
        get_remittance(&env, remittance_id)
    }

    /// Returns the current collateral balance staked by `anchor` (0 if none).
    pub fn get_collateral(env: Env, anchor: Address) -> i128 {
        get_collateral_balance(&env, &anchor)
    }

    /// Returns the configured admin address.
    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized))
    }

    /// Returns the configured token address.
    pub fn get_token(env: Env) -> Result<Address, Error> {
        get_token(&env)
    }
}

#[cfg(test)]
mod test;
