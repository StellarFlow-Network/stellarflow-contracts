//! Integration tests for Timelocked Protocol Treasury Emergency Rescue Handler (Issue #783)

#[cfg(test)]
mod rescue_integration_tests {
    use soroban_sdk::{
        testutils::{Address as _, Ledger, LedgerInfo},
        token, Address, Env,
    };
    use stellarflow_contracts::{
        rescue::{RescueProposalStatus, RESCUE_TIMELOCK_DELAY},
        ContractError, TimeLockedUpgradeContract, TimeLockedUpgradeContractClient,
    };

    fn setup_test() -> (
        Env,
        TimeLockedUpgradeContractClient<'static>,
        Address,
        Address,
    ) {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register_contract(None, TimeLockedUpgradeContract);
        let client = TimeLockedUpgradeContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);

        client.initialize(&admin, &treasury);

        (env, client, admin, treasury)
    }

    fn advance_timestamp(env: &Env, delta_seconds: u64) {
        let ts = env.ledger().timestamp();
        env.ledger().set(LedgerInfo {
            timestamp: ts + delta_seconds,
            protocol_version: env.ledger().protocol_version(),
            sequence_number: env.ledger().sequence() + 1,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 0,
            min_persistent_entry_ttl: 0,
            max_entry_ttl: u32::MAX,
        });
    }

    #[test]
    fn test_rescue_handler_register_protected_assets() {
        let (_env, client, admin, _treasury) = setup_test();
        let env = &client.env;
        let pool_asset = Address::generate(env);
        let vault_reserve_asset = Address::generate(env);

        assert!(!client.is_protected_asset(&pool_asset));
        assert!(!client.is_protected_asset(&vault_reserve_asset));

        client.register_protected_asset(&admin, &pool_asset);
        client.register_protected_asset(&admin, &vault_reserve_asset);

        assert!(client.is_protected_asset(&pool_asset));
        assert!(client.is_protected_asset(&vault_reserve_asset));
    }

    #[test]
    fn test_rescue_handler_queue_proposal_rejects_protected_asset() {
        let (_env, client, admin, treasury) = setup_test();
        let env = &client.env;
        let pool_asset = Address::generate(env);

        client.register_protected_asset(&admin, &pool_asset);

        let res = client.try_queue_token_rescue(&admin, &pool_asset, &10_000, &treasury);
        assert_eq!(
            res,
            Err(Ok(ContractError::ProtectedAssetNotRescueable))
        );
    }

    #[test]
    fn test_rescue_handler_premature_execution_fails() {
        let (_env, client, admin, treasury) = setup_test();
        let env = &client.env;
        let mis_sent_token = Address::generate(env);

        let pid = client.queue_token_rescue(&admin, &mis_sent_token, &5_000, &treasury);
        let proposal = client.get_rescue_proposal(&pid).unwrap();

        assert_eq!(proposal.status, RescueProposalStatus::Pending);
        assert_eq!(proposal.execute_at, proposal.staged_at + RESCUE_TIMELOCK_DELAY);

        advance_timestamp(env, RESCUE_TIMELOCK_DELAY - 100);

        let res = client.try_execute_token_rescue(&admin, &pid);
        assert_eq!(res, Err(Ok(ContractError::RescueTimelockNotExpired)));
    }

    #[test]
    fn test_rescue_handler_execution_transfers_to_treasury() {
        let (_env, client, admin, treasury) = setup_test();
        let env = &client.env;

        let token_admin = Address::generate(env);
        let token_contract_id = env.register_stellar_asset_contract(token_admin.clone());
        let token_client = token::Client::new(env, &token_contract_id);
        let stellar_asset_admin = token::StellarAssetClient::new(env, &token_contract_id);

        // Mint mis-sent tokens directly to the contract address
        stellar_asset_admin.mint(&client.address, &50_000);
        assert_eq!(token_client.balance(&client.address), 50_000);
        assert_eq!(token_client.balance(&treasury), 0);

        let pid = client.queue_token_rescue(&admin, &token_contract_id, &50_000, &treasury);

        advance_timestamp(env, RESCUE_TIMELOCK_DELAY + 10);

        client.execute_token_rescue(&admin, &pid);

        // Verify tokens transferred to treasury address
        assert_eq!(token_client.balance(&client.address), 0);
        assert_eq!(token_client.balance(&treasury), 50_000);

        let proposal = client.get_rescue_proposal(&pid).unwrap();
        assert_eq!(proposal.status, RescueProposalStatus::Executed);
    }

    #[test]
    fn test_rescue_handler_cancellation() {
        let (_env, client, admin, treasury) = setup_test();
        let env = &client.env;
        let mis_sent_token = Address::generate(env);

        let pid = client.queue_token_rescue(&admin, &mis_sent_token, &1_000, &treasury);

        client.cancel_token_rescue(&admin, &pid);

        let proposal = client.get_rescue_proposal(&pid).unwrap();
        assert_eq!(proposal.status, RescueProposalStatus::Cancelled);

        advance_timestamp(env, RESCUE_TIMELOCK_DELAY + 10);

        let res = client.try_execute_token_rescue(&admin, &pid);
        assert_eq!(res, Err(Ok(ContractError::RescueProposalNotPending)));
    }
}
