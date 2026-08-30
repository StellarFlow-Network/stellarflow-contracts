use soroban_sdk::{contracttype, token, Address, Env, IntoVal};

use crate::ContractError;

const REWARD_PRECISION: i128 = 1_000_000_000_000_000_000;
pub const DEFAULT_EMISSION_MULTIPLIER: u32 = 10_000;
pub const MAX_EMISSION_MULTIPLIER: u32 = 100_000;

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct FarmingConfig {
    pub admin: Address,
    pub lp_token: Address,
    pub reward_token: Address,
    pub emission_per_ledger: i128,
    pub emission_multiplier: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FarmingStorageKey {
    Config,
    TotalShares,
    AccRewardPerShare,
    LastRewardLedger,
    Shares(Address),
    RewardDebt(Address),
    AccruedRewards(Address),
}

fn load_config(env: &Env) -> Result<FarmingConfig, ContractError> {
    env.storage()
        .instance()
        .get(&FarmingStorageKey::Config)
        .ok_or(ContractError::NotInitialized)
}

fn read_i128(env: &Env, key: &FarmingStorageKey) -> i128 {
    env.storage().persistent().get(key).unwrap_or(0)
}

fn write_i128(env: &Env, key: &FarmingStorageKey, value: i128) {
    if value == 0 {
        env.storage().persistent().remove(key);
    } else {
        env.storage().persistent().set(key, &value);
    }
}

fn total_shares(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&FarmingStorageKey::TotalShares)
        .unwrap_or(0)
}

fn accumulated_reward_per_share(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&FarmingStorageKey::AccRewardPerShare)
        .unwrap_or(0)
}

fn update_pool(env: &Env, config: &FarmingConfig) -> Result<i128, ContractError> {
    let current_ledger = env.ledger().sequence();
    let last_ledger: u32 = env
        .storage()
        .instance()
        .get(&FarmingStorageKey::LastRewardLedger)
        .unwrap_or(current_ledger);
    if current_ledger <= last_ledger {
        return Ok(accumulated_reward_per_share(env));
    }

    let shares = total_shares(env);
    let mut acc_reward_per_share = accumulated_reward_per_share(env);
    if shares > 0 {
        let elapsed = i128::from(current_ledger - last_ledger);
        let emitted = config
            .emission_per_ledger
            .checked_mul(elapsed)
            .ok_or(ContractError::MathOverflow)?
            .checked_mul(i128::from(config.emission_multiplier))
            .ok_or(ContractError::MathOverflow)?
            .checked_div(i128::from(DEFAULT_EMISSION_MULTIPLIER))
            .ok_or(ContractError::DivisionByZero)?;
        let reward_per_share = emitted
            .checked_mul(REWARD_PRECISION)
            .ok_or(ContractError::MathOverflow)?
            .checked_div(shares)
            .ok_or(ContractError::DivisionByZero)?;
        acc_reward_per_share = acc_reward_per_share
            .checked_add(reward_per_share)
            .ok_or(ContractError::MathOverflow)?;
        env.storage()
            .instance()
            .set(&FarmingStorageKey::AccRewardPerShare, &acc_reward_per_share);
    }
    env.storage()
        .instance()
        .set(&FarmingStorageKey::LastRewardLedger, &current_ledger);
    Ok(acc_reward_per_share)
}

fn settle_user(env: &Env, user: &Address, acc_reward_per_share: i128) -> Result<(), ContractError> {
    let shares = read_i128(env, &FarmingStorageKey::Shares(user.clone()));
    let reward_debt = read_i128(env, &FarmingStorageKey::RewardDebt(user.clone()));
    let accrued = shares
        .checked_mul(acc_reward_per_share)
        .ok_or(ContractError::MathOverflow)?
        .checked_div(REWARD_PRECISION)
        .ok_or(ContractError::DivisionByZero)?;
    let pending = accrued
        .checked_sub(reward_debt)
        .ok_or(ContractError::MathOverflow)?;
    if pending > 0 {
        let current = read_i128(env, &FarmingStorageKey::AccruedRewards(user.clone()));
        write_i128(
            env,
            &FarmingStorageKey::AccruedRewards(user.clone()),
            current.checked_add(pending).ok_or(ContractError::MathOverflow)?,
        );
    }
    write_i128(
        env,
        &FarmingStorageKey::RewardDebt(user.clone()),
        accrued,
    );
    Ok(())
}

pub fn initialize(
    env: &Env,
    admin: Address,
    lp_token: Address,
    reward_token: Address,
    emission_per_ledger: i128,
) -> Result<FarmingConfig, ContractError> {
    if env.storage().instance().has(&FarmingStorageKey::Config) {
        return Err(ContractError::AlreadyInitialized);
    }
    if emission_per_ledger < 0 {
        return Err(ContractError::InvalidStakeAmount);
    }
    admin.require_auth();
    let config = FarmingConfig {
        admin,
        lp_token,
        reward_token,
        emission_per_ledger,
        emission_multiplier: DEFAULT_EMISSION_MULTIPLIER,
    };
    env.storage().instance().set(&FarmingStorageKey::Config, &config);
    env.storage().instance().set(&FarmingStorageKey::LastRewardLedger, &env.ledger().sequence());
    Ok(config)
}

pub fn fund_rewards(env: &Env, funder: Address, amount: i128) -> Result<(), ContractError> {
    if amount <= 0 {
        return Err(ContractError::InvalidStakeAmount);
    }
    let config = load_config(env)?;
    funder.require_auth();
    token::Client::new(env, &config.reward_token).transfer(
        &funder,
        &env.current_contract_address(),
        &amount,
    );
    Ok(())
}

pub fn stake(env: &Env, user: Address, amount: i128) -> Result<i128, ContractError> {
    if amount <= 0 {
        return Err(ContractError::InvalidStakeAmount);
    }
    let config = load_config(env)?;
    user.require_auth();
    let acc_reward_per_share = update_pool(env, &config)?;
    settle_user(env, &user, acc_reward_per_share)?;
    token::Client::new(env, &config.lp_token).transfer(
        &user,
        &env.current_contract_address(),
        &amount,
    );
    let shares = read_i128(env, &FarmingStorageKey::Shares(user.clone()));
    write_i128(env, &FarmingStorageKey::Shares(user.clone()), shares.checked_add(amount).ok_or(ContractError::MathOverflow)?);
    env.storage().instance().set(
        &FarmingStorageKey::TotalShares,
        &total_shares(env).checked_add(amount).ok_or(ContractError::MathOverflow)?,
    );
    Ok(amount)
}

pub fn claim_rewards(env: &Env, user: Address) -> Result<i128, ContractError> {
    let config = load_config(env)?;
    user.require_auth();
    let acc_reward_per_share = update_pool(env, &config)?;
    settle_user(env, &user, acc_reward_per_share)?;
    let amount = read_i128(env, &FarmingStorageKey::AccruedRewards(user.clone()));
    if amount > 0 {
        write_i128(env, &FarmingStorageKey::AccruedRewards(user.clone()), 0);
        token::Client::new(env, &config.reward_token).transfer(
            &env.current_contract_address(),
            &user,
            &amount,
        );
    }
    Ok(amount)
}

pub fn exit(env: &Env, user: Address) -> Result<(i128, i128), ContractError> {
    let config = load_config(env)?;
    user.require_auth();
    let acc_reward_per_share = update_pool(env, &config)?;
    settle_user(env, &user, acc_reward_per_share)?;
    let reward = read_i128(env, &FarmingStorageKey::AccruedRewards(user.clone()));
    let shares = read_i128(env, &FarmingStorageKey::Shares(user.clone()));
    if reward > 0 {
        write_i128(env, &FarmingStorageKey::AccruedRewards(user.clone()), 0);
        token::Client::new(env, &config.reward_token).transfer(
            &env.current_contract_address(),
            &user,
            &reward,
        );
    }
    if shares > 0 {
        write_i128(env, &FarmingStorageKey::Shares(user.clone()), 0);
        write_i128(
            env,
            &FarmingStorageKey::RewardDebt(user.clone()),
            0,
        );
        env.storage().instance().set(
            &FarmingStorageKey::TotalShares,
            &total_shares(env).checked_sub(shares).ok_or(ContractError::MathOverflow)?,
        );
        token::Client::new(env, &config.lp_token).transfer(
            &env.current_contract_address(),
            &user,
            &shares,
        );
    }
    Ok((shares, reward))
}

/// Emergency unstake: return the full LP position immediately, bypassing the
/// reward distribution math entirely.
///
/// Unlike [`exit`], this does not settle or credit any pending rewards — any
/// yield accrued up to this ledger is forfeited back to the farm reward pool.
/// The user's staked-balance mapping (and reward bookkeeping) is zeroed so no
/// further rewards can be claimed on the position.
pub fn emergency_withdraw(env: &Env, user: Address) -> Result<i128, ContractError> {
    let config = load_config(env)?;
    user.require_auth();
    let shares = read_i128(env, &FarmingStorageKey::Shares(user.clone()));
    if shares <= 0 {
        return Err(ContractError::InvalidStakeAmount);
    }
    write_i128(env, &FarmingStorageKey::Shares(user.clone()), 0);
    write_i128(env, &FarmingStorageKey::RewardDebt(user.clone()), 0);
    write_i128(env, &FarmingStorageKey::AccruedRewards(user.clone()), 0);
    env.storage().instance().set(
        &FarmingStorageKey::TotalShares,
        &total_shares(env).checked_sub(shares).ok_or(ContractError::MathOverflow)?,
    );
    token::Client::new(env, &config.lp_token).transfer(
        &env.current_contract_address(),
        &user,
        &shares,
    );
    Ok(shares)
}

pub fn set_emission_multiplier(
    env: &Env,
    governance: Address,
    multiplier: u32,
) -> Result<FarmingConfig, ContractError> {
    let mut config = load_config(env)?;
    if config.admin != governance {
        return Err(ContractError::NotAdmin);
    }
    governance.require_auth();
    if multiplier > MAX_EMISSION_MULTIPLIER {
        return Err(ContractError::FeeCeilingExceeded);
    }
    update_pool(env, &config)?;
    config.emission_multiplier = multiplier;
    env.storage().instance().set(&FarmingStorageKey::Config, &config);
    Ok(config)
}

pub fn get_config(env: &Env) -> Option<FarmingConfig> {
    env.storage().instance().get(&FarmingStorageKey::Config)
}

pub fn get_share_balance(env: &Env, user: Address) -> i128 {
    read_i128(env, &FarmingStorageKey::Shares(user))
}

pub fn pending_rewards(env: &Env, user: Address) -> Result<i128, ContractError> {
    let config = load_config(env)?;
    let acc_reward_per_share = update_pool(env, &config)?;
    let shares = read_i128(env, &FarmingStorageKey::Shares(user.clone()));
    let reward_debt = read_i128(env, &FarmingStorageKey::RewardDebt(user.clone()));
    let accrued = shares
        .checked_mul(acc_reward_per_share)
        .ok_or(ContractError::MathOverflow)?
        .checked_div(REWARD_PRECISION)
        .ok_or(ContractError::DivisionByZero)?;
    Ok(read_i128(env, &FarmingStorageKey::AccruedRewards(user)).checked_add(
        accrued.checked_sub(reward_debt).ok_or(ContractError::MathOverflow)?,
    ).ok_or(ContractError::MathOverflow)?)
}
