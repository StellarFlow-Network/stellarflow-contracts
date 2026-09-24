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
    load_config(env)?;
    user.require_auth();
    stake_preauthorized(env, user, amount)
}

/// [`stake`] without the `require_auth` call.
///
/// Soroban allows only one `require_auth` per address per invocation frame —
/// a second one aborts the host rather than returning an error. Callers that
/// have already asserted `user`'s authorization earlier in the same frame
/// (see [`crate::vaults::harvest_compound::harvest_and_compound`], where
/// [`claim_rewards`] does it) must use this entry point instead of [`stake`].
///
/// Not exported as a contract entry point: reaching it always requires having
/// authorized `user` first.
pub(crate) fn stake_preauthorized(
    env: &Env,
    user: Address,
    amount: i128,
) -> Result<i128, ContractError> {
    if amount <= 0 {
        return Err(ContractError::InvalidStakeAmount);
    }
    let config = load_config(env)?;
    let acc_reward_per_share = update_pool(env, &config)?;
    settle_user(env, &user, acc_reward_per_share)?;
    token::Client::new(env, &config.lp_token).transfer(
        &user,
        &env.current_contract_address(),
        &amount,
    );
    let shares = read_i128(env, &FarmingStorageKey::Shares(user.clone()));
    let new_shares = shares.checked_add(amount).ok_or(ContractError::MathOverflow)?;
    write_i128(env, &FarmingStorageKey::Shares(user.clone()), new_shares);
    // Re-baseline the reward debt against the *post-stake* share count.
    // `settle_user` above set it from the old count, so without this the newly
    // staked `amount` carries zero debt and the next settle credits it with
    // `amount * acc_reward_per_share / REWARD_PRECISION` — the pool's entire
    // accrued history, paid out on a stake that has earned nothing yet.
    write_i128(
        env,
        &FarmingStorageKey::RewardDebt(user.clone()),
        new_shares
            .checked_mul(acc_reward_per_share)
            .ok_or(ContractError::MathOverflow)?
            .checked_div(REWARD_PRECISION)
            .ok_or(ContractError::DivisionByZero)?,
    );
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

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger};

    const STAKE: i128 = 1_000_000;
    const EMISSION: i128 = 100;
    const LEDGERS: u32 = 10;

    struct Farm {
        env: Env,
        client: crate::TimeLockedUpgradeContractClient<'static>,
        lp_token: Address,
    }

    fn mint(env: &Env, asset: &Address, to: &Address, amount: i128) {
        token::StellarAssetClient::new(env, asset).mint(to, &amount);
    }

    /// Editing the live `LedgerInfo` rather than building a fresh one keeps the
    /// entry-TTL policy that the registered Stellar-asset contracts were
    /// written under; resetting those fields makes later token calls fail with
    /// `Error(Context, InternalError)` on soroban-sdk 20.0.0.
    fn advance_ledgers(env: &Env, count: u32) {
        let mut info = env.ledger().get();
        info.sequence_number += count;
        info.timestamp += 5 * count as u64;
        env.ledger().set(info);
    }

    fn setup() -> Farm {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let client = crate::TimeLockedUpgradeContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        client.initialize(&admin, &treasury);

        let issuer = Address::generate(&env);
        let lp_token = env.register_stellar_asset_contract(issuer.clone());
        let reward_token = env.register_stellar_asset_contract(issuer);
        client.init_yield_farming(&admin, &lp_token, &reward_token, &EMISSION);

        let funder = Address::generate(&env);
        mint(&env, &reward_token, &funder, 1_000_000);
        client.fund_yield_rewards(&funder, &1_000_000);

        Farm {
            env,
            client,
            lp_token,
        }
    }

    fn new_staker(f: &Farm, amount: i128) -> Address {
        let who = Address::generate(&f.env);
        mint(&f.env, &f.lp_token, &who, amount);
        f.client.stake_lp(&who, &amount);
        who
    }

    /// Regression test: `stake` used to leave `RewardDebt` at the pre-stake
    /// share count, so a staker entering a pool that had already accrued
    /// `acc_reward_per_share` was immediately credited
    /// `amount * acc / REWARD_PRECISION` — the pool's whole history — and could
    /// drain the reward pot by staking and claiming in one ledger.
    #[test]
    fn staking_into_a_mature_pool_earns_nothing_for_earlier_ledgers() {
        let f = setup();
        let alice = new_staker(&f, STAKE);
        advance_ledgers(&f.env, LEDGERS);
        assert_eq!(
            f.client.pending_yield_rewards(&alice),
            EMISSION * LEDGERS as i128,
            "the sole staker should hold every emission so far"
        );

        // Bob joins now and claims in the same ledger: zero time in the pool.
        let bob = new_staker(&f, STAKE);
        assert_eq!(
            f.client.pending_yield_rewards(&bob),
            0,
            "a staker with zero ledgers in the pool is owed zero"
        );
        assert_eq!(f.client.claim_rewards(&bob), 0);

        // Alice's entitlement is untouched by Bob's arrival.
        assert_eq!(
            f.client.pending_yield_rewards(&alice),
            EMISSION * LEDGERS as i128
        );
    }

    #[test]
    fn emissions_split_by_share_weight_after_a_second_staker_joins() {
        let f = setup();
        let alice = new_staker(&f, STAKE);
        advance_ledgers(&f.env, LEDGERS);
        let bob = new_staker(&f, STAKE);

        let alice_before = f.client.pending_yield_rewards(&alice);
        advance_ledgers(&f.env, LEDGERS);

        // Equal stakes, so the emissions of the second window split evenly.
        let window = EMISSION * LEDGERS as i128;
        assert_eq!(
            f.client.pending_yield_rewards(&bob),
            window / 2,
            "bob earns only his half of the window he was present for"
        );
        assert_eq!(
            f.client.pending_yield_rewards(&alice) - alice_before,
            window / 2
        );
    }

    #[test]
    fn topping_up_a_stake_does_not_mint_phantom_rewards() {
        let f = setup();
        let alice = new_staker(&f, STAKE);
        advance_ledgers(&f.env, LEDGERS);

        let before = f.client.pending_yield_rewards(&alice);
        mint(&f.env, &f.lp_token, &alice, STAKE);
        f.client.stake_lp(&alice, &STAKE);

        assert_eq!(
            f.client.pending_yield_rewards(&alice),
            before,
            "doubling the stake must not retroactively pay on the new half"
        );
        assert_eq!(f.client.yield_farming_share_balance(&alice), STAKE * 2);
    }
}
