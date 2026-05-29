#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, panic_with_error, token, Address, Env, Vec,
};

#[derive(Clone)]
#[contracttype]
pub struct Recipient {
    pub address: Address,
    pub share: u32, // Percentage share (basis points: 10000 = 100%)
}

#[derive(Clone)]
#[contracttype]
pub enum DataKey {
    Admin,
    Token,
    Recipients,
    Initialized,
    TotalShares,
    DefaultAdmin,
    DefaultToken,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    Unauthorized = 2,
    InvalidShare = 3,
    TotalSharesExceeded = 4,
    NoRecipients = 5,
    InsufficientBalance = 6,
    ZeroAmount = 7,
    TokenNotSet = 8,
}

#[contract]
pub struct RewardSplitter;

#[contractimpl]
impl RewardSplitter {
    /// Initialize the contract with admin address and token to distribute
    pub fn initialize(env: Env, admin: Address, token: Address) {
        if env.storage().instance().has(&DataKey::Initialized) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }

        // Store current values as defaults
        env.storage().instance().set(&DataKey::DefaultAdmin, &admin);
        env.storage().instance().set(&DataKey::DefaultToken, &token);

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage()
            .instance()
            .set(&DataKey::Recipients, &Vec::<Recipient>::new(&env));
        env.storage().instance().set(&DataKey::TotalShares, &0u32);
        env.storage()
            .instance()
            .set(&DataKey::Initialized, &true);
    }

    /// Add a recipient with a fixed share percentage (in basis points)
    pub fn add_recipient(env: Env, admin: Address, recipient: Address, share: u32) {
        Self::require_admin(&env, &admin);

        if share == 0 || share > 10000 {
            panic_with_error!(&env, Error::InvalidShare);
        }

        let mut recipients: Vec<Recipient> = env
            .storage()
            .instance()
            .get(&DataKey::Recipients)
            .unwrap_or_else(|| Vec::new(&env));

        let mut total_shares: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TotalShares)
            .unwrap_or(0);

        // Check if total shares would exceed 10000 (100%)
        if total_shares + share > 10000 {
            panic_with_error!(&env, Error::TotalSharesExceeded);
        }

        // Add recipient
        recipients.push_back(Recipient {
            address: recipient,
            share,
        });

        total_shares += share;

        env.storage()
            .instance()
            .set(&DataKey::Recipients, &recipients);
        env.storage().instance().set(&DataKey::TotalShares, &total_shares);
    }

    /// Remove a recipient
    pub fn remove_recipient(env: Env, admin: Address, recipient: Address) {
        Self::require_admin(&env, &admin);

        let mut recipients: Vec<Recipient> = env
            .storage()
            .instance()
            .get(&DataKey::Recipients)
            .unwrap_or_else(|| Vec::new(&env));

        let mut total_shares: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TotalShares)
            .unwrap_or(0);

        let mut found = false;
        let mut new_recipients = Vec::new(&env);

        for r in recipients.iter() {
            if r.address == recipient {
                total_shares -= r.share;
                found = true;
            } else {
                new_recipients.push_back(r.clone());
            }
        }

        if !found {
            return; // Recipient not found, nothing to do
        }

        env.storage()
            .instance()
            .set(&DataKey::Recipients, &new_recipients);
        env.storage().instance().set(&DataKey::TotalShares, &total_shares);
    }

    /// Update a recipient's share
    pub fn update_recipient_share(env: Env, admin: Address, recipient: Address, new_share: u32) {
        Self::require_admin(&env, &admin);

        if new_share == 0 || new_share > 10000 {
            panic_with_error!(&env, Error::InvalidShare);
        }

        let mut recipients: Vec<Recipient> = env
            .storage()
            .instance()
            .get(&DataKey::Recipients)
            .unwrap_or_else(|| Vec::new(&env));

        let mut total_shares: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TotalShares)
            .unwrap_or(0);

        let mut found = false;
        let mut old_share = 0u32;

        for r in recipients.iter() {
            if r.address == recipient {
                old_share = r.share;
                found = true;
                break;
            }
        }

        if !found {
            return; // Recipient not found
        }

        // Check if new total would exceed 10000
        let new_total = total_shares - old_share + new_share;
        if new_total > 10000 {
            panic_with_error!(&env, Error::TotalSharesExceeded);
        }

        // Update the recipient
        let mut new_recipients = Vec::new(&env);
        for r in recipients.iter() {
            if r.address == recipient {
                new_recipients.push_back(Recipient {
                    address: recipient,
                    share: new_share,
                });
            } else {
                new_recipients.push_back(r.clone());
            }
        }

        env.storage()
            .instance()
            .set(&DataKey::Recipients, &new_recipients);
        env.storage().instance().set(&DataKey::TotalShares, &new_total);
    }

    /// Distribute tokens to all recipients according to their fixed shares
    pub fn distribute(env: Env, amount: i128) {
        if amount <= 0 {
            panic_with_error!(&env, Error::ZeroAmount);
        }

        let token: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or_else(|| panic_with_error!(&env, Error::TokenNotSet))
            .unwrap();

        let recipients: Vec<Recipient> = env
            .storage()
            .instance()
            .get(&DataKey::Recipients)
            .unwrap_or_else(|| Vec::new(&env));

        if recipients.is_empty() {
            panic_with_error!(&env, Error::NoRecipients);
        }

        let total_shares: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TotalShares)
            .unwrap_or(0);

        if total_shares == 0 {
            panic_with_error!(&env, Error::NoRecipients);
        }

        let contract_address = env.current_contract_address();
        let token_client = token::Client::new(&env, &token);

        // Check contract balance
        let balance = token_client.balance(&contract_address);
        if balance < amount {
            panic_with_error!(&env, Error::InsufficientBalance);
        }

        // Distribute to each recipient
        for recipient in recipients.iter() {
            let share_amount = (amount * recipient.share as i128) / total_shares as i128;
            if share_amount > 0 {
                token_client.transfer(&contract_address, &recipient.address, &share_amount);
            }
        }
    }

    /// Get all recipients
    pub fn get_recipients(env: Env) -> Vec<Recipient> {
        env.storage()
            .instance()
            .get(&DataKey::Recipients)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Get total shares
    pub fn get_total_shares(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::TotalShares)
            .unwrap_or(0)
    }

    /// Get admin address
    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap()
    }

    /// Get token address
    pub fn get_token(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Token)
            .unwrap()
    }

    /// Transfer admin to a new address
    pub fn transfer_admin(env: Env, current_admin: Address, new_admin: Address) {
        Self::require_admin(&env, &current_admin);
        env.storage().instance().set(&DataKey::Admin, &new_admin);
    }

    /// Update the token to distribute
    pub fn update_token(env: Env, admin: Address, new_token: Address) {
        Self::require_admin(&env, &admin);
        env.storage().instance().set(&DataKey::Token, &new_token);
    }

    /// Reset all parameters to their default values
    pub fn reset_parameters(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);

        let default_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::DefaultAdmin)
            .unwrap();
        let default_token: Address = env
            .storage()
            .instance()
            .get(&DataKey::DefaultToken)
            .unwrap();

        env.storage().instance().set(&DataKey::Admin, &default_admin);
        env.storage().instance().set(&DataKey::Token, &default_token);
        env.storage()
            .instance()
            .set(&DataKey::Recipients, &Vec::<Recipient>::new(&env));
        env.storage().instance().set(&DataKey::TotalShares, &0u32);
    }

    /// Get the default admin address
    pub fn get_default_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::DefaultAdmin)
            .unwrap()
    }

    /// Get the default token address
    pub fn get_default_token(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::DefaultToken)
            .unwrap()
    }

    /// Helper function to require admin authorization
    fn require_admin(env: &Env, admin: &Address) {
        admin.require_auth();
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap();
        if stored_admin != *admin {
            panic_with_error!(env, Error::Unauthorized);
        }
    }
}

mod test;
