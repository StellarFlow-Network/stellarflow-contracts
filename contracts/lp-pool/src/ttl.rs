//! Shared TTL (time-to-live) management for the pool's storage entries.
//!
//! Every Soroban ledger entry carries a time-to-live measured in ledgers. A
//! freshly written entry only gets the network minimum
//! (`min_persistent_entry_ttl`, 4,096 ledgers on the sandbox), and once that
//! lapses a persistent entry is *archived*: reads and writes against it fail
//! until it is explicitly restored. The contract's own **instance** entry is a
//! persistent entry too, so a pool that never extends anything disappears
//! entirely, even while its reserves are still being traded against.
//!
//! Three things can therefore expire independently:
//!
//! * the **contract instance** (the whole pool),
//! * the **pool records** written once by `initialize` (the two token
//!   addresses, the initialized flag and the reserves), and
//! * each user's **share record**.
//!
//! [`extend_instance`] and [`extend_if_present`] refresh those entries. Both
//! are deliberately cheap on the hot path: `extend_ttl` is a no-op while the
//! remaining TTL is still at or above [`BUMP_THRESHOLD`], so a busily traded
//! pool performs no redundant ledger writes and only pays for the threshold
//! check.

use crate::types::DataKey;
use soroban_sdk::Env;

/// Remaining-TTL floor below which an entry is extended. At roughly five
/// seconds per ledger this is about 30 days, so any key touched at least
/// monthly never approaches archival.
pub const BUMP_THRESHOLD: u32 = 518_400;

/// TTL a low entry is extended *to*, measured from the current ledger. About
/// 60 days at five seconds per ledger, leaving a wide margin before the next
/// extension is required.
pub const BUMP_AMOUNT: u32 = 1_036_800;

/// Extend the contract instance's TTL when it is running low.
///
/// The instance must be kept alive for *any* entrypoint to run at all, so the
/// state-changing paths call this alongside the persistent extensions.
pub fn extend_instance(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(BUMP_THRESHOLD, BUMP_AMOUNT);
}

/// Extend `key`'s TTL when the persistent entry exists and is running low.
///
/// Absent entries (the reserves key before `initialize`, a user's share
/// record before their first deposit, and `TokenA`/`TokenB`/`Initialized` on a
/// fresh contract) are left untouched, as are entries whose remaining TTL is
/// still above [`BUMP_THRESHOLD`].
pub fn extend_if_present(env: &Env, key: &DataKey) {
    let storage = env.storage().persistent();
    if storage.has(key) {
        storage.extend_ttl(key, BUMP_THRESHOLD, BUMP_AMOUNT);
    }
}
