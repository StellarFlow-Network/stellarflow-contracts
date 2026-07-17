#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, panic_with_error, token, Address, Env, String, Vec,
};

#[derive(Clone)]
#[contracttype]
pub struct Recipient {
    pub address: Address,
    pub share: u32, // Percentage share (basis points: 10000 = 100%)
}

#[derive(Clone)]
#[contracttype]
pub enum CooldownActionType {
    UpdateToken,
    ResetParameters,
    EmergencyStop,
}

#[derive(Clone)]
#[contracttype]
pub struct CooldownAction {
    pub action_id: u64,
    pub action_type: CooldownActionType,
    pub proposed_by: Address,
    pub current_stage: u32,
    pub proposed_at: u64,
    pub data: soroban_sdk::String, // Additional data for the action
    pub executed: bool,
    pub cancelled: bool,
}

#[derive(Clone)]
#[contracttype]
pub struct CooldownStage {
    pub stage_number: u32,
    pub cooldown_seconds: u64,
    pub description: soroban_sdk::String,
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
    CooldownStage(u64),
    CooldownAction(u64),
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
    CooldownNotExpired = 9,
    ActionNotFound = 10,
    ActionAlreadyExecuted = 11,
    ActionAlreadyCancelled = 12,
    InvalidStage = 13,
    InvalidActionType = 14,
}

#[contract]
pub struct RewardSplitter;

// Default cooldown stages (in seconds)
const STAGE_1_COOLDOWN: u64 = 3_600; // 1 hour
const STAGE_2_COOLDOWN: u64 = 28_800; // 8 hours
const STAGE_3_COOLDOWN: u64 = 86_400; // 24 hours

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
        env.storage().instance().set(&DataKey::Initialized, &true);

        // Initialize default cooldown stages
        Self::initialize_default_cooldown_stages(&env);
    }

    /// Initialize default cooldown stages for governance actions
    fn initialize_default_cooldown_stages(env: &Env) {
        let stage1 = CooldownStage {
            stage_number: 1,
            cooldown_seconds: STAGE_1_COOLDOWN,
            description: String::from_str(env, "Initial proposal stage - 1 hour cooldown"),
        };
        let stage2 = CooldownStage {
            stage_number: 2,
            cooldown_seconds: STAGE_2_COOLDOWN,
            description: String::from_str(env, "Review stage - 8 hour cooldown"),
        };
        let stage3 = CooldownStage {
            stage_number: 3,
            cooldown_seconds: STAGE_3_COOLDOWN,
            description: String::from_str(env, "Final approval stage - 24 hour cooldown"),
        };

        env.storage()
            .instance()
            .set(&DataKey::CooldownStage(1), &stage1);
        env.storage()
            .instance()
            .set(&DataKey::CooldownStage(2), &stage2);
        env.storage()
            .instance()
            .set(&DataKey::CooldownStage(3), &stage3);
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
        env.storage()
            .instance()
            .set(&DataKey::TotalShares, &total_shares);
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
        env.storage()
            .instance()
            .set(&DataKey::TotalShares, &total_shares);
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
        env.storage()
            .instance()
            .set(&DataKey::TotalShares, &new_total);
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
        env.storage().instance().get(&DataKey::Admin).unwrap()
    }

    /// Get token address
    pub fn get_token(env: Env) -> Address {
        env.storage().instance().get(&DataKey::Token).unwrap()
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

        env.storage()
            .instance()
            .set(&DataKey::Admin, &default_admin);
        env.storage()
            .instance()
            .set(&DataKey::Token, &default_token);
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
        let stored_admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        if stored_admin != *admin {
            panic_with_error!(env, Error::Unauthorized);
        }
    }

    /// Propose a new governance action through the cooldown gateway
    pub fn propose_action(
        env: Env,
        admin: Address,
        action_type: CooldownActionType,
        data: String,
    ) -> u64 {
        Self::require_admin(&env, &admin);

        // Generate action ID (using timestamp as simple ID)
        let action_id = env.ledger().timestamp();
        let now = env.ledger().timestamp();

        let action = CooldownAction {
            action_id,
            action_type: action_type.clone(),
            proposed_by: admin,
            current_stage: 1,
            proposed_at: now,
            data,
            executed: false,
            cancelled: false,
        };

        env.storage()
            .instance()
            .set(&DataKey::CooldownAction(action_id), &action);

        action_id
    }

    /// Advance an action to the next cooldown stage
    pub fn advance_action(env: Env, admin: Address, action_id: u64) {
        Self::require_admin(&env, &admin);

        let mut action: CooldownAction = env
            .storage()
            .instance()
            .get(&DataKey::CooldownAction(action_id))
            .ok_or_else(|| panic_with_error!(&env, Error::ActionNotFound))
            .unwrap();

        if action.executed {
            panic_with_error!(&env, Error::ActionAlreadyExecuted);
        }
        if action.cancelled {
            panic_with_error!(&env, Error::ActionAlreadyCancelled);
        }

        // Prevent advancing past stage 3
        if action.current_stage > 3 {
            panic_with_error!(&env, Error::InvalidStage);
        }

        let current_stage = action.current_stage;
        let stage: CooldownStage = env
            .storage()
            .instance()
            .get(&DataKey::CooldownStage(current_stage as u64))
            .ok_or_else(|| panic_with_error!(&env, Error::InvalidStage))
            .unwrap();

        let now = env.ledger().timestamp();
        let stage_expiry = action.proposed_at + stage.cooldown_seconds;

        if now < stage_expiry {
            panic_with_error!(&env, Error::CooldownNotExpired);
        }

        // Advance to next stage
        action.current_stage += 1;
        env.storage()
            .instance()
            .set(&DataKey::CooldownAction(action_id), &action);
    }

    /// Execute a governance action after all cooldown stages complete
    pub fn execute_action(env: Env, admin: Address, action_id: u64) {
        Self::require_admin(&env, &admin);

        let mut action: CooldownAction = env
            .storage()
            .instance()
            .get(&DataKey::CooldownAction(action_id))
            .ok_or_else(|| panic_with_error!(&env, Error::ActionNotFound))
            .unwrap();

        if action.executed {
            panic_with_error!(&env, Error::ActionAlreadyExecuted);
        }
        if action.cancelled {
            panic_with_error!(&env, Error::ActionAlreadyCancelled);
        }

        // Check if all stages are complete (stage 4 means past stage 3)
        if action.current_stage < 4 {
            panic_with_error!(&env, Error::CooldownNotExpired);
        }

        // Execute the action based on type
        match action.action_type {
            CooldownActionType::UpdateToken => {
                let new_token: Address = Address::from_string(&env, &action.data);
                env.storage().instance().set(&DataKey::Token, &new_token);
            }
            CooldownActionType::ResetParameters => {
                Self::reset_parameters_internal(&env);
            }
            CooldownActionType::EmergencyStop => {
                // Emergency stop logic - could pause the contract
                // For now, this is a placeholder
            }
        }

        action.executed = true;
        env.storage()
            .instance()
            .set(&DataKey::CooldownAction(action_id), &action);
    }

    /// Cancel a pending governance action
    pub fn cancel_action(env: Env, admin: Address, action_id: u64) {
        Self::require_admin(&env, &admin);

        let mut action: CooldownAction = env
            .storage()
            .instance()
            .get(&DataKey::CooldownAction(action_id))
            .ok_or_else(|| panic_with_error!(&env, Error::ActionNotFound))
            .unwrap();

        if action.executed {
            panic_with_error!(&env, Error::ActionAlreadyExecuted);
        }
        if action.cancelled {
            panic_with_error!(&env, Error::ActionAlreadyCancelled);
        }

        action.cancelled = true;
        env.storage()
            .instance()
            .set(&DataKey::CooldownAction(action_id), &action);
    }

    /// Get the status of a governance action
    pub fn get_action(env: Env, action_id: u64) -> Option<CooldownAction> {
        env.storage()
            .instance()
            .get(&DataKey::CooldownAction(action_id))
    }

    /// Get the cooldown time remaining for the current stage
    pub fn get_cooldown_remaining(env: Env, action_id: u64) -> Option<u64> {
        let action: CooldownAction = env
            .storage()
            .instance()
            .get(&DataKey::CooldownAction(action_id))?;

        if action.executed || action.cancelled {
            return Some(0);
        }

        let stage: CooldownStage = env
            .storage()
            .instance()
            .get(&DataKey::CooldownStage(action.current_stage as u64))?;

        let now = env.ledger().timestamp();
        let stage_expiry = action.proposed_at + stage.cooldown_seconds;

        if now >= stage_expiry {
            Some(0)
        } else {
            Some(stage_expiry - now)
        }
    }

    /// Configure a cooldown stage (admin only)
    pub fn configure_cooldown_stage(
        env: Env,
        admin: Address,
        stage_number: u32,
        cooldown_seconds: u64,
        description: String,
    ) {
        Self::require_admin(&env, &admin);

        if stage_number < 1 || stage_number > 3 {
            panic_with_error!(&env, Error::InvalidStage);
        }

        let stage = CooldownStage {
            stage_number,
            cooldown_seconds,
            description,
        };

        env.storage()
            .instance()
            .set(&DataKey::CooldownStage(stage_number as u64), &stage);
    }

    /// Get a cooldown stage configuration
    pub fn get_cooldown_stage(env: Env, stage_number: u32) -> Option<CooldownStage> {
        env.storage()
            .instance()
            .get(&DataKey::CooldownStage(stage_number as u64))
    }

    /// Internal function to reset parameters (used by cooldown gateway)
    fn reset_parameters_internal(env: &Env) {
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

        env.storage()
            .instance()
            .set(&DataKey::Admin, &default_admin);
        env.storage()
            .instance()
            .set(&DataKey::Token, &default_token);
        env.storage()
            .instance()
            .set(&DataKey::Recipients, &Vec::<Recipient>::new(env));
        env.storage().instance().set(&DataKey::TotalShares, &0u32);
    }
}

mod test;
