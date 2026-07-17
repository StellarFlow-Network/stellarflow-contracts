//! Proactive rent evaluation module (closes #542).
//!
//! Extends the storage lifespan of active tracking slots during query execution,
//! passing associated maintenance charges to the calling context.

use soroban_sdk::{Env, Symbol};

use crate::types::DataKey;

/// Ledger bump amounts for persistent and temporary storage.
const PERSISTENT_BUMP_AMOUNT: u32 = 535_680; // ~30 days in ledgers
const PERSISTENT_THRESHOLD: u32 = 267_840; // bump when < 15 days remain

/// Extend the TTL of a verified price entry if below threshold.
///
/// Called during query execution so callers implicitly pay rent for the
/// storage slots they access.
pub fn extend_price_ttl(env: &Env, asset: &Symbol) {
    env.storage().persistent().extend_ttl(
        &DataKey::VerifiedPrice(asset.clone()),
        PERSISTENT_THRESHOLD,
        PERSISTENT_BUMP_AMOUNT,
    );
}

/// Extend the TTL of an asset-info entry if below threshold.
pub fn extend_asset_info_ttl(env: &Env, asset: &Symbol) {
    env.storage().persistent().extend_ttl(
        &DataKey::AssetInfo(asset.clone()),
        PERSISTENT_THRESHOLD,
        PERSISTENT_BUMP_AMOUNT,
    );
}

/// Extend the TTL of a TWAP buffer entry if below threshold.
pub fn extend_twap_ttl(env: &Env, asset: &Symbol) {
    env.storage().persistent().extend_ttl(
        &DataKey::Twap(asset.clone()),
        PERSISTENT_THRESHOLD,
        PERSISTENT_BUMP_AMOUNT,
    );
}

/// Extend the TTL of the contract instance itself.
pub fn extend_instance_ttl(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(PERSISTENT_THRESHOLD, PERSISTENT_BUMP_AMOUNT);
}


#[cfg(test)]
mod storage_tests {
    use super::*;
    use soroban_sdk::{contract, contractimpl, Env, Symbol};

    #[contract]
    struct TestContract;

    #[contractimpl]
    impl TestContract {}

    #[test]
    fn test_extend_instance_ttl_does_not_panic() {
        let env = Env::default();
        let cid = env.register(TestContract, ());
        env.as_contract(&cid, || {
            extend_instance_ttl(&env);
        });
    }

    #[test]
    fn test_extend_price_ttl_missing_key_does_not_panic() {
        let env = Env::default();
        let cid = env.register(TestContract, ());
        env.as_contract(&cid, || {
            let asset = Symbol::new(&env, "XLM");
            // Key doesn't exist yet — extend_ttl on missing persistent key is a no-op in the test env.
            extend_price_ttl(&env, &asset);
        });
    }
}
