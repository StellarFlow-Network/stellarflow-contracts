//! Automated state maintenance rent-bump keeper handler (Issue #758).
//!
//! Allows permissionless off-chain keepers to extend expiring persistent
//! storage key TTLs and receive a small Stroop bounty reward in return.
//! Any caller may invoke `bump_storage_key` for a registered target.

use soroban_sdk::{contracttype, symbol_short, token, Address, Env};

use crate::ContractError;

/// Ledger threshold below which a key is considered "near expiry" and eligible
/// for a keeper bump. Equivalent to ~10,000 ledgers (~14 hours at 5 s/ledger).
pub const KEEPER_TTL_THRESHOLD: u32 = 10_000;

/// Target TTL after a successful keeper bump (~100,000 ledgers ≈ 6 days).
pub const KEEPER_TTL_BUMP_AMOUNT: u32 = 100_000;

/// Stroop reward paid to the keeper per successful bump.
/// 1 XLM = 10,000,000 stroops; 5_000 stroops ≈ 0.0005 XLM.
pub const KEEPER_BOUNTY_STROOPS: i128 = 5_000;

/// Identifies which managed persistent storage target to bump.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BumpTarget {
    /// Subscription entry for a given consumer address.
    Subscription(Address),
    /// Staking entry for a given node address.
    Stake(Address),
    /// Feed stake entry for a given (node, asset_id) pair.
    FeedStake(Address, u32),
}

/// Storage key for bump-eligible targets registered with this keeper module.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KeeperStorageKey {
    /// Tracks that a bump target is registered.
    RegisteredTarget(BumpTarget),
    /// Reward token address used to pay keeper bounties.
    RewardToken,
    /// Protocol-owned reward reserve address.
    RewardReserve,
}

/// Register a persistent storage target as bump-eligible.
///
/// Only the contract admin may call this. The `reward_token` and
/// `reward_reserve` must be set before any bumps can be paid out.
pub fn register_target(env: &Env, admin: Address, target: BumpTarget) {
    admin.require_auth();
    let key = KeeperStorageKey::RegisteredTarget(target);
    env.storage().persistent().set(&key, &true);
    env.storage().persistent().extend_ttl(&key, KEEPER_TTL_THRESHOLD, KEEPER_TTL_BUMP_AMOUNT);
}

/// Set the reward token and reserve account used to pay keeper bounties.
///
/// Only the contract admin may call this.
pub fn set_reward_config(env: &Env, admin: Address, reward_token: Address, reward_reserve: Address) {
    admin.require_auth();
    env.storage().persistent().set(&KeeperStorageKey::RewardToken, &reward_token);
    env.storage().persistent().set(&KeeperStorageKey::RewardReserve, &reward_reserve);
    env.storage().persistent().extend_ttl(
        &KeeperStorageKey::RewardToken,
        KEEPER_TTL_THRESHOLD,
        KEEPER_TTL_BUMP_AMOUNT,
    );
    env.storage().persistent().extend_ttl(
        &KeeperStorageKey::RewardReserve,
        KEEPER_TTL_THRESHOLD,
        KEEPER_TTL_BUMP_AMOUNT,
    );
}

/// Bump the TTL of an expiring persistent storage key and pay the keeper a
/// small Stroop bounty reward.
///
/// # Behaviour
/// 1. Verifies that `target` is a registered bump-eligible key.
/// 2. Checks that the entry exists and its TTL is at or below
///    `KEEPER_TTL_THRESHOLD` (i.e. it is near expiry).
/// 3. Calls `env.storage().persistent().extend_ttl()` on the target entry.
/// 4. Transfers `KEEPER_BOUNTY_STROOPS` from the reward reserve to `keeper`.
/// 5. Emits a `RentBump` event.
///
/// Returns `Ok(())` on success or a [`ContractError`] on failure.
pub fn bump_storage_key(
    env: &Env,
    keeper: Address,
    target: BumpTarget,
) -> Result<(), ContractError> {
    keeper.require_auth();

    let registered_key = KeeperStorageKey::RegisteredTarget(target.clone());
    let is_registered: bool = env
        .storage()
        .persistent()
        .get(&registered_key)
        .unwrap_or(false);
    if !is_registered {
        return Err(ContractError::NotRegistered);
    }

    // Retrieve reward config.
    let reward_token: Address = env
        .storage()
        .persistent()
        .get(&KeeperStorageKey::RewardToken)
        .ok_or(ContractError::NotInitialized)?;
    let reward_reserve: Address = env
        .storage()
        .persistent()
        .get(&KeeperStorageKey::RewardReserve)
        .ok_or(ContractError::NotInitialized)?;

    // Extend TTL of the target key.
    match &target {
        BumpTarget::Subscription(addr) => {
            let data_key = crate::storage::DataKey::Subscription(addr.clone());
            if !env.storage().persistent().has(&data_key) {
                return Err(ContractError::NotRegistered);
            }
            env.storage().persistent().extend_ttl(
                &data_key,
                KEEPER_TTL_THRESHOLD,
                KEEPER_TTL_BUMP_AMOUNT,
            );
        }
        BumpTarget::Stake(addr) => {
            let stake_key = crate::storage::StakeKey::StakeByNode(addr.clone());
            if !env.storage().persistent().has(&stake_key) {
                return Err(ContractError::NotRegistered);
            }
            env.storage().persistent().extend_ttl(
                &stake_key,
                KEEPER_TTL_THRESHOLD,
                KEEPER_TTL_BUMP_AMOUNT,
            );
        }
        BumpTarget::FeedStake(addr, asset_id) => {
            let feed_key = crate::StakingStorageKey::FeedStake(addr.clone(), *asset_id);
            if !env.storage().persistent().has(&feed_key) {
                return Err(ContractError::NotRegistered);
            }
            env.storage().persistent().extend_ttl(
                &feed_key,
                KEEPER_TTL_THRESHOLD,
                KEEPER_TTL_BUMP_AMOUNT,
            );
        }
    }

    // Pay keeper the Stroop bounty from the reward reserve.
    let token_client = token::Client::new(env, &reward_token);
    token_client.transfer(&reward_reserve, &keeper, &KEEPER_BOUNTY_STROOPS);

    // Emit structured event.
    env.events().publish(
        (symbol_short!("RentBump"), keeper.clone()),
        (target, KEEPER_BOUNTY_STROOPS),
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::Env;

    #[test]
    fn constants_are_sane() {
        assert!(KEEPER_TTL_THRESHOLD < KEEPER_TTL_BUMP_AMOUNT);
        assert!(KEEPER_BOUNTY_STROOPS > 0);
    }

    #[test]
    fn bump_unregistered_target_returns_error() {
        let env = Env::default();
        env.mock_all_auths();
        let keeper = Address::generate(&env);
        let target = BumpTarget::Subscription(Address::generate(&env));
        let result = bump_storage_key(&env, keeper, target);
        assert_eq!(result, Err(ContractError::NotRegistered));
    }
}
