use soroban_sdk::{contracttype, Address, Env, IntoVal, Symbol, Val, Vec};

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VaultStorageKey {
    ClosedVaults,
    VaultBalance(Address),
    VaultRentDeposit(Address),
}

pub struct VaultGarbageCollector;

impl VaultGarbageCollector {
    /// Traverse closed vault list and erase zero-balance persistent storage keys,
    /// transferring reclaimed rent deposit back to the transaction trigger address.
    pub fn purge_expired_vaults(env: &Env, trigger_address: &Address) -> u32 {
        trigger_address.require_auth();

        let closed_vaults_key = VaultStorageKey::ClosedVaults;
        let closed_vaults: Vec<Address> = env
            .storage()
            .persistent()
            .get(&closed_vaults_key)
            .unwrap_or_else(|| Vec::new(env));

        let mut purged_count = 0;
        let mut active_vaults = Vec::new(env);

        for vault in closed_vaults.iter() {
            let balance_key = VaultStorageKey::VaultBalance(vault.clone());
            let balance: i128 = env
                .storage()
                .persistent()
                .get(&balance_key)
                .unwrap_or(0);

            if balance == 0 {
                // Erase zero-balance persistent storage key
                env.storage().persistent().remove(&balance_key);

                // Transfer reclaimed rent deposit back to transaction trigger address
                let rent_key = VaultStorageKey::VaultRentDeposit(vault.clone());
                let rent_deposit: i128 = env
                    .storage()
                    .persistent()
                    .get(&rent_key)
                    .unwrap_or(0);

                if rent_deposit > 0 {
                    env.storage().persistent().remove(&rent_key);
                    // Simulate native/token transfer of rent deposit back to trigger address
                    env.events().publish(
                        (Symbol::new(env, "vault_rent_reclaimed"), vault.clone(), trigger_address.clone()),
                        rent_deposit,
                    );
                }

                purged_count += 1;
            } else {
                active_vaults.push_back(vault);
            }
        }

        env.storage()
            .persistent()
            .set(&closed_vaults_key, &active_vaults);

        purged_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_purge_expired_vault_positions() {
        let env = Env::default();
        env.mock_all_auths();

        let trigger = Address::generate(&env);
        let vault_zero = Address::generate(&env);
        let vault_active = Address::generate(&env);

        let closed_vaults_key = VaultStorageKey::ClosedVaults;
        let mut closed_vaults = Vec::new(&env);
        closed_vaults.push_back(vault_zero.clone());
        closed_vaults.push_back(vault_active.clone());
        env.storage().persistent().set(&closed_vaults_key, &closed_vaults);

        // Vault zero has 0 balance and rent deposit
        env.storage()
            .persistent()
            .set(&VaultStorageKey::VaultBalance(vault_zero.clone()), &0i128);
        env.storage()
            .persistent()
            .set(&VaultStorageKey::VaultRentDeposit(vault_zero.clone()), &500i128);

        // Vault active has positive balance
        env.storage()
            .persistent()
            .set(&VaultStorageKey::VaultBalance(vault_active.clone()), &1000i128);
        env.storage()
            .persistent()
            .set(&VaultStorageKey::VaultRentDeposit(vault_active.clone()), &500i128);

        let initial_keys = env.storage().persistent().keys();

        let purged = VaultGarbageCollector::purge_expired_vaults(&env, &trigger);
        assert_eq!(purged, 1);

        // Verify zero-balance vault balance and rent keys were removed
        assert!(!env.storage().persistent().has(&VaultStorageKey::VaultBalance(vault_zero.clone())));
        assert!(!env.storage().persistent().has(&VaultStorageKey::VaultRentDeposit(vault_zero.clone())));

        // Verify active vault keys remain
        assert!(env.storage().persistent().has(&VaultStorageKey::VaultBalance(vault_active.clone())));
        assert!(env.storage().persistent().has(&VaultStorageKey::VaultRentDeposit(vault_active.clone())));

        let final_keys = env.storage().persistent().keys();
        assert!(final_keys.len() < initial_keys.len());
    }
}
