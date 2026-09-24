#![cfg(test)]

use super::*;
use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::Env;

fn setup() -> (Env, Address, LpPoolClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, LpPool);
    let client = LpPoolClient::new(&env, &contract_id);
    (env, contract_id, client)
}

// ---------------------------------------------------------------------
// Basic mechanics / correctness
// ---------------------------------------------------------------------

#[test]
fn test_initialize_sets_zero_reserves() {
    let (env, _contract_id, client) = setup();
    let token_a = Address::generate(&env);
    let token_b = Address::generate(&env);

    client.initialize(&token_a, &token_b);

    assert_eq!(client.get_reserves(), (0, 0));
    assert_eq!(client.get_total_shares(), 0);
    assert_eq!(client.get_tokens(), (token_a, token_b));
}

#[test]
fn test_initialize_twice_fails() {
    let (env, _contract_id, client) = setup();
    let token_a = Address::generate(&env);
    let token_b = Address::generate(&env);

    client.initialize(&token_a, &token_b);
    let result = client.try_initialize(&token_a, &token_b);
    match result {
        Err(Ok(e)) => assert_eq!(e, Error::AlreadyInitialized),
        other => panic!("expected AlreadyInitialized, got {:?}", other),
    }
}

#[test]
fn test_first_deposit_mints_sqrt_shares() {
    let (env, _contract_id, client) = setup();
    let token_a = Address::generate(&env);
    let token_b = Address::generate(&env);
    let user = Address::generate(&env);

    client.initialize(&token_a, &token_b);
    let minted = client.deposit(&user, &1_000_000, &1_000_000);

    // isqrt(1_000_000 * 1_000_000) == 1_000_000
    assert_eq!(minted, 1_000_000);
    assert_eq!(client.get_reserves(), (1_000_000, 1_000_000));
    assert_eq!(client.get_total_shares(), 1_000_000);
    assert_eq!(client.get_shares(&user), 1_000_000);
}

#[test]
fn test_second_deposit_mints_proportional_shares() {
    let (env, _contract_id, client) = setup();
    let token_a = Address::generate(&env);
    let token_b = Address::generate(&env);
    let user1 = Address::generate(&env);
    let user2 = Address::generate(&env);

    client.initialize(&token_a, &token_b);
    client.deposit(&user1, &1_000_000, &1_000_000);
    let minted = client.deposit(&user2, &500_000, &500_000);

    assert_eq!(minted, 500_000);
    assert_eq!(client.get_reserves(), (1_500_000, 1_500_000));
    assert_eq!(client.get_total_shares(), 1_500_000);
    assert_eq!(client.get_shares(&user2), 500_000);
}

#[test]
fn test_swap_preserves_invariant_and_respects_slippage() {
    let (env, _contract_id, client) = setup();
    let token_a = Address::generate(&env);
    let token_b = Address::generate(&env);
    let user = Address::generate(&env);

    client.initialize(&token_a, &token_b);
    client.deposit(&user, &1_500_000, &1_500_000);

    let (reserve_a_before, reserve_b_before) = client.get_reserves();
    let k_before = reserve_a_before * reserve_b_before;

    let amount_out = client.swap(&user, &token_a, &100_000, &93_750);
    assert_eq!(amount_out, 93_750);

    let (reserve_a_after, reserve_b_after) = client.get_reserves();
    assert_eq!(reserve_a_after, 1_600_000);
    assert_eq!(reserve_b_after, 1_406_250);

    let k_after = reserve_a_after * reserve_b_after;
    // No-fee constant-product swaps round the output down, so k never decreases.
    assert!(k_after >= k_before);

    // A stricter min_amount_out than what the pool can deliver must fail.
    let result = client.try_swap(&user, &token_a, &100_000, &999_999);
    match result {
        Err(Ok(e)) => assert_eq!(e, Error::SlippageExceeded),
        other => panic!("expected SlippageExceeded, got {:?}", other),
    }
}

#[test]
fn test_swap_with_invalid_token_fails() {
    let (env, _contract_id, client) = setup();
    let token_a = Address::generate(&env);
    let token_b = Address::generate(&env);
    let stranger_token = Address::generate(&env);
    let user = Address::generate(&env);

    client.initialize(&token_a, &token_b);
    client.deposit(&user, &1_000_000, &1_000_000);

    let result = client.try_swap(&user, &stranger_token, &1_000, &0);
    match result {
        Err(Ok(e)) => assert_eq!(e, Error::InvalidToken),
        other => panic!("expected InvalidToken, got {:?}", other),
    }
}

#[test]
fn test_withdraw_returns_proportional_reserves() {
    let (env, _contract_id, client) = setup();
    let token_a = Address::generate(&env);
    let token_b = Address::generate(&env);
    let user = Address::generate(&env);

    client.initialize(&token_a, &token_b);
    client.deposit(&user, &1_500_000, &1_500_000);
    client.swap(&user, &token_a, &100_000, &93_750);

    // reserves are now (1_600_000, 1_406_250), total_shares = 1_500_000
    let (amount_a, amount_b) = client.withdraw(&user, &750_000);

    assert_eq!(amount_a, 800_000);
    assert_eq!(amount_b, 703_125);
    assert_eq!(client.get_shares(&user), 750_000);
    assert_eq!(client.get_reserves(), (800_000, 703_125));
}

#[test]
fn test_withdraw_more_than_owned_fails() {
    let (env, _contract_id, client) = setup();
    let token_a = Address::generate(&env);
    let token_b = Address::generate(&env);
    let user = Address::generate(&env);

    client.initialize(&token_a, &token_b);
    client.deposit(&user, &1_000_000, &1_000_000);

    let result = client.try_withdraw(&user, &2_000_000);
    match result {
        Err(Ok(e)) => assert_eq!(e, Error::InsufficientShares),
        other => panic!("expected InsufficientShares, got {:?}", other),
    }
}

#[test]
fn test_zero_amount_deposit_and_swap_fail() {
    let (env, _contract_id, client) = setup();
    let token_a = Address::generate(&env);
    let token_b = Address::generate(&env);
    let user = Address::generate(&env);

    client.initialize(&token_a, &token_b);

    let deposit_result = client.try_deposit(&user, &0, &1_000);
    match deposit_result {
        Err(Ok(e)) => assert_eq!(e, Error::ZeroAmount),
        other => panic!("expected ZeroAmount, got {:?}", other),
    }

    client.deposit(&user, &1_000_000, &1_000_000);
    let swap_result = client.try_swap(&user, &token_a, &0, &0);
    match swap_result {
        Err(Ok(e)) => assert_eq!(e, Error::ZeroAmount),
        other => panic!("expected ZeroAmount, got {:?}", other),
    }
}

#[test]
fn test_deposit_before_initialize_fails() {
    let (env, _contract_id, client) = setup();
    let user = Address::generate(&env);

    let result = client.try_deposit(&user, &1_000, &1_000);
    match result {
        Err(Ok(e)) => assert_eq!(e, Error::NotInitialized),
        other => panic!("expected NotInitialized, got {:?}", other),
    }
}

// ---------------------------------------------------------------------
// TTL extension — the actual deliverable of issue #768
// ---------------------------------------------------------------------

// soroban-sdk 20.x (pinned by this workspace) does not expose a `get_ttl`
// reader on persistent storage — that landed in a later SDK version — so
// these tests prove the extension behaviorally instead of by reading the
// raw TTL counter: the sandbox's fresh persistent entries start out with
// only `min_persistent_entry_ttl` (4096 ledgers, see `Env::default()`'s
// `LedgerInfo`), which is far below this contract's `BUMP_AMOUNT`
// (1_036_800 ledgers). Jumping the ledger forward to one ledger before a
// never-bumped entry (TTL 4096) would have become archived, and having the
// *next* call still succeed (rather than panic on an archived entry) is
// only possible if the previous call actually extended the TTL out to
// `BUMP_AMOUNT` — so a passing test is direct proof the bump ran.
//
// Note on chaining a second jump: `extend_ttl` only bumps an entry that is
// actually below `BUMP_THRESHOLD` — an already-healthy entry is left alone
// (that's the "zero extra cost" no-op the doc comment on `bump_pool_ttl`
// promises). So an entry's *next* expiration point stays anchored to
// whichever call last actually bumped it, not to "BUMP_AMOUNT from now" on
// every subsequent call — a second full-length jump from a call that was a
// no-op would overshoot the real expiration and archive the entry for real.
// These tests therefore each prove one clean bump cycle rather than
// chaining jumps against a moving, call-dependent baseline.

/// Proves that `deposit` extends the pool reserves' and the caller's own
/// share record's TTL when it is running low, so a pool that keeps getting
/// used never lets those entries reach archival — even when a call arrives
/// just before expiration.
#[test]
fn test_deposit_bumps_pool_and_user_ttl_before_expiration() {
    let (env, _contract_id, client) = setup();
    let token_a = Address::generate(&env);
    let token_b = Address::generate(&env);
    let user = Address::generate(&env);

    env.ledger().with_mut(|li| li.sequence_number = 1_000);
    client.initialize(&token_a, &token_b);
    // First touch: entries start at the sandbox default (4096 ledgers) and
    // this call must bump both up to BUMP_AMOUNT since that's below
    // BUMP_THRESHOLD.
    client.deposit(&user, &1_000_000, &1_000_000);

    // Jump to one ledger before a never-bumped entry (TTL 4096) would have
    // become archived, and confirm the pool is still alive and usable —
    // proof the bump actually happened.
    let current_seq = env.ledger().sequence();
    env.ledger()
        .with_mut(|li| li.sequence_number = current_seq + 4_095);
    client.deposit(&user, &1, &1);

    assert_eq!(client.get_shares(&user), 1_000_001);
}

/// Same proof as above but for `swap`.
#[test]
fn test_swap_bumps_pool_ttl_before_expiration() {
    let (env, _contract_id, client) = setup();
    let token_a = Address::generate(&env);
    let token_b = Address::generate(&env);
    let user = Address::generate(&env);

    env.ledger().with_mut(|li| li.sequence_number = 1_000);
    client.initialize(&token_a, &token_b);
    client.deposit(&user, &10_000_000, &10_000_000);

    let current_seq = env.ledger().sequence();
    env.ledger()
        .with_mut(|li| li.sequence_number = current_seq + 4_095);

    // Must not panic / must not be treated as archived.
    client.swap(&user, &token_a, &1_000, &0);

    let (reserve_a, _) = client.get_reserves();
    assert_eq!(reserve_a, 10_001_000);
}

/// Same proof as above but for `withdraw`, including the user's own share
/// record.
#[test]
fn test_withdraw_bumps_pool_and_user_ttl_before_expiration() {
    let (env, _contract_id, client) = setup();
    let token_a = Address::generate(&env);
    let token_b = Address::generate(&env);
    let user = Address::generate(&env);

    env.ledger().with_mut(|li| li.sequence_number = 1_000);
    client.initialize(&token_a, &token_b);
    client.deposit(&user, &1_000_000, &1_000_000);

    let current_seq = env.ledger().sequence();
    env.ledger()
        .with_mut(|li| li.sequence_number = current_seq + 4_095);

    // Must not panic / must not be treated as archived.
    client.withdraw(&user, &1_000);

    assert_eq!(client.get_shares(&user), 1_000_000 - 1_000);
}

/// A rapid sequence of calls with no meaningful gap between them must also
/// never fail — bumping TTL on an already-healthy entry is a no-op, not a
/// disruption.
#[test]
fn test_rapid_successive_calls_never_disrupted() {
    let (env, _contract_id, client) = setup();
    let token_a = Address::generate(&env);
    let token_b = Address::generate(&env);
    let user = Address::generate(&env);

    env.ledger().with_mut(|li| li.sequence_number = 1_000);
    client.initialize(&token_a, &token_b);
    client.deposit(&user, &1_000_000, &1_000_000);

    env.ledger().with_mut(|li| li.sequence_number = 1_001);
    client.deposit(&user, &1, &1);

    env.ledger().with_mut(|li| li.sequence_number = 1_002);
    client.swap(&user, &token_a, &1_000, &0);

    env.ledger().with_mut(|li| li.sequence_number = 1_003);
    client.withdraw(&user, &1_000);
}
