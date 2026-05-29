#![cfg(test)]

use super::*;
use soroban_sdk::{Address, Env};

#[test]
fn test_initialize() {
    let env = Env::default();
    let contract_id = env.register(RewardSplitter, ());
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);

    client.initialize(&admin, &token);

    assert_eq!(client.get_admin(), admin);
    assert_eq!(client.get_token(), token);
}

#[test]
#[should_panic(expected = "Error(AlreadyInitialized)")]
fn test_initialize_twice() {
    let env = Env::default();
    let contract_id = env.register(RewardSplitter, ());
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);

    client.initialize(&admin, &token);
    client.initialize(&admin, &token);
}

#[test]
fn test_add_recipient() {
    let env = Env::default();
    let contract_id = env.register(RewardSplitter, ());
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
#[should_panic(expected = "Error(Unauthorized)")]
fn test_add_recipient_unauthorized() {
    let env = Env::default();
    let contract_id = env.register(RewardSplitter, ());
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let recipient = Address::generate(&env);
    let unauthorized = Address::generate(&env);

    client.initialize(&admin, &token);

    client.add_recipient(&unauthorized, &recipient, &5000);
}

#[test]
#[should_panic(expected = "Error(InvalidShare)")]
fn test_add_recipient_invalid_share_zero() {
    let env = Env::default();
    let contract_id = env.register(RewardSplitter, ());
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let recipient = Address::generate(&env);

    client.initialize(&admin, &token);

    client.add_recipient(&admin, &recipient, &0);
}

#[test]
#[should_panic(expected = "Error(InvalidShare)")]
fn test_add_recipient_invalid_share_exceeds_100() {
    let env = Env::default();
    let contract_id = env.register(RewardSplitter, ());
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let recipient = Address::generate(&env);

    client.initialize(&admin, &token);

    client.add_recipient(&admin, &recipient, &10001);
}

#[test]
#[should_panic(expected = "Error(TotalSharesExceeded)")]
fn test_add_recipient_total_exceeded() {
    let env = Env::default();
    let contract_id = env.register(RewardSplitter, ());
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let recipient1 = Address::generate(&env);
    let recipient2 = Address::generate(&env);

    client.initialize(&admin, &token);

    client.add_recipient(&admin, &recipient1, &6000); // 60%
    client.add_recipient(&admin, &recipient2, &5000); // 50% - would exceed 100%
}

#[test]
fn test_remove_recipient() {
    let env = Env::default();
    let contract_id = env.register(RewardSplitter, ());
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
    let contract_id = env.register(RewardSplitter, ());
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
#[should_panic(expected = "Error(TotalSharesExceeded)")]
fn test_update_recipient_share_exceeds_total() {
    let env = Env::default();
    let contract_id = env.register(RewardSplitter, ());
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let recipient1 = Address::generate(&env);
    let recipient2 = Address::generate(&env);

    client.initialize(&admin, &token);

    client.add_recipient(&admin, &recipient1, &5000); // 50%
    client.add_recipient(&admin, &recipient2, &3000); // 30%

    client.update_recipient_share(&admin, &recipient1, &8000); // Would exceed 100%
}

#[test]
fn test_transfer_admin() {
    let env = Env::default();
    let contract_id = env.register(RewardSplitter, ());
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
    let contract_id = env.register(RewardSplitter, ());
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
    let contract_id = env.register(RewardSplitter, ());
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

    // Mint tokens to the splitter contract
    let splitter_address = env.current_contract_address();
    token_client.mint(&splitter_address, &1000);

    // Update the token address in the splitter to match the mock token
    client.update_token(&admin, &token_contract_id);

    // Distribute
    client.distribute(&1000);

    // Check balances
    assert_eq!(token_client.balance(&recipient1), 500);
    assert_eq!(token_client.balance(&recipient2), 500);
}

#[test]
#[should_panic(expected = "Error(ZeroAmount)")]
fn test_distribute_zero_amount() {
    let env = Env::default();
    let contract_id = env.register(RewardSplitter, ());
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let recipient = Address::generate(&env);

    client.initialize(&admin, &token);
    client.add_recipient(&admin, &recipient, &10000);

    client.distribute(&0);
}

#[test]
#[should_panic(expected = "Error(NoRecipients)")]
fn test_distribute_no_recipients() {
    let env = Env::default();
    let contract_id = env.register(RewardSplitter, ());
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);

    client.initialize(&admin, &token);

    client.distribute(&1000);
}

#[test]
fn test_get_default_values() {
    let env = Env::default();
    let contract_id = env.register(RewardSplitter, ());
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
    let contract_id = env.register(RewardSplitter, ());
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
#[should_panic(expected = "Error(Unauthorized)")]
fn test_reset_parameters_unauthorized() {
    let env = Env::default();
    let contract_id = env.register(RewardSplitter, ());
    let client = RewardSplitterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let unauthorized = Address::generate(&env);

    client.initialize(&admin, &token);

    client.reset_parameters(&unauthorized);
}
