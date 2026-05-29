//! Slashing module — malicious node collateral slashing (issue #260).
//!
//! When the off-chain monitoring engine flags bad data or extended downtime,
//! governance can penalise a relayer by calling `execute_slash` (direct admin
//! path) or by going through the full propose → vote → execute pipeline
//! (`propose_action` with `action_type = 5`).
//!
//! # Flow
//! 1. Admin(s) call `propose_action(action_type=5, target=bad_relayer, data="<amount>")`.
//! 2. Other admins vote via `vote_for_action`.
//! 3. Once the threshold is met, any admin calls `execute_proposed_action`.
//!    Internally this calls `execute_slash_internal` below.
//!
//! Alternatively, a single authorized admin can call `execute_slash` directly
//! (suitable for single-admin deployments or emergency situations).
//!
//! # Storage layout
//! | Key                              | Type      | Description                              |
//! |----------------------------------|-----------|------------------------------------------|
//! | `DataKey::ProviderStake(addr)`   | `i128`    | Staked collateral per relayer (stroops)  |
//! | `DataKey::SlashToken`            | `Address` | SEP-41 token used for staking/slashing   |
//! | `DataKey::InsuranceReserve`      | `Address` | Destination for slashed funds            |

use soroban_sdk::{token, Address, Env, String, Symbol};

use crate::types::DataKey;
use crate::Error;
use crate::SlashExecutedEvent;

// ─────────────────────────────────────────────────────────────────────────────
// Internal helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Read the staked balance for a relayer. Returns 0 if no stake has been deposited.
pub fn get_stake(env: &Env, relayer: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::ProviderStake(relayer.clone()))
        .unwrap_or(0)
}

/// Overwrite the staked balance for a relayer.
fn set_stake(env: &Env, relayer: &Address, amount: i128) {
    env.storage()
        .persistent()
        .set(&DataKey::ProviderStake(relayer.clone()), &amount);
}

/// Parse a slash amount from the governance proposal's `data` string.
///
/// The data field is expected to contain a plain decimal integer string,
/// e.g. `"5000000000"` (5 000 000 000 stroops = 500 tokens at 7 decimals).
///
/// Returns `Error::InvalidSlashAmount` if the string is empty, contains
/// non-digit characters, or would overflow `i128`.
pub fn parse_slash_amount(_env: &Env, data: &String) -> Result<i128, Error> {
    let len = data.len() as usize;
    if len == 0 {
        return Err(Error::InvalidSlashAmount);
    }

    // i128::MAX is 39 digits; 40 bytes is a safe upper bound.
    if len > 39 {
        return Err(Error::InvalidSlashAmount);
    }

    // Copy the string bytes into a stack-allocated buffer.
    let mut buf = [0u8; 39];
    data.copy_into_slice(&mut buf[..len]);

    let mut result: i128 = 0;
    for i in 0..len {
        let ch = buf[i];
        if ch < b'0' || ch > b'9' {
            return Err(Error::InvalidSlashAmount);
        }
        let digit = (ch - b'0') as i128;
        result = result
            .checked_mul(10)
            .and_then(|v| v.checked_add(digit))
            .ok_or(Error::InvalidSlashAmount)?;
    }

    if result <= 0 {
        return Err(Error::InvalidSlashAmount);
    }

    Ok(result)
}

// ─────────────────────────────────────────────────────────────────────────────
// Core slash logic
// ─────────────────────────────────────────────────────────────────────────────

/// Execute a slash against a relayer's staked collateral.
///
/// This is the single authoritative implementation called by both:
/// - `PriceOracle::execute_slash` (direct admin path), and
/// - the `AdminAction::Slash` arm inside `execute_proposed_action` (governance pipeline).
///
/// # Preconditions (checked by callers before this function is invoked)
/// - Contract is not destroyed.
/// - Contract is not frozen.
/// - `executor` has provided auth and is an authorized admin.
///
/// # Checks performed here
/// - `amount` must be > 0.
/// - `SlashToken` must be configured.
/// - `InsuranceReserve` must be configured.
/// - `bad_relayer` must have a stake ≥ `amount`.
///
/// # Effects
/// 1. Deducts `amount` from `bad_relayer`'s on-chain stake balance.
/// 2. Transfers `amount` tokens from the contract's custody to the insurance reserve.
/// 3. If the relayer's remaining stake reaches zero, removes them from the
///    active provider whitelist (they can re-stake and be re-added later).
/// 4. Emits a `SlashExecutedEvent`.
pub fn execute_slash_internal(
    env: &Env,
    executor: &Address,
    bad_relayer: &Address,
    amount: i128,
) -> Result<(), Error> {
    // ── Validate amount ──────────────────────────────────────────────────────
    if amount <= 0 {
        return Err(Error::InvalidSlashAmount);
    }

    // ── Resolve token and reserve ────────────────────────────────────────────
    let token_address: Address = env
        .storage()
        .persistent()
        .get(&DataKey::SlashToken)
        .ok_or(Error::SlashTokenNotSet)?;

    let reserve: Address = env
        .storage()
        .persistent()
        .get(&DataKey::InsuranceReserve)
        .ok_or(Error::InsuranceReserveNotSet)?;

    // ── Check stake balance ──────────────────────────────────────────────────
    let current_stake = get_stake(env, bad_relayer);
    if amount > current_stake {
        return Err(Error::InsufficientStake);
    }

    // ── Deduct stake ─────────────────────────────────────────────────────────
    let remaining_stake = current_stake - amount;
    set_stake(env, bad_relayer, remaining_stake);

    // ── Transfer slashed tokens to the insurance reserve ─────────────────────
    // The contract holds the staked tokens in its own custody, so we transfer
    // from `current_contract_address()` to the reserve.
    let token_client = token::Client::new(env, &token_address);
    token_client.transfer(&env.current_contract_address(), &reserve, &amount);

    // ── Auto-delist relayer if fully slashed ─────────────────────────────────
    // A relayer with zero stake can no longer be trusted to submit prices.
    // Remove them from the whitelist so they cannot submit until they re-stake
    // and are explicitly re-added by an admin.
    if remaining_stake == 0 {
        crate::auth::_remove_provider(env, bad_relayer);
    }

    // ── Emit event ───────────────────────────────────────────────────────────
    env.events().publish_event(&SlashExecutedEvent {
        bad_relayer: bad_relayer.clone(),
        amount,
        reserve: reserve.clone(),
        executor: executor.clone(),
    });

    // Also publish a plain tuple event for off-chain indexers that don't parse
    // the typed event schema.
    env.events().publish(
        (Symbol::new(env, "slash_executed"),),
        (
            bad_relayer.clone(),
            amount,
            reserve,
            executor.clone(),
            remaining_stake,
        ),
    );

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod slashing_tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::{Env, String};

    // ── parse_slash_amount ────────────────────────────────────────────────────

    #[test]
    fn test_parse_slash_amount_valid() {
        let env = Env::default();
        let s = String::from_str(&env, "5000000000");
        assert_eq!(parse_slash_amount(&env, &s).unwrap(), 5_000_000_000_i128);
    }

    #[test]
    fn test_parse_slash_amount_single_digit() {
        let env = Env::default();
        let s = String::from_str(&env, "1");
        assert_eq!(parse_slash_amount(&env, &s).unwrap(), 1_i128);
    }

    #[test]
    fn test_parse_slash_amount_empty_fails() {
        let env = Env::default();
        let s = String::from_str(&env, "");
        assert_eq!(parse_slash_amount(&env, &s), Err(Error::InvalidSlashAmount));
    }

    #[test]
    fn test_parse_slash_amount_zero_fails() {
        let env = Env::default();
        let s = String::from_str(&env, "0");
        assert_eq!(parse_slash_amount(&env, &s), Err(Error::InvalidSlashAmount));
    }

    #[test]
    fn test_parse_slash_amount_non_digit_fails() {
        let env = Env::default();
        let s = String::from_str(&env, "100abc");
        assert_eq!(parse_slash_amount(&env, &s), Err(Error::InvalidSlashAmount));
    }

    // ── get_stake / set_stake ─────────────────────────────────────────────────

    #[test]
    fn test_get_stake_returns_zero_when_unset() {
        let env = Env::default();
        let relayer = Address::generate(&env);
        assert_eq!(get_stake(&env, &relayer), 0);
    }

    #[test]
    fn test_set_and_get_stake() {
        let env = Env::default();
        let relayer = Address::generate(&env);
        set_stake(&env, &relayer, 1_000_000);
        assert_eq!(get_stake(&env, &relayer), 1_000_000);
    }
}
