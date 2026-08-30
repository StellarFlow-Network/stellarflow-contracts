use soroban_sdk::{contracttype, Address, Env, Map, Symbol};

use crate::storage::{NodeProfileKey, SignerKey, StakeKey};
use crate::{AssetFeedMetrics, ContractError, NodeProfile};

pub const SCHEMA_VERSION: u32 = 2;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MigrationDataKey {
    SchemaVersion,
}

pub fn ensure_schema_version(env: &Env) -> Result<u32, ContractError> {
    let version: Option<u32> = env.storage().instance().get(&MigrationDataKey::SchemaVersion);

    match version {
        Some(current) if current >= SCHEMA_VERSION => Ok(current),
        Some(current) => {
            migrate_from_version(env, current)?;
            env.storage().instance().set(&MigrationDataKey::SchemaVersion, &SCHEMA_VERSION);
            Ok(SCHEMA_VERSION)
        }
        None => {
            migrate_from_version(env, 0)?;
            env.storage().instance().set(&MigrationDataKey::SchemaVersion, &SCHEMA_VERSION);
            Ok(SCHEMA_VERSION)
        }
    }
}

fn migrate_from_version(env: &Env, from_version: u32) -> Result<(), ContractError> {
    if from_version >= SCHEMA_VERSION {
        return Ok(());
    }

    let legacy_node_profiles_key = Symbol::new(env, "NODES");
    if env.storage().instance().has(&legacy_node_profiles_key) {
        if let Some(profiles) = env.storage().instance().get::<_, Map<Address, NodeProfile>>(&legacy_node_profiles_key) {
            for (address, profile) in profiles.iter() {
                let key = NodeProfileKey::ProfileByNode(address.clone());
                env.storage().persistent().set(&key, &profile);
            }
        }
        env.storage().instance().remove(&legacy_node_profiles_key);
    }

    let legacy_signers_key = Symbol::new(env, "SIGNERS");
    if env.storage().instance().has(&legacy_signers_key) {
        if let Some(signers) = env.storage().instance().get::<_, Map<Address, bool>>(&legacy_signers_key) {
            for (address, _) in signers.iter() {
                let key = SignerKey::SignerByAddress(address.clone());
                env.storage().instance().set(&key, &true);
            }
            // The legacy signer Map lives under the same symbol that now holds
            // the signer *count* (`SIGNERS_KEY`): overwrite it in place rather
            // than removing it, so the converted multi-sig tally survives.
            env.storage().instance().set(&crate::SIGNERS_KEY, &(signers.len() as u32));
        } else {
            env.storage().instance().remove(&legacy_signers_key);
        }
    }

    let legacy_stake_registry_key = Symbol::new(env, "STAKES");
    if env.storage().instance().has(&legacy_stake_registry_key) {
        if let Some(stakes) = env.storage().instance().get::<_, Map<Address, u64>>(&legacy_stake_registry_key) {
            for (address, amount) in stakes.iter() {
                let key = StakeKey::StakeByNode(address.clone());
                env.storage().instance().set(&key, &amount);
            }
            // The legacy registry Map shares its symbol with the active
            // `STAKE_REGISTRY_KEY`; the converted Map supersedes it in place.
            env.storage().instance().set(&crate::STAKE_REGISTRY_KEY, &stakes);
        } else {
            env.storage().instance().remove(&legacy_stake_registry_key);
        }
    }

    let legacy_total_staked_key = Symbol::new(env, "TOTAL");
    if env.storage().instance().has(&legacy_total_staked_key) {
        let total_staked: u64 = env.storage().instance().get(&legacy_total_staked_key).unwrap_or(0u64);
        // Legacy and active keys are the same symbol: overwrite (not remove).
        env.storage().instance().set(&crate::TOTAL_STAKED_KEY, &total_staked);
    }

    let legacy_heartbeat_key = Symbol::new(env, "HBEAT");
    if env.storage().instance().has(&legacy_heartbeat_key) {
        if let Some(heartbeats) = env.storage().instance().get::<_, Map<u32, u64>>(&legacy_heartbeat_key) {
            for (asset_id, timestamp) in heartbeats.iter() {
                let key = crate::HeartbeatKey(asset_id);
                env.storage().temporary().set(&key, &timestamp);
            }
        }
        env.storage().instance().remove(&legacy_heartbeat_key);
    }

    Ok(())
}

pub fn migrate_feed_metrics(env: &Env, asset: u32) -> AssetFeedMetrics {
    let metrics_key = crate::StakingStorageKey::AssetMetrics(asset);
    if let Some(existing) = env.storage().persistent().get(&metrics_key) {
        return existing;
    }

    let legacy_key = Symbol::new(env, "METRICS");
    if let Some(legacy) = env.storage().instance().get::<_, AssetFeedMetrics>(&legacy_key) {
        env.storage().persistent().set(&metrics_key, &legacy);
        return legacy;
    }

    AssetFeedMetrics { volume_score: 0, volatility_bps: 0 }
}

