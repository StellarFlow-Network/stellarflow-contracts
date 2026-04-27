#[cfg(test)]
mod tests {
    use soroban_sdk::{testutils::Address as _, Env, Symbol};

    use crate::{
        interface::PriceOracleStorageTrait, types::PriceData, PriceOracleStorage,
        PriceOracleStorageClient,
    };

    fn setup() -> (Env, soroban_sdk::Address, PriceOracleStorageClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register(PriceOracleStorage, ());
        let client = PriceOracleStorageClient::new(&env, &contract_id);
        (env, contract_id, client)
    }

    #[test]
    fn test_set_and_get_admin() {
        let (env, _, client) = setup();
        let admin = soroban_sdk::Address::random(&env);
        client.set_admin(&admin);
        let retrieved_admin = client.get_admin().unwrap();
        assert_eq!(retrieved_admin, admin);
    }

    #[test]
    fn test_is_admin_true() {
        let (env, _, client) = setup();
        let admin = soroban_sdk::Address::random(&env);
        client.set_admin(&admin);
        assert!(client.is_admin(&admin));
    }

    #[test]
    fn test_is_admin_false() {
        let (env, _, client) = setup();
        let admin = soroban_sdk::Address::random(&env);
        let other = soroban_sdk::Address::random(&env);
        client.set_admin(&admin);
        assert!(!client.is_admin(&other));
    }

    #[test]
    fn test_set_and_get_verified_price() {
        let (env, _, client) = setup();
        let asset = Symbol::new(&env, "NGN");
        let provider = soroban_sdk::Address::random(&env);
        let price_data = PriceData {
            price: 100_000_000,
            timestamp: 1000,
            provider: provider.clone(),
            decimals: 9,
        };

        client.set_verified_price(&asset, &price_data);
        let retrieved = client.get_verified_price(&asset).unwrap();

        assert_eq!(retrieved.price, price_data.price);
        assert_eq!(retrieved.timestamp, price_data.timestamp);
        assert_eq!(retrieved.decimals, price_data.decimals);
    }

    #[test]
    fn test_set_and_get_community_price() {
        let (env, _, client) = setup();
        let asset = Symbol::new(&env, "GHS");
        let provider = soroban_sdk::Address::random(&env);
        let price_data = PriceData {
            price: 50_000_000,
            timestamp: 2000,
            provider: provider.clone(),
            decimals: 9,
        };

        client.set_community_price(&asset, &price_data);
        let retrieved = client.get_community_price(&asset).unwrap();

        assert_eq!(retrieved.price, price_data.price);
        assert_eq!(retrieved.timestamp, price_data.timestamp);
    }

    #[test]
    fn test_verified_and_community_prices_independent() {
        let (env, _, client) = setup();
        let asset = Symbol::new(&env, "NGN");
        let provider = soroban_sdk::Address::random(&env);

        let verified_price = PriceData {
            price: 100_000_000,
            timestamp: 1000,
            provider: provider.clone(),
            decimals: 9,
        };

        let community_price = PriceData {
            price: 95_000_000,
            timestamp: 1500,
            provider: provider.clone(),
            decimals: 9,
        };

        client.set_verified_price(&asset, &verified_price);
        client.set_community_price(&asset, &community_price);

        let retrieved_verified = client.get_verified_price(&asset).unwrap();
        let retrieved_community = client.get_community_price(&asset).unwrap();

        assert_eq!(retrieved_verified.price, 100_000_000);
        assert_eq!(retrieved_community.price, 95_000_000);
    }

    #[test]
    fn test_add_asset() {
        let (env, _, client) = setup();
        let asset = Symbol::new(&env, "NGN");
        let result = client.try_add_asset(&asset);
        assert!(result.is_ok());

        let assets = client.get_all_assets();
        assert_eq!(assets.len(), 1);
        assert_eq!(assets.get(0).unwrap(), asset);
    }

    #[test]
    fn test_add_asset_duplicate_fails() {
        let (env, _, client) = setup();
        let asset = Symbol::new(&env, "NGN");
        client.add_asset(&asset);
        let result = client.try_add_asset(&asset);
        assert!(result.is_err());
    }

    #[test]
    fn test_add_multiple_assets() {
        let (env, _, client) = setup();
        let ngn = Symbol::new(&env, "NGN");
        let ghs = Symbol::new(&env, "GHS");
        let kes = Symbol::new(&env, "KES");

        client.add_asset(&ngn);
        client.add_asset(&ghs);
        client.add_asset(&kes);

        let assets = client.get_all_assets();
        assert_eq!(assets.len(), 3);
    }

    #[test]
    fn test_get_asset_count() {
        let (env, _, client) = setup();
        assert_eq!(client.get_asset_count(), 0);

        let asset1 = Symbol::new(&env, "NGN");
        let asset2 = Symbol::new(&env, "GHS");

        client.add_asset(&asset1);
        assert_eq!(client.get_asset_count(), 1);

        client.add_asset(&asset2);
        assert_eq!(client.get_asset_count(), 2);
    }

    #[test]
    fn test_get_all_assets_empty() {
        let (_, _, client) = setup();
        let assets = client.get_all_assets();
        assert_eq!(assets.len(), 0);
    }

    #[test]
    fn test_set_and_get_asset_meta() {
        let (env, _, client) = setup();
        let asset = Symbol::new(&env, "NGN");
        let meta = crate::types::AssetMeta {
            base_decimals: 2,
            quote_decimals: 7,
        };

        client.set_asset_meta(&asset, &meta);
        let retrieved = client.get_asset_meta(&asset).unwrap();

        assert_eq!(retrieved.base_decimals, meta.base_decimals);
        assert_eq!(retrieved.quote_decimals, meta.quote_decimals);
    }

    #[test]
    fn test_subscribe() {
        let (env, _, client) = setup();
        let subscriber = soroban_sdk::Address::random(&env);

        let result = client.try_subscribe(&subscriber);
        assert!(result.is_ok());

        let subscribers = client.get_subscribers();
        assert_eq!(subscribers.len(), 1);
        assert_eq!(subscribers.get(0).unwrap(), subscriber);
    }

    #[test]
    fn test_subscribe_duplicate_fails() {
        let (env, _, client) = setup();
        let subscriber = soroban_sdk::Address::random(&env);

        client.subscribe(&subscriber);
        let result = client.try_subscribe(&subscriber);
        assert!(result.is_err());
    }

    #[test]
    fn test_subscribe_multiple() {
        let (env, _, client) = setup();
        let sub1 = soroban_sdk::Address::random(&env);
        let sub2 = soroban_sdk::Address::random(&env);
        let sub3 = soroban_sdk::Address::random(&env);

        client.subscribe(&sub1);
        client.subscribe(&sub2);
        client.subscribe(&sub3);

        let subscribers = client.get_subscribers();
        assert_eq!(subscribers.len(), 3);
    }

    #[test]
    fn test_unsubscribe() {
        let (env, _, client) = setup();
        let subscriber = soroban_sdk::Address::random(&env);

        client.subscribe(&subscriber);
        assert_eq!(client.get_subscribers().len(), 1);

        let result = client.try_unsubscribe(&subscriber);
        assert!(result.is_ok());
        assert_eq!(client.get_subscribers().len(), 0);
    }

    #[test]
    fn test_unsubscribe_nonexistent_fails() {
        let (env, _, client) = setup();
        let subscriber = soroban_sdk::Address::random(&env);
        let result = client.try_unsubscribe(&subscriber);
        assert!(result.is_err());
    }

    #[test]
    fn test_unsubscribe_from_multiple() {
        let (env, _, client) = setup();
        let sub1 = soroban_sdk::Address::random(&env);
        let sub2 = soroban_sdk::Address::random(&env);
        let sub3 = soroban_sdk::Address::random(&env);

        client.subscribe(&sub1);
        client.subscribe(&sub2);
        client.subscribe(&sub3);

        client.unsubscribe(&sub2);

        let subscribers = client.get_subscribers();
        assert_eq!(subscribers.len(), 2);
        assert!(subscribers.iter().any(|s| s == sub1));
        assert!(subscribers.iter().any(|s| s == sub3));
    }

    #[test]
    fn test_get_subscribers_empty() {
        let (_, _, client) = setup();
        let subscribers = client.get_subscribers();
        assert_eq!(subscribers.len(), 0);
    }

    #[test]
    fn test_initialize() {
        let (env, _, client) = setup();
        let admin = soroban_sdk::Address::random(&env);
        let result = client.try_initialize(&admin);
        assert!(result.is_ok());
        assert!(client.is_initialized());
        assert_eq!(client.get_admin().unwrap(), admin);
    }

    #[test]
    fn test_initialize_twice_fails() {
        let (env, _, client) = setup();
        let admin = soroban_sdk::Address::random(&env);
        client.initialize(&admin);
        let result = client.try_initialize(&admin);
        assert!(result.is_err());
    }

    #[test]
    fn test_is_initialized_false() {
        let (_, _, client) = setup();
        assert!(!client.is_initialized());
    }

    #[test]
    fn test_is_initialized_true() {
        let (env, _, client) = setup();
        let admin = soroban_sdk::Address::random(&env);
        client.initialize(&admin);
        assert!(client.is_initialized());
    }

    #[test]
    fn test_full_workflow() {
        let (env, _, client) = setup();
        let admin = soroban_sdk::Address::random(&env);

        client.initialize(&admin);
        assert!(client.is_initialized());

        let ngn = Symbol::new(&env, "NGN");
        let ghs = Symbol::new(&env, "GHS");
        client.add_asset(&ngn);
        client.add_asset(&ghs);
        assert_eq!(client.get_asset_count(), 2);

        let provider = soroban_sdk::Address::random(&env);
        let ngn_price = PriceData {
            price: 100_000_000,
            timestamp: 1000,
            provider: provider.clone(),
            decimals: 9,
        };
        client.set_verified_price(&ngn, &ngn_price);

        let subscriber = soroban_sdk::Address::random(&env);
        client.subscribe(&subscriber);
        assert_eq!(client.get_subscribers().len(), 1);

        assert_eq!(client.get_admin().unwrap(), admin);
        assert_eq!(client.get_verified_price(&ngn).unwrap().price, 100_000_000);
        assert_eq!(client.get_asset_count(), 2);
        assert_eq!(client.get_subscribers().len(), 1);
    }
}
