//! Anchor Remittance Timeout Dispute Escalation Guard (Issue #917).
//!
//! Provides automated dispute resolution when cross-border fiat payouts exceed
//! their processing window. After a 24-hour expiration, the original sender may
//! trigger a dispute, slashing the anchor's locked collateral bond and receiving
//! a full refund of the original remittance principal.

use soroban_sdk::{contracttype, symbol_short, token, Address, Env};

use crate::ContractError;

/// Processing window after which a sender may trigger a dispute (24 hours in seconds).
pub const DISPUTE_EXPIRY_SECONDS: u64 = 24 * 60 * 60;

/// Storage keys for the anchored remittance dispute module.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DisputeStorageKey {
    /// Anchored remittance record keyed by its unique ID.
    Remittance(u64),
    /// Monotonically increasing ID counter.
    RemittanceCounter,
    /// Slash destination (e.g. protocol treasury or insurance fund).
    SlashDestination,
}

/// Status of an anchored remittance.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RemittanceStatus {
    /// Remittance is active and within the processing window.
    Pending,
    /// Anchor submitted proof of payout; remittance settled successfully.
    Settled,
    /// Dispute escalated: anchor slashed, principal refunded to sender.
    Disputed,
}

/// An anchored cross-border remittance record stored on-chain.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnchoredRemittance {
    /// Original sender of the remittance.
    pub sender: Address,
    /// Anchor entity responsible for executing the fiat payout.
    pub anchor: Address,
    /// Token used for the on-chain leg of the remittance.
    pub token: Address,
    /// Principal amount held in escrow.
    pub principal: i128,
    /// Anchor's locked collateral bond amount (slashed on dispute).
    pub anchor_bond: i128,
    /// Ledger timestamp at which the remittance was created.
    pub created_at: u64,
    /// Current status of this remittance.
    pub status: RemittanceStatus,
}

/// Result returned after a successful dispute escalation.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemittanceDisputeResult {
    /// Unique ID of the disputed remittance.
    pub remittance_id: u64,
    /// Principal amount refunded to the sender.
    pub principal_refunded: i128,
    /// Collateral bond amount slashed from the anchor.
    pub bond_slashed: i128,
    /// Ledger timestamp of the dispute resolution.
    pub resolved_at: u64,
}

/// Create a new anchored remittance escrow.
///
/// Transfers `principal` from `sender` and `anchor_bond` from `anchor` into
/// the contract, locking both until the anchor submits proof of payout or a
/// dispute is escalated.
///
/// Returns the unique remittance ID.
pub fn create_anchored_remittance(
    env: &Env,
    sender: Address,
    anchor: Address,
    token: Address,
    principal: i128,
    anchor_bond: i128,
) -> Result<u64, ContractError> {
    sender.require_auth();
    anchor.require_auth();

    if principal <= 0 {
        return Err(ContractError::AmountTooLow);
    }
    if anchor_bond <= 0 {
        return Err(ContractError::InvalidStakeAmount);
    }

    let token_client = token::Client::new(env, &token);

    // Lock principal from sender.
    token_client.transfer(&sender, &env.current_contract_address(), &principal);
    // Lock anchor bond from anchor.
    token_client.transfer(&anchor, &env.current_contract_address(), &anchor_bond);

    // Assign a new ID.
    let mut counter: u64 = env
        .storage()
        .persistent()
        .get(&DisputeStorageKey::RemittanceCounter)
        .unwrap_or(0u64);
    counter += 1;
    env.storage().persistent().set(&DisputeStorageKey::RemittanceCounter, &counter);

    let remittance = AnchoredRemittance {
        sender: sender.clone(),
        anchor: anchor.clone(),
        token: token.clone(),
        principal,
        anchor_bond,
        created_at: env.ledger().timestamp(),
        status: RemittanceStatus::Pending,
    };
    env.storage()
        .persistent()
        .set(&DisputeStorageKey::Remittance(counter), &remittance);

    env.events().publish(
        (symbol_short!("RemitNew"), sender),
        (counter, token, principal, anchor_bond),
    );

    Ok(counter)
}

/// Mark a remittance as settled (called by the anchor after fiat payout proof).
///
/// Only the anchor may call this, and only while the remittance is still Pending.
pub fn settle_remittance(env: &Env, remittance_id: u64) -> Result<(), ContractError> {
    let key = DisputeStorageKey::Remittance(remittance_id);
    let mut remittance: AnchoredRemittance = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(ContractError::NotRegistered)?;

    remittance.anchor.require_auth();

    if remittance.status != RemittanceStatus::Pending {
        return Err(ContractError::InvalidEscrowState);
    }

    let token_client = token::Client::new(env, &remittance.token);

    // Return principal to sender and release bond back to anchor.
    token_client.transfer(
        &env.current_contract_address(),
        &remittance.sender,
        &remittance.principal,
    );
    token_client.transfer(
        &env.current_contract_address(),
        &remittance.anchor,
        &remittance.anchor_bond,
    );

    remittance.status = RemittanceStatus::Settled;
    env.storage().persistent().set(&key, &remittance);

    env.events().publish(
        (symbol_short!("RemitSetl"), remittance.anchor),
        remittance_id,
    );

    Ok(())
}

/// Trigger a dispute after the 24-hour processing window has elapsed.
///
/// # Behaviour
/// 1. Checks that at least `DISPUTE_EXPIRY_SECONDS` have passed since creation.
/// 2. Slashes the anchor's locked collateral bond $B_{anchor}$ to the slash
///    destination (protocol treasury / insurance fund).
/// 3. Refunds the original remittance principal directly to the sender.
/// 4. Emits a structured `RemitDisp` event.
///
/// Only the original sender may call this.
pub fn escalate_dispute(
    env: &Env,
    sender: Address,
    remittance_id: u64,
) -> Result<RemittanceDisputeResult, ContractError> {
    sender.require_auth();

    let key = DisputeStorageKey::Remittance(remittance_id);
    let mut remittance: AnchoredRemittance = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(ContractError::NotRegistered)?;

    // Only the original sender may escalate.
    if remittance.sender != sender {
        return Err(ContractError::Unauthorized);
    }

    // Must be in Pending status (not already settled or disputed).
    if remittance.status != RemittanceStatus::Pending {
        return Err(ContractError::InvalidEscrowState);
    }

    // Enforce 24-hour expiry window.
    let now = env.ledger().timestamp();
    let elapsed = now.saturating_sub(remittance.created_at);
    if elapsed < DISPUTE_EXPIRY_SECONDS {
        return Err(ContractError::DeadlineNotReached);
    }

    let slash_destination: Address = env
        .storage()
        .persistent()
        .get(&DisputeStorageKey::SlashDestination)
        .ok_or(ContractError::NotInitialized)?;

    let token_client = token::Client::new(env, &remittance.token);

    // Slash anchor collateral bond to the protocol slash destination.
    token_client.transfer(
        &env.current_contract_address(),
        &slash_destination,
        &remittance.anchor_bond,
    );

    // Refund original principal to sender.
    token_client.transfer(
        &env.current_contract_address(),
        &remittance.sender,
        &remittance.principal,
    );

    remittance.status = RemittanceStatus::Disputed;
    env.storage().persistent().set(&key, &remittance);

    let result = RemittanceDisputeResult {
        remittance_id,
        principal_refunded: remittance.principal,
        bond_slashed: remittance.anchor_bond,
        resolved_at: now,
    };

    // Emit structured dispute event.
    env.events().publish(
        (symbol_short!("RemitDisp"), sender),
        result.clone(),
    );

    Ok(result)
}

/// Configure the slash destination address (admin only).
pub fn set_slash_destination(env: &Env, admin: Address, destination: Address) {
    admin.require_auth();
    env.storage()
        .persistent()
        .set(&DisputeStorageKey::SlashDestination, &destination);
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger};
    use soroban_sdk::Env;

    #[test]
    fn dispute_expiry_is_24_hours() {
        assert_eq!(DISPUTE_EXPIRY_SECONDS, 86_400);
    }

    #[test]
    fn dispute_before_expiry_returns_deadline_not_reached() {
        let env = Env::default();
        env.mock_all_auths();
        let sender = Address::generate(&env);

        // Fabricate a pending remittance that was just created.
        let remittance = AnchoredRemittance {
            sender: sender.clone(),
            anchor: Address::generate(&env),
            token: Address::generate(&env),
            principal: 1_000_000,
            anchor_bond: 500_000,
            created_at: env.ledger().timestamp(),
            status: RemittanceStatus::Pending,
        };
        env.storage()
            .persistent()
            .set(&DisputeStorageKey::Remittance(1u64), &remittance);

        let result = escalate_dispute(&env, sender, 1u64);
        assert_eq!(result, Err(ContractError::DeadlineNotReached));
    }

    #[test]
    fn dispute_missing_remittance_returns_not_registered() {
        let env = Env::default();
        env.mock_all_auths();
        let result = escalate_dispute(&env, Address::generate(&env), 999u64);
        assert_eq!(result, Err(ContractError::NotRegistered));
    }
}
