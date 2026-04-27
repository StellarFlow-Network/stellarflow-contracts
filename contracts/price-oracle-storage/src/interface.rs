use soroban_sdk::{contractclient, Address, Env, Symbol, Vec};
use crate::types::{AssetMeta, PriceData};
use crate::Error;

/// Storage contract interface for price oracle
#[contractclient(name = "PriceOracleStorageClient")]
pub trait PriceOracleStorageTrait {
    fn set_admin(env: Env, admin: Address);
    fn get_admin(env: Env) -> Result<Address, Error>;
    fn is_admin(env: Env, address: Address) -> bool;
    fn set_verified_price(env: Env, asset: Symbol, price: PriceData);
    fn get_verified_price(env: Env, asset: Symbol) -> Result<PriceData, Error>;
    fn set_community_price(env: Env, asset: Symbol, price: PriceData);
    fn get_community_price(env: Env, asset: Symbol) -> Result<PriceData, Error>;
    fn set_asset_meta(env: Env, asset: Symbol, meta: AssetMeta);
    fn get_asset_meta(env: Env, asset: Symbol) -> Result<AssetMeta, Error>;
    fn add_asset(env: Env, asset: Symbol) -> Result<(), Error>;
    fn get_all_assets(env: Env) -> Vec<Symbol>;
    fn get_asset_count(env: Env) -> u32;
    fn subscribe(env: Env, callback_contract: Address) -> Result<(), Error>;
    fn unsubscribe(env: Env, callback_contract: Address) -> Result<(), Error>;
    fn get_subscribers(env: Env) -> Vec<Address>;
    fn initialize(env: Env, admin: Address) -> Result<(), Error>;
    fn is_initialized(env: Env) -> bool;
}
