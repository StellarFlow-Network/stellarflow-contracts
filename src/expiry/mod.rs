//! Private remittance deposit commitments with an expiry guard.
//!
//! # Relationship to the rest of the crate
//!
//! **This module is intentionally self-contained.** It does not `use` any item
//! from any other module in this crate. The crate is currently in a state where
//! `main` does not compile (see issue #1114), so this module is written to be
//! type-checkable in isolation from the rest of the crate, against
//! `soroban-sdk` alone.
//!
//! This means the contract entrypoints (in `guard.rs`) re-declare the small
//! error and event surface they need locally rather than importing
//! `crate::ContractError`. If maintainers later want this integrated into the
//! main contract error enum, that is a mechanical change, but it would make
//! this module depend on a file that does not currently parse.
//!
//! # What this implements (issue #965)
//!
//! Issue #965 asks for a "private remittance deposit commitment expiry guard":
//!
//! 1. Attach a deposit timestamp (`t_deposit`) to Merkle tree commitment
//!    entries.
//! 2. Expire commitments after 30 days.
//! 3. Let the depositor emergency-refund after expiry.
//! 4. Reject third-party withdrawals after expiry.
//!
//! The existing `crate::escrow::merkle` tree stores bare `BytesN<32>`
//! commitments with no timestamp, no depositor identity, no amount, and no
//! withdrawal path — so tasks 2, 3, and 4 have nothing to attach to. This
//! module is a from-scratch interpretation of what #965 requires, not an
//! extension of existing logic. See the PR description for the full design
//! rationale.

pub mod guard;
pub mod tree;

/// Deposit lifetime. `30 * 24 * 60 * 60 = 2,592,000` seconds.
///
/// Written out multiplicatively to match the crate's existing duration
/// convention (`FIAT_PAYOUT_TIMEOUT_SECS`, `PROPOSAL_EXPIRY_SECONDS`,
/// `DEFAULT_ROOT_VALIDITY_DURATION`, ...).
pub const DEPOSIT_EXPIRY_SECS: u64 = 30 * 24 * 60 * 60;

/// Storage keys for the expiry-guard module. `Expiry`-prefixed so they cannot
/// collide with any other key family in the contract's storage.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExpiryStorageKey {
    /// One entry per deposit, keyed by its commitment leaf index.
    Deposit(u64),
    /// Monotonic counter: total deposits ever created.
    DepositCount,
    /// Guard configuration: expiry on/off and per-deposit lifetime override.
    Config,
    /// Module admin address.
    Admin,
}

/// A single private-remittance deposit commitment record.
///
/// The commitment itself is a salted hash computed off-chain:
/// `H(salt || recipient || amount || token_id || ...)` — this contract never
/// sees the preimage, which is what keeps the deposit private. But the
/// contract *does* need to know, in the clear, enough to enforce the expiry
/// guard: who is allowed to refund after expiry, what was deposited, and when.
/// Those fields are stored unhashed on purpose (see PR description,
/// "Design decisions").
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DepositRecord {
    /// Monotonic index, identical to the leaf index in the commitment tree.
    pub index: u64,
    /// Commitment hash inserted into the Merkle tree (`BytesN<32>` leaf).
    pub commitment: BytesN<32>,
    /// Who funded the deposit — the only address that may emergency-refund.
    pub depositor: Address,
    /// Who may withdraw before expiry (anyone who can open the commitment
    /// off-chain, typically the remittance recipient).
    pub recipient: Address,
    /// SAC or token contract id the deposit was funded in.
    pub token: Address,
    /// Amount held back by this record.
    pub amount: i128,
    /// `env.ledger().timestamp()` at insertion. This is the `t_deposit` that
    /// issue #965 asks to attach to commitment entries.
    pub deposited_at: u64,
    /// `deposited_at + expiry_secs`, fixed at insert time.
    pub expires_at: u64,
    /// False while the deposit is live; set once withdrawn or refunded.
    pub settled: bool,
    /// False while live; set by `refund_expired` to distinguish a refund
    /// from a normal withdrawal in the event log.
    pub refunded: bool,
}

/// Guard configuration. Admin-controlled; sane defaults if never set.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpiryConfig {
    /// Master switch for the expiry guard. If false, deposits never expire.
    pub expiry_enabled: bool,
    /// Per-deposit lifetime override. `None` → use `DEPOSIT_EXPIRY_SECS`.
    pub expiry_secs_override: Option<u64>,
}

impl Default for ExpiryConfig {
    fn default() -> Self {
        Self {
            expiry_enabled: true,
            expiry_secs_override: None,
        }
    }
}

/// Local error type. Kept module-local so this module has no dependency on
/// `crate::errors` (which currently does not parse — see issue #1114).
#[contracterror]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExpiryError {
    /// Caller is not the depositor of this deposit.
    NotDepositor = 1,
    /// Deposit index does not exist.
    DepositNotFound = 2,
    /// Deposit was already withdrawn or refunded.
    AlreadySettled = 3,
    /// Withdrawal attempted after the deposit expired.
    DepositExpired = 4,
    /// Refund attempted before the deposit expired.
    NotYetExpired = 5,
    /// Expiry guard is disabled by config.
    ExpiryDisabled = 6,
    /// No admin has been configured for this module.
    AdminNotSet = 7,
    /// Caller is not the module admin.
    NotAdmin = 8,
    /// `expiry_secs_override` is zero.
    InvalidExpiryDuration = 9,
    /// Amount must be strictly positive.
    InvalidAmount = 10,
    /// Token transfer in failed.
    TransferInFailed = 11,
    /// Token transfer out failed.
    TransferOutFailed = 12,
}

impl std::fmt::Display for ExpiryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            ExpiryError::NotDepositor => "caller is not the depositor",
            ExpiryError::DepositNotFound => "deposit not found",
            ExpiryError::AlreadySettled => "deposit already settled",
            ExpiryError::DepositExpired => "deposit has expired",
            ExpiryError::NotYetExpired => "deposit has not expired yet",
            ExpiryError::ExpiryDisabled => "expiry guard is disabled",
            ExpiryError::AdminNotSet => "module admin not set",
            ExpiryError::NotAdmin => "caller is not admin",
            ExpiryError::InvalidExpiryDuration => "expiry duration must be nonzero",
            ExpiryError::InvalidAmount => "amount must be positive",
            ExpiryError::TransferInFailed => "token transfer in failed",
            ExpiryError::TransferOutFailed => "token transfer out failed",
        };
        write!(f, "{}", msg)
    }
}

impl std::error::Error for ExpiryError {}

/// Events emitted by this module, following the crate's dominant convention of
/// tuple-topic publishes (`(topic, key)` + data payload), as in `zk::merkle`
/// (`(EV_ZK_DEPOSIT, commitment)`), `zk::batch_insert` (`EV_ZK_BATCH_COMMIT`)
/// and `escrow::merkle` (`"merkle_add"`) — rather than `#[derive(Event)]`
/// structs, which the rest of this crate does not use.
///
/// Topic symbols (all ≤9 chars, `symbol_short`-compatible):
/// - `dep_create` — a deposit was created
/// - `dep_withdr` — a deposit was withdrawn before expiry
/// - `dep_refund` — an expired deposit was refunded to the depositor
/// - `dep_cfg`    — admin changed the expiry configuration
pub mod events {
    use soroban_sdk::{Address, BytesN, Env, Symbol};

    /// A deposit was created: `t_deposit` and `expires_at` are both published
    /// so indexers can compute expiry countdowns without re-deriving them.
    pub fn emit_create(
        env: &Env,
        index: u64,
        commitment: &BytesN<32>,
        depositor: &Address,
        recipient: &Address,
        token: &Address,
        amount: i128,
        deposited_at: u64,
        expires_at: u64,
    ) {
        env.events().publish(
            (Symbol::new(env, "dep_create"), index),
            (
                commitment.clone(),
                depositor.clone(),
                recipient.clone(),
                token.clone(),
                amount,
                deposited_at,
                expires_at,
            ),
        );
    }

    /// A live deposit was withdrawn by (or on behalf of) the recipient.
    pub fn emit_withdraw(env: &Env, index: u64, recipient: &Address, amount: i128) {
        env.events().publish(
            (Symbol::new(env, "dep_withdr"), index),
            (recipient.clone(), amount),
        );
    }

    /// An expired deposit was emergency-refunded to its depositor.
    pub fn emit_refund(env: &Env, index: u64, depositor: &Address, amount: i128) {
        env.events().publish(
            (Symbol::new(env, "dep_refund"), index),
            (depositor.clone(), amount),
        );
    }

    /// Admin updated the expiry configuration.
    pub fn emit_config(env: &Env, admin: &Address, enabled: bool, override_secs: Option<u64>) {
        env.events().publish(
            (Symbol::new(env, "dep_cfg"), admin.clone()),
            (enabled, override_secs),
        );
    }
}
