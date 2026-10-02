#![no_std]

//! Atomic yield-strategy migration helper (Issue #973).
//!
//! When a protocol upgrade replaces one yield strategy with another, a vault's
//! assets must move from `Strategy A` to `Strategy B` without ever shrinking the
//! vault's position. This contract exposes a single owner-gated `migrate`
//! entrypoint that withdraws the position from the source strategy, deposits it
//! into the destination strategy, and reverts the whole atomic call unless the
//! destination ends up holding at least as much as the source released —
//! enforcing `B_final >= B_initial`.
//!
//! Strategies are arbitrary contracts that expose the [`Strategy`] interface
//! (`withdraw`/`deposit`); the migrator never needs to know how a strategy
//! invests its assets. Migration routes are configured per asset, so each asset
//! can be pointed at a new strategy pair independently.

use soroban_sdk::{
    contract, contractclient, contracterror, contractimpl, contracttype, token, Address, Env,
    Symbol,
};

/// Interface every yield strategy must expose for the migrator to move a
/// position. Assets are always moved by the migrator itself: a strategy only
/// releases funds on `withdraw` and credits the beneficiary on `deposit`.
#[contractclient(name = "StrategyClient")]
pub trait Strategy {
    /// Release `amount` of `asset` to `recipient` and return the amount actually
    /// withdrawn.
    fn withdraw(env: Env, asset: Address, amount: i128, recipient: Address) -> i128;

    /// Credit a deposit of `amount` of `asset` (already transferred to the
    /// strategy) to `beneficiary` and return the amount credited.
    fn deposit(env: Env, asset: Address, amount: i128, beneficiary: Address) -> i128;
}

/// Contract error codes.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    /// `initialize` has already been called.
    AlreadyInitialized = 1,
    /// The contract has not been initialized.
    NotInitialized = 2,
    /// The caller is not the configured owner.
    Unauthorized = 3,
    /// An amount was zero or negative.
    InvalidAmount = 4,
    /// The configured source and destination strategies are the same address.
    InvalidStrategies = 5,
    /// No migration route has been configured for the asset.
    StrategiesNotSet = 6,
    /// The source strategy released no assets.
    InsufficientWithdrawal = 7,
    /// Migration would leave the migrator holding less than it started with.
    MigrationLoss = 8,
    /// An arithmetic operation would overflow.
    Overflow = 9,
}

/// A source/destination strategy route for one asset.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StrategyPair {
    /// Strategy the assets are withdrawn from.
    pub from: Address,
    /// Strategy the assets are deposited into.
    pub to: Address,
}

/// Storage keys for contract data held in instance storage.
#[contracttype]
pub enum DataKey {
    /// Address allowed to configure routes and trigger migrations.
    Owner,
    /// Configured migration route for an asset (`StrategyPair`).
    Strategies(Address),
    /// Cumulative amount migrated for an asset.
    TotalMigrated(Address),
}

#[contract]
pub struct StrategyMigrator;

#[contractimpl]
impl StrategyMigrator {
    /// Initialize the migrator with the owner allowed to configure routes and
    /// trigger migrations. Can only be called once.
    pub fn initialize(env: Env, owner: Address) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Owner) {
            return Err(Error::AlreadyInitialized);
        }
        owner.require_auth();
        env.storage().instance().set(&DataKey::Owner, &owner);
        Ok(())
    }

    /// Set the source (`from`) and destination (`to`) strategies used to migrate
    /// `asset`. Owner-gated; the two strategies must differ.
    pub fn set_strategies(
        env: Env,
        caller: Address,
        asset: Address,
        from: Address,
        to: Address,
    ) -> Result<(), Error> {
        Self::require_owner(&env, &caller)?;
        if from == to {
            return Err(Error::InvalidStrategies);
        }
        env.storage()
            .instance()
            .set(&DataKey::Strategies(asset), &StrategyPair { from, to });
        Ok(())
    }

    /// Atomically migrate `amount` of `asset` from the configured source
    /// strategy to the configured destination strategy.
    ///
    /// The source is asked to release the assets into this contract, which then
    /// forwards them to the destination and notifies it. Before any state is
    /// written the migrator re-reads its own and the destination's balances and
    /// reverts the entire call with [`Error::MigrationLoss`] if the destination
    /// did not end up holding at least the amount the source released —
    /// guaranteeing `B_final >= B_initial`.
    ///
    /// Returns the amount actually moved and emits a `StrategyMigrated` event.
    pub fn migrate(
        env: Env,
        caller: Address,
        asset: Address,
        amount: i128,
    ) -> Result<i128, Error> {
        Self::require_owner(&env, &caller)?;

        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let pair: StrategyPair = env
            .storage()
            .instance()
            .get(&DataKey::Strategies(asset.clone()))
            .ok_or(Error::StrategiesNotSet)?;
        if pair.from == pair.to {
            return Err(Error::InvalidStrategies);
        }

        let asset_client = token::Client::new(&env, &asset);
        let migrator = env.current_contract_address();

        // Assets held by this migrator plus those already held by the
        // destination strategy. The source strategy is excluded: its balance is
        // expected to fall as its position is migrated away.
        let migrator_before = asset_client.balance(&migrator);
        let balance_before = migrator_before
            .checked_add(asset_client.balance(&pair.to))
            .ok_or(Error::Overflow)?;

        // 1. Withdraw the position from Strategy A into this contract.
        let withdrawn =
            StrategyClient::new(&env, &pair.from).withdraw(&asset, &amount, &migrator);
        if withdrawn <= 0 {
            return Err(Error::InsufficientWithdrawal);
        }
        let received = asset_client
            .balance(&migrator)
            .checked_sub(migrator_before)
            .ok_or(Error::Overflow)?;
        if received <= 0 {
            return Err(Error::InsufficientWithdrawal);
        }

        // 2. Deposit the released assets into Strategy B.
        asset_client.transfer(&migrator, &pair.to, &received);
        StrategyClient::new(&env, &pair.to).deposit(&asset, &received, &migrator);

        // 3. Zero-loss invariant: the assets under this migrator's control must
        //    not shrink relative to the amount the source strategy released.
        let balance_after = asset_client
            .balance(&migrator)
            .checked_add(asset_client.balance(&pair.to))
            .ok_or(Error::Overflow)?;
        if balance_after < balance_before.checked_add(withdrawn).ok_or(Error::Overflow)? {
            return Err(Error::MigrationLoss);
        }

        // 4. Record cumulative volume and publish the migration event.
        let migrated_key = DataKey::TotalMigrated(asset.clone());
        let total_migrated = env
            .storage()
            .instance()
            .get(&migrated_key)
            .unwrap_or(0i128)
            .checked_add(received)
            .ok_or(Error::Overflow)?;
        env.storage().instance().set(&migrated_key, &total_migrated);

        env.events().publish(
            (Symbol::new(&env, "StrategyMigrated"), asset.clone()),
            (pair.from, pair.to, received, total_migrated),
        );

        Ok(received)
    }

    /// Return the cumulative amount migrated for `asset`.
    pub fn get_total_migrated(env: Env, asset: Address) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::TotalMigrated(asset))
            .unwrap_or(0)
    }

    /// Return the configured migration route for `asset`, if any.
    pub fn get_strategies(env: Env, asset: Address) -> Option<StrategyPair> {
        env.storage().instance().get(&DataKey::Strategies(asset))
    }

    /// Return the configured owner, if the contract has been initialized.
    pub fn get_owner(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Owner)
    }

    /// Require that `caller` is the configured owner and authorized the call.
    fn require_owner(env: &Env, caller: &Address) -> Result<(), Error> {
        let owner: Address = env
            .storage()
            .instance()
            .get(&DataKey::Owner)
            .ok_or(Error::NotInitialized)?;
        if caller != &owner {
            return Err(Error::Unauthorized);
        }
        caller.require_auth();
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Events};
    use soroban_sdk::token::StellarAssetClient;
    use soroban_sdk::TryIntoVal;

    /// In-memory accounting a real strategy would keep for its depositors.
    #[contracttype]
    enum MockKey {
        Credited(Address),
    }

    /// A well-behaved strategy: releases exactly what it is asked for, and
    /// credits whatever a depositor actually transferred in.
    #[contract]
    pub struct MockStrategy;

    #[contractimpl]
    impl MockStrategy {
        pub fn withdraw(env: Env, asset: Address, amount: i128, recipient: Address) -> i128 {
            token::Client::new(&env, &asset).transfer(
                &env.current_contract_address(),
                &recipient,
                &amount,
            );
            amount
        }

        pub fn deposit(env: Env, asset: Address, amount: i128, beneficiary: Address) -> i128 {
            // The migrator transfers `amount` to this strategy before notifying
            // it, so only what is actually held can be credited.
            let held = token::Client::new(&env, &asset)
                .balance(&env.current_contract_address());
            let credited = if held >= amount { amount } else { held };
            let key = MockKey::Credited(beneficiary);
            let prior: i128 = env.storage().instance().get(&key).unwrap_or(0);
            env.storage().instance().set(&key, &(prior + credited));
            credited
        }

        pub fn credited(env: Env, beneficiary: Address) -> i128 {
            env.storage()
                .instance()
                .get(&MockKey::Credited(beneficiary))
                .unwrap_or(0)
        }
    }

    /// A misbehaving source strategy: it promises the full amount but only
    /// releases half, which must trip the zero-loss invariant.
    #[contract]
    pub struct UnderDeliveringStrategy;

    #[contractimpl]
    impl UnderDeliveringStrategy {
        pub fn withdraw(env: Env, asset: Address, amount: i128, recipient: Address) -> i128 {
            token::Client::new(&env, &asset).transfer(
                &env.current_contract_address(),
                &recipient,
                &(amount / 2),
            );
            amount
        }

        pub fn deposit(env: Env, _asset: Address, amount: i128, _beneficiary: Address) -> i128 {
            amount
        }
    }

    type Ctx = (
        Env,
        StrategyMigratorClient<'static>,
        Address,
        Address,
        Address,
        Address,
    );

    /// `(env, migrator, owner, asset, strategy_a, strategy_b)`.
    fn setup() -> Ctx {
        let env = Env::default();
        env.mock_all_auths();

        let migrator_id = env.register_contract(None, StrategyMigrator);
        let migrator = StrategyMigratorClient::new(&env, &migrator_id);
        let owner = Address::generate(&env);
        migrator.initialize(&owner);

        let issuer = Address::generate(&env);
        let asset = env.register_stellar_asset_contract(issuer);
        let strategy_a = env.register_contract(None, MockStrategy);
        let strategy_b = env.register_contract(None, MockStrategy);
        migrator.set_strategies(&owner, &asset, &strategy_a, &strategy_b);

        (env, migrator, owner, asset, strategy_a, strategy_b)
    }

    fn mint(env: &Env, asset: &Address, to: &Address, amount: i128) {
        StellarAssetClient::new(env, asset).mint(to, &amount);
    }

    fn balance(env: &Env, asset: &Address, who: &Address) -> i128 {
        token::Client::new(env, asset).balance(who)
    }

    #[test]
    fn migrates_assets_between_strategies_and_emits_event() {
        let (env, migrator, owner, asset, strategy_a, strategy_b) = setup();
        let amount = 1_000i128;
        mint(&env, &asset, &strategy_a, amount);

        let moved = migrator.migrate(&owner, &asset, &amount);

        assert_eq!(moved, amount);
        assert_eq!(balance(&env, &asset, &strategy_a), 0);
        assert_eq!(balance(&env, &asset, &strategy_b), amount);
        assert_eq!(migrator.get_total_migrated(&asset), amount);
        assert_eq!(
            MockStrategyClient::new(&env, &strategy_b).credited(&migrator.address),
            amount
        );

        // A single `StrategyMigrated` event carries the moved and cumulative
        // amounts for the asset.
        let events = env.events().all();
        let migrated = events.iter().find(|(_contract, topics, _data)| {
            let topic0: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            let topic1: Address = topics.get(1).unwrap().try_into_val(&env).unwrap();
            topic0 == Symbol::new(&env, "StrategyMigrated") && topic1 == asset
        });
        assert!(migrated.is_some());

        let (_, _, data) = migrated.unwrap();
        let (from, to, moved_total, cumulative): (Address, Address, i128, i128) =
            data.try_into_val(&env).expect("StrategyMigrated payload");
        assert_eq!(from, strategy_a);
        assert_eq!(to, strategy_b);
        assert_eq!(moved_total, amount);
        assert_eq!(cumulative, amount);
    }

    #[test]
    fn preserves_destination_balance_zero_loss_invariant() {
        let (env, migrator, owner, asset, strategy_a, strategy_b) = setup();
        let amount = 5_000i128;
        mint(&env, &asset, &strategy_a, amount);

        let destination_before = balance(&env, &asset, &strategy_b);
        migrator.migrate(&owner, &asset, &amount);
        let destination_after = balance(&env, &asset, &strategy_b);

        // The destination only ever grows, and nothing is stranded.
        assert!(destination_after >= destination_before);
        assert_eq!(destination_after, destination_before + amount);
        assert_eq!(balance(&env, &asset, &strategy_a), 0);
        assert_eq!(balance(&env, &asset, &migrator.address), 0);
    }

    #[test]
    fn rejects_migration_that_would_lose_funds() {
        let (env, migrator, owner, asset, _strategy_a, strategy_b) = setup();
        let lossy = env.register_contract(None, UnderDeliveringStrategy);
        migrator.set_strategies(&owner, &asset, &lossy, &strategy_b);

        let amount = 1_000i128;
        mint(&env, &asset, &lossy, amount);

        let result = migrator.try_migrate(&owner, &asset, &amount);
        assert_eq!(result, Err(Ok(Error::MigrationLoss)));

        // The failed migration is rolled back atomically.
        assert_eq!(balance(&env, &asset, &lossy), amount);
        assert_eq!(balance(&env, &asset, &strategy_b), 0);
        assert_eq!(migrator.get_total_migrated(&asset), 0);
    }

    #[test]
    fn rejects_non_owner_migration() {
        let (env, migrator, _owner, asset, _strategy_a, _strategy_b) = setup();
        let stranger = Address::generate(&env);
        let result = migrator.try_migrate(&stranger, &asset, &100);
        assert_eq!(result, Err(Ok(Error::Unauthorized)));
    }

    #[test]
    fn rejects_migration_without_configured_route() {
        let (env, migrator, owner, _asset, _strategy_a, _strategy_b) = setup();
        let other_asset = env.register_stellar_asset_contract(Address::generate(&env));
        let result = migrator.try_migrate(&owner, &other_asset, &100);
        assert_eq!(result, Err(Ok(Error::StrategiesNotSet)));
    }

    #[test]
    fn rejects_identical_strategies() {
        let (env, migrator, owner, asset, strategy_a, _strategy_b) = setup();
        let result = migrator.try_set_strategies(&owner, &asset, &strategy_a, &strategy_a);
        assert_eq!(result, Err(Ok(Error::InvalidStrategies)));
    }

    #[test]
    fn initialize_can_only_run_once() {
        let (_env, migrator, owner, _asset, _strategy_a, _strategy_b) = setup();
        assert_eq!(
            migrator.try_initialize(&owner),
            Err(Ok(Error::AlreadyInitialized))
        );
    }
}
