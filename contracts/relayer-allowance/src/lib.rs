#![no_std]

use soroban_sdk::{contract, contractimpl, contracttype, contracterror, token, Address, Env, Symbol};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum AllowanceError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    NotAdmin = 3,
    Unauthorized = 4,
    AllowanceNotFound = 5,
    AllowanceExpired = 6,
    AllowanceDepleted = 7,
    InvalidAmount = 8,
    InvalidExpiry = 9,
    AllowanceAlreadyExists = 10,
    Overflow = 11,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct RelayerAllowance {
    pub owner: Address,
    pub relayer: Address,
    pub max_amount: i128,
    pub spent_amount: i128,
    pub expiry_ledger: u32,
    pub created_ledger: u32,
    pub active: bool,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct AllowanceKey(pub Address, pub Address); // (owner, relayer)

#[contracttype]
pub enum DataKey {
    Admin,
    Token,
}

#[contract]
pub struct RelayerAllowanceContract;

#[contractimpl]
impl RelayerAllowanceContract {
    /// Initialize the allowance contract with admin and token address.
    pub fn initialize(env: Env, admin: Address, token: Address) -> Result<(), AllowanceError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(AllowanceError::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Token, &token);
        Ok(())
    }

    /// Grant an allowance to a relayer account for automated contract execution.
    ///
    /// # Parameters
    /// - `owner`: The user granting the allowance (must be auth'd)
    /// - `relayer`: The automated relayer address receiving the allowance
    /// - `max_amount`: Maximum tokens the relayer can spend on behalf of the owner
    /// - `expiry_ledger`: Ledger sequence after which the allowance expires
    pub fn grant_allowance(
        env: Env,
        owner: Address,
        relayer: Address,
        max_amount: i128,
        expiry_ledger: u32,
    ) -> Result<(), AllowanceError> {
        owner.require_auth();

        if max_amount <= 0 {
            return Err(AllowanceError::InvalidAmount);
        }
        let current_ledger = env.ledger().sequence();
        if expiry_ledger <= current_ledger {
            return Err(AllowanceError::InvalidExpiry);
        }

        let key = AllowanceKey(owner.clone(), relayer.clone());
        if env.storage().persistent().has(&key) {
            return Err(AllowanceError::AllowanceAlreadyExists);
        }

        let allowance = RelayerAllowance {
            owner: owner.clone(),
            relayer: relayer.clone(),
            max_amount,
            spent_amount: 0,
            expiry_ledger,
            created_ledger: current_ledger,
            active: true,
        };

        env.storage().persistent().set(&key, &allowance);

        // Emit event
        env.events().publish(
            (Symbol::new(&env, "allow_grant"),),
            (owner, relayer, max_amount, expiry_ledger),
        );

        Ok(())
    }

    /// Execute a relayer-delegated transfer using the granted allowance.
    ///
    /// The relayer calls this to spend tokens on behalf of the owner up to
    /// the allowed maximum. Checks expiry, spending cap, and active status.
    ///
    /// # Parameters
    /// - `owner`: The owner of the allowance
    /// - `relayer`: The relayer executing the transfer (must be auth'd)
    /// - `amount`: Amount to spend from the allowance
    /// - `recipient`: Address to receive the tokens
    pub fn execute_allowance(
        env: Env,
        owner: Address,
        relayer: Address,
        amount: i128,
        recipient: Address,
    ) -> Result<(), AllowanceError> {
        relayer.require_auth();

        if amount <= 0 {
            return Err(AllowanceError::InvalidAmount);
        }

        let key = AllowanceKey(owner.clone(), relayer.clone());
        let mut allowance: RelayerAllowance = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(AllowanceError::AllowanceNotFound)?;

        // Check active status
        if !allowance.active {
            return Err(AllowanceError::AllowanceDepleted);
        }

        // Check expiry
        let current_ledger = env.ledger().sequence();
        if current_ledger > allowance.expiry_ledger {
            return Err(AllowanceError::AllowanceExpired);
        }

        // Check spending cap
        let new_spent = allowance
            .spent_amount
            .checked_add(amount)
            .ok_or(AllowanceError::Overflow)?;
        if new_spent > allowance.max_amount {
            return Err(AllowanceError::AllowanceDepleted);
        }

        // Update spent amount
        allowance.spent_amount = new_spent;
        env.storage().persistent().set(&key, &allowance);

        // Transfer tokens from owner to recipient
        let token_addr: Address = env.storage().instance().get(&DataKey::Token).ok_or(AllowanceError::NotInitialized)?;
        let token_client = token::Client::new(&env, &token_addr);
        token_client.transfer(&owner, &recipient, &amount);

        // Emit event
        env.events().publish(
            (Symbol::new(&env, "allow_exec"),),
            (owner, relayer, recipient, amount),
        );

        Ok(())
    }

    /// Instantly revoke a relayer allowance. Can be called by the owner or admin.
    ///
    /// # Parameters
    /// - `owner`: The owner of the allowance to revoke
    /// - `relayer`: The relayer whose allowance is being revoked
    /// - `caller`: Address revoking (must be owner or admin)
    pub fn revoke_allowance(
        env: Env,
        owner: Address,
        relayer: Address,
        caller: Address,
    ) -> Result<(), AllowanceError> {
        caller.require_auth();

        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(AllowanceError::NotInitialized)?;

        // Only owner or admin can revoke
        if caller != owner && caller != stored_admin {
            return Err(AllowanceError::Unauthorized);
        }

        let key = AllowanceKey(owner.clone(), relayer.clone());
        let mut allowance: RelayerAllowance = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(AllowanceError::AllowanceNotFound)?;

        allowance.active = false;
        env.storage().persistent().set(&key, &allowance);

        // Emit event
        env.events().publish(
            (Symbol::new(&env, "allow_revoke"),),
            (owner, relayer, caller),
        );

        Ok(())
    }

    /// Get the current allowance details for a given owner-relayer pair.
    pub fn get_allowance(env: Env, owner: Address, relayer: Address) -> Option<RelayerAllowance> {
        let key = AllowanceKey(owner, relayer);
        env.storage().persistent().get(&key)
    }

    /// Check if an allowance is currently valid (active and not expired).
    pub fn is_allowance_valid(env: Env, owner: Address, relayer: Address) -> bool {
        let key = AllowanceKey(owner, relayer);
        match env.storage().persistent().get::<_, RelayerAllowance>(&key) {
            Some(a) => {
                let current_ledger = env.ledger().sequence();
                a.active && current_ledger <= a.expiry_ledger
            }
            None => false,
        }
    }

    /// Get remaining spending capacity for a given allowance.
    pub fn get_remaining_capacity(env: Env, owner: Address, relayer: Address) -> i128 {
        let key = AllowanceKey(owner, relayer);
        match env.storage().persistent().get::<_, RelayerAllowance>(&key) {
            Some(a) => {
                let current_ledger = env.ledger().sequence();
                if !a.active || current_ledger > a.expiry_ledger {
                    0
                } else {
                    a.max_amount - a.spent_amount
                }
            }
            None => 0,
        }
    }

    /// Get the admin address.
    pub fn get_admin(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Admin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger, LedgerInfo};
    use soroban_sdk::{Env};

    fn setup() -> (Env, RelayerAllowanceContractClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();
        let id = env.register_contract(None, RelayerAllowanceContract);
        let client = RelayerAllowanceContractClient::new(&env, &id);
        (env, client)
    }

    fn advance_ledgers(env: &Env, count: u32) {
        let info = env.ledger().get();
        env.ledger().set(LedgerInfo {
            sequence_number: info.sequence_number + count,
            timestamp: info.timestamp,
            protocol_version: info.protocol_version,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 4096,
            min_persistent_entry_ttl: 4096,
            max_entry_ttl: 6_312_000,
        });
    }

    #[test]
    fn test_initialize() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let token = Address::generate(&env);

        client.initialize(&admin, &token);
        assert_eq!(client.get_admin(), Some(admin));
    }

    #[test]
    fn test_grant_allowance() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let token = Address::generate(&env);
        let owner = Address::generate(&env);
        let relayer = Address::generate(&env);

        client.initialize(&admin, &token);
        client.grant_allowance(&owner, &relayer, &1000_0000000, &500);

        let allowance = client.get_allowance(&owner, &relayer).unwrap();
        assert_eq!(allowance.max_amount, 1000_0000000);
        assert_eq!(allowance.spent_amount, 0);
        assert_eq!(allowance.expiry_ledger, 500);
        assert!(allowance.active);
    }

    #[test]
    fn test_is_valid_before_expiry() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let token = Address::generate(&env);
        let owner = Address::generate(&env);
        let relayer = Address::generate(&env);

        client.initialize(&admin, &token);
        client.grant_allowance(&owner, &relayer, &1000_0000000, &500);

        assert!(client.is_allowance_valid(&owner, &relayer));
    }

    #[test]
    fn test_is_expired_after_ledger() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let token = Address::generate(&env);
        let owner = Address::generate(&env);
        let relayer = Address::generate(&env);

        client.initialize(&admin, &token);
        client.grant_allowance(&owner, &relayer, &1000_0000000, &100);

        advance_ledgers(&env, 101);
        assert!(!client.is_allowance_valid(&owner, &relayer));
    }

    #[test]
    fn test_revoke_allowance() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let token = Address::generate(&env);
        let owner = Address::generate(&env);
        let relayer = Address::generate(&env);

        client.initialize(&admin, &token);
        client.grant_allowance(&owner, &relayer, &1000_0000000, &500);
        assert!(client.is_allowance_valid(&owner, &relayer));

        client.revoke_allowance(&owner, &relayer, &owner);
        assert!(!client.is_allowance_valid(&owner, &relayer));
    }

    #[test]
    fn test_get_remaining_capacity() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let token = Address::generate(&env);
        let owner = Address::generate(&env);
        let relayer = Address::generate(&env);

        client.initialize(&admin, &token);
        client.grant_allowance(&owner, &relayer, &1000_0000000, &500);

        assert_eq!(client.get_remaining_capacity(&owner, &relayer), 1000_0000000);
    }

    #[test]
    fn test_cannot_grant_zero_amount() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let token = Address::generate(&env);
        let owner = Address::generate(&env);
        let relayer = Address::generate(&env);

        client.initialize(&admin, &token);
        let result = client.try_grant_allowance(&owner, &relayer, &0, &500);
        assert_eq!(result, Err(Ok(AllowanceError::InvalidAmount)));
    }

    #[test]
    fn test_cannot_grant_expired_allowance() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let token = Address::generate(&env);
        let owner = Address::generate(&env);
        let relayer = Address::generate(&env);

        client.initialize(&admin, &token);
        let result = client.try_grant_allowance(&owner, &relayer, &1000_0000000, &0);
        assert_eq!(result, Err(Ok(AllowanceError::InvalidExpiry)));
    }
}
