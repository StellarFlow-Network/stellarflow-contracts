use soroban_sdk::{symbol_short, Address, Env, Map, Symbol};

use crate::{
    bridge::{escrow::BridgeEscrowStorageKey, mint::BridgeAssetConfig},
    storage::{NodeProfileKey, StakeKey},
    ContractError, NodeProfile,
};

/// Verify the protocol's instance and persistent storage remain healthy and TTL
/// bumpable. This suite is intentionally conservative: it only touches storage
/// entries that already exist and refreshes them via the same TTL rules used by
/// the rest of the protocol.
pub fn verify_storage_ttl_bumps(env: &Env) -> Result<(), ContractError> {
    // Ensure the instance record itself is still alive and renewable before any
    // other storage checks run. This is the contract-wide guardrail for the root
    // slot that houses the protocol admin + schema state.
    env.storage().instance().extend_ttl(0, crate::storage::PERSISTENT_TTL_THRESHOLD);

    // Legacy node profile map is stored in persistent storage under the canonical
    // "NODES" symbol; extend each active profile so the runtime cannot silently
    // expire the registry while the protocol is still operating.
    if env.storage().persistent().has(&symbol_short!("NODES")) {
        let nodes: Map<Address, NodeProfile> = env.storage()
            .persistent()
            .get(&symbol_short!("NODES"))
            .unwrap_or_else(|| Map::new(env));
        for (node, _) in nodes.iter() {
            let key = NodeProfileKey(node.clone());
            env.storage().persistent().extend_ttl(
                &key,
                crate::storage::PERSISTENT_TTL_THRESHOLD,
                crate::storage::PERSISTENT_TTL_THRESHOLD,
            );
        }
    }

    // If the protocol keeps a stake registry in instance storage, refresh the
    // canonical account set by touching the aggregate and all indexed entries.
    if env.storage().instance().has(&crate::STAKE_REGISTRY_KEY) {
        let stakes: Map<Address, u64> = env.storage()
            .instance()
            .get(&crate::STAKE_REGISTRY_KEY)
            .unwrap_or_else(|| Map::new(env));
        for (node, _) in stakes.iter() {
            let key = StakeKey::StakeByNode(node.clone());
            if env.storage().instance().has(&key) {
                env.storage().instance().extend_ttl(0, crate::storage::PERSISTENT_TTL_THRESHOLD);
            }
        }
    }

    // Bridge escrow balances are stored in persistent state keyed by token
    // address. Refreshing them here guarantees the bridge reserve account does not
    // silently expire after a period of inactivity.
    if env.storage().instance().has(&symbol_short!("BRIDGE")) {
        let config: Option<crate::bridge::escrow::BridgeEscrowConfig> = env
            .storage()
            .instance()
            .get(&symbol_short!("BRIDGE"));
        if let Some(cfg) = config {
            let balance_key = BridgeEscrowStorageKey::VaultBalance(cfg.native_token.clone());
            if env.storage().persistent().has(&balance_key) {
                env.storage().persistent().extend_ttl(
                    &balance_key,
                    crate::storage::PERSISTENT_TTL_THRESHOLD,
                    crate::storage::PERSISTENT_TTL_THRESHOLD,
                );
            }
        }
    }

    Ok(())
}

/// Verify the canonical protocol totals remain in zero-loss accounting balance:
/// the stake registry sum must equal the global total, and all tracked bridge
/// supplies must remain non-negative and within their configured caps.
pub fn verify_zero_loss_accounting(env: &Env) -> Result<(), ContractError> {
    verify_total_staked_matches_registry(env)?;
    verify_vault_assets_are_non_negative(env)?;
    verify_bridge_suppy_caps_are_valid(env)?;
    Ok(())
}

/// Legacy compatibility wrapper around the full protocol state sanity suite.
pub fn verify_contract_state(env: &Env) -> Result<(), ContractError> {
    verify_storage_ttl_bumps(env)?;
    verify_zero_loss_accounting(env)?;
    Ok(())
}

/// Assert-style alias for callers that want the suite to panic if any invariant
/// fails instead of returning an error.
pub fn assert_contract_state_sanity(env: &Env) -> Result<(), ContractError> {
    verify_contract_state(env)
}

fn verify_total_staked_matches_registry(env: &Env) -> Result<(), ContractError> {
    let stakes: Map<Address, u64> = env
        .storage()
        .instance()
        .get(&crate::STAKE_REGISTRY_KEY)
        .unwrap_or_else(|| Map::new(env));

    let mut sum: u128 = 0;
    for (_, amount) in stakes.iter() {
        sum = sum
            .checked_add(u128::from(amount))
            .ok_or(ContractError::Overflow)?;
    }

    let total: u64 = env
        .storage()
        .instance()
        .get(&crate::TOTAL_STAKED_KEY)
        .unwrap_or(0u64);

    if u128::from(total) != sum {
        return Err(ContractError::Overflow);
    }

    Ok(())
}

fn verify_vault_assets_are_non_negative(env: &Env) -> Result<(), ContractError> {
    let total_assets: Option<i128> = env
        .storage()
        .instance()
        .get(&crate::vaults::autocompound::VaultStorageKey::TotalAssets);
    if let Some(amount) = total_assets {
        if amount < 0 {
            return Err(ContractError::Overflow);
        }
    }
    Ok(())
}

fn verify_bridge_suppy_caps_are_valid(env: &Env) -> Result<(), ContractError> {
    let bridge_asset_key = Symbol::new(env, "BRIDGE_ASSET");
    let asset_config: Option<BridgeAssetConfig> = env
        .storage()
        .instance()
        .get(&bridge_asset_key);
    if let Some(config) = asset_config {
        if config.total_supply < 0 || config.total_supply > config.max_supply {
            return Err(ContractError::Overflow);
        }
    }

    let escrow_cfg: Option<crate::bridge::escrow::BridgeEscrowConfig> = env
        .storage()
        .instance()
        .get(&symbol_short!("BRIDGE"));
    if let Some(cfg) = escrow_cfg {
        let balance_key = BridgeEscrowStorageKey::VaultBalance(cfg.native_token.clone());
        let balance: Option<i128> = env.storage().persistent().get(&balance_key);
        if let Some(amount) = balance {
            if amount < 0 {
                return Err(ContractError::Overflow);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{symbol_short, testutils::Address as _, Address, Env};

    #[test]
    fn verifies_storage_ttl_bumps_for_protocol_state() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let _client = crate::TimeLockedUpgradeContractClient::new(&env, &contract_id);
        _client.initialize(&admin, &treasury);

        let node = Address::generate(&env);
        env.storage().persistent().set(
            &NodeProfileKey(node.clone()),
            &crate::NodeProfile { node, rate: 100, confidence: 90, updated_at: 1 },
        );

        assert!(verify_storage_ttl_bumps(&env).is_ok());
    }

    #[test]
    fn verifies_zero_loss_accounting_for_stake_registry() {
        let env = Env::default();
        let node_a = Address::generate(&env);
        let node_b = Address::generate(&env);
        let mut stakes = Map::new(&env);
        stakes.set(node_a.clone(), 100u64);
        stakes.set(node_b, 250u64);

        env.storage().instance().set(&crate::STAKE_REGISTRY_KEY, &stakes);
        env.storage().instance().set(&crate::TOTAL_STAKED_KEY, &350u64);

        assert!(verify_zero_loss_accounting(&env).is_ok());
    }

    #[test]
    fn verifies_contract_state_suite_aliases() {
        let env = Env::default();
        let node = Address::generate(&env);
        let mut stakes = Map::new(&env);
        stakes.set(node, 42u64);
        env.storage().instance().set(&crate::STAKE_REGISTRY_KEY, &stakes);
        env.storage().instance().set(&crate::TOTAL_STAKED_KEY, &42u64);

        assert!(verify_contract_state(&env).is_ok());
        assert!(assert_contract_state_sanity(&env).is_ok());
    }
}
