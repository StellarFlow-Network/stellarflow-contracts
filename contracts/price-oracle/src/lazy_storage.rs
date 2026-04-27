//! FE-213: Lazy state loading — separates Price and AssetInfo into different storage keys.

use soroban_sdk::{contracttype, Symbol};

#[contracttype]
pub enum StorageKey {
    /// Stores only the latest price for an asset — loaded on every price update.
    Price(Symbol),
    /// Stores asset description/metadata — loaded only when explicitly requested.
    AssetInfo(Symbol),
}

// By using separate keys, price updates only read/write Price(asset),
// never touching AssetInfo(asset) unless the caller explicitly requests it.
// This reduces storage I/O on the hot path.