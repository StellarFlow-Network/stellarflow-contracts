#![cfg(test)]

use super::*;
use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{symbol_short, token, Address, Env, String, Symbol};

fn advance_timestamp(env: &Env, delta: u64) {
    let info = env.ledger().get();
    env.ledger().set(soroban_sdk::testutils::LedgerInfo {
        protocol_version: info.protocol_version,
        sequence_number: info.sequence_number,
        timestamp: info.timestamp + delta,
        network_id: Default::default(),
        base_reserve: 10,
        min_temp_entry_ttl: 4096,
        min_persistent_entry_ttl: 4096,
        max_entry_ttl: 6_312_000,
    });
}

#[test]
fn test_initialize() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(&None, RewardSplitter);
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);

    client.initialize(&admin, &token);

    assert_eq!(client.get_admin(), admin);
    assert_eq!(client.get_token(), token);
}

#[test]
fn test_initialize_twice() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(&None, RewardSplitter);
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);

    client.initialize(&admin, &token);
    assert!(matches!(
        client.try_initialize(&admin, &token),
        Err(Ok(Error::AlreadyInitialized))
    ));
}

#[test]
fn test_add_recipient() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(&None, RewardSplitter);
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let recipient1 = Address::generate(&env);
    let recipient2 = Address::generate(&env);

    client.initialize(&admin, &token);

    client.add_recipient(&admin, &recipient1, &5000); // 50%
    client.add_recipient(&admin, &recipient2, &5000); // 50%

    let recipients = client.get_recipients();
    assert_eq!(recipients.len(), 2);
    assert_eq!(client.get_total_shares(), 10000);
}

#[test]
fn test_add_recipient_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(&None, RewardSplitter);
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let recipient = Address::generate(&env);
    let unauthorized = Address::generate(&env);

    client.initialize(&admin, &token);

    assert!(matches!(
        client.try_add_recipient(&unauthorized, &recipient, &5000),
        Err(Ok(Error::Unauthorized))
    ));
}

#[test]
fn test_add_recipient_invalid_share_zero() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(&None, RewardSplitter);
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let recipient = Address::generate(&env);

    client.initialize(&admin, &token);

    assert!(matches!(
        client.try_add_recipient(&admin, &recipient, &0),
        Err(Ok(Error::InvalidShare))
    ));
}

#[test]
fn test_add_recipient_invalid_share_exceeds_100() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(&None, RewardSplitter);
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let recipient = Address::generate(&env);

    client.initialize(&admin, &token);

    assert!(client.try_add_recipient(&admin, &recipient, &10001).is_err());
}

#[test]
fn test_add_recipient_total_exceeded() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(&None, RewardSplitter);
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let recipient1 = Address::generate(&env);
    let recipient2 = Address::generate(&env);

    client.initialize(&admin, &token);

    client.add_recipient(&admin, &recipient1, &6000); // 60%
    assert!(matches!(
        client.try_add_recipient(&admin, &recipient2, &5000), // 50% - would exceed 100%
        Err(Ok(Error::TotalSharesExceeded))
    ));
}

#[test]
fn test_remove_recipient() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(&None, RewardSplitter);
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let recipient1 = Address::generate(&env);
    let recipient2 = Address::generate(&env);

    client.initialize(&admin, &token);

    client.add_recipient(&admin, &recipient1, &5000);
    client.add_recipient(&admin, &recipient2, &5000);

    client.remove_recipient(&admin, &recipient1);

    let recipients = client.get_recipients();
    assert_eq!(recipients.len(), 1);
    assert_eq!(client.get_total_shares(), 5000);
}

#[test]
fn test_update_recipient_share() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(&None, RewardSplitter);
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let recipient1 = Address::generate(&env);
    let recipient2 = Address::generate(&env);

    client.initialize(&admin, &token);

    client.add_recipient(&admin, &recipient1, &3000); // 30%
    client.add_recipient(&admin, &recipient2, &3000); // 30%

    client.update_recipient_share(&admin, &recipient1, &5000); // Update to 50%

    assert_eq!(client.get_total_shares(), 8000);
}

#[test]
fn test_update_recipient_share_exceeds_total() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(&None, RewardSplitter);
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let recipient1 = Address::generate(&env);
    let recipient2 = Address::generate(&env);

    client.initialize(&admin, &token);

    client.add_recipient(&admin, &recipient1, &5000); // 50%
    client.add_recipient(&admin, &recipient2, &3000); // 30%

    assert!(matches!(
        client.try_update_recipient_share(&admin, &recipient1, &8000), // Would exceed 100%
        Err(Ok(Error::TotalSharesExceeded))
    ));
}

#[test]
fn test_transfer_admin() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(&None, RewardSplitter);
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let new_admin = Address::generate(&env);
    let token = Address::generate(&env);

    client.initialize(&admin, &token);

    client.transfer_admin(&admin, &new_admin);

    assert_eq!(client.get_admin(), new_admin);
}

#[test]
fn test_update_token() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(&None, RewardSplitter);
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let new_token = Address::generate(&env);

    client.initialize(&admin, &token);

    client.update_token(&admin, &new_token);

    assert_eq!(client.get_token(), new_token);
}

#[test]
fn test_distribute() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(&None, RewardSplitter);
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let recipient1 = Address::generate(&env);
    let recipient2 = Address::generate(&env);

    client.initialize(&admin, &token);
    client.add_recipient(&admin, &recipient1, &5000); // 50%
    client.add_recipient(&admin, &recipient2, &5000); // 50%

    // Create a mock token contract using soroban-sdk testutils
    let token_contract_id = env.register_stellar_asset_contract(token.clone());
    let token_client = soroban_sdk::token::StellarAssetClient::new(&env, &token_contract_id);

    // Mint tokens to a funder account, then send them to the splitter contract
    let funder = Address::generate(&env);
    token_client.set_admin(&admin);
    token_client.mint(&funder, &1000);
    token::Client::new(&env, &token_contract_id).transfer(&funder, &contract_id, &1000);

    // Update the token address in the splitter to match the mock token
    client.update_token(&admin, &token_contract_id);

    // Distribute
    client.distribute(&1000);

    // Check balances
    assert_eq!(token::Client::new(&env, &token_contract_id).balance(&recipient1), 500);
    assert_eq!(token::Client::new(&env, &token_contract_id).balance(&recipient2), 500);
}

#[test]
fn test_distribute_zero_amount() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(&None, RewardSplitter);
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let recipient = Address::generate(&env);

    client.initialize(&admin, &token);
    client.add_recipient(&admin, &recipient, &10000);

    assert!(matches!(client.try_distribute(&0), Err(Ok(Error::ZeroAmount))));
}

#[test]
fn test_distribute_no_recipients() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(&None, RewardSplitter);
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);

    client.initialize(&admin, &token);

    assert!(matches!(
        client.try_distribute(&1000),
        Err(Ok(Error::NoRecipients))
    ));
}

#[test]
fn test_get_default_values() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(&None, RewardSplitter);
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);

    client.initialize(&admin, &token);

    assert_eq!(client.get_default_admin(), admin);
    assert_eq!(client.get_default_token(), token);
}

#[test]
fn test_reset_parameters() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(&None, RewardSplitter);
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let new_admin = Address::generate(&env);
    let new_token = Address::generate(&env);
    let recipient = Address::generate(&env);

    client.initialize(&admin, &token);
    client.add_recipient(&admin, &recipient, &10000);

    // Change parameters
    client.transfer_admin(&admin, &new_admin);
    client.update_token(&new_admin, &new_token);

    // Verify parameters changed
    assert_eq!(client.get_admin(), new_admin);
    assert_eq!(client.get_token(), new_token);
    assert_eq!(client.get_total_shares(), 10000);

    // Reset to defaults
    client.reset_parameters(&new_admin);

    // Verify parameters reset to defaults
    assert_eq!(client.get_admin(), admin);
    assert_eq!(client.get_token(), token);
    assert_eq!(client.get_total_shares(), 0);
    assert_eq!(client.get_recipients().len(), 0);
}

#[test]
fn test_reset_parameters_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(&None, RewardSplitter);
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let unauthorized = Address::generate(&env);

    client.initialize(&admin, &token);

    assert!(matches!(
        client.try_reset_parameters(&unauthorized),
        Err(Ok(Error::Unauthorized))
    ));
}

#[test]
fn test_propose_action() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(&None, RewardSplitter);
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);

    client.initialize(&admin, &token);

    let action_id = client.propose_action(
        &admin,
        &CooldownActionType::ResetParameters,
        &String::from_str(&env, "test"),
    );

    let action = client.get_action(&action_id).unwrap();
    assert_eq!(action.current_stage, 1);
    assert_eq!(action.executed, false);
    assert_eq!(action.cancelled, false);
}

#[test]
fn test_propose_action_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(&None, RewardSplitter);
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let unauthorized = Address::generate(&env);

    client.initialize(&admin, &token);

    assert!(matches!(
        client.try_propose_action(
            &unauthorized,
            &CooldownActionType::ResetParameters,
            &String::from_str(&env, "test"),
        ),
        Err(Ok(Error::Unauthorized))
    ));
}

#[test]
fn test_advance_action() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(&None, RewardSplitter);
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);

    client.initialize(&admin, &token);

    let action_id = client.propose_action(
        &admin,
        &CooldownActionType::ResetParameters,
        &String::from_str(&env, "test"),
    );

    // Advance time past stage 1 cooldown
    advance_timestamp(&env, 4000);

    client.advance_action(&admin, &action_id);

    let action = client.get_action(&action_id).unwrap();
    assert_eq!(action.current_stage, 2);
}

#[test]
fn test_advance_action_too_soon() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(&None, RewardSplitter);
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);

    client.initialize(&admin, &token);

    let action_id = client.propose_action(
        &admin,
        &CooldownActionType::ResetParameters,
        &String::from_str(&env, "test"),
    );

    // Try to advance without waiting
    assert!(matches!(
        client.try_advance_action(&admin, &action_id),
        Err(Ok(Error::CooldownNotExpired))
    ));
}

#[test]
fn test_execute_action() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(&None, RewardSplitter);
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let recipient = Address::generate(&env);

    client.initialize(&admin, &token);
    client.add_recipient(&admin, &recipient, &10000);

    let action_id = client.propose_action(
        &admin,
        &CooldownActionType::ResetParameters,
        &String::from_str(&env, "test"),
    );

    // Advance through all stages
    advance_timestamp(&env, 4000);
    client.advance_action(&admin, &action_id);

    advance_timestamp(&env, 29000);
    client.advance_action(&admin, &action_id);

    advance_timestamp(&env, 87000);
    client.advance_action(&admin, &action_id);

    client.execute_action(&admin, &action_id);

    let action = client.get_action(&action_id).unwrap();
    assert_eq!(action.executed, true);
    assert_eq!(client.get_total_shares(), 0);
}

#[test]
fn test_cancel_action() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(&None, RewardSplitter);
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);

    client.initialize(&admin, &token);

    let action_id = client.propose_action(
        &admin,
        &CooldownActionType::ResetParameters,
        &String::from_str(&env, "test"),
    );

    client.cancel_action(&admin, &action_id);

    let action = client.get_action(&action_id).unwrap();
    assert_eq!(action.cancelled, true);
}

#[test]
fn test_get_cooldown_remaining() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(&None, RewardSplitter);
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);

    client.initialize(&admin, &token);

    let action_id = client.propose_action(
        &admin,
        &CooldownActionType::ResetParameters,
        &String::from_str(&env, "test"),
    );

    let remaining = client.get_cooldown_remaining(&action_id).unwrap();
    assert!(remaining > 0);
}

#[test]
fn test_configure_cooldown_stage() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(&None, RewardSplitter);
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);

    client.initialize(&admin, &token);

    client.configure_cooldown_stage(&admin, &1, &7200, &symbol_short!("CUSTOM"));

    let stage = client.get_cooldown_stage(&1).unwrap();
    assert_eq!(stage.cooldown_seconds, 7200);
}

#[test]
fn test_configure_cooldown_stage_invalid() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(&None, RewardSplitter);
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);

    client.initialize(&admin, &token);

    assert!(matches!(
        client.try_configure_cooldown_stage(&admin, &5, &7200, &symbol_short!("INVALID")),
        Err(Ok(Error::InvalidStage))
    ));
}
