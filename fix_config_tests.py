import re

with open('src/config.rs', 'r') as f:
    text = f.read()

# Fix the tests by registering the contract and using client
# But since we just want to test get/set_price_variance_config, we can just use env.as_contract
replacement = """
    #[test]
    fn get_returns_default_before_any_set() {
        let env = soroban_sdk::Env::default();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        
        // Use client or run inside as_contract
        env.as_contract(&contract_id, || {
            let cfg = get_price_variance_config(&env);
            assert_eq!(cfg, PriceVarianceConfig::default());
        });
    }

    #[test]
    fn set_and_get_round_trips_full_struct() {
        use soroban_sdk::testutils::Address as _;
        use soroban_sdk::Address;

        let env = soroban_sdk::Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);

        env.as_contract(&contract_id, || {
            // Bootstrap contract state so `set_price_variance_config` can read DATA_KEY.
            let admin = Address::generate(&env);
            let data = crate::ContractData {
                admin: admin.clone(),
                value: 0,
            };
            env.storage().instance().set(&crate::DATA_KEY, &data);

            let custom = PriceVarianceConfig {
                max_spread_bps: 150,
                max_deviation_bps: 400,
                min_submission_count: 5,
                max_submission_age_secs: 120,
            };

            set_price_variance_config(&env, &admin, custom.clone())
                .expect("set should succeed with valid config");

            let retrieved = get_price_variance_config(&env);
            assert_eq!(retrieved, custom);
        });
    }

    #[test]
    fn set_rejects_non_admin_caller() {
        use soroban_sdk::testutils::Address as _;
        use soroban_sdk::Address;

        let env = soroban_sdk::Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);

        env.as_contract(&contract_id, || {
            let admin = Address::generate(&env);
            let intruder = Address::generate(&env);

            let data = crate::ContractData {
                admin: admin.clone(),
                value: 0,
            };
            env.storage().instance().set(&crate::DATA_KEY, &data);

            let result =
                set_price_variance_config(&env, &intruder, PriceVarianceConfig::default());
            assert_eq!(result, Err(ContractError::NotAdmin));
        });
    }

    #[test]
    fn set_rejects_invalid_config() {
        use soroban_sdk::testutils::Address as _;
        use soroban_sdk::Address;

        let env = soroban_sdk::Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);

        env.as_contract(&contract_id, || {
            let admin = Address::generate(&env);
            let data = crate::ContractData {
                admin: admin.clone(),
                value: 0,
            };
            env.storage().instance().set(&crate::DATA_KEY, &data);

            let bad = PriceVarianceConfig {
                max_spread_bps: 0, // violates lower-bound invariant
                ..PriceVarianceConfig::default()
            };
            assert_eq!(
                set_price_variance_config(&env, &admin, bad),
                Err(ContractError::InvalidVarianceConfig)
            );
        });
    }
"""

text = re.sub(r"#[test]\s*fn get_returns_default_before_any_set\(\) \{.*?(?=^})}", replacement, text, flags=re.DOTALL | re.MULTILINE)
# Wait, my regex is bad for replacing 4 functions.
