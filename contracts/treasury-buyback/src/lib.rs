#![no_std]

use soroban_sdk::{contract, contractimpl, contracttype, contracterror, token, Address, Bytes, Env, Vec, symbol_short};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ContractError {
    /// Recovery steps: Inspect the state for AlreadyInitialized and retry with valid inputs or proper conditions.
    AlreadyInitialized = 1,
    /// Recovery steps: Inspect the state for NotInitialized and retry with valid inputs or proper conditions.
    NotInitialized = 2,
    /// Recovery steps: Inspect the state for NotAdmin and retry with valid inputs or proper conditions.
    NotAdmin = 3,
    /// Recovery steps: Inspect the state for InsufficientFees and retry with valid inputs or proper conditions.
    InsufficientFees = 4,
    /// Recovery steps: Inspect the state for InvalidAmount and retry with valid inputs or proper conditions.
    InvalidAmount = 5,
    /// Recovery steps: Inspect the state for Overflow and retry with valid inputs or proper conditions.
    Overflow = 6,
    /// Recovery steps: Inspect the state for PoolAlreadyRegistered and retry with valid inputs or proper conditions.
    PoolAlreadyRegistered = 7,
    /// Recovery steps: Inspect the state for PoolNotFound and retry with valid inputs or proper conditions.
    PoolNotFound = 8,
    /// Recovery steps: Inspect the state for InvalidRatio and retry with valid inputs or proper conditions.
    InvalidRatio = 9,
    /// Recovery steps: Inspect the state for NotAuthorized and retry with valid inputs or proper conditions.
    NotAuthorized = 10,
    /// Recovery steps: Inspect the state for SurplusBelowCap and retry when treasury surplus exceeds the configured cap.
    SurplusBelowCap = 11,
    /// Recovery steps: Inspect the state for BuybackExceedsPoolCap and retry with an amount within 5% of pool depth.
    BuybackExceedsPoolCap = 12,
    /// Recovery steps: Inspect the state for InvalidBurnAddress and retry with a valid unspendable address.
    InvalidBurnAddress = 13,
}

/// Basis-point denominator: 10000 = 100%.
const BPS_DENOMINATOR: i128 = 10_000;
/// Maximum single-transaction buyback as a fraction of pool depth, in basis points.
/// 500 bps = 5% of the target AMM pool depth.
const MAX_SINGLE_BUYBACK_BPS: i128 = 500;

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct FeeBalance {
    pub token: Address,
    pub amount: i128,
    pub last_collected_ledger: u32,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct LiquidityPool {
    pub pool_id: soroban_sdk::BytesN<32>,
    pub token_a: Address,
    pub token_b: Address,
    pub lp_token: Address,
    pub ratio_a_bps: u32, // ratio of token A in basis points (10000 = 100%)
    /// Total depth of the pool in fee-token units, used for the buyback cap.
    pub pool_depth: i128,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct BuybackRecord {
    pub pool_id: soroban_sdk::BytesN<32>,
    pub fee_token: Address,
    pub fee_amount: i128,
    pub swapped_amount_a: i128,
    pub swapped_amount_b: i128,
    pub lp_shares_received: i128,
    pub executed_ledger: u32,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct BurnRecord {
    pub token: Address,
    pub amount: i128,
    pub burn_address: Address,
    pub executed_ledger: u32,
}

#[contracttype]
pub enum DataKey {
    Admin,
    Treasury,
    Keeper,
    /// Minimum treasury surplus (in fee-token units) required before a buyback may execute.
    SurplusCap,
    /// Default unspendable address to which acquired tokens are sent.
    BurnAddress,
}

#[contract]
pub struct TreasuryBuybackContract;

#[contractimpl]
impl TreasuryBuybackContract {
    /// Initialize the treasury buyback engine.
    ///
    /// # Parameters
    /// - `admin`: Admin address with management privileges
    /// - `treasury`: Protocol treasury address that holds LP shares
    /// - `surplus_cap`: Minimum treasury surplus (in fee-token units) required
    ///   before a buyback may execute. Set to 0 to allow immediate buybacks.
    /// - `burn_address`: Unspendable address to which acquired tokens are sent
    ///   during a burn operation. Must not equal the admin or treasury.
    pub fn initialize(
        env: Env,
        admin: Address,
        treasury: Address,
        surplus_cap: i128,
        burn_address: Address,
    ) -> Result<(), ContractError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(ContractError::AlreadyInitialized);
        }
        if surplus_cap < 0 {
            return Err(ContractError::InvalidAmount);
        }
        if burn_address == admin || burn_address == treasury {
            return Err(ContractError::InvalidBurnAddress);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Treasury, &treasury);
        env.storage().instance().set(&DataKey::SurplusCap, &surplus_cap);
        env.storage().instance().set(&DataKey::BurnAddress, &burn_address);
        Ok(())
    }

    /// Update the treasury surplus cap. Only the admin may call this.
    pub fn set_surplus_cap(
        env: Env,
        admin: Address,
        surplus_cap: i128,
    ) -> Result<(), ContractError> {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(ContractError::NotInitialized)?;
        if admin != stored_admin {
            return Err(ContractError::NotAdmin);
        }
        if surplus_cap < 0 {
            return Err(ContractError::InvalidAmount);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::SurplusCap, &surplus_cap);
        Ok(())
    }

    /// Get the configured treasury surplus cap.
    pub fn get_surplus_cap(env: Env) -> i128 {
        env.storage().instance().get(&DataKey::SurplusCap).unwrap_or(0)
    }

    /// Get the configured burn address.
    pub fn get_burn_address(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::BurnAddress)
    }

    /// Set the authorized keeper address allowed to trigger asset sweeps.
    ///
    /// Only the contract admin (governance) can update the keeper.
    pub fn set_keeper(
        env: Env,
        admin: Address,
        keeper: Address,
    ) -> Result<(), ContractError> {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(ContractError::NotInitialized)?;
        if admin != stored_admin {
            return Err(ContractError::NotAdmin);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Keeper, &keeper);
        Ok(())
    }

    /// Get the configured keeper address.
    pub fn get_keeper(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Keeper)
    }

    /// Collect accrued fee balances from a protocol contract.
    ///
    /// # Parameters
    /// - `admin`: Admin collecting fees
    /// - `token`: Address of the fee token
    /// - `amount`: Amount of fees collected
    pub fn collect_fees(
        env: Env,
        admin: Address,
        token: Address,
        amount: i128,
    ) -> Result<FeeBalance, ContractError> {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(ContractError::NotInitialized)?;
        if admin != stored_admin {
            return Err(ContractError::NotAdmin);
        }
        admin.require_auth();

        if amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }

        let current_ledger = env.ledger().sequence();
        let fee_key = FeeBalanceKey(token.clone());
        let existing: FeeBalance = env
            .storage()
            .persistent()
            .get(&fee_key)
            .unwrap_or(FeeBalance {
                token: token.clone(),
                amount: 0,
                last_collected_ledger: current_ledger,
            });

        let new_amount = existing
            .amount
            .checked_add(amount)
            .ok_or(ContractError::Overflow)?;

        let updated = FeeBalance {
            token: token.clone(),
            amount: new_amount,
            last_collected_ledger: current_ledger,
        };
        env.storage().persistent().set(&fee_key, &updated);

        env.events().publish(
            (symbol_short!("fee_collect"),),
            (token, amount, new_amount),
        );

        Ok(updated)
    }

    /// Sweep stray fee tokens from secondary contract addresses into the DAO treasury.
    pub fn sweep_assets(
        env: Env,
        caller: Address,
        token: Address,
        sources: Vec<Address>,
    ) -> Result<i128, ContractError> {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(ContractError::NotInitialized)?;
        let keeper: Option<Address> = env.storage().instance().get(&DataKey::Keeper);
        let is_authorized = caller == stored_admin
            || keeper.map(|k| caller == k).unwrap_or(false);
        if !is_authorized {
            return Err(ContractError::NotAuthorized);
        }
        caller.require_auth();

        let treasury: Address = env
            .storage()
            .instance()
            .get(&DataKey::Treasury)
            .ok_or(ContractError::NotInitialized)?;

        let token_client = token::Client::new(&env, &token);
        let spender = env.current_contract_address();
        let mut total_swept: i128 = 0;

        for source in sources.iter() {
            let balance = token_client.balance(&source);
            if balance > 0 {
                let _ = token_client.transfer_from(&spender, &source, &treasury, &balance);
                total_swept = total_swept
                    .checked_add(balance)
                    .ok_or(ContractError::Overflow)?;
            }
        }

        if total_swept > 0 {
            env.events().publish(
                (symbol_short!("sweep"),),
                (token, total_swept, treasury),
            );
        }

        Ok(total_swept)
    }

    /// Alias for `sweep_assets` for fee-specific callers.
    pub fn sweep_fees(
        env: Env,
        caller: Address,
        token: Address,
        sources: Vec<Address>,
    ) -> Result<i128, ContractError> {
        Self::sweep_assets(env, caller, token, sources)
    }

    /// Get the current fee balance for a specific token.
    pub fn get_fee_balance(env: Env, token: Address) -> FeeBalance {
        let fee_key = FeeBalanceKey(token.clone());
        env.storage().persistent().get(&fee_key).unwrap_or(FeeBalance {
            token,
            amount: 0,
            last_collected_ledger: 0,
        })
    }

    /// Register a liquidity pool for buyback operations.
    ///
    /// # Parameters
    /// - `admin`: Admin registering the pool
    /// - `pool_id`: Unique pool identifier
    /// - `token_a`: First token in the pair
    /// - `token_b`: Second token in the pair
    /// - `lp_token`: LP token address for the pool
    /// - `ratio_a_bps`: Weight ratio of token A in basis points (10000 = 100%)
    /// - `pool_depth`: Total depth of the pool in fee-token units, used to
    ///   enforce the 5% single-transaction buyback cap.
    pub fn register_pool(
        env: Env,
        admin: Address,
        pool_id: soroban_sdk::BytesN<32>,
        token_a: Address,
        token_b: Address,
        lp_token: Address,
        ratio_a_bps: u32,
        pool_depth: i128,
    ) -> Result<LiquidityPool, ContractError> {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(ContractError::NotInitialized)?;
        if admin != stored_admin {
            return Err(ContractError::NotAdmin);
        }
        admin.require_auth();

        if ratio_a_bps == 0 || ratio_a_bps >= 10000 {
            return Err(ContractError::InvalidRatio);
        }
        if pool_depth <= 0 {
            return Err(ContractError::InvalidAmount);
        }

        let pool_key = PoolKey(pool_id.clone());
        if env.storage().persistent().has(&pool_key) {
            return Err(ContractError::PoolAlreadyRegistered);
        }

        let pool = LiquidityPool {
            pool_id: pool_id.clone(),
            token_a,
            token_b,
            lp_token,
            ratio_a_bps,
            pool_depth,
        };

        env.storage().persistent().set(&pool_key, &pool);

        env.events().publish(
            (symbol_short!("pool_reg"),),
            (pool_id,),
        );

        Ok(pool)
    }

    /// Get liquidity pool details.
    pub fn get_pool(env: Env, pool_id: soroban_sdk::BytesN<32>) -> Option<LiquidityPool> {
        env.storage()
            .persistent()
            .get(&PoolKey(pool_id))
    }

    /// Get the total LP shares held in the treasury for a given pool.
    pub fn get_treasury_lp_shares(env: Env, pool_id: soroban_sdk::BytesN<32>) -> i128 {
        let treasury_lp_key = TreasuryLPKey(pool_id);
        env.storage()
            .persistent()
            .get(&treasury_lp_key)
            .unwrap_or(0)
    }

    /// Execute a guarded buyback: convert accumulated fees into an LP position,
    /// subject to the treasury-surplus cap and the 5% pool-depth limit.
    ///
    /// Guards enforced before any state mutation:
    /// 1. `fee_balance.amount >= surplus_cap` — the treasury surplus must meet
    ///    the configured cap, otherwise the buyback is rejected.
    /// 2. `swap_amount <= pool_depth * MAX_SINGLE_BUYBACK_BPS / BPS_DENOMINATOR`
    ///    — a single transaction may not exceed 5% of the target pool depth.
    ///
    /// # Parameters
    /// - `admin`: Admin executing the buyback
    /// - `pool_id`: Identifier of the target liquidity pool
    /// - `fee_token`: Address of the fee token to convert
    /// - `swap_amount`: Amount of fees to swap into LP position
    pub fn execute_buyback(
        env: Env,
        admin: Address,
        pool_id: soroban_sdk::BytesN<32>,
        fee_token: Address,
        swap_amount: i128,
    ) -> Result<BuybackRecord, ContractError> {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(ContractError::NotInitialized)?;
        if admin != stored_admin {
            return Err(ContractError::NotAdmin);
        }
        admin.require_auth();

        if swap_amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }

        // Guard 1: treasury surplus cap — buyback only when surplus meets the cap.
        let surplus_cap: i128 = env
            .storage()
            .instance()
            .get(&DataKey::SurplusCap)
            .unwrap_or(0);

        let fee_key = FeeBalanceKey(fee_token.clone());
        let mut fee_balance: FeeBalance = env
            .storage()
            .persistent()
            .get(&fee_key)
            .ok_or(ContractError::InsufficientFees)?;

        if fee_balance.amount < surplus_cap {
            return Err(ContractError::SurplusBelowCap);
        }
        if fee_balance.amount < swap_amount {
            return Err(ContractError::InsufficientFees);
        }

        // Guard 2: 5% pool-depth cap on a single transaction.
        let pool_key = PoolKey(pool_id.clone());
        let pool: LiquidityPool = env
            .storage()
            .persistent()
            .get(&pool_key)
            .ok_or(ContractError::PoolNotFound)?;

        let max_single = pool
            .pool_depth
            .checked_mul(MAX_SINGLE_BUYBACK_BPS)
            .ok_or(ContractError::Overflow)?
            .checked_div(BPS_DENOMINATOR)
            .ok_or(ContractError::Overflow)?;

        if swap_amount > max_single {
            return Err(ContractError::BuybackExceedsPoolCap);
        }

        // Determine which side of the pair the fee token is.
        let is_token_a = fee_token == pool.token_a;
        let is_token_b = fee_token == pool.token_b;

        if !is_token_a && !is_token_b {
            return Err(ContractError::InvalidAmount);
        }

        let (amount_a, amount_b) = if is_token_a {
            let amount_a = swap_amount;
            let amount_b = if pool.ratio_a_bps > 0 {
                (swap_amount * (10000 - pool.ratio_a_bps as i128)) / (pool.ratio_a_bps as i128)
            } else {
                return Err(ContractError::InvalidRatio);
            };
            (amount_a, amount_b)
        } else {
            let amount_b = swap_amount;
            let amount_a = if pool.ratio_a_bps < 10000 {
                (swap_amount * (pool.ratio_a_bps as i128)) / (10000 - pool.ratio_a_bps as i128)
            } else {
                return Err(ContractError::InvalidRatio);
            };
            (amount_a, amount_b)
        };

        let lp_shares = isqrt(amount_a * amount_b);

        // Deduct fees.
        fee_balance.amount -= swap_amount;
        env.storage().persistent().set(&fee_key, &fee_balance);

        // Record LP share acquisition in treasury.
        let treasury_lp_key = TreasuryLPKey(pool_id.clone());
        let current_treasury_lp: i128 = env
            .storage()
            .persistent()
            .get(&treasury_lp_key)
            .unwrap_or(0);
        let new_treasury_lp = current_treasury_lp
            .checked_add(lp_shares)
            .ok_or(ContractError::Overflow)?;
        env.storage().persistent().set(&treasury_lp_key, &new_treasury_lp);

        let current_ledger = env.ledger().sequence();
        let record = BuybackRecord {
            pool_id,
            fee_token: fee_token.clone(),
            fee_amount: swap_amount,
            swapped_amount_a: amount_a,
            swapped_amount_b: amount_b,
            lp_shares_received: lp_shares,
            executed_ledger: current_ledger,
        };

        env.events().publish(
            (symbol_short!("buyback"),),
            (
                record.fee_token,
                record.fee_amount,
                record.lp_shares_received,
            ),
        );

        Ok(record)
    }

    /// Burn acquired tokens permanently by sending them to the configured
    /// unspendable address.
    ///
    /// The tokens are transferred from the contract's own balance to the
    /// burn address recorded at initialization. Because the burn address is
    /// not controlled by any key that the protocol can sign with, the tokens
    /// are effectively removed from circulation.
    ///
    /// # Parameters
    /// - `admin`: Admin executing the burn
    /// - `token`: Address of the token to burn
    /// - `amount`: Amount to burn (must be > 0 and <= contract balance)
    pub fn execute_burn(
        env: Env,
        admin: Address,
        token: Address,
        amount: i128,
    ) -> Result<BurnRecord, ContractError> {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(ContractError::NotInitialized)?;
        if admin != stored_admin {
            return Err(ContractError::NotAdmin);
        }
        admin.require_auth();

        if amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }

        let burn_address: Address = env
            .storage()
            .instance()
            .get(&DataKey::BurnAddress)
            .ok_or(ContractError::NotInitialized)?;

        let treasury: Address = env
            .storage()
            .instance()
            .get(&DataKey::Treasury)
            .ok_or(ContractError::NotInitialized)?;

        let token_client = token::Client::new(&env, &token);
        let contract_balance = token_client.balance(&treasury);
        if contract_balance < amount {
            return Err(ContractError::InsufficientFees);
        }

        // Transfer from treasury to the unspendable burn address.
        let _ = token_client.transfer(&treasury, &burn_address, &amount);

        let current_ledger = env.ledger().sequence();
        let record = BurnRecord {
            token: token.clone(),
            amount,
            burn_address: burn_address.clone(),
            executed_ledger: current_ledger,
        };

        env.events().publish(
            (symbol_short!("burn"),),
            (token, amount, burn_address),
        );

        Ok(record)
    }

    /// Get the admin address.
    pub fn get_admin(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Admin)
    }

    /// Get the treasury address.
    pub fn get_treasury(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Treasury)
    }
}

// Storage key types
#[contracttype]
struct FeeBalanceKey(Address);

#[contracttype]
struct PoolKey(soroban_sdk::BytesN<32>);

#[contracttype]
struct TreasuryLPKey(soroban_sdk::BytesN<32>);

/// Integer square root using Newton's method.
fn isqrt(n: i128) -> i128 {
    if n <= 0 {
        return 0;
    }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}
