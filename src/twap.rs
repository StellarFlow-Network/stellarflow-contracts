//! Automated TWAP oracle price buffer storage (issue #722).
//!
//! Maintains a per-asset ring buffer of historical price observations in
//! contract storage together with a running cumulative price sum, and exposes
//! a manipulation-resistant `get_twap(time_window)` view.
//!
//! A fixed-capacity ring buffer keeps on-chain footprint bounded: each new
//! observation either appends (buffer warm-up) or overwrites the slot at the
//! write cursor (steady state). The cumulative sum is maintained incrementally
//! so `get_twap` never re-scans the whole history for the average.

use crate::AssetId;
use soroban_sdk::{contracttype, Env, Vec};

/// Default number of price observations retained per asset.
pub const TWAP_DEFAULT_BUFFER_CAPACITY: u32 = 96;

/// Single price observation recorded for an asset.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TwapObservation {
    /// Observed price (in base currency stroops).
    pub price: u64,
    /// Ledger timestamp (seconds) the observation was recorded at.
    pub timestamp: u64,
    /// Ledger sequence number the observation was recorded at.
    pub sequence: u32,
}

/// Storage keys for the TWAP oracle.
///
/// NOTE: variant names must stay globally unique across the whole contract's
/// storage keys (see the collision notes in `src/storage.rs`), hence the
/// `Twap*` prefix on every variant.
#[contracttype]
pub enum TwapStorageKey {
    /// Configured ring-buffer capacity (instance storage).
    TwapConfig,
    /// History of observations for an asset (persistent).
    TwapObservations(AssetId),
    /// Write cursor — next ring slot to overwrite when full (persistent).
    TwapCursor(AssetId),
    /// Running cumulative sum of all retained prices (persistent).
    TwapCumulative(AssetId),
}

/// Ring-buffer capacity currently configured for the TWAP oracle.
pub fn buffer_capacity(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&TwapStorageKey::TwapConfig)
        .unwrap_or(TWAP_DEFAULT_BUFFER_CAPACITY)
}

/// Set the ring-buffer capacity. `0` is coerced to `1`; the minimum usable
/// window contains at least the most recent observation.
pub fn set_buffer_capacity(env: &Env, capacity: u32) -> u32 {
    let capacity = capacity.max(1);
    env.storage().instance().set(&TwapStorageKey::TwapConfig, &capacity);
    capacity
}

/// Append an observation for `asset` into its ring buffer and update the
/// running cumulative sum. O(1) steady-state write.
///
/// When the buffer is full the oldest observation (the slot at the write
/// cursor) is evicted and its price is subtracted from the cumulative sum.
pub fn record_price(env: &Env, asset: AssetId, price: u64) {
    let capacity = buffer_capacity(env);
    let observation = TwapObservation {
        price,
        timestamp: env.ledger().timestamp(),
        sequence: env.ledger().sequence(),
    };

    let mut buffer: Vec<TwapObservation> = env
        .storage()
        .persistent()
        .get(&TwapStorageKey::TwapObservations(asset))
        .unwrap_or_else(|| Vec::new(env));
    let mut cursor: u32 = env
        .storage()
        .persistent()
        .get(&TwapStorageKey::TwapCursor(asset))
        .unwrap_or(0);
    let mut cumulative: u64 = env
        .storage()
        .persistent()
        .get(&TwapStorageKey::TwapCumulative(asset))
        .unwrap_or(0);

    if buffer.len() == capacity {
        // Steady state: overwrite the oldest slot, evicting its price from
        // the running sum.
        let evicted = buffer.get(cursor).unwrap_or_else(|| TwapObservation {
            price: 0,
            timestamp: 0,
            sequence: 0,
        });
        cumulative = cumulative.saturating_sub(evicted.price);
        buffer.set(cursor, observation);
    } else {
        buffer.push_back(observation);
    }
    cumulative = cumulative.saturating_add(price);
    cursor = (cursor + 1) % capacity;

    env.storage()
        .persistent()
        .set(&TwapStorageKey::TwapObservations(asset), &buffer);
    env.storage()
        .persistent()
        .set(&TwapStorageKey::TwapCursor(asset), &cursor);
    env.storage()
        .persistent()
        .set(&TwapStorageKey::TwapCumulative(asset), &cumulative);

    env.storage().persistent().extend_ttl(
        &TwapStorageKey::TwapObservations(asset),
        crate::storage::PERSISTENT_TTL_THRESHOLD,
        crate::storage::PERSISTENT_TTL_THRESHOLD,
    );
}

/// Time-weighted manipulation-resistant average price for `asset` over
/// `time_window` seconds.
///
/// `time_window == 0` averages over the entire retained buffer. Returns `None`
/// while the buffer holds no observations inside the requested window.
pub fn get_twap(env: &Env, asset: AssetId, time_window: u64) -> Option<u64> {
    let now = env.ledger().timestamp();
    let buffer: Vec<TwapObservation> = env
        .storage()
        .persistent()
        .get(&TwapStorageKey::TwapObservations(asset))
        .unwrap_or_else(|| Vec::new(env));

    let mut sum: u64 = 0;
    let mut count: u64 = 0;
    for i in 0..buffer.len() {
        let observation = buffer.get(i).unwrap();
        if time_window == 0 || now.saturating_sub(observation.timestamp) <= time_window {
            sum = sum.saturating_add(observation.price);
            count += 1;
        }
    }
    if count == 0 {
        None
    } else {
        Some(sum / count)
    }
}

/// Number of observations currently retained in the buffer for `asset`.
pub fn observation_count(env: &Env, asset: AssetId) -> u32 {
    env.storage()
        .persistent()
        .get(&TwapStorageKey::TwapObservations(asset))
        .map(|buffer: Vec<TwapObservation>| buffer.len())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Ledger as _;
    use soroban_sdk::{testutils::Address as _, testutils::LedgerInfo, Address, Env};

    const ASSET: AssetId = 42;

    fn setup() -> (
        Env,
        crate::TimeLockedUpgradeContractClient<'static>,
        Address,
    ) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let client = crate::TimeLockedUpgradeContractClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        client.initialize(&admin, &treasury);
        (env, client, admin)
    }

    fn set_timestamp(env: &Env, timestamp: u64) {
        let info = env.ledger().get();
        env.ledger().set(LedgerInfo {
            protocol_version: info.protocol_version,
            sequence_number: info.sequence_number,
            timestamp,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 0,
            min_persistent_entry_ttl: 0,
            max_entry_ttl: u32::MAX,
        });
    }

    #[test]
    fn twap_averages_retained_observations() {
        let (env, client, _admin) = setup();
        set_timestamp(&env, 1_000);
        client.record_twap_observation(&ASSET, &100);
        set_timestamp(&env, 1_060);
        client.record_twap_observation(&ASSET, &200);
        assert_eq!(client.get_twap(&ASSET, &0), Some(150));
        assert_eq!(client.twap_observation_count(&ASSET), 2);
    }

    #[test]
    fn twap_respects_time_window() {
        let (env, client, _admin) = setup();
        set_timestamp(&env, 1_000);
        client.record_twap_observation(&ASSET, &100);
        set_timestamp(&env, 3_000);
        client.record_twap_observation(&ASSET, &200);
        // Only the 3000s observation falls within the trailing 60s window.
        assert_eq!(client.get_twap(&ASSET, &60), Some(200));
    }

    #[test]
    fn twap_evicts_oldest_when_full() {
        let (env, client, admin) = setup();
        client.set_twap_buffer_capacity(&admin, &3);
        set_timestamp(&env, 1_000);
        client.record_twap_observation(&ASSET, &100);
        set_timestamp(&env, 2_000);
        client.record_twap_observation(&ASSET, &200);
        set_timestamp(&env, 3_000);
        client.record_twap_observation(&ASSET, &300);
        assert_eq!(client.twap_observation_count(&ASSET), 3);
        // Fourth write evicts the 100 observation.
        set_timestamp(&env, 4_000);
        client.record_twap_observation(&ASSET, &400);
        assert_eq!(client.twap_observation_count(&ASSET), 3);
        assert_eq!(client.get_twap(&ASSET, &0), Some(300));
    }

    #[test]
    fn twap_returns_none_without_observations() {
        let (_env, client, _admin) = setup();
        assert_eq!(client.get_twap(&ASSET, &0), None);
    }

    #[test]
    fn twap_records_from_price_bundle() {
        let (env, client, _admin) = setup();
        let node = Address::generate(&env);
        client.stake_and_register(&node, &2_000);

        let asset = crate::symbol_to_asset_id(&soroban_sdk::symbol_short!("NGN"));
        set_timestamp(&env, 1_050);
        let mut updates: soroban_sdk::Vec<crate::validation::AssetPriceUpdate> =
            soroban_sdk::Vec::new(&env);
        updates.push_back(crate::validation::AssetPriceUpdate {
            asset,
            price: 100,
            timestamp: 1_000,
        });
        client.update_prices_bundle(&node, &updates);

        set_timestamp(&env, 2_050);
        let mut updates: soroban_sdk::Vec<crate::validation::AssetPriceUpdate> =
            soroban_sdk::Vec::new(&env);
        updates.push_back(crate::validation::AssetPriceUpdate {
            asset,
            price: 300,
            timestamp: 2_000,
        });
        client.update_prices_bundle(&node, &updates);

        // Averaging over the whole retained buffer (100+300)/2.
        assert_eq!(client.get_twap(&asset, &0), Some(200));
        // Trailing-60s window only captures the newest observation.
        assert_eq!(client.get_twap(&asset, &60), Some(300));
    }
}