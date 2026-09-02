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

/// Scaling factor for share value precision.
pub const SHARE_VALUE_PRECISION: i128 = 1_000_000_000_000_000_000;

/// Maximum allowed drawdown from peak share value in basis points (1000 bps = 10%).
pub const MAX_DRAWDOWN_BPS: u32 = 1_000;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VaultStorageKey {
    Config,
    TotalShares,
    TotalAssets,
    /// Per-holder `sfvToken` share balance.
    Shares(Address),
    /// Peak vault share value tracked in persistent storage.
    PeakShareValue,
    /// Circuit breaker drawdown state (true if triggered).
    DrawdownState,
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

/// Calculates current share value scaled by `SHARE_VALUE_PRECISION`.
/// Returns `None` if `total_shares` is 0.
pub fn calculate_share_value(total_assets: i128, total_shares: i128) -> Option<i128> {
    if total_shares <= 0 {
        return None;
    }
    total_assets
        .checked_mul(SHARE_VALUE_PRECISION)?
        .checked_div(total_shares)
}

/// Reads the peak vault share value stored in persistent storage.
pub fn get_peak_share_value(env: &Env) -> i128 {
    env.storage()
        .persistent()
        .get(&VaultStorageKey::PeakShareValue)
        .unwrap_or(0i128)
}

/// Returns `true` if the drawdown circuit breaker has been triggered.
pub fn is_circuit_breaker_triggered(env: &Env) -> bool {
    env.storage()
        .persistent()
        .get(&VaultStorageKey::DrawdownState)
        .unwrap_or(false)
}

/// Updates the peak vault share value in persistent storage if `current_val` exceeds the previous peak.
pub fn update_peak_share_value(env: &Env, current_val: i128) {
    if current_val <= 0 {
        return;
    }
    let peak = get_peak_share_value(env);
    if current_val > peak {
        let key = VaultStorageKey::PeakShareValue;
        env.storage().persistent().set(&key, &current_val);
        env.storage().persistent().extend_ttl(
            &key,
            crate::storage::PERSISTENT_TTL_THRESHOLD,
            crate::storage::PERSISTENT_TTL_THRESHOLD,
        );
    }
}

/// Transitions vault to emergency withdrawal state and triggers circuit breaker.
pub fn trigger_circuit_breaker(env: &Env) {
    let key = VaultStorageKey::DrawdownState;
    env.storage().persistent().set(&key, &true);
    env.storage().persistent().extend_ttl(
        &key,
        crate::storage::PERSISTENT_TTL_THRESHOLD,
        crate::storage::PERSISTENT_TTL_THRESHOLD,
    );

    // Pause vault and mark emergency withdrawal state
    env.storage().instance().set(&crate::vaults::pause_guard::VAULT_PAUSED_KEY, &true);
    env.storage().instance().set(&crate::vaults::pause_guard::EMRG_WD_KEY, &true);

    env.events().publish(
        (soroban_sdk::symbol_short!("circuit"), soroban_sdk::symbol_short!("drawdown")),
        env.ledger().timestamp(),
    );
}

/// Checks if the current share value reflects >10% drawdown from the peak share value.
/// If triggered, transitions the vault to emergency withdrawal state and returns Ok(true).
pub fn check_and_trigger_circuit_breaker(env: &Env) -> Result<bool, ContractError> {
    if is_circuit_breaker_triggered(env) {
        return Ok(true);
    }

    let shares = total_shares(env);
    let assets = total_assets(env);
    let peak = get_peak_share_value(env);

    if shares > 0 && peak > 0 {
        if let Some(current_val) = calculate_share_value(assets, shares) {
            if current_val < peak {
                let loss = peak.checked_sub(current_val).ok_or(ContractError::MathOverflow)?;
                let loss_bps = loss
                    .checked_mul(BPS_DENOMINATOR)
                    .ok_or(ContractError::MathOverflow)?
                    .checked_div(peak)
                    .ok_or(ContractError::DivisionByZero)?;

                if loss_bps > i128::from(MAX_DRAWDOWN_BPS) {
                    trigger_circuit_breaker(env);
                    return Ok(true);
                }
            }
        }
    }

    Ok(false)
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
    // No reentrancy guard here: the `vault_deposit` entry point in `lib.rs`
    // already holds it for the whole call, and the lock is not re-entrant, so
    // taking it twice made every deposit fail with `ReentrancyDetected`.
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

    if let Some(share_val) = calculate_share_value(new_assets, new_shares) {
        update_peak_share_value(env, share_val);
    }

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
    // See `deposit`: the guard belongs to the `vault_withdraw` entry point.
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
    crate::vaults::pause_guard::require_vault_not_paused(env)?;
    if yield_amount <= 0 {
        return Err(ContractError::VaultZeroAmount);
    }
    let config = load_config(env)?;
    keeper.require_auth();

    // Circuit breaker check: revert harvest and transition to emergency state if drawdown > 10%
    if check_and_trigger_circuit_breaker(env)? {
        return Err(ContractError::VaultMaxDrawdownExceeded);
    }

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

    let shares = total_shares(env);
    if let Some(new_share_val) = calculate_share_value(new_assets, shares) {
        update_peak_share_value(env, new_share_val);
    }

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

    #[test]
    fn peak_share_value_tracked_in_persistent_storage() {
        let (env, client, vault_admin, fee_recipient, asset) = setup();
        client.init_vault(&vault_admin, &asset, &fee_recipient);
        let depositor = Address::generate(&env);
        mint(&env, &asset, &depositor, 10_000);

        // Initial deposit
        client.vault_deposit(&depositor, &10_000);
        let initial_peak = client.vault_peak_share_value();
        assert_eq!(initial_peak, SHARE_VALUE_PRECISION);

        // Harvest increases share value and updates peak
        let keeper = Address::generate(&env);
        mint(&env, &asset, &keeper, 2_000);
        client.vault_harvest(&keeper, &2_000);

        let new_peak = client.vault_peak_share_value();
        assert!(new_peak > initial_peak);
        assert_eq!(new_peak, 1_196_000_000_000_000_000); // (10000 + 1960) * 1e18 / 10000
    }

    #[test]
    fn harvest_reverts_if_drawdown_exceeds_ten_percent() {
        let (env, client, vault_admin, fee_recipient, asset) = setup();
        client.init_vault(&vault_admin, &asset, &fee_recipient);
        let depositor = Address::generate(&env);
        mint(&env, &asset, &depositor, 10_000);
        client.vault_deposit(&depositor, &10_000);

        // Grow vault share price via harvest
        let keeper = Address::generate(&env);
        mint(&env, &asset, &keeper, 10_000);
        client.vault_harvest(&keeper, &10_000);

        // Peak share price is now ~1.98e18
        let peak = client.vault_peak_share_value();
        assert!(peak > 1_900_000_000_000_000_000);

        // Simulate strategy loss: reduce TotalAssets to 15,000 (drawdown ~24% > 10%)
        env.as_contract(&client.address, || {
            env.storage().instance().set(&VaultStorageKey::TotalAssets, &15_000i128);
        });

        // Harvest must be rejected due to maximum drawdown exceeded
        mint(&env, &asset, &keeper, 500);
        let res = client.try_vault_harvest(&keeper, &500);
        assert!(res.is_err());
        assert_eq!(client.vault_is_circuit_breaker_triggered(), true);
    }

    #[test]
    fn circuit_breaker_transitions_vault_to_emergency_withdrawal_state() {
        let (env, client, vault_admin, fee_recipient, asset) = setup();
        client.init_vault(&vault_admin, &asset, &fee_recipient);
        let depositor = Address::generate(&env);
        mint(&env, &asset, &depositor, 10_000);
        client.vault_deposit(&depositor, &10_000);

        // Set peak share price to 2.0e18
        env.as_contract(&client.address, || {
            env.storage().persistent().set(&VaultStorageKey::PeakShareValue, &(2 * SHARE_VALUE_PRECISION));
        });

        // Current share price is 1.0e18 (50% loss from peak)
        let triggered = client.vault_check_circuit_breaker().unwrap();
        assert_eq!(triggered, true);
        assert_eq!(client.vault_is_circuit_breaker_triggered(), true);

        // Vault is now paused & in emergency withdrawal mode
        env.as_contract(&client.address, || {
            assert_eq!(crate::vaults::pause_guard::is_vault_paused(&env), true);
            assert_eq!(crate::vaults::pause_guard::is_emergency_withdrawal_active(&env), true);
        });
    }
}
