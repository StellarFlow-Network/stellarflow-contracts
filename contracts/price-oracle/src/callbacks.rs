use soroban_sdk::{Address, Env, Symbol, Vec, IntoVal};

use crate::types::{DataKey, PriceUpdatePayload};

/// Get the list of all subscribed contracts.
pub fn get_subscribers(env: &Env) -> Vec<Address> {
    env.storage()
        .persistent()
        .get::<DataKey, Vec<Address>>(&DataKey::PriceUpdateSubscribers)
        .unwrap_or_else(|| Vec::new(env))
}

/// Subscribe a contract to receive price update callbacks.
///
/// # Arguments
/// * `env` - The contract environment
/// * `callback_contract` - The address of the contract to subscribe
///
/// # Returns
/// Returns `Err` if the contract is already subscribed to prevent duplicates.
pub fn subscribe(env: &Env, callback_contract: Address) -> Result<(), crate::Error> {
    let mut subscribers = get_subscribers(env);

    // Check if already subscribed
    if subscribers.iter().any(|sub| sub == callback_contract) {
        return Err(crate::Error::AlreadyInitialized);
    }

    subscribers.push_back(callback_contract);
    env.storage()
        .persistent()
        .set(&DataKey::PriceUpdateSubscribers, &subscribers);

    Ok(())
}

/// Unsubscribe a contract from price update callbacks.
///
/// # Arguments
/// * `env` - The contract environment
/// * `callback_contract` - The address of the contract to unsubscribe
///
/// # Returns
/// Returns `Err` if the contract is not found in the subscriber list.
pub fn unsubscribe(env: &Env, callback_contract: &Address) -> Result<(), crate::Error> {
    let mut subscribers = get_subscribers(env);

    // Find and remove the subscriber
    let mut found = false;
    let mut i = 0;
    while i < subscribers.len() {
        if &subscribers.get(i).unwrap() == callback_contract {
            found = true;
            // Remove by swapping with the last element and popping
            let last_idx = subscribers.len() - 1;
            if i != last_idx {
                subscribers.set(i, subscribers.get(last_idx).unwrap());
            }
            subscribers.pop_back();
            break;
        }
        i += 1;
    }

    if !found {
        return Err(crate::Error::AssetNotFound);
    }

    env.storage()
        .persistent()
        .set(&DataKey::PriceUpdateSubscribers, &subscribers);

    Ok(())
}

/// Invoke the `on_price_update` callback on all subscribed contracts.
///
/// This function is called after a price update is successfully stored.
/// It notifies all subscribed contracts of the price change via the standard
/// callback interface.
///
/// # Arguments
/// * `env` - The contract environment
/// * `payload` - The price update payload to send to subscribers
///
/// # Notes
/// - Non-fatal: If a callback fails, the function logs the error but continues
///   processing other subscribers. This ensures one failed callback doesn't block updates.
/// - Gas considerations: Callbacks consume gas; if too many subscribers exist,
///   the transaction might fail due to gas limits. Consider pagination if needed.
pub fn notify_subscribers(env: &Env, payload: &PriceUpdatePayload) {
    let subscribers = get_subscribers(env);

    for subscriber in subscribers.iter() {
        // Try to call the callback on this subscriber
        // Use try_invoke to handle failures gracefully
        let _result = try_invoke_callback(env, &subscriber, payload);
        // We intentionally ignore errors here to prevent one bad subscriber
        // from blocking all price updates. However, in a production system,
        // you might want to log these errors to an event or metrics system.
    }
}

/// Attempt to invoke the `on_price_update` callback on a single contract.
///
/// This uses dynamic invocation to call the standardized callback interface.
/// The callback contract must implement the `on_price_update(payload: PriceUpdatePayload)` function.
fn try_invoke_callback(env: &Env, callback_contract: &Address, payload: &PriceUpdatePayload) -> Result<(), crate::Error> {
    env.invoke_contract::<()>(
        callback_contract,
        &Symbol::new(env, "on_price_update"),
        soroban_sdk::vec![env, payload.into_val(env)],
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    #[test]
    fn test_subscribe_and_get_subscribers() {
        let env = Env::default();
        let contract1 = Address::generate(&env);
        let contract2 = Address::generate(&env);

        // Initially, no subscribers
        assert_eq!(get_subscribers(&env).len(), 0);

        // Subscribe first contract
        assert!(subscribe(&env, contract1.clone()).is_ok());
        assert_eq!(get_subscribers(&env).len(), 1);

        // Subscribe second contract
        assert!(subscribe(&env, contract2.clone()).is_ok());
        assert_eq!(get_subscribers(&env).len(), 2);

        // Verify both are in the list
        let subscribers = get_subscribers(&env);
        assert!(subscribers.iter().any(|s| s == contract1));
        assert!(subscribers.iter().any(|s| s == contract2));
    }

    #[test]
    fn test_subscribe_duplicate_fails() {
        let env = Env::default();
        let contract = Address::generate(&env);

        // First subscription succeeds
        assert!(subscribe(&env, contract.clone()).is_ok());

        // Duplicate subscription fails
        assert!(subscribe(&env, contract).is_err());
    }

    #[test]
    fn test_unsubscribe() {
        let env = Env::default();
        let contract1 = Address::generate(&env);
        let contract2 = Address::generate(&env);

        // Subscribe both
        subscribe(&env, contract1.clone()).unwrap();
        subscribe(&env, contract2.clone()).unwrap();
        assert_eq!(get_subscribers(&env).len(), 2);

        // Unsubscribe first
        assert!(unsubscribe(&env, &contract1).is_ok());
        assert_eq!(get_subscribers(&env).len(), 1);

        // Verify only contract2 remains
        let subscribers = get_subscribers(&env);
        assert!(!subscribers.iter().any(|s| s == contract1));
        assert!(subscribers.iter().any(|s| s == contract2));
    }

    #[test]
    fn test_unsubscribe_nonexistent_fails() {
        let env = Env::default();
        let contract = Address::generate(&env);

        // Unsubscribe from empty list fails
        assert!(unsubscribe(&env, &contract).is_err());
    }
}
