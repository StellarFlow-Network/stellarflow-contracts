use crate::AssetId;
use soroban_sdk::{contracttype, Address, Env, Symbol};

/// Structured payload for the `liquidity_added` event.
///
/// Duplicates the indexed provider and pool identifier in the payload so RPC
/// consumers can filter on topics and still hydrate a self-contained record.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiquidityAddedEvent {
    /// Address of the liquidity provider adding assets to the corridor pool.
    pub provider: Address,
    /// Canonical corridor pool identifier used by the contract.
    pub pool_id: AssetId,
    /// Amount of the first pool token supplied.
    pub token_a_amount: i128,
    /// Amount of the second pool token supplied.
    pub token_b_amount: i128,
    /// LP units minted to the provider.
    pub minted_lp_units: i128,
}

/// Structured payload for the `liquidity_removed` event.
///
/// Mirrors the add-liquidity schema while swapping minted units for burned
/// units to keep downstream indexers stable across both state transitions.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiquidityRemovedEvent {
    /// Address of the liquidity provider withdrawing assets from the pool.
    pub provider: Address,
    /// Canonical corridor pool identifier used by the contract.
    pub pool_id: AssetId,
    /// Amount of the first pool token returned to the provider.
    pub token_a_amount: i128,
    /// Amount of the second pool token returned to the provider.
    pub token_b_amount: i128,
    /// LP units burned from the provider.
    pub burned_lp_units: i128,
}

/// Publishes a standardized `LiquidityAddedEvent`.
///
/// Topics follow the RPC-friendly schema:
/// `("stellarflow", "liquidity_added", pool_id, provider)`.
pub fn publish_liquidity_added(
    env: &Env,
    provider: &Address,
    pool_id: AssetId,
    token_a_amount: i128,
    token_b_amount: i128,
    minted_lp_units: i128,
) {
    let topics = (
        Symbol::new(env, "stellarflow"),
        Symbol::new(env, "liquidity_added"),
        pool_id,
        provider.clone(),
    );

    let payload = LiquidityAddedEvent {
        provider: provider.clone(),
        pool_id,
        token_a_amount,
        token_b_amount,
        minted_lp_units,
    };

    env.events().publish(topics, payload);
}

/// Publishes a standardized `LiquidityRemovedEvent`.
///
/// Topics follow the RPC-friendly schema:
/// `("stellarflow", "liquidity_removed", pool_id, provider)`.
pub fn publish_liquidity_removed(
    env: &Env,
    provider: &Address,
    pool_id: AssetId,
    token_a_amount: i128,
    token_b_amount: i128,
    burned_lp_units: i128,
) {
    let topics = (
        Symbol::new(env, "stellarflow"),
        Symbol::new(env, "liquidity_removed"),
        pool_id,
        provider.clone(),
    );

    let payload = LiquidityRemovedEvent {
        provider: provider.clone(),
        pool_id,
        token_a_amount,
        token_b_amount,
        burned_lp_units,
    };

    env.events().publish(topics, payload);
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Events};
    use soroban_sdk::{IntoVal, TryFromVal, Val};

    #[test]
    fn test_publish_liquidity_added() {
        let env = Env::default();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let provider = Address::generate(&env);
        let pool_id: AssetId = 2654435761;

        env.as_contract(&contract_id, || {
            publish_liquidity_added(&env, &provider, pool_id, 5_000, 7_500, 1_250);

            let events = env.events().all();
            assert_eq!(events.len(), 1);

            let (_, topics, data) = events.get(0).unwrap();
            let expected_topics = soroban_sdk::vec![
                &env,
                Symbol::new(&env, "stellarflow").into_val(&env),
                Symbol::new(&env, "liquidity_added").into_val(&env),
                pool_id.into_val(&env),
                provider.clone().into_val(&env),
            ];

            assert_eq!(topics, expected_topics);

            let payload = LiquidityAddedEvent::try_from_val(&env, &data).unwrap();
            assert_eq!(
                payload,
                LiquidityAddedEvent {
                    provider: provider.clone(),
                    pool_id,
                    token_a_amount: 5_000,
                    token_b_amount: 7_500,
                    minted_lp_units: 1_250,
                }
            );
        });
    }

    #[test]
    fn test_publish_liquidity_removed() {
        let env = Env::default();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let provider = Address::generate(&env);
        let pool_id: AssetId = 3897123275;

        env.as_contract(&contract_id, || {
            publish_liquidity_removed(&env, &provider, pool_id, 2_100, 3_900, 800);

            let events = env.events().all();
            assert_eq!(events.len(), 1);

            let (_, topics, data) = events.get(0).unwrap();
            let expected_topics = soroban_sdk::vec![
                &env,
                Symbol::new(&env, "stellarflow").into_val(&env),
                Symbol::new(&env, "liquidity_removed").into_val(&env),
                pool_id.into_val(&env),
                provider.clone().into_val(&env),
            ];

            assert_eq!(topics, expected_topics);

            let payload = LiquidityRemovedEvent::try_from_val(&env, &data).unwrap();
            assert_eq!(
                payload,
                LiquidityRemovedEvent {
                    provider,
                    pool_id,
                    token_a_amount: 2_100,
                    token_b_amount: 3_900,
                    burned_lp_units: 800,
                }
            );
        });
    }
}

/// Structured payload for a liquidity-provider alert raised when the bid-ask
/// spread of an order book expands beyond the 5% safety threshold.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiquidityProviderAlert {
    /// Asset pair whose book is reporting the imbalance.
    pub pair: crate::orders::limit::AssetPair,
    /// Highest resting bid price (`P_bid_max`), fixed-point at
    /// `orders::limit::PRICE_SCALE`.
    pub best_bid: i128,
    /// Lowest resting ask price (`P_ask_min`), fixed-point at
    /// `orders::limit::PRICE_SCALE`.
    pub best_ask: i128,
    /// Relative spread `S = (ask_min - bid_max) / bid_max`, fixed-point at
    /// `orders::limit::PRICE_SCALE`.
    pub spread_ratio: i128,
}

/// Publish a `LiquidityProviderAlert` for a spread-imbalance monitor.
///
/// Topics follow the RPC-friendly schema used by the other liquidity events:
/// `("stellarflow", "liquidity_provider_alert")`, with the offending book state
/// carried in the payload.
pub fn publish_liquidity_provider_alert(
    env: &Env,
    pair: &crate::orders::limit::AssetPair,
    best_bid: i128,
    best_ask: i128,
    spread_ratio: i128,
) {
    let topics = (
        Symbol::new(env, "stellarflow"),
        Symbol::new(env, "liquidity_provider_alert"),
    );
    let payload = LiquidityProviderAlert {
        pair: pair.clone(),
        best_bid,
        best_ask,
        spread_ratio,
    };
    env.events().publish(topics, payload);
}
