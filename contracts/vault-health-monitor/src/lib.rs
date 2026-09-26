#![no_std]

extern crate alloc;

use soroban_sdk::{contract, contracterror, contractimpl, contracttype, Address, Env, Symbol};

/// Health factor scale: 10_000 represents 1.00.
pub const HEALTH_FACTOR_SCALE: i128 = 10_000;
/// A health factor of 1.10 is the upper bound for liquidation warnings.
pub const WARNING_HEALTH_FACTOR_BPS: i128 = 11_000;

#[derive(Clone)]
#[contracttype]
pub enum DataKey {
    Vault,
}

/// Data carried by `VaultHealthWarning` for indexers and account-notification services.
///
/// All values are integer values. `health_factor_bps` uses a 10_000 scale, so 10_500
/// represents a health factor of 1.05.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct VaultHealthWarning {
    /// The vault whose position was evaluated.
    pub vault: Address,
    /// Account to notify.
    pub account: Address,
    /// Health factor using [`HEALTH_FACTOR_SCALE`].
    pub health_factor_bps: i128,
    /// Liquidation threshold used in the health-factor calculation, in bps.
    pub liquidation_threshold_bps: i128,
    /// Current value of the account's collateral.
    pub collateral_value: i128,
    /// Current value of the account's debt.
    pub debt_value: i128,
    /// Ledger timestamp at which the warning was produced.
    pub timestamp: u64,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    UnauthorizedVault = 3,
    InvalidValue = 4,
    ArithmeticOverflow = 5,
}

#[contract]
pub struct VaultHealthMonitor;

#[contractimpl]
impl VaultHealthMonitor {
    /// Bind this monitor to the lending vault permitted to submit account valuations.
    pub fn initialize(env: Env, vault: Address) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Vault) {
            return Err(Error::AlreadyInitialized);
        }
        vault.require_auth();
        env.storage().instance().set(&DataKey::Vault, &vault);
        Ok(())
    }

    /// Evaluate an account's current vault position and publish an indexer warning when
    /// `1.00 < health_factor <= 1.10`.
    ///
    /// The configured vault must authorize the call, preventing arbitrary callers from
    /// producing user-notification events. The health factor is calculated dynamically as:
    /// `collateral_value * liquidation_threshold_bps / debt_value`.
    pub fn assess_vault_health(
        env: Env,
        vault: Address,
        account: Address,
        collateral_value: i128,
        debt_value: i128,
        liquidation_threshold_bps: i128,
    ) -> Result<i128, Error> {
        let configured_vault: Address = env
            .storage()
            .instance()
            .get(&DataKey::Vault)
            .ok_or(Error::NotInitialized)?;
        if vault != configured_vault {
            return Err(Error::UnauthorizedVault);
        }
        vault.require_auth();

        if collateral_value < 0
            || debt_value <= 0
            || liquidation_threshold_bps <= 0
            || liquidation_threshold_bps > HEALTH_FACTOR_SCALE
        {
            return Err(Error::InvalidValue);
        }

        let health_factor_bps = collateral_value
            .checked_mul(liquidation_threshold_bps)
            .ok_or(Error::ArithmeticOverflow)?
            .checked_div(debt_value)
            .ok_or(Error::ArithmeticOverflow)?;

        if health_factor_bps > HEALTH_FACTOR_SCALE && health_factor_bps <= WARNING_HEALTH_FACTOR_BPS
        {
            let warning = VaultHealthWarning {
                vault,
                account: account.clone(),
                health_factor_bps,
                liquidation_threshold_bps,
                collateral_value,
                debt_value,
                timestamp: env.ledger().timestamp(),
            };
            // The first topic gives indexers a stable event name; the account topic
            // allows notification services to filter for a single user efficiently.
            env.events()
                .publish((Symbol::new(&env, "VaultHealthWarning"), account), warning);
        }

        Ok(health_factor_bps)
    }

    pub fn get_vault(env: Env) -> Result<Address, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Vault)
            .ok_or(Error::NotInitialized)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::{
        testutils::{Address as _, Events, Ledger},
        Env,
    };

    fn setup() -> (Env, VaultHealthMonitorClient<'static>, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, VaultHealthMonitor);
        let client = VaultHealthMonitorClient::new(&env, &contract_id);
        let vault = Address::generate(&env);
        let account = Address::generate(&env);
        client.initialize(&vault);
        (env, client, vault, account)
    }

    #[test]
    fn emits_warning_with_notification_payload_inside_horizon() {
        let (env, client, vault, account) = setup();
        env.ledger().with_mut(|ledger| ledger.timestamp = 1_234);

        // 13_125 * 8_000 / 10_000 = 10_500 (1.05).
        assert_eq!(
            client.assess_vault_health(&vault, &account, &13_125, &10_000, &8_000),
            10_500
        );

        let events = env.events().all();
        assert_eq!(events.len(), 1);
        let event_debug = alloc::format!("{:?}", events.get(0).unwrap());
        assert!(event_debug.contains("VaultHealthWarning"));
        assert!(event_debug.contains("10500"));
        assert!(event_debug.contains("1234"));
    }

    #[test]
    fn does_not_emit_outside_warning_horizon() {
        let (env, client, vault, account) = setup();

        assert_eq!(
            client.assess_vault_health(&vault, &account, &12_500, &10_000, &8_000),
            10_000
        );
        assert_eq!(
            client.assess_vault_health(&vault, &account, &15_000, &10_000, &8_000),
            12_000
        );
        assert_eq!(env.events().all().len(), 0);
    }
}
