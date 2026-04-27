#![no_std]

use soroban_sdk::{contract, contractimpl, contracterror, Address, Env, Symbol, Vec};

mod types;
mod interface;

pub use types::{AssetMeta, AssetWeight, DataKey, PriceData};
pub use interface::PriceOracleStorageTrait;

/// Errors for storage operations
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub enum Error {
    NotFound = 1,
    AlreadyExists = 2,
    Unauthorized = 3,
    InvalidInput = 4,
}

#[contract]
pub struct PriceOracleStorage;

#[contractimpl]
impl PriceOracleStorageTrait for PriceOracleStorage {
    fn set_admin(env: Env, admin: Address) {
        env.storage().instance().set(&DataKey::Admin, &admin);
    }

    fn get_admin(env: Env) -> Result<Address, Error> {
        env.storage()
            .instance()
            .get::<DataKey, Address>(&DataKey::Admin)
            .ok_or(Error::NotFound)
    }

    fn is_admin(env: Env, address: Address) -> bool {
        env.storage()
            .instance()
            .get::<DataKey, Address>(&DataKey::Admin)
            .map(|admin| admin == address)
            .unwrap_or(false)
    }

    fn set_verified_price(env: Env, asset: Symbol, price: PriceData) {
        env.storage()
            .temporary()
            .set(&DataKey::VerifiedPrice(asset), &price);
    }

    fn get_verified_price(env: Env, asset: Symbol) -> Result<PriceData, Error> {
        env.storage()
            .temporary()
            .get::<DataKey, PriceData>(&DataKey::VerifiedPrice(asset))
            .ok_or(Error::NotFound)
    }

    fn set_community_price(env: Env, asset: Symbol, price: PriceData) {
        env.storage()
            .temporary()
            .set(&DataKey::CommunityPrice(asset), &price);
    }

    fn get_community_price(env: Env, asset: Symbol) -> Result<PriceData, Error> {
        env.storage()
            .temporary()
            .get::<DataKey, PriceData>(&DataKey::CommunityPrice(asset))
            .ok_or(Error::NotFound)
    }

    fn set_asset_meta(env: Env, asset: Symbol, meta: AssetMeta) {
        env.storage()
            .persistent()
            .set(&DataKey::AssetMeta(asset), &meta);
    }

    fn get_asset_meta(env: Env, asset: Symbol) -> Result<AssetMeta, Error> {
        env.storage()
            .persistent()
            .get::<DataKey, AssetMeta>(&DataKey::AssetMeta(asset))
            .ok_or(Error::NotFound)
    }

    fn add_asset(env: Env, asset: Symbol) -> Result<(), Error> {
        let mut assets = env.storage()
            .persistent()
            .get::<DataKey, Vec<Symbol>>(&DataKey::BaseCurrencyPairs)
            .unwrap_or_else(|| Vec::new(&env));

        if assets.iter().any(|a| a == asset) {
            return Err(Error::AlreadyExists);
        }

        assets.push_back(asset);
        env.storage()
            .persistent()
            .set(&DataKey::BaseCurrencyPairs, &assets);

        Ok(())
    }

    fn get_all_assets(env: Env) -> Vec<Symbol> {
        env.storage()
            .persistent()
            .get::<DataKey, Vec<Symbol>>(&DataKey::BaseCurrencyPairs)
            .unwrap_or_else(|| Vec::new(&env))
    }

    fn get_asset_count(env: Env) -> u32 {
        env.storage()
            .persistent()
            .get::<DataKey, Vec<Symbol>>(&DataKey::BaseCurrencyPairs)
            .map(|assets| assets.len() as u32)
            .unwrap_or(0)
    }

    fn subscribe(env: Env, callback_contract: Address) -> Result<(), Error> {
        let mut subscribers = env.storage()
            .persistent()
            .get::<DataKey, Vec<Address>>(&DataKey::PriceUpdateSubscribers)
            .unwrap_or_else(|| Vec::new(&env));

        if subscribers.iter().any(|sub| sub == callback_contract) {
            return Err(Error::AlreadyExists);
        }

        subscribers.push_back(callback_contract);
        env.storage()
            .persistent()
            .set(&DataKey::PriceUpdateSubscribers, &subscribers);

        Ok(())
    }

    fn unsubscribe(env: Env, callback_contract: Address) -> Result<(), Error> {
        let subscribers = env.storage()
            .persistent()
            .get::<DataKey, Vec<Address>>(&DataKey::PriceUpdateSubscribers)
            .unwrap_or_else(|| Vec::new(&env));

        let original_len = subscribers.len();
        let filtered = {
            let mut f = Vec::new(&env);
            for sub in subscribers.iter() {
                if sub != callback_contract {
                    f.push_back(sub);
                }
            }
            f
        };

        if filtered.len() == original_len {
            return Err(Error::NotFound);
        }

        env.storage()
            .persistent()
            .set(&DataKey::PriceUpdateSubscribers, &filtered);

        Ok(())
    }

    fn get_subscribers(env: Env) -> Vec<Address> {
        env.storage()
            .persistent()
            .get::<DataKey, Vec<Address>>(&DataKey::PriceUpdateSubscribers)
            .unwrap_or_else(|| Vec::new(&env))
    }

    fn initialize(env: Env, admin: Address) -> Result<(), Error> {
        if Self::is_initialized(env.clone()) {
            return Err(Error::AlreadyExists);
        }

        env.storage().instance().set(&DataKey::Initialized, &true);
        Self::set_admin(env, admin);

        Ok(())
    }

    fn is_initialized(env: Env) -> bool {
        env.storage()
            .instance()
            .get::<DataKey, bool>(&DataKey::Initialized)
            .unwrap_or(false)
    }
}
