//! Auto-compounding liquidity vault strategy engine (Issue #694).
//!
//! Depositors receive pro-rata `sfvToken` vault shares against a single
//! underlying asset. Any party ("keeper bot") may call [`harvest`] to feed
//! externally-collected yield back into the vault: a configurable protocol
//! performance fee (2% by default) is skimmed to the fee recipient and the
//! remainder is folded into `total_assets`, which raises the value of every
//! existing share without minting new ones — the auto-compounding step.
//!
//! Harvest is safe to leave permissionless because the yield amount is only
//! ever *pulled* from the caller's own token balance via `transfer`; a
//! keeper can only ever add value to the vault, never fabricate or drain it.

use soroban_sdk::{contracttype, token, Address, Env, IntoVal};

use crate::ContractError;

/// Denominator for basis-point fee math (10_000 bps == 100%).
pub const BPS_DENOMINATOR: i128 = 10_000;

/// Protocol default performance fee: 2%.
pub const DEFAULT_PERFORMANCE_FEE_BPS: u32 = 200;

/// Hard ceiling on the configurable performance fee: 20%.
pub const MAX_PERFORMANCE_FEE_BPS: u32 = 2_000;

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct VaultConfig {
    pub admin: Address,
    pub asset: Address,
    pub fee_bps: u32,
    pub fee_recipient: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VaultStorageKey {
    Config,
    TotalShares,
    TotalAssets,
    /// Per-holder `sfvToken` share balance.
    Shares(Address),
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct HarvestResult {
    pub gross_yield: i128,
    pub performance_fee: i128,
    pub compounded: i128,
    pub total_assets: i128,
}

/// Lend vault assets to `borrower` for the duration of one transaction.
/// The borrower must implement `on_flash_loan(asset, amount, fee)` and return
/// at least the principal plus the protocol fee before the callback returns.
pub fn flash_loan(env: &Env, borrower: Address, amount: i128) -> Result<i128, ContractError> {
    if amount <= 0 {
        return Err(ContractError::VaultZeroAmount);
    }
    let config = load_config(env)?;
    let token_client = token::Client::new(env, &config.asset);
    let initial_balance = token_client.balance(&env.current_contract_address());
    let fee = amount
        .checked_mul(i128::from(config.fee_bps))
        .ok_or(ContractError::MathOverflow)?
        .checked_div(BPS_DENOMINATOR)
        .ok_or(ContractError::DivisionByZero)?;
    let required_balance = initial_balance
        .checked_add(fee)
        .ok_or(ContractError::MathOverflow)?;

    token_client.transfer(&env.current_contract_address(), &borrower, &amount);
    env.invoke_contract::<()>(
        &borrower,
        &soroban_sdk::Symbol::new(env, "on_flash_loan"),
        soroban_sdk::vec![
            env,
            config.asset.into_val(env),
            amount.into_val(env),
            fee.into_val(env),
        ],
    );

    let final_balance = token_client.balance(&env.current_contract_address());
    assert!(
        final_balance >= required_balance,
        "Flash loan repayment incomplete: final={}, required={}",
        final_balance,
        required_balance
    );

    Ok(fee)
}

fn load_config(env: &Env) -> Result<VaultConfig, ContractError> {
    env.storage()
        .instance()
        .get(&VaultStorageKey::Config)
        .ok_or(ContractError::VaultNotInitialized)
}

fn total_shares(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&VaultStorageKey::TotalShares)
        .unwrap_or(0i128)
}

fn total_assets(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&VaultStorageKey::TotalAssets)
        .unwrap_or(0i128)
}

fn share_balance_of(env: &Env, holder: &Address) -> i128 {
    let key = VaultStorageKey::Shares(holder.clone());
    env.storage().persistent().get(&key).unwrap_or(0i128)
}

fn set_share_balance(env: &Env, holder: &Address, balance: i128) {
    let key = VaultStorageKey::Shares(holder.clone());
    if balance == 0 {
        env.storage().persistent().remove(&key);
    } else {
        env.storage().persistent().set(&key, &balance);
        env.storage().persistent().extend_ttl(
            &key,
            crate::storage::PERSISTENT_TTL_THRESHOLD,
            crate::storage::PERSISTENT_TTL_THRESHOLD,
        );
    }
}

/// Invariant check: assert token reserves exactly match internal balance ledger.
/// Panics immediately if any drift is detected between actual contract balance
/// and the internally tracked TotalAssets.
fn assert_balance_invariant(env: &Env, config: &VaultConfig) {
    let token_client = token::Client::new(env, &config.asset);
    let actual_balance = token_client.balance(&env.current_contract_address());
    let tracked_assets = total_assets(env);
    
    assert_eq!(
        actual_balance,
        tracked_assets,
        "Balance invariant violated: actual={}, tracked_assets={}",
        actual_balance,
        tracked_assets
    );
}

/// One-time vault setup. `admin` governs the performance fee configuration;
/// it is independent of the main contract's admin so a vault can be deployed
/// and re-parented without touching protocol-wide administration.
pub fn initialize(
    env: &Env,
    admin: Address,
    asset: Address,
    fee_recipient: Address,
) -> Result<VaultConfig, ContractError> {
    if env.storage().instance().has(&VaultStorageKey::Config) {
        return Err(ContractError::VaultAlreadyInitialized);
    }
    admin.require_auth();

    let config = VaultConfig {
        admin,
        asset,
        fee_bps: DEFAULT_PERFORMANCE_FEE_BPS,
        fee_recipient,
    };
    env.storage().instance().set(&VaultStorageKey::Config, &config);
    env.storage().instance().set(&VaultStorageKey::TotalShares, &0i128);
    env.storage().instance().set(&VaultStorageKey::TotalAssets, &0i128);
    Ok(config)
}

/// Update the protocol performance fee. Admin-only, capped at
/// [`MAX_PERFORMANCE_FEE_BPS`] so misconfiguration can never confiscate more
/// than 20% of harvested yield.
pub fn set_performance_fee(env: &Env, admin: Address, fee_bps: u32) -> Result<VaultConfig, ContractError> {
    let mut config = load_config(env)?;
    if config.admin != admin {
        return Err(ContractError::NotAdmin);
    }
    admin.require_auth();
    if fee_bps > MAX_PERFORMANCE_FEE_BPS {
        return Err(ContractError::VaultInvalidPerformanceFee);
    }
    config.fee_bps = fee_bps;
    env.storage().instance().set(&VaultStorageKey::Config, &config);
    Ok(config)
}

/// Deposit `amount` of the underlying asset and mint pro-rata `sfvToken`
/// shares. The first depositor mints 1:1.
pub fn deposit(env: &Env, depositor: Address, amount: i128) -> Result<i128, ContractError> {
    if amount <= 0 {
        return Err(ContractError::VaultZeroAmount);
    }
    let config = load_config(env)?;
    depositor.require_auth();

    // Invariant check: verify balance consistency before state change
    assert_balance_invariant(env, &config);

    let assets = total_assets(env);
    let shares = total_shares(env);

    let minted = if shares == 0 || assets == 0 {
        amount
    } else {
        amount
            .checked_mul(shares)
            .ok_or(ContractError::MathOverflow)?
            .checked_div(assets)
            .ok_or(ContractError::DivisionByZero)?
    };
    if minted <= 0 {
        return Err(ContractError::VaultZeroAmount);
    }

    let token_client = token::Client::new(env, &config.asset);
    token_client.transfer(&depositor, &env.current_contract_address(), &amount);

    let new_assets = assets.checked_add(amount).ok_or(ContractError::MathOverflow)?;
    let new_shares = shares.checked_add(minted).ok_or(ContractError::MathOverflow)?;
    env.storage().instance().set(&VaultStorageKey::TotalAssets, &new_assets);
    env.storage().instance().set(&VaultStorageKey::TotalShares, &new_shares);

    let holder_balance = share_balance_of(env, &depositor);
    set_share_balance(
        env,
        &depositor,
        holder_balance.checked_add(minted).ok_or(ContractError::MathOverflow)?,
    );

    // Invariant check: verify balance consistency after state change
    assert_balance_invariant(env, &config);

    Ok(minted)
}

/// Burn `shares` and withdraw the pro-rata amount of underlying asset.
pub fn withdraw(env: &Env, owner: Address, shares: i128) -> Result<i128, ContractError> {
    if shares <= 0 {

        return Err(ContractError::VaultZeroAmount);
    }
    let config = load_config(env)?;
    owner.require_auth();

    // Invariant check: verify balance consistency before state change
    assert_balance_invariant(env, &config);

    let holder_balance = share_balance_of(env, &owner);
    if shares > holder_balance {
        return Err(ContractError::VaultInsufficientShares);
    }

    let assets = total_assets(env);
    let total_share_supply = total_shares(env);
    if total_share_supply == 0 {
        return Err(ContractError::VaultInsufficientShares);
    }

    let owed = shares
        .checked_mul(assets)
        .ok_or(ContractError::MathOverflow)?
        .checked_div(total_share_supply)
        .ok_or(ContractError::DivisionByZero)?;
    if owed <= 0 || owed > assets {
        return Err(ContractError::VaultInsufficientBalance);
    }

    set_share_balance(env, &owner, holder_balance - shares);
    env.storage()
        .instance()
        .set(&VaultStorageKey::TotalShares, &(total_share_supply - shares));
    env.storage()
        .instance()
        .set(&VaultStorageKey::TotalAssets, &(assets - owed));

    let token_client = token::Client::new(env, &config.asset);
    token_client.transfer(&env.current_contract_address(), &owner, &owed);

    // Invariant check: verify balance consistency after state change
    assert_balance_invariant(env, &config);

    Ok(owed)
}

/// Keeper-facing harvest entrypoint. Anyone may call this — the caller must
/// hold (and authorize the transfer of) `yield_amount` of the underlying
/// asset, which is pulled into the vault. A performance fee is skimmed to
/// the configured fee recipient and the remainder compounds into
/// `total_assets`, raising the value of every outstanding `sfvToken` share.
pub fn harvest(env: &Env, keeper: Address, yield_amount: i128) -> Result<HarvestResult, ContractError> {
    if yield_amount <= 0 {
        return Err(ContractError::VaultZeroAmount);
    }
    let config = load_config(env)?;
    keeper.require_auth();

    // Invariant check: verify balance consistency before state change
    assert_balance_invariant(env, &config);

    let token_client = token::Client::new(env, &config.asset);
    token_client.transfer(&keeper, &env.current_contract_address(), &yield_amount);

    let fee = yield_amount
        .checked_mul(i128::from(config.fee_bps))
        .ok_or(ContractError::MathOverflow)?
        .checked_div(BPS_DENOMINATOR)
        .ok_or(ContractError::DivisionByZero)?;
    let compounded = yield_amount.checked_sub(fee).ok_or(ContractError::MathOverflow)?;

    if fee > 0 {
        token_client.transfer(&env.current_contract_address(), &config.fee_recipient, &fee);
    }

    let assets = total_assets(env);
    let new_assets = assets.checked_add(compounded).ok_or(ContractError::MathOverflow)?;
    env.storage().instance().set(&VaultStorageKey::TotalAssets, &new_assets);

    env.events().publish(
        (soroban_sdk::symbol_short!("harvest"), keeper),
        (yield_amount, fee, compounded, new_assets),
    );

    // Invariant check: verify balance consistency after state change
    assert_balance_invariant(env, &config);

    Ok(HarvestResult {
        gross_yield: yield_amount,
        performance_fee: fee,
        compounded,
        total_assets: new_assets,
    })
}

pub fn get_config(env: &Env) -> Option<VaultConfig> {
    env.storage().instance().get(&VaultStorageKey::Config)
}

pub fn get_total_assets(env: &Env) -> i128 {
    total_assets(env)
}

pub fn get_total_shares(env: &Env) -> i128 {
    total_shares(env)
}

pub fn get_share_balance(env: &Env, holder: Address) -> i128 {
    share_balance_of(env, &holder)
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    fn setup() -> (
        Env,
        crate::TimeLockedUpgradeContractClient<'static>,
        Address,
        Address,
        Address,
    ) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let client = crate::TimeLockedUpgradeContractClient::new(&env, &contract_id);
        let vault_admin = Address::generate(&env);
        let fee_recipient = Address::generate(&env);
        let asset_issuer = Address::generate(&env);
        let asset = env.register_stellar_asset_contract(asset_issuer);
        (env, client, vault_admin, fee_recipient, asset)
    }

    fn mint(env: &Env, asset: &Address, to: &Address, amount: i128) {
        soroban_sdk::token::StellarAssetClient::new(env, asset).mint(to, &amount);
    }

    #[test]
    fn first_deposit_mints_shares_one_to_one() {
        let (env, client, vault_admin, fee_recipient, asset) = setup();
        client.init_vault(&vault_admin, &asset, &fee_recipient);
        let depositor = Address::generate(&env);
        mint(&env, &asset, &depositor, 1_000);
        let minted = client.vault_deposit(&depositor, &1_000);
        assert_eq!(minted, 1_000);
        assert_eq!(client.vault_share_balance(&depositor), 1_000);
    }

    #[test]
    fn harvest_deducts_default_two_percent_fee_and_compounds_rest() {
        let (env, client, vault_admin, fee_recipient, asset) = setup();
        client.init_vault(&vault_admin, &asset, &fee_recipient);
        let depositor = Address::generate(&env);
        mint(&env, &asset, &depositor, 10_000);
        client.vault_deposit(&depositor, &10_000);

        let keeper = Address::generate(&env);
        mint(&env, &asset, &keeper, 1_000);
        let result = client.vault_harvest(&keeper, &1_000);

        assert_eq!(result.gross_yield, 1_000);
        assert_eq!(result.performance_fee, 20); // 2% of 1000
        assert_eq!(result.compounded, 980);
        assert_eq!(client.vault_total_assets(), 10_980);

        let token_client = soroban_sdk::token::Client::new(&env, &asset);
        assert_eq!(token_client.balance(&fee_recipient), 20);
    }

    #[test]
    fn harvest_raises_share_price_for_existing_holders() {
        let (env, client, vault_admin, fee_recipient, asset) = setup();
        client.init_vault(&vault_admin, &asset, &fee_recipient);
        let depositor = Address::generate(&env);
        mint(&env, &asset, &depositor, 1_000);
        client.vault_deposit(&depositor, &1_000);

        let keeper = Address::generate(&env);
        mint(&env, &asset, &keeper, 1_000);
        client.vault_harvest(&keeper, &1_000);

        // Same share count now redeems for more underlying asset.
        let withdrawn = client.vault_withdraw(&depositor, &1_000);
        assert_eq!(withdrawn, 1_980);
    }

    #[test]
    fn set_performance_fee_rejects_above_cap() {
        let (env, client, vault_admin, fee_recipient, asset) = setup();
        client.init_vault(&vault_admin, &asset, &fee_recipient);
        let _ = env;
        let result = client.try_set_vault_performance_fee(&vault_admin, &(MAX_PERFORMANCE_FEE_BPS + 1));
        assert_eq!(result, Err(Ok(ContractError::VaultInvalidPerformanceFee)));
    }

    #[test]
    fn withdraw_rejects_more_shares_than_held() {
        let (env, client, vault_admin, fee_recipient, asset) = setup();
        client.init_vault(&vault_admin, &asset, &fee_recipient);
        let depositor = Address::generate(&env);
        mint(&env, &asset, &depositor, 500);
        client.vault_deposit(&depositor, &500);

        let result = client.try_vault_withdraw(&depositor, &600);
        assert_eq!(result, Err(Ok(ContractError::VaultInsufficientShares)));
    }

    #[test]
    fn non_admin_cannot_change_performance_fee() {
        let (env, client, vault_admin, fee_recipient, asset) = setup();
        client.init_vault(&vault_admin, &asset, &fee_recipient);
        let attacker = Address::generate(&env);
        let result = client.try_set_vault_performance_fee(&attacker, &500);
        assert_eq!(result, Err(Ok(ContractError::NotAdmin)));
    }
}
