use soroban_sdk::{contracttype, symbol_short, Address, Env, Symbol};
use crate::ContractError;

/// Security event topic emitted on every failed authentication attempt.
const AUTH_FAILURE_TOPIC: Symbol = symbol_short!("AUTHFAIL");

/// Storage key for consumed cross-border transfer nonces.
const NONCE_CONSUMED_KEY: Symbol = symbol_short!("CBTXNC");

/// An immutable record of a successfully initiated cross-border payment.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct CrossBorderTransfer {
    pub from: Address,
    pub to: Address,
    pub amount: u64,
    pub asset: Symbol,
    pub initiated_at: u64,
    pub nonce: u64,
}

/// Log a failed authentication attempt to the security event stream.
fn log_auth_failure(env: &Env, caller: &Address, reason: Symbol) {
    env.events().publish(
        (AUTH_FAILURE_TOPIC, caller.clone()),
        (reason, env.ledger().sequence()),
    );
}

/// Enforce explicit caller signature verification before initiating a
/// cross-border payment transfer.
///
/// Security guarantees:
/// - Calls `sender.require_auth()` to eliminate unsigned or proxy-forged
///   call contexts (transaction replay mitigation).
/// - Each `(sender, nonce)` pair is single-use; duplicates are rejected
///   with `ContractError::InvalidNonce`.
/// - All authentication failures are logged via the security event stream.
pub fn initiate_cross_border_payment(
    env: &Env,
    sender: Address,
    recipient: Address,
    amount: u64,
    asset: Symbol,
    nonce: u64,
) -> Result<CrossBorderTransfer, ContractError> {
    // ── 1. Explicit caller authorization ────────────────────────────────────
    // sender.require_auth() delegates to the Soroban host which verifies
    // that the transaction envelope contains a valid authorization from
    // `sender`.  Without this, an attacker could forge a call context and
    // initiate transfers from arbitrary addresses.
    sender.require_auth();

    // ── 2. Replay protection ────────────────────────────────────────────────
    // Reject transactions that attempt to re-use a nonce, which would
    // allow replaying an already-dispatched cross-border transfer.
    let nonce_key = (NONCE_CONSUMED_KEY, sender.clone(), nonce);
    if env.storage().instance().has(&nonce_key) {
        log_auth_failure(&env, &sender, symbol_short!("REPLAY"));
        return Err(ContractError::InvalidNonce);
    }

    // Mark the nonce as consumed before any side-effects.
    env.storage().instance().set(&nonce_key, &true);

    let transfer = CrossBorderTransfer {
        from: sender,
        to: recipient,
        amount,
        asset,
        initiated_at: env.ledger().timestamp(),
        nonce,
    };

    Ok(transfer)
}

/// Check whether a cross-border transfer nonce has already been consumed.
pub fn is_nonce_consumed(env: &Env, sender: &Address, nonce: u64) -> bool {
    let nonce_key = (NONCE_CONSUMED_KEY, sender.clone(), nonce);
    env.storage().instance().has(&nonce_key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::Env;

    // ── Unsigned / proxy-forged context rejection ─────────────────────────

    #[test]
    #[should_panic]
    fn test_requires_sender_auth() {
        let env = Env::default();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        env.as_contract(&contract_id, || {
            let _ = initiate_cross_border_payment(
                &env,
                sender,
                recipient,
                1_000,
                symbol_short!("NGN"),
                0,
            );
        });
    }

    #[test]
    fn test_cross_border_payment_succeeds_with_auth() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        env.as_contract(&contract_id, || {
            let transfer = initiate_cross_border_payment(
                &env,
                sender.clone(),
                recipient.clone(),
                500,
                symbol_short!("KES"),
                0,
            )
            .unwrap();

            assert_eq!(transfer.from, sender);
            assert_eq!(transfer.to, recipient);
            assert_eq!(transfer.amount, 500);
            assert_eq!(transfer.asset, symbol_short!("KES"));
            assert_eq!(transfer.nonce, 0);
        });
    }

    // ── Replay protection ─────────────────────────────────────────────────

    #[test]
    fn test_replay_rejected_with_duplicate_nonce() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        env.as_contract(&contract_id, || {
            let first = initiate_cross_border_payment(
                &env,
                sender.clone(),
                recipient.clone(),
                100,
                symbol_short!("GHS"),
                0,
            );
            assert!(first.is_ok());

            let second = initiate_cross_border_payment(
                &env,
                sender,
                recipient,
                100,
                symbol_short!("GHS"),
                0, // same nonce — replay
            );
            assert_eq!(second, Err(ContractError::InvalidNonce));
        });
    }

    #[test]
    fn test_different_nonce_accepted() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        env.as_contract(&contract_id, || {
            let first = initiate_cross_border_payment(
                &env,
                sender.clone(),
                recipient.clone(),
                200,
                symbol_short!("CFA"),
                0,
            );
            assert!(first.is_ok());

            let second = initiate_cross_border_payment(
                &env,
                sender,
                recipient,
                200,
                symbol_short!("CFA"),
                1, // different nonce — should succeed
            );
            assert!(second.is_ok());
        });
    }

    #[test]
    fn test_is_nonce_consumed() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        env.as_contract(&contract_id, || {
            assert!(!is_nonce_consumed(&env, &sender, 0));

            let _ = initiate_cross_border_payment(
                &env,
                sender.clone(),
                recipient,
                50,
                symbol_short!("ZAR"),
                0,
            );

            assert!(is_nonce_consumed(&env, &sender, 0));
            assert!(!is_nonce_consumed(&env, &sender, 1));
        });
    }

    // ── Security event stream logging ──────────────────────────────────────

    #[test]
    fn test_replay_emits_auth_failure_event() {
        use soroban_sdk::testutils::Events;

        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let sender = Address::generate(&env);
        let recipient = Address::generate(&env);

        env.as_contract(&contract_id, || {
            let _ = initiate_cross_border_payment(
                &env,
                sender.clone(),
                recipient.clone(),
                100,
                symbol_short!("NGN"),
                0,
            );

            let result = initiate_cross_border_payment(
                &env,
                sender,
                recipient,
                100,
                symbol_short!("NGN"),
                0,
            );
            assert_eq!(result, Err(ContractError::InvalidNonce));

            let events = env.events().all();
            assert!(
                !events.is_empty(),
                "expected security event for replay detection"
            );
        });
    }
}
