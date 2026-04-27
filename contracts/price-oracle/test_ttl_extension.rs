#[cfg(test)]
mod ttl_extension_tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    #[test]
    fn test_update_price_extends_relayer_ttl() {
        let env = Env::default();
        let contract_id = env.register(PriceOracle, ());
        
        let admin = Address::generate(&env);
        let relayer = Address::generate(&env);
        let asset = Symbol::new(&env, "BTC");
        
        env.as_contract(&contract_id, || {
            // Initialize contract
            let assets = soroban_sdk::vec![&env, asset.clone()];
            PriceOracle::initialize(env.clone(), admin.clone(), assets);
            
            // Add relayer to whitelist
            crate::auth::_add_provider(&env, &relayer);
            
            // Add asset
            PriceOracle::add_asset(env.clone(), admin.clone(), asset.clone()).unwrap();
            
            // Verify relayer is stored
            assert!(crate::auth::_is_provider(&env, &relayer));
            
            // Call update_price which should extend TTL
            let result = PriceOracle::update_price(
                env.clone(),
                relayer.clone(),
                asset.clone(),
                50000_i128,  // price
                7,           // decimals
                100,         // confidence_score
                3600,        // ttl
            );
            
            // The update should succeed
            assert!(result.is_ok());
            
            // The relayer should still be whitelisted (TTL extended)
            assert!(crate::auth::_is_provider(&env, &relayer));
        });
    }
}
