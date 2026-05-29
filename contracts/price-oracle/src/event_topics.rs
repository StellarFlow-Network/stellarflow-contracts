//! FE-216: Efficient event indexing via topic mapping.
//! Uses publish_event() with typed structs matching the core proxy interface.

use crate::PriceUpdatedEvent;
use soroban_sdk::Env;
use soroban_sdk::Symbol;

/// Publishes a price update event using the typed PriceUpdatedEvent struct.
pub fn publish_price_event(env: &Env, asset: Symbol, price: i128) {
    env.events().publish_event(&PriceUpdatedEvent { asset, price });
}
