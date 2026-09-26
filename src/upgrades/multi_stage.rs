//! Multi-stage timelock execution queue for major protocol upgrades (Issue #996).
//!
//! Structural protocol updates must pass through three sequential stages before
//! the replacement WASM may be installed:
//!
//! 1. **Stage 1 — Public proposal intent notification** (24-hour delay).
//!    The intent is announced and must remain visible for a full day before the
//!    proposal can advance.
//! 2. **Stage 2 — Code payload verification and approval vote** (48-hour delay).
//!    The code payload is verified and an approval vote is recorded; the
//!    proposal must then wait a further 48 hours.
//! 3. **Stage 3 — Final execution window activation** (24-hour window).
//!    Once the Stage 2 delay elapses a 24-hour execution window opens. The
//!    upgrade may only be executed while that window is open; after it closes
//!    the proposal expires and must be re-proposed.
//!
//! The module is intentionally free of storage side effects so the state
//! machine can be unit-tested in isolation; the contract entry points in
//! `lib.rs` persist the returned state.

use soroban_sdk::{contracttype, Address, BytesN};

/// Stage 1 delay: public proposal intent notification (24 hours).
pub const STAGE1_INTENT_DELAY_SECONDS: u64 = 24 * 60 * 60;

/// Stage 2 delay: code payload verification and approval vote (48 hours).
pub const STAGE2_APPROVAL_DELAY_SECONDS: u64 = 48 * 60 * 60;

/// Stage 3 window: final execution window activation (24 hours).
pub const STAGE3_EXECUTION_WINDOW_SECONDS: u64 = 24 * 60 * 60;

/// The stage a queued upgrade is currently in.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimelockStage {
    /// Stage 1: intent announced, awaiting the 24-hour notification delay.
    IntentNotified,
    /// Stage 2: payload verified and approved, awaiting the 48-hour delay.
    PayloadApproved,
    /// Stage 3: execution window open, awaiting final execution.
    ExecutionWindowOpen,
    /// Terminal: the upgrade has been executed.
    Executed,
    /// Terminal: the execution window closed before execution.
    Expired,
}

/// A major upgrade travelling through the multi-stage timelock queue.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiStageUpgrade {
    /// WASM hash of the proposed replacement.
    pub new_wasm_hash: BytesN<32>,
    /// Address that announced the intent.
    pub proposer: Address,
    /// Ledger timestamp at which Stage 1 was entered.
    pub intent_at: u64,
    /// Ledger timestamp at which Stage 2 was entered (0 until approved).
    pub approved_at: u64,
    /// Ledger timestamp at which the Stage 3 window opens (0 until approved).
    pub window_opens_at: u64,
    /// Ledger timestamp at which the Stage 3 window closes (0 until approved).
    pub window_closes_at: u64,
    /// Current stage of the queue entry.
    pub stage: TimelockStage,
}

/// Announce the Stage 1 public proposal intent.
///
/// Returns a fresh queue entry in the [`TimelockStage::IntentNotified`] stage.
pub fn notify_intent(
    new_wasm_hash: BytesN<32>,
    proposer: Address,
    now: u64,
) -> MultiStageUpgrade {
    MultiStageUpgrade {
        new_wasm_hash,
        proposer,
        intent_at: now,
        approved_at: 0,
        window_opens_at: 0,
        window_closes_at: 0,
        stage: TimelockStage::IntentNotified,
    }
}

/// Whether the Stage 1 notification delay has fully elapsed.
pub fn is_intent_delay_elapsed(entry: &MultiStageUpgrade, now: u64) -> bool {
    now.saturating_sub(entry.intent_at) >= STAGE1_INTENT_DELAY_SECONDS
}

/// Advance from Stage 1 to Stage 2 after verifying the code payload.
///
/// Fails with `None` if the entry is not in Stage 1 or the 24-hour intent
/// notification delay has not yet elapsed.
pub fn approve_payload(
    entry: &MultiStageUpgrade,
    now: u64,
) -> Option<MultiStageUpgrade> {
    if entry.stage != TimelockStage::IntentNotified {
        return None;
    }
    if !is_intent_delay_elapsed(entry, now) {
        return None;
    }

    let window_opens_at = now.checked_add(STAGE2_APPROVAL_DELAY_SECONDS)?;
    let window_closes_at = window_opens_at.checked_add(STAGE3_EXECUTION_WINDOW_SECONDS)?;

    Some(MultiStageUpgrade {
        new_wasm_hash: entry.new_wasm_hash.clone(),
        proposer: entry.proposer.clone(),
        intent_at: entry.intent_at,
        approved_at: now,
        window_opens_at,
        window_closes_at,
        stage: TimelockStage::PayloadApproved,
    })
}

/// Whether the Stage 3 execution window is currently open.
pub fn is_execution_window_open(entry: &MultiStageUpgrade, now: u64) -> bool {
    entry.stage == TimelockStage::ExecutionWindowOpen
        && now >= entry.window_opens_at
        && now <= entry.window_closes_at
}

/// Refresh the stage of an approved entry against the current timestamp.
///
/// Transitions `PayloadApproved` to `ExecutionWindowOpen` once the 48-hour
/// delay elapses, and to `Expired` once the 24-hour window closes.
pub fn refresh_stage(entry: &MultiStageUpgrade, now: u64) -> MultiStageUpgrade {
    if entry.stage != TimelockStage::PayloadApproved {
        return entry.clone();
    }

    let mut updated = entry.clone();
    if now > entry.window_closes_at {
        updated.stage = TimelockStage::Expired;
    } else if now >= entry.window_opens_at {
        updated.stage = TimelockStage::ExecutionWindowOpen;
    }
    updated
}

/// Attempt to execute the queued upgrade.
///
/// Returns the executed entry (in the [`TimelockStage::Executed`] stage) when
/// the Stage 3 execution window is open, otherwise `None`.
pub fn execute(entry: &MultiStageUpgrade, now: u64) -> Option<MultiStageUpgrade> {
    let refreshed = refresh_stage(entry, now);
    if !is_execution_window_open(&refreshed, now) {
        return None;
    }

    let mut executed = refreshed;
    executed.stage = TimelockStage::Executed;
    Some(executed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{BytesN, Env};

    fn entry(env: &Env, now: u64) -> MultiStageUpgrade {
        let hash = BytesN::from_array(env, &[7u8; 32]);
        let proposer = Address::generate(env);
        notify_intent(hash, proposer, now)
    }

    #[test]
    fn stage1_requires_full_24h_delay() {
        let env = Env::default();
        let e = entry(&env, 1_000);

        assert!(!is_intent_delay_elapsed(&e, 1_000 + STAGE1_INTENT_DELAY_SECONDS - 1));
        assert!(is_intent_delay_elapsed(&e, 1_000 + STAGE1_INTENT_DELAY_SECONDS));
        assert!(approve_payload(&e, 1_000 + STAGE1_INTENT_DELAY_SECONDS - 1).is_none());
    }

    #[test]
    fn stage2_opens_window_after_48h() {
        let env = Env::default();
        let e = entry(&env, 0);
        let approved_at = STAGE1_INTENT_DELAY_SECONDS;
        let approved = approve_payload(&e, approved_at).unwrap();

        assert_eq!(approved.stage, TimelockStage::PayloadApproved);
        assert_eq!(
            approved.window_opens_at,
            approved_at + STAGE2_APPROVAL_DELAY_SECONDS
        );
        assert_eq!(
            approved.window_closes_at,
            approved.window_opens_at + STAGE3_EXECUTION_WINDOW_SECONDS
        );

        // Still waiting: window not yet open.
        let before = refresh_stage(&approved, approved.window_opens_at - 1);
        assert_eq!(before.stage, TimelockStage::PayloadApproved);
        assert!(execute(&approved, approved.window_opens_at - 1).is_none());

        // Window opens exactly at window_opens_at.
        let open = refresh_stage(&approved, approved.window_opens_at);
        assert_eq!(open.stage, TimelockStage::ExecutionWindowOpen);
    }

    #[test]
    fn stage3_executes_only_inside_window() {
        let env = Env::default();
        let e = entry(&env, 0);
        let approved = approve_payload(&e, STAGE1_INTENT_DELAY_SECONDS).unwrap();

        let executed = execute(&approved, approved.window_opens_at).unwrap();
        assert_eq!(executed.stage, TimelockStage::Executed);

        // Last instant of the window is still valid.
        assert!(execute(&approved, approved.window_closes_at).is_some());

        // One second past the window the proposal expires.
        let expired = refresh_stage(&approved, approved.window_closes_at + 1);
        assert_eq!(expired.stage, TimelockStage::Expired);
        assert!(execute(&approved, approved.window_closes_at + 1).is_none());
    }

    #[test]
    fn cannot_approve_twice_or_out_of_order() {
        let env = Env::default();
        let e = entry(&env, 0);
        let approved = approve_payload(&e, STAGE1_INTENT_DELAY_SECONDS).unwrap();

        // Approving an already-approved entry is rejected.
        assert!(approve_payload(&approved, STAGE1_INTENT_DELAY_SECONDS * 2).is_none());
    }

    #[test]
    fn rejects_timestamp_overflow() {
        let env = Env::default();
        let e = entry(&env, u64::MAX - STAGE1_INTENT_DELAY_SECONDS);
        // intent delay elapsed, but window arithmetic overflows.
        assert!(approve_payload(&e, u64::MAX).is_none());
    }
}
