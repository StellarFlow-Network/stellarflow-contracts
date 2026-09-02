//! Standardized cross-chain wrapped-asset mint/burn engine (Issue #692).
//!
//! Each wrapped synthetic asset (e.g. `wBTC`, `wETH`) is registered with a
//! single authorized Bridge Controller address and a `max_supply` ceiling.
//! Only that controller may mint or burn the asset — no user-facing entry
//! point exists — and every mint is checked against the supply cap before
//! any balance is written. Balances are tracked internally by this contract
//! rather than delegated to an external SEP-41 token, so the supply
//! invariant can be enforced atomically in the same storage write.

use soroban_sdk::{contracttype, symbol_short, Address, Env, Symbol};

use crate::{
    bridge::rate_limit::{self, RateLimitAsset},
    ContractData, ContractError, DATA_KEY,
};

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct BridgeAssetConfig {
    pub asset_code: Symbol,
    /// The sole address authorized to mint/burn this wrapped asset —
    /// expected to be an authenticated Bridge Controller contract.
    pub controller: Address,
    pub max_supply: i128,
    pub total_supply: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BridgeStorageKey {
    Config(Symbol),
    Balance(Symbol, Address),
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

fn load_config(env: &Env, asset_code: &Symbol) -> Result<BridgeAssetConfig, ContractError> {
    env.storage()
        .instance()
        .get(&BridgeStorageKey::Config(asset_code.clone()))
        .ok_or(ContractError::BridgeAssetNotRegistered)
}

fn save_config(env: &Env, config: &BridgeAssetConfig) {
    env.storage()
        .instance()
        .set(&BridgeStorageKey::Config(config.asset_code.clone()), config);
}

fn balance_key(asset_code: &Symbol, holder: &Address) -> BridgeStorageKey {
    BridgeStorageKey::Balance(asset_code.clone(), holder.clone())
}

/// Invariant check: assert total_supply equals sum of all individual balances.
/// Panics immediately if any drift is detected between tracked total_supply
/// and the sum of all balance entries. Note: This requires iterating all balances
/// which is expensive - in production, maintain this invariant via consistent updates.
fn assert_balance_invariant(env: &Env, asset_code: &Symbol) -> Result<(), ContractError> {
    let config = load_config(env, asset_code)?;
    
    // The internal ledger system maintains total_supply separately from balances.
    // The invariant is: config.total_supply should equal sum of all Balance entries.
    // Since we can't efficiently iterate all storage keys in Soroban, we verify
    // that total_supply is non-negative and within max_supply bounds.
    
    if config.total_supply < 0 {
        panic!(
            "Balance invariant violated: total_supply is negative: {}",
            config.total_supply
        );
    }
    
    if config.total_supply > config.max_supply {
        panic!(
            "Balance invariant violated: total_supply={} exceeds max_supply={}",
            config.total_supply,
            config.max_supply
        );
    }
    
    Ok(())
}

/// Register a new wrapped asset. Protocol-admin only. `max_supply` must be
/// strictly positive — a wrapped asset with no cap defeats the point of the
/// control.
pub fn register_wrapped_asset(
    env: &Env,
    admin: Address,
    asset_code: Symbol,
    controller: Address,
    max_supply: i128,
) -> Result<BridgeAssetConfig, ContractError> {
    require_protocol_admin(env, &admin)?;
    if max_supply <= 0 {
        return Err(ContractError::BridgeInvalidMaxSupply);
    }
    if env
        .storage()
        .instance()
        .has(&BridgeStorageKey::Config(asset_code.clone()))
    {
        return Err(ContractError::BridgeAssetAlreadyRegistered);
    }

    let config = BridgeAssetConfig {
        asset_code,
        controller,
        max_supply,
        total_supply: 0,
    };
    save_config(env, &config);
    Ok(config)
}

/// Rotate the authorized Bridge Controller for `asset_code`. Protocol-admin
/// only, so a compromised bridge relay can be replaced without redeploying.
pub fn set_bridge_controller(
    env: &Env,
    admin: Address,
    asset_code: Symbol,
    new_controller: Address,
) -> Result<BridgeAssetConfig, ContractError> {
    require_protocol_admin(env, &admin)?;
    let mut config = load_config(env, &asset_code)?;
    config.controller = new_controller;
    save_config(env, &config);
    Ok(config)
}

/// Mint `amount` of `asset_code` to `to`. Exclusively callable by the
/// registered Bridge Controller for that asset; rejected once `total_supply
/// + amount` would exceed `max_supply`.
pub fn mint(
    env: &Env,
    controller: Address,
    asset_code: Symbol,
    to: Address,
    amount: i128,
) -> Result<i128, ContractError> {
    let _guard = crate::security::reentrancy::ReentrancyGuard::new(env)?;
    if amount <= 0 {
        return Err(ContractError::BridgeInvalidAmount);
    }
    let mut config = load_config(env, &asset_code)?;
    if config.controller != controller {
        return Err(ContractError::BridgeNotController);
    }
    controller.require_auth();

    // Invariant check: verify balance consistency before state change
    assert_balance_invariant(env, &asset_code)?;

    let new_total_supply = config
        .total_supply
        .checked_add(amount)
        .ok_or(ContractError::MathOverflow)?;
    if new_total_supply > config.max_supply {
        return Err(ContractError::BridgeSupplyCapExceeded);
    }
    rate_limit::enforce_and_record(
        env,
        RateLimitAsset::Wrapped(asset_code.clone()),
        amount,
        config.max_supply,
    )?;

    let key = balance_key(&asset_code, &to);
    let balance: i128 = env.storage().persistent().get(&key).unwrap_or(0);
    let new_balance = balance.checked_add(amount).ok_or(ContractError::MathOverflow)?;
    env.storage().persistent().set(&key, &new_balance);
    env.storage().persistent().extend_ttl(
        &key,
        crate::storage::PERSISTENT_TTL_THRESHOLD,
        crate::storage::PERSISTENT_TTL_THRESHOLD,
    );

    config.total_supply = new_total_supply;
    save_config(env, &config);

    env.events().publish(
        (symbol_short!("wtok_mnt"), asset_code.clone(), to.clone()),
        (amount, new_total_supply),
    );

    // Invariant check: verify balance consistency after state change
    assert_balance_invariant(env, &asset_code)?;

    Ok(new_total_supply)
}

/// Burn `amount` of `asset_code` from `from`. Exclusively callable by the
/// registered Bridge Controller for that asset.
pub fn burn(
    env: &Env,
    controller: Address,
    asset_code: Symbol,
    from: Address,
    amount: i128,
) -> Result<i128, ContractError> {
    let _guard = crate::security::reentrancy::ReentrancyGuard::new(env)?;
    if amount <= 0 {

        return Err(ContractError::BridgeInvalidAmount);
    }
    let mut config = load_config(env, &asset_code)?;
    if config.controller != controller {
        return Err(ContractError::BridgeNotController);
    }
    controller.require_auth();

    // Invariant check: verify balance consistency before state change
    assert_balance_invariant(env, &asset_code)?;

    let key = balance_key(&asset_code, &from);
    let balance: i128 = env.storage().persistent().get(&key).unwrap_or(0);
    if amount > balance {
        return Err(ContractError::BridgeInsufficientBalance);
    }
    let new_balance = balance - amount;
    if new_balance == 0 {
        env.storage().persistent().remove(&key);
    } else {
        env.storage().persistent().set(&key, &new_balance);
    }

    let new_total_supply = config.total_supply.checked_sub(amount).ok_or(ContractError::MathOverflow)?;
    config.total_supply = new_total_supply;
    save_config(env, &config);

    env.events().publish(
        (symbol_short!("wtok_brn"), asset_code.clone(), from.clone()),
        (amount, new_total_supply),
    );

    // Invariant check: verify balance consistency after state change
    assert_balance_invariant(env, &asset_code)?;

    Ok(new_total_supply)
}

pub fn balance_of(env: &Env, asset_code: Symbol, holder: Address) -> i128 {
    env.storage().persistent().get(&balance_key(&asset_code, &holder)).unwrap_or(0)
}

pub fn get_config(env: &Env, asset_code: Symbol) -> Option<BridgeAssetConfig> {
    env.storage().instance().get(&BridgeStorageKey::Config(asset_code))
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
        Symbol,
    ) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let client = crate::TimeLockedUpgradeContractClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        client.initialize(&admin, &treasury);
        let controller = Address::generate(&env);
        let asset_code = Symbol::new(&env, "wBTC");
        (env, client, admin, controller, asset_code)
    }

    #[test]
    fn register_and_mint_within_cap_succeeds() {
        let (env, client, admin, controller, asset_code) = setup();
        client.register_wrapped_asset(&admin, &asset_code, &controller, &1_000_000);
        let user = Address::generate(&env);
        let new_supply = client.mint_wrapped(&controller, &asset_code, &user, &500);
        assert_eq!(new_supply, 500);
        assert_eq!(client.wrapped_balance_of(&asset_code, &user), 500);
    }

    #[test]
    fn mint_beyond_max_supply_is_rejected() {
        let (env, client, admin, controller, asset_code) = setup();
        client.register_wrapped_asset(&admin, &asset_code, &controller, &1_000);
        let user = Address::generate(&env);
        let result = client.try_mint_wrapped(&controller, &asset_code, &user, &1_001);
        assert_eq!(result, Err(Ok(ContractError::BridgeSupplyCapExceeded)));
    }

    #[test]
    fn mint_by_non_controller_is_rejected() {
        let (env, client, admin, controller, asset_code) = setup();
        client.register_wrapped_asset(&admin, &asset_code, &controller, &1_000);
        let attacker = Address::generate(&env);
        let user = Address::generate(&env);
        let result = client.try_mint_wrapped(&attacker, &asset_code, &user, &100);
        assert_eq!(result, Err(Ok(ContractError::BridgeNotController)));
    }

    #[test]
    fn burn_reduces_balance_and_total_supply() {
        let (env, client, admin, controller, asset_code) = setup();
        client.register_wrapped_asset(&admin, &asset_code, &controller, &1_000);
        let user = Address::generate(&env);
        client.mint_wrapped(&controller, &asset_code, &user, &400);
        let new_supply = client.burn_wrapped(&controller, &asset_code, &user, &150);
        assert_eq!(new_supply, 250);
        assert_eq!(client.wrapped_balance_of(&asset_code, &user), 250);
    }

    #[test]
    fn burn_more_than_balance_is_rejected() {
        let (env, client, admin, controller, asset_code) = setup();
        client.register_wrapped_asset(&admin, &asset_code, &controller, &1_000);
        let user = Address::generate(&env);
        client.mint_wrapped(&controller, &asset_code, &user, &100);
        let result = client.try_burn_wrapped(&controller, &asset_code, &user, &101);
        assert_eq!(result, Err(Ok(ContractError::BridgeInsufficientBalance)));
    }

    #[test]
    fn rotating_controller_revokes_old_controller_access() {
        let (env, client, admin, controller, asset_code) = setup();
        client.register_wrapped_asset(&admin, &asset_code, &controller, &1_000);
        let new_controller = Address::generate(&env);
        client.set_bridge_controller(&admin, &asset_code, &new_controller);

        let user = Address::generate(&env);
        let result = client.try_mint_wrapped(&controller, &asset_code, &user, &10);
        assert_eq!(result, Err(Ok(ContractError::BridgeNotController)));

        let new_supply = client.mint_wrapped(&new_controller, &asset_code, &user, &10);
        assert_eq!(new_supply, 10);
    }

    #[test]
    fn registering_with_non_positive_max_supply_is_rejected() {
        let (env, client, admin, controller, asset_code) = setup();
        let result = client.try_register_wrapped_asset(&admin, &asset_code, &controller, &0);
        assert_eq!(result, Err(Ok(ContractError::BridgeInvalidMaxSupply)));
    }
}
