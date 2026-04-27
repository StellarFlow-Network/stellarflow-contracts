//! FE-216: Efficient event indexing via topic mapping.
//! The first topic in env.events().publish() is the asset Symbol
//! so indexers can filter by asset pair (e.g. NGN/XLM) without scanning all events.

use soroban_sdk::{Env, Symbol};

/// Publishes a price update event with the asset symbol as the first topic.
/// Indexers can filter on topic[0] == asset to find all events for that pair.
pub fn publish_price_event(env: &Env, asset: Symbol, price: i128, timestamp: u64) {
    env.events().publish(
        (asset,),                          // topic[0] = asset symbol for efficient filtering
        (price, timestamp),                // data payload
    );
}