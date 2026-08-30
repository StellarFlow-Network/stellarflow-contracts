//! Native-asset bridge escrow for origin-chain lock and destination proof unlocks.

use soroban_sdk::{contracttype, symbol_short, token, Address, BytesN, Env, Vec};

use crate::{bridge::relayer, ContractData, ContractError, DATA_KEY};

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct BridgeEscrowConfig {
    pub native_token: Address,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct TokenLock {
    pub id: u64,
    pub depositor: Address,
    pub token: Address,
    pub amount: i128,
    pub target_chain_id: u32,
    pub recipient_address: Address,
    pub locked_at_ledger: u32,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct UnlockProof {
    pub proof_hash: BytesN<32>,
    pub source_chain_id: u32,
    pub recipient: Address,
    pub amount: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BridgeEscrowStorageKey {
    Config,
    NextLockId,
    Lock(u64),
    VaultBalance(Address),
}

fn require_protocol_admin(env: &Env, caller: &Address) -> Result<(), ContractError> {
    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;
    if &data.admin != caller {
        return Err(ContractError::NotAdmin);
    }
    caller.require_auth();
    Ok(())
}

fn load_config(env: &Env) -> Result<BridgeEscrowConfig, ContractError> {
    env.storage()
        .instance()
        .get(&BridgeEscrowStorageKey::Config)
        .ok_or(ContractError::BridgeEscrowNotConfigured)
}

fn next_lock_id(env: &Env) -> u64 {
    let id: u64 = env
        .storage()
        .instance()
        .get(&BridgeEscrowStorageKey::NextLockId)
        .unwrap_or(0);
    env.storage()
        .instance()
        .set(&BridgeEscrowStorageKey::NextLockId, &(id + 1));
    id
}

fn checked_add_balance(current: i128, delta: i128) -> Result<i128, ContractError> {
    current.checked_add(delta).ok_or(ContractError::MathOverflow)
}

fn checked_sub_balance(current: i128, delta: i128) -> Result<i128, ContractError> {
    if delta > current {
        return Err(ContractError::BridgeInsufficientBalance);
    }
    current.checked_sub(delta).ok_or(ContractError::MathOverflow)
}

/// Invariant check: assert token reserves exactly match internal balance ledger.
/// Panics immediately if any drift is detected between actual contract balance
/// and the internally tracked VaultBalance.
fn assert_balance_invariant(env: &Env, config: &BridgeEscrowConfig) {
    let token_client = token::Client::new(env, &config.native_token);
    let actual_balance = token_client.balance(&env.current_contract_address());
    
    let balance_key = BridgeEscrowStorageKey::VaultBalance(config.native_token.clone());
    let tracked_balance: i128 = env.storage().persistent().get(&balance_key).unwrap_or(0);
    
    assert_eq!(
        actual_balance,
        tracked_balance,
        "Balance invariant violated: actual={}, tracked={}",
        actual_balance,
        tracked_balance
    );
}

pub fn configure(
    env: &Env,
    admin: Address,
    native_token: Address,
) -> Result<BridgeEscrowConfig, ContractError> {
    require_protocol_admin(env, &admin)?;
    let config = BridgeEscrowConfig { native_token };
    env.storage()
        .instance()
        .set(&BridgeEscrowStorageKey::Config, &config);
    Ok(config)
}

pub fn lock_tokens(
    env: &Env,
    depositor: Address,
    amount: i128,
    target_chain_id: u32,
    recipient_address: Address,
) -> Result<TokenLock, ContractError> {
    if amount <= 0 {
        return Err(ContractError::BridgeInvalidAmount);
    }

    depositor.require_auth();
    let config = load_config(env)?;
    
    // Invariant check: verify balance consistency before state change
    assert_balance_invariant(env, &config);
    
    let token_client = token::Client::new(env, &config.native_token);
    token_client.transfer(&depositor, &env.current_contract_address(), &amount);

    let balance_key = BridgeEscrowStorageKey::VaultBalance(config.native_token.clone());
    let current_balance: i128 = env.storage().persistent().get(&balance_key).unwrap_or(0);
    let new_balance = checked_add_balance(current_balance, amount)?;
    env.storage().persistent().set(&balance_key, &new_balance);
    env.storage().persistent().extend_ttl(
        &balance_key,
        crate::storage::PERSISTENT_TTL_THRESHOLD,
        crate::storage::PERSISTENT_TTL_THRESHOLD,
    );

    let lock = TokenLock {
        id: next_lock_id(env),
        depositor: depositor.clone(),
        token: config.native_token.clone(),
        amount,
        target_chain_id,
        recipient_address: recipient_address.clone(),
        locked_at_ledger: env.ledger().sequence(),
    };

    let lock_key = BridgeEscrowStorageKey::Lock(lock.id);
    env.storage().persistent().set(&lock_key, &lock);
    env.storage().persistent().extend_ttl(
        &lock_key,
        crate::storage::PERSISTENT_TTL_THRESHOLD,
        crate::storage::PERSISTENT_TTL_THRESHOLD,
    );

    env.events().publish(
        (symbol_short!("tok_lock"), config.native_token.clone(), depositor),
        (lock.id, amount, target_chain_id, recipient_address),
    );

    // Invariant check: verify balance consistency after state change
    assert_balance_invariant(env, &config);

    Ok(lock)
}

pub fn unlock_tokens(
    env: &Env,
    proof: UnlockProof,
    signatures: Vec<(BytesN<32>, BytesN<64>)>,
) -> Result<i128, ContractError> {
    if proof.amount <= 0 {
        return Err(ContractError::BridgeInvalidAmount);
    }

    relayer::verify_cross_chain_payload(env, proof.proof_hash.clone(), signatures)?;
    let config = load_config(env)?;

    // Invariant check: verify balance consistency before state change
    assert_balance_invariant(env, &config);

    let balance_key = BridgeEscrowStorageKey::VaultBalance(config.native_token.clone());
    let current_balance: i128 = env.storage().persistent().get(&balance_key).unwrap_or(0);
    let new_balance = checked_sub_balance(current_balance, proof.amount)?;
    if new_balance == 0 {
        env.storage().persistent().remove(&balance_key);
    } else {
        env.storage().persistent().set(&balance_key, &new_balance);
    }

    let token_client = token::Client::new(env, &config.native_token);
    token_client.transfer(
        &env.current_contract_address(),
        &proof.recipient,
        &proof.amount,
    );

    env.events().publish(
        (symbol_short!("tok_unlk"), config.native_token.clone(), proof.recipient),
        (proof.proof_hash, proof.source_chain_id, proof.amount, new_balance),
    );

    // Invariant check: verify balance consistency after state change
    assert_balance_invariant(env, &config);

    Ok(new_balance)
}

pub fn get_lock(env: &Env, lock_id: u64) -> Option<TokenLock> {
    env.storage()
        .persistent()
        .get(&BridgeEscrowStorageKey::Lock(lock_id))
}

pub fn vault_balance(env: &Env) -> i128 {
    match load_config(env) {
        Ok(config) => env
            .storage()
            .persistent()
            .get(&BridgeEscrowStorageKey::VaultBalance(config.native_token))
            .unwrap_or(0),
        Err(_) => 0,
    }
}

pub fn get_config(env: &Env) -> Option<BridgeEscrowConfig> {
    env.storage().instance().get(&BridgeEscrowStorageKey::Config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Events};

    fn setup() -> (
        Env,
        crate::TimeLockedUpgradeContractClient<'static>,
        Address,
        Address,
        Address,
        Address,
        Address,
    ) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let client = crate::TimeLockedUpgradeContractClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        client.initialize(&admin, &treasury);

        let issuer = Address::generate(&env);
        let token = env.register_stellar_asset_contract(issuer.clone());
        let depositor = Address::generate(&env);
        let recipient = Address::generate(&env);
        soroban_sdk::token::StellarAssetClient::new(&env, &token).mint(&depositor, &10_000);

        (env, client, contract_id, admin, token, depositor, recipient)
    }

    #[test]
    fn lock_tokens_transfers_to_vault_and_emits_event() {
        let (env, client, contract_id, admin, token, depositor, recipient) = setup();
        client.configure_bridge_escrow(&admin, &token);

        let lock = client.lock_tokens(&depositor, &1_500, &42, &recipient);
        assert_eq!(lock.id, 0);
        assert_eq!(lock.amount, 1_500);
        assert_eq!(client.bridge_vault_balance(), 1_500);

        let token_client = token::Client::new(&env, &token);
        assert_eq!(token_client.balance(&depositor), 8_500);
        assert_eq!(token_client.balance(&contract_id), 1_500);
        let vault_event_count = env
            .events()
            .all()
            .into_iter()
            .filter(|e| e.0 == contract_id)
            .count();
        assert_eq!(vault_event_count, 1);
    }

    #[test]
    fn lock_tokens_rejects_non_positive_amount() {
        let (_env, client, _contract_id, admin, token, depositor, recipient) = setup();
        client.configure_bridge_escrow(&admin, &token);

        let result = client.try_lock_tokens(&depositor, &0, &42, &recipient);
        assert_eq!(result, Err(Ok(ContractError::BridgeInvalidAmount)));
    }

    #[test]
    fn configured_lock_can_be_queried() {
        let (_env, client, _contract_id, admin, token, depositor, recipient) = setup();
        client.configure_bridge_escrow(&admin, &token);

        client.lock_tokens(&depositor, &500, &7, &recipient);
        let stored = client.get_bridge_lock(&0).unwrap();
        assert_eq!(stored.depositor, depositor);
        assert_eq!(stored.target_chain_id, 7);
        assert_eq!(stored.recipient_address, recipient);
    }
}
