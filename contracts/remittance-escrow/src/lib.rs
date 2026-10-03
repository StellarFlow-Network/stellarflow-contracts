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
//!   `ContractError::AlreadyResolved`.

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

/// Minimum collateral bond `B_min` (in token stroops) an anchor must stake
/// before it may accept remittance transactions (Issue #929).
pub const BOND_MIN: i128 = 20_000;

/// Percentage of an anchor's locked bond that is slashed into the protocol
/// treasury when a payout proof is not submitted before the deadline
/// (Issue #929).
pub const BOND_SLASH_PERCENT: i128 = 20;

/// Error types for the remittance escrow contract.
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
    /// Caller is not authorized to perform this action.
    /// Recovery steps: Inspect the state for Unauthorized and retry with valid inputs or proper conditions.
    Unauthorized = 3,
    /// Amount must be greater than zero.
    /// Recovery steps: Inspect the state for ZeroAmount and retry with valid inputs or proper conditions.
    ZeroAmount = 4,
    /// No remittance exists with the given id.
    /// Recovery steps: Inspect the state for RemittanceNotFound and retry with valid inputs or proper conditions.
    RemittanceNotFound = 5,
    /// The remittance is no longer `Pending` (already `Completed` or `Refunded`).
    /// Recovery steps: Inspect the state for AlreadyResolved and retry with valid inputs or proper conditions.
    AlreadyResolved = 6,
    /// The 24-hour dispute window has not fully elapsed past the deadline yet.
    /// Recovery steps: Inspect the state for TooEarlyToDispute and retry with valid inputs or proper conditions.
    TooEarlyToDispute = 7,
    /// A checked arithmetic operation would have overflowed.
    /// Recovery steps: Inspect the state for ArithmeticOverflow and retry with valid inputs or proper conditions.
    ArithmeticOverflow = 8,
    /// The anchor has not staked the minimum collateral bond (`BOND_MIN`).
    InsufficientBond = 9,
    /// No protocol treasury has been configured for bond slashing.
    TreasuryNotSet = 10,
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

/// Emitted when an anchor's bond becomes locked by a new pending settlement.
#[contracttype]
pub struct BondLockedEvent {
    pub anchor: Address,
    pub pending_remittances: u32,
}

/// Emitted when an anchor's bond is unlocked after all settlements finalize.
#[contracttype]
pub struct BondUnlockedEvent {
    pub anchor: Address,
}

/// Emitted when 20% of an anchor's locked bond is slashed into the treasury.
#[contracttype]
pub struct BondSlashedEvent {
    pub anchor: Address,
    pub remittance_id: u64,
    pub slashed: i128,
    pub treasury: Address,
}

/// Emitted when the protocol treasury address is configured.
#[contracttype]
pub struct TreasurySetEvent {
    pub admin: Address,
    pub treasury: Address,
}

/// Returns `Err(Error::NotInitialized)` unless `initialize` has run.
///
/// Deliberately returns a `Result` (propagated via `?`) rather than
/// panicking, like every other fallible helper below: soroban-sdk 20.x
/// (pinned by this workspace) contract dispatch handles an `Err` return
/// from a `#[contractimpl]` method as a normal, structured failure, whereas
/// an actual Rust panic has to survive a panic/unwind round trip through
/// the host.
fn require_initialized(env: &Env) -> Result<(), ContractError> {
    if !env.storage().instance().has(&DataKey::Initialized) {
        return Err(ContractError::NotInitialized);
    }
    Ok(())
}

fn get_token(env: &Env) -> Result<Address, ContractError> {
    env.storage()
        .instance()
        .get(&DataKey::Token)
        .ok_or(ContractError::NotInitialized)
}

fn get_remittance(env: &Env, id: u64) -> Result<Remittance, ContractError> {
    env.storage()
        .persistent()
        .get(&DataKey::Remittance(id))
        .ok_or(ContractError::RemittanceNotFound)
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

fn checked_add(a: i128, b: i128) -> Result<i128, ContractError> {
    a.checked_add(b).ok_or(ContractError::ArithmeticOverflow)
}

fn checked_sub(a: i128, b: i128) -> Result<i128, ContractError> {
    a.checked_sub(b).ok_or(ContractError::ArithmeticOverflow)
}

// ── Liquidity bond staking guard (Issue #929) ────────────────────────────────

/// Number of active (Pending) settlement tasks currently assigned to `anchor`.
fn get_pending_remittances(env: &Env, anchor: &Address) -> u32 {
    env.storage()
        .instance()
        .get(&DataKey::PendingRemittances(anchor.clone()))
        .unwrap_or(0)
}

fn set_pending_remittances(env: &Env, anchor: &Address, count: u32) {
    env.storage()
        .instance()
        .set(&DataKey::PendingRemittances(anchor.clone()), &count);
}

/// Whether the anchor's bond is currently locked in instance storage.
fn is_bond_locked(env: &Env, anchor: &Address) -> bool {
    env.storage()
        .instance()
        .get(&DataKey::BondLocked(anchor.clone()))
        .unwrap_or(false)
}

fn set_bond_locked(env: &Env, anchor: &Address, locked: bool) {
    env.storage()
        .instance()
        .set(&DataKey::BondLocked(anchor.clone()), &locked);
}

/// The configured protocol treasury address (bond slashing sink).
fn get_treasury(env: &Env) -> Result<Address, Error> {
    env.storage()
        .instance()
        .get(&DataKey::Treasury)
        .ok_or(Error::TreasuryNotSet)
}

/// Decrement the anchor's active settlement count, unlocking its instance
/// bond lock once every pending task has been finalized (Issue #929).
fn decrement_pending_and_unlock(env: &Env, anchor: &Address) {
    let pending = get_pending_remittances(env, anchor);
    let next = if pending > 0 { pending - 1 } else { 0 };
    set_pending_remittances(env, anchor, next);

    if next == 0 && is_bond_locked(env, anchor) {
        set_bond_locked(env, anchor, false);
        env.events().publish(
            (symbol_short!("bondunlk"),),
            BondUnlockedEvent { anchor: anchor.clone() },
        );
    }
}

#[contractimpl]
impl RemittanceEscrow {
    /// Initialize the contract with an admin and the SAC/SEP-41 token used
    /// for both remittance amounts and anchor collateral. Can only be called once.
    pub fn initialize(env: Env, admin: Address, token: Address) -> Result<(), ContractError> {
        if env.storage().instance().has(&DataKey::Initialized) {
            return Err(ContractError::AlreadyInitialized);
        }

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage()
            .instance()
            .set(&DataKey::NextRemittanceId, &0u64);
        env.storage().instance().set(&DataKey::Initialized, &true);

        let seq = next_event_sequence_id(&env)?;
        env.events().publish(
            (symbol_short!("cinit"), seq),
            ContractInitializedEvent { admin, token },
        );

        Ok(())
    }

    /// Configure the protocol treasury that receives 20% bond slashes
    /// (Issue #929). Admin-only; may be called once or re-pointed later.
    pub fn set_treasury(env: Env, admin: Address, treasury: Address) -> Result<(), Error> {
        require_initialized(&env)?;
        admin.require_auth();

        let configured_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;

        if configured_admin != admin {
            return Err(Error::Unauthorized);
        }

        env.storage().instance().set(&DataKey::Treasury, &treasury);

        env.events().publish(
            (symbol_short!("treasury"),),
            TreasurySetEvent { admin, treasury },
        );

        Ok(())
    }

    /// Read the configured protocol treasury address, if any.
    pub fn get_treasury_address(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Treasury)
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
    ) -> Result<u64, ContractError> {
        require_initialized(&env)?;
        sender.require_auth();

        if amount <= 0 {
            return Err(ContractError::ZeroAmount);
        }

        // Relayer liquidity bond guard (Issue #929): an anchor must stake the
        // minimum collateral bond `B_min` before accepting transactions.
        if get_collateral_balance(&env, &anchor) < BOND_MIN {
            return Err(Error::InsufficientBond);
        }

        let now = env.ledger().timestamp();
        let deadline = now
            .checked_add(deadline_secs)
            .ok_or(ContractError::ArithmeticOverflow)?;

        let token_client = token::Client::new(&env, &get_token(&env)?);
        token_client.transfer(&sender, &env.current_contract_address(), &amount);

        let id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextRemittanceId)
            .unwrap_or(0);
        let next_id = id.checked_add(1).ok_or(ContractError::ArithmeticOverflow)?;
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

        // Lock the anchor's bond in instance storage until all active
        // settlement tasks are finalized (Issue #929).
        let pending = get_pending_remittances(&env, &anchor);
        let next_pending = pending.checked_add(1).ok_or(Error::ArithmeticOverflow)?;
        set_pending_remittances(&env, &anchor, next_pending);
        if next_pending == 1 {
            set_bond_locked(&env, &anchor, true);
            env.events().publish(
                (symbol_short!("bondlock"),),
                BondLockedEvent {
                    anchor: anchor.clone(),
                    pending_remittances: next_pending,
                },
            );
        }

        env.events().publish(
            (symbol_short!("remcreat"), seq),
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
    ) -> Result<(), ContractError> {
        require_initialized(&env)?;
        anchor.require_auth();

        let mut remittance = get_remittance(&env, remittance_id)?;

        if remittance.anchor != anchor {
            return Err(ContractError::Unauthorized);
        }
        if remittance.status != RemittanceStatus::Pending {
            return Err(ContractError::AlreadyResolved);
        }

        remittance.status = RemittanceStatus::Completed;
        remittance.proof = proof;
        set_remittance(&env, &remittance);

        // The settlement task is finalized; unlock the bond when no more
        // pending remittances remain (Issue #929).
        decrement_pending_and_unlock(&env, &remittance.anchor);

        env.events().publish(
            (symbol_short!("paycomp"), seq),
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
    pub fn deposit_collateral(env: Env, anchor: Address, amount: i128) -> Result<(), ContractError> {
        require_initialized(&env)?;
        anchor.require_auth();

        if amount <= 0 {
            return Err(ContractError::ZeroAmount);
        }

        let token_client = token::Client::new(&env, &get_token(&env)?);
        token_client.transfer(&anchor, &env.current_contract_address(), &amount);

        let current = get_collateral_balance(&env, &anchor);
        let total = checked_add(current, amount)?;
        set_collateral_balance(&env, &anchor, total);

        let seq = next_event_sequence_id(&env)?;
        env.events().publish(
            (symbol_short!("coldep"), seq),
            CollateralDepositedEvent {
                anchor,
                amount,
                total,
            },
        );

        Ok(())
    }

    /// An anchor stakes `amount` toward its minimum liquidity bond
    /// `BOND_MIN` (Issue #929). Semantically identical to
    /// [`deposit_collateral`] — the name communicates that the stake backs
    /// the anchor's relayer bond requirement.
    ///
    /// The bond becomes **locked** the moment the anchor accepts a remittance
    /// and stays locked until all active settlement tasks are finalized.
    pub fn deposit_bond(env: Env, anchor: Address, amount: i128) -> Result<(), Error> {
        Self::deposit_collateral(env, anchor, amount)
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
    pub fn open_dispute(env: Env, sender: Address, remittance_id: u64) -> Result<(), ContractError> {
        require_initialized(&env)?;
        sender.require_auth();

        let mut remittance = get_remittance(&env, remittance_id)?;

        if remittance.sender != sender {
            return Err(ContractError::Unauthorized);
        }
        if remittance.status != RemittanceStatus::Pending {
            return Err(ContractError::AlreadyResolved);
        }

        let now = env.ledger().timestamp();
        let dispute_open_at = remittance
            .deadline
            .checked_add(DISPUTE_WINDOW_SECS)
            .ok_or(ContractError::ArithmeticOverflow)?;

        if now < dispute_open_at {
            return Err(ContractError::TooEarlyToDispute);
        }

        // Liquidity bond guard (Issue #929): the anchor missed its
        // payout-proof deadline, so slash 20% of its locked bond into the
        // protocol treasury.
        let treasury = get_treasury(&env)?;
        let current_bond = get_collateral_balance(&env, &remittance.anchor);
        if current_bond > 0 {
            let slash = (current_bond * BOND_SLASH_PERCENT) / 100;
            if slash > 0 {
                let slash_client = token::Client::new(&env, &get_token(&env)?);
                slash_client.transfer(&env.current_contract_address(), &treasury, &slash);
                let remaining = checked_sub(current_bond, slash)?;
                set_collateral_balance(&env, &remittance.anchor, remaining);

                env.events().publish(
                    (symbol_short!("bondslash"),),
                    BondSlashedEvent {
                        anchor: remittance.anchor.clone(),
                        remittance_id,
                        slashed: slash,
                        treasury,
                    },
                );
            }
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
        token_client.transfer(&env.current_contract_address(), &sender, &remittance.amount);

        remittance.status = RemittanceStatus::Refunded;
        set_remittance(&env, &remittance);

        let dispute_seq = next_event_sequence_id(&env)?;
        env.events().publish(
            (symbol_short!("paydisp"), dispute_seq),
            PayoutDisputedEvent {
                id: remittance_id,
                sender: sender.clone(),
                anchor: remittance.anchor.clone(),
                locked_collateral: locked,
            },
        );
        let refund_seq = next_event_sequence_id(&env)?;
        env.events().publish(
            (symbol_short!("remrefnd"), refund_seq),
            RemittanceRefundedEvent {
                id: remittance_id,
                sender,
                amount: remittance.amount,
            },
        );

        // Finalize this settlement task; unlock the bond when none remain.
        decrement_pending_and_unlock(&env, &remittance.anchor);

        Ok(())
    }

    /// Returns the full record for a remittance, or `ContractError::RemittanceNotFound`
    /// if it does not exist.
    pub fn get_remittance(env: Env, remittance_id: u64) -> Result<Remittance, ContractError> {
        get_remittance(&env, remittance_id)
    }

    /// Returns the current collateral balance staked by `anchor` (0 if none).
    pub fn get_collateral(env: Env, anchor: Address) -> i128 {
        get_collateral_balance(&env, &anchor)
    }

    /// Returns the number of active (Pending) settlement tasks for `anchor`.
    pub fn get_pending_remittance_count(env: Env, anchor: Address) -> u32 {
        get_pending_remittances(&env, &anchor)
    }

    /// Returns `true` while the anchor's bond is locked by active settlements.
    pub fn is_bond_locked(env: Env, anchor: Address) -> bool {
        is_bond_locked(&env, &anchor)
    }

    /// Returns the configured admin address.
    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, ContractError::NotInitialized))
    }

    /// Returns the configured token address.
    pub fn get_token(env: Env) -> Result<Address, ContractError> {
        get_token(&env)
    }
}

#[cfg(test)]
mod test;
