//! Rolling 24-hour bridge mint/unlock rate limiter.

use soroban_sdk::{contracttype, Address, Env, Symbol, Vec};

use crate::{ContractData, ContractError, DATA_KEY};

pub const WINDOW_SECONDS: u64 = 24 * 60 * 60;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RateLimitAsset {
    Wrapped(Symbol),
    Native(Address),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RateLimitKey {
    Cap(RateLimitAsset),
    Mints(RateLimitAsset),
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct MintWindowEntry {
    pub timestamp: u64,
    pub amount: i128,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct MintRateLimit {
    pub asset: RateLimitAsset,
    pub max_rolling_amount: i128,
    pub window_seconds: u64,
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

pub fn set_limit(
    env: &Env,
    admin: Address,
    asset: RateLimitAsset,
    max_rolling_amount: i128,
) -> Result<MintRateLimit, ContractError> {
    require_protocol_admin(env, &admin)?;
    if max_rolling_amount <= 0 {
        return Err(ContractError::InvalidBridgeRateLimit);
    }

    let limit = MintRateLimit {
        asset: asset.clone(),
        max_rolling_amount,
        window_seconds: WINDOW_SECONDS,
    };
    env.storage()
        .persistent()
        .set(&RateLimitKey::Cap(asset), &limit);
    Ok(limit)
}

pub fn get_limit(env: &Env, asset: RateLimitAsset) -> Option<MintRateLimit> {
    env.storage().persistent().get(&RateLimitKey::Cap(asset))
}

pub fn enforce_and_record(env: &Env, asset: RateLimitAsset, amount: i128, fallback_cap: i128) -> Result<(), ContractError> {
    if amount <= 0 {
        return Err(ContractError::BridgeInvalidAmount);
    }

    let cap = get_limit(env, asset.clone())
        .map(|limit| limit.max_rolling_amount)
        .unwrap_or(fallback_cap);
    if cap <= 0 {
        return Err(ContractError::InvalidBridgeRateLimit);
    }

    let now = env.ledger().timestamp();
    let cutoff = now.saturating_sub(WINDOW_SECONDS);
    let key = RateLimitKey::Mints(asset);
    let entries: Vec<MintWindowEntry> = env.storage().persistent().get(&key).unwrap_or_else(|| Vec::new(env));
    let mut pruned = Vec::new(env);
    let mut rolling_total = 0_i128;

    for entry in entries.iter() {
        if entry.timestamp > cutoff {
            rolling_total = rolling_total
                .checked_add(entry.amount)
                .ok_or(ContractError::MathOverflow)?;
            pruned.push_back(entry);
        }
    }

    let next_total = rolling_total.checked_add(amount).ok_or(ContractError::MathOverflow)?;
    if next_total > cap {
        return Err(ContractError::BridgeRateLimitExceeded);
    }

    pruned.push_back(MintWindowEntry { timestamp: now, amount });
    env.storage().persistent().set(&key, &pruned);
    env.storage().persistent().extend_ttl(
        &key,
        crate::storage::PERSISTENT_TTL_THRESHOLD,
        crate::storage::PERSISTENT_TTL_THRESHOLD,
    );
    Ok(())
}
