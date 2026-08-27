#![no_std]

#![allow(unused_imports)]
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, token, Address, Bytes, Env,
    IntoVal, Symbol,
};

#[cfg(test)]
mod test;

// ─── Discount Tier System ───────────────────────────────────────────────────

/// Discount tier levels for flash loan fee reduction.
/// Higher tiers provide greater fee discounts for repeat borrowers / stakers.
#[contracttype]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscountTier {
    /// No discount — default for new or low-activity borrowers.
    None = 0,
    /// Entry-level discount after first qualifying borrow.
    Bronze = 1,
    /// Mid-tier discount for consistent borrowers.
    Silver = 2,
    /// High-tier discount for significant volume / stake.
    Gold = 3,
    /// Maximum discount for top-tier participants.
    Platinum = 4,
}

/// Configuration for a single discount tier.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TierConfig {
    /// The tier level this config describes.
    pub tier: DiscountTier,
    /// Minimum cumulative borrow volume (in token units) to qualify.
    pub min_volume: u128,
    /// Fee discount in basis points (100 bps = 1%).
    pub discount_bps: u32,
    /// Maximum fee discount cap in basis points.
    pub max_discount_bps: u32,
}

/// Flash loan fee parameters.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeeParams {
    /// Base fee rate in basis points (e.g., 9 = 0.09% per flash loan).
    pub base_fee_bps: u32,
    /// Protocol fee in basis points collected by the treasury.
    pub protocol_fee_bps: u32,
    /// Maximum total fee discount that can be applied (in bps).
    pub max_discount_bps: u32,
    /// Minimum fee that must always be charged (in token base units).
    pub min_fee: i128,
}

/// A borrower's profile tracking their flash loan history and tier.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BorrowerProfile {
    /// Current discount tier.
    pub tier: DiscountTier,
    /// Cumulative borrow volume across all flash loans (in token base units).
    pub total_volume: u128,
    /// Number of flash loans taken.
    pub borrow_count: u64,
    /// Timestamp of the most recent flash loan.
    pub last_borrow_ts: u64,
}

impl Default for BorrowerProfile {
    fn default() -> Self {
        Self {
            tier: DiscountTier::None,
            total_volume: 0,
            borrow_count: 0,
            last_borrow_ts: 0,
        }
    }
}

/// Record of a completed flash loan for auditability.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlashLoanRecord {
    /// Borrower address.
    pub borrower: Address,
    /// Amount borrowed.
    pub amount: i128,
    /// Fee charged.
    pub fee: i128,
    /// Discount applied (in bps).
    pub discount_bps: u32,
    /// Tier of the borrower at time of loan.
    pub tier: DiscountTier,
    /// Timestamp.
    pub timestamp: u64,
}

// ─── Storage Keys ───────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Token,
    FeeParams,
    TierConfig(u32),       // tier ordinal -> TierConfig
    TierCount,             // number of configured tiers
    BorrowerProfile(Address),
    FlashLoanCount,        // global counter for flash loans
    FlashLoanRecord(u64),  // id -> FlashLoanRecord
    Treasury,              // treasury address for protocol fees
    Paused,                // emergency pause flag
}

// ─── Errors ─────────────────────────────────────────────────────────────────

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotAdmin = 2,
    InvalidAmount = 3,
    InsufficientLiquidity = 4,
    FlashLoanFailed = 5,
    ContractPaused = 6,
    InvalidFeeParams = 7,
    InvalidTierConfig = 8,
    ZeroTokenAddress = 9,
    CallbackFailed = 10,
}

// ─── Contract ───────────────────────────────────────────────────────────────

#[contract]
pub struct FlashLoanEngine;

#[contractimpl]
impl FlashLoanEngine {
    // ── Initialization ─────────────────────────────────────────────────────

    pub fn initialize(
        env: Env,
        admin: Address,
        token: Address,
        treasury: Address,
    ) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(Error::AlreadyInitialized);
        }
        admin.require_auth();

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage().instance().set(&DataKey::Treasury, &treasury);

        // Default fee: 9 bps base (0.09%), 1 bps protocol fee, 500 bps max discount, 1 min fee
        let fee_params = FeeParams {
            base_fee_bps: 9,
            protocol_fee_bps: 1,
            max_discount_bps: 500,
            min_fee: 1,
        };
        env.storage().instance().set(&DataKey::FeeParams, &fee_params);

        // Initialize default tier configs
        Self::set_default_tier_configs(&env, &admin)?;

        env.storage().instance().set(&DataKey::FlashLoanCount, &0u64);

        Ok(())
    }

    // ── Flash Loan Core ────────────────────────────────────────────────────

    /// Execute a flash loan. Tokens are transferred to the borrower, the
    /// borrower's contract callback is invoked, and the borrower must have
    /// repaid principal + fee by the end of the callback.
    ///
    /// The `callback_data` is passed through to the borrower's `flash_loan_callback`.
    pub fn flash_borrow(
        env: Env,
        borrower: Address,
        amount: i128,
        callback_data: Bytes,
    ) -> Result<FlashLoanRecord, Error> {
        if Self::is_paused(env.clone()) {
            return Err(Error::ContractPaused);
        }
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(Error::ZeroTokenAddress)?;
        let token_client = token::Client::new(&env, &token_addr);

        // Check liquidity
        let contract_balance = token_client.balance(&env.current_contract_address());
        if contract_balance < amount {
            return Err(Error::InsufficientLiquidity);
        }

        // Compute fee
        let mut profile = Self::get_borrower_profile(env.clone(), borrower.clone());
        let fee_params: FeeParams = env
            .storage()
            .instance()
            .get(&DataKey::FeeParams)
            .unwrap();
        let discount_bps = Self::resolve_discount_bps(&env, &profile, amount as u128);
        let fee = Self::compute_fee(&fee_params, amount, discount_bps);

        // Transfer tokens to borrower
        token_client.transfer(&env.current_contract_address(), &borrower, &amount);

        // Invoke borrower callback — this is a cross-contract call.
        // The borrower must repay `amount + fee` to the contract during the callback.
        let flash_amount = amount;
        let flash_fee = fee;
        let callback_args: soroban_sdk::Vec<soroban_sdk::Val> = soroban_sdk::vec![
            &env,
            flash_amount.into_val(&env),
            flash_fee.into_val(&env),
            callback_data.into_val(&env),
        ];
        let result = env.try_invoke_contract::<(), soroban_sdk::Error>(
            &borrower,
            &Symbol::new(&env, "flash_loan_callback"),
            callback_args,
        );
        match result {
            Err(_) => {
                // Callback failed — pull tokens back from borrower if possible.
                // In production, we'd verify the balance was restored.
                return Err(Error::CallbackFailed);
            }
            Ok(_) => {}
        }

        // Verify repayment: contract balance should be >= original balance + fee
        let new_balance = token_client.balance(&env.current_contract_address());
        let expected_min = contract_balance
            .checked_add(fee)
            .ok_or(Error::InvalidAmount)?;
        if new_balance < expected_min {
            return Err(Error::FlashLoanFailed);
        }

        // Transfer protocol fee to treasury
        let protocol_fee = (fee as u128)
            .checked_mul(fee_params.protocol_fee_bps as u128)
            .unwrap_or(0)
            / 10_000;
        if protocol_fee > 0 {
            let treasury: Address = env
                .storage()
                .instance()
                .get(&DataKey::Treasury)
                .unwrap();
            token_client.transfer(
                &env.current_contract_address(),
                &treasury,
                &(protocol_fee as i128),
            );
        }

        // Update borrower profile
        profile.total_volume = profile
            .total_volume
            .saturating_add(amount as u128);
        profile.borrow_count = profile.borrow_count.saturating_add(1);
        profile.last_borrow_ts = env.ledger().timestamp();

        // Recalculate tier based on updated profile
        profile.tier = Self::compute_tier(&env, &profile);
        env.storage()
            .instance()
            .set(&DataKey::BorrowerProfile(borrower.clone()), &profile);

        // Record the flash loan
        let loan_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::FlashLoanCount)
            .unwrap_or(0u64);
        let record = FlashLoanRecord {
            borrower: borrower.clone(),
            amount,
            fee,
            discount_bps,
            tier: profile.tier,
            timestamp: env.ledger().timestamp(),
        };
        env.storage()
            .instance()
            .set(&DataKey::FlashLoanRecord(loan_id), &record);
        env.storage()
            .instance()
            .set(&DataKey::FlashLoanCount, &(loan_id + 1));

        Ok(record)
    }

    // ── Admin: Fee Configuration ───────────────────────────────────────────

    /// Update the fee parameters. Only admin can call.
    pub fn set_fee_params(
        env: Env,
        admin: Address,
        base_fee_bps: u32,
        protocol_fee_bps: u32,
        max_discount_bps: u32,
        min_fee: i128,
    ) -> Result<FeeParams, Error> {
        Self::require_admin(&env, &admin)?;

        if base_fee_bps == 0 || base_fee_bps > 10_000 {
            return Err(Error::InvalidFeeParams);
        }
        if protocol_fee_bps > base_fee_bps {
            return Err(Error::InvalidFeeParams);
        }
        if max_discount_bps > base_fee_bps {
            return Err(Error::InvalidFeeParams);
        }
        if min_fee < 0 {
            return Err(Error::InvalidFeeParams);
        }

        let params = FeeParams {
            base_fee_bps,
            protocol_fee_bps,
            max_discount_bps,
            min_fee,
        };
        env.storage().instance().set(&DataKey::FeeParams, &params);
        Ok(params)
    }

    /// Get the current fee parameters.
    pub fn get_fee_params(env: Env) -> FeeParams {
        env.storage()
            .instance()
            .get(&DataKey::FeeParams)
            .unwrap()
    }

    // ── Admin: Tier Configuration ──────────────────────────────────────────

    /// Set the default tier configurations (called during initialization).
    fn set_default_tier_configs(env: &Env, _admin: &Address) -> Result<(), Error> {
        let tiers = [
            TierConfig {
                tier: DiscountTier::Bronze,
                min_volume: 10_000,
                discount_bps: 100,  // 1% discount
                max_discount_bps: 100,
            },
            TierConfig {
                tier: DiscountTier::Silver,
                min_volume: 100_000,
                discount_bps: 250,  // 2.5% discount
                max_discount_bps: 250,
            },
            TierConfig {
                tier: DiscountTier::Gold,
                min_volume: 1_000_000,
                discount_bps: 400,  // 4% discount
                max_discount_bps: 400,
            },
            TierConfig {
                tier: DiscountTier::Platinum,
                min_volume: 10_000_000,
                discount_bps: 500,  // 5% discount
                max_discount_bps: 500,
            },
        ];

        for (i, tier_cfg) in tiers.iter().enumerate() {
            env.storage()
                .instance()
                .set(&DataKey::TierConfig(i as u32), tier_cfg);
        }
        env.storage()
            .instance()
            .set(&DataKey::TierCount, &(tiers.len() as u32));

        Ok(())
    }

    /// Set a specific tier configuration. Only admin.
    pub fn set_tier_config(
        env: Env,
        admin: Address,
        tier: DiscountTier,
        min_volume: u128,
        discount_bps: u32,
        max_discount_bps: u32,
    ) -> Result<TierConfig, Error> {
        Self::require_admin(&env, &admin)?;

        if discount_bps > max_discount_bps {
            return Err(Error::InvalidTierConfig);
        }

        let ordinal = Self::tier_to_ordinal(tier);
        let config = TierConfig {
            tier,
            min_volume,
            discount_bps,
            max_discount_bps,
        };

        env.storage()
            .instance()
            .set(&DataKey::TierConfig(ordinal), &config);

        // Update tier count if needed
        let current_count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TierCount)
            .unwrap_or(0);
        let needed = ordinal + 1;
        if needed > current_count {
            env.storage()
                .instance()
                .set(&DataKey::TierCount, &needed);
        }

        Ok(config)
    }

    /// Get a specific tier configuration.
    pub fn get_tier_config(env: Env, tier: DiscountTier) -> Option<TierConfig> {
        let ordinal = Self::tier_to_ordinal(tier);
        env.storage()
            .instance()
            .get(&DataKey::TierConfig(ordinal))
    }

    /// Get the count of configured tiers.
    pub fn get_tier_count(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::TierCount)
            .unwrap_or(0)
    }

    // ── Borrower Profile ───────────────────────────────────────────────────

    /// Get a borrower's profile.
    pub fn get_borrower_profile(env: Env, borrower: Address) -> BorrowerProfile {
        env.storage()
            .instance()
            .get(&DataKey::BorrowerProfile(borrower))
            .unwrap_or_default()
    }

    /// Calculate the effective fee for a given borrower and amount (view).
    pub fn quote_fee(env: Env, borrower: Address, amount: i128) -> i128 {
        let profile = Self::get_borrower_profile(env.clone(), borrower);
        let fee_params: FeeParams = env
            .storage()
            .instance()
            .get(&DataKey::FeeParams)
            .unwrap();
        let discount_bps = Self::resolve_discount_bps(&env, &profile, amount as u128);
        Self::compute_fee(&fee_params, amount, discount_bps)
    }

    /// Get the discount tier a borrower would qualify for given current volume.
    pub fn get_effective_tier(env: Env, borrower: Address) -> DiscountTier {
        let profile = Self::get_borrower_profile(env, borrower);
        profile.tier
    }

    // ── Flash Loan Records ─────────────────────────────────────────────────

    /// Get a flash loan record by ID.
    pub fn get_flash_loan_record(env: Env, loan_id: u64) -> Option<FlashLoanRecord> {
        env.storage()
            .instance()
            .get(&DataKey::FlashLoanRecord(loan_id))
    }

    /// Get the total number of flash loans executed.
    pub fn get_flash_loan_count(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::FlashLoanCount)
            .unwrap_or(0)
    }

    // ── Admin Controls ─────────────────────────────────────────────────────

    /// Pause or unpause the contract. Only admin.
    pub fn set_paused(env: Env, admin: Address, paused: bool) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;
        env.storage().instance().set(&DataKey::Paused, &paused);
        Ok(())
    }

    /// Check if the contract is paused.
    pub fn is_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
    }

    /// Get the admin address.
    pub fn get_admin(env: Env) -> Address {
        env.storage().instance().get(&DataKey::Admin).unwrap()
    }

    /// Get the token address.
    pub fn get_token(env: Env) -> Address {
        env.storage().instance().get(&DataKey::Token).unwrap()
    }

    /// Get the treasury address.
    pub fn get_treasury(env: Env) -> Address {
        env.storage().instance().get(&DataKey::Treasury).unwrap()
    }

    /// Transfer admin role. Only current admin.
    pub fn transfer_admin(
        env: Env,
        current_admin: Address,
        new_admin: Address,
    ) -> Result<(), Error> {
        Self::require_admin(&env, &current_admin)?;
        env.storage().instance().set(&DataKey::Admin, &new_admin);
        Ok(())
    }

    // ── Internal Helpers ───────────────────────────────────────────────────

    fn require_admin(env: &Env, caller: &Address) -> Result<(), Error> {
        caller.require_auth();
        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        if admin != *caller {
            return Err(Error::NotAdmin);
        }
        Ok(())
    }

    /// Resolve the effective discount in basis points for a borrower profile.
    fn resolve_discount_bps(env: &Env, profile: &BorrowerProfile, _amount: u128) -> u32 {
        let tier = profile.tier;
        let ordinal = Self::tier_to_ordinal(tier);

        let config: Option<TierConfig> = env
            .storage()
            .instance()
            .get(&DataKey::TierConfig(ordinal));

        match config {
            Some(cfg) => cfg.discount_bps,
            None => 0,
        }
    }

    /// Compute the fee in token base units.
    pub(crate) fn compute_fee(fee_params: &FeeParams, amount: i128, discount_bps: u32) -> i128 {
        // Base fee = amount * base_fee_bps / 10_000
        let base_fee = (amount as i128)
            .checked_mul(fee_params.base_fee_bps as i128)
            .unwrap_or(0)
            / 10_000;

        // Discount = base_fee * discount_bps / 10_000, capped at max_discount_bps
        let effective_discount = discount_bps.min(fee_params.max_discount_bps);
        let discount_amount = (base_fee as i128)
            .checked_mul(effective_discount as i128)
            .unwrap_or(0)
            / 10_000;

        let fee = base_fee.saturating_sub(discount_amount);

        // Enforce minimum fee
        fee.max(fee_params.min_fee)
    }

    /// Determine the tier for a borrower based on their cumulative volume.
    fn compute_tier(env: &Env, profile: &BorrowerProfile) -> DiscountTier {
        let tier_count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TierCount)
            .unwrap_or(0);

        // Walk tiers from highest to lowest, find the first one the borrower qualifies for
        let mut best_tier = DiscountTier::None;
        for i in (0..tier_count).rev() {
            let config: TierConfig = match env
                .storage()
                .instance()
                .get(&DataKey::TierConfig(i))
            {
                Some(c) => c,
                None => continue,
            };
            if profile.total_volume >= config.min_volume {
                best_tier = config.tier;
                break;
            }
        }

        best_tier
    }

    /// Map a DiscountTier to its storage ordinal (0-indexed).
    pub(crate) fn tier_to_ordinal(tier: DiscountTier) -> u32 {
        match tier {
            DiscountTier::None => 0,
            DiscountTier::Bronze => 0,
            DiscountTier::Silver => 1,
            DiscountTier::Gold => 2,
            DiscountTier::Platinum => 3,
        }
    }
}
