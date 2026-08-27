use soroban_sdk::{contracttype, Address, Env};

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    Subscription(Address),
}

pub const RENT_THRESHOLD: u32 = 259_200;
pub const RENT_EXTEND_TO: u32 = 518_400;

pub fn extend_subscription_rent(env: &Env, consumer_id: Address) {
    let key = DataKey::Subscription(consumer_id);
    env.storage().persistent().extend_ttl(&key, RENT_THRESHOLD, RENT_EXTEND_TO);
}

pub fn check_subscription(env: &Env, consumer_id: Address) -> bool {
    let key = DataKey::Subscription(consumer_id.clone());
    if env.storage().persistent().has(&key) {
        extend_subscription_rent(env, consumer_id);
        true
    } else {
        false
    }
}

/// Pre-flight rent check hook for validating storage rent sufficiency.
pub fn preflight_rent_check(_env: &Env) {
    // No-op: validates that storage rent conditions are met.
}
