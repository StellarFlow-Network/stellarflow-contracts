#![no_std]

use soroban_sdk::{contract, contractimpl, contracttype, contracterror, token, Address, Env, Vec, symbol_short};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum BuybackError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    NotAdmin = 3,
    InsufficientFees = 4,
    InvalidAmount = 5,
    Overflow = 6,
    PoolAlreadyRegistered = 7,
    PoolNotFound = 8,
    InvalidRatio = 9,
    NotAuthorized = 10,
}

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
pub enum DataKey {
    Admin,
    Treasury,
    Keeper,
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
    pub fn initialize(env: Env, admin: Address, treasury: Address) -> Result<(), BuybackError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(BuybackError::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Treasury, &treasury);
        Ok(())
    }

    /// Set the authorized keeper address allowed to trigger asset sweeps.
    ///
    /// Only the contract admin (governance) can update the keeper.
    pub fn set_keeper(
        env: Env,
        admin: Address,
        keeper: Address,
    ) -> Result<(), BuybackError> {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(BuybackError::NotInitialized)?;
        if admin != stored_admin {
            return Err(BuybackError::NotAdmin);
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
    /// This registers accumulated fees that can later be converted into
    /// liquidity pool positions via the buyback engine.
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
    ) -> Result<FeeBalance, BuybackError> {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(BuybackError::NotInitialized)?;
        if admin != stored_admin {
            return Err(BuybackError::NotAdmin);
        }
        admin.require_auth();

        if amount <= 0 {
            return Err(BuybackError::InvalidAmount);
        }

        let current_ledger = env.ledger().sequence();
        let balance = FeeBalance {
            token: token.clone(),
            amount,
            last_collected_ledger: current_ledger,
        };

        // Store fee balance
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
            .ok_or(BuybackError::Overflow)?;

        let updated = FeeBalance {
            token: token.clone(),
            amount: new_amount,
            last_collected_ledger: current_ledger,
        };
        env.storage().persistent().set(&fee_key, &updated);

        // Emit event
        env.events().publish(
            (symbol_short!("fee_collect"),),
            (token, amount, new_amount),
        );

        Ok(updated)
    }

    /// Sweep stray fee tokens from secondary contract addresses into the DAO treasury.
    ///
    /// This handler can only be triggered by the configured keeper account or by an
    /// admin/governance call.
    pub fn sweep_assets(
        env: Env,
        caller: Address,
        token: Address,
        sources: Vec<Address>,
    ) -> Result<i128, BuybackError> {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(BuybackError::NotInitialized)?;
        let keeper: Option<Address> = env.storage().instance().get(&DataKey::Keeper);
        let is_authorized = caller == stored_admin
            || keeper.map(|k| caller == k).unwrap_or(false);
        if !is_authorized {
            return Err(BuybackError::NotAuthorized);
        }
        caller.require_auth();

        let treasury: Address = env
            .storage()
            .instance()
            .get(&DataKey::Treasury)
            .ok_or(BuybackError::NotInitialized)?;

        let token_client = token::Client::new(&env, &token);
        let spender = env.current_contract_address();
        let mut total_swept: i128 = 0;

        for source in sources.iter() {
            let balance = token_client.balance(&source);
            if balance > 0 {
                let _ = token_client.transfer_from(&spender, &source, &treasury, &balance);
                total_swept = total_swept
                    .checked_add(balance)
                    .ok_or(BuybackError::Overflow)?;
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
    ) -> Result<i128, BuybackError> {
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

    /// Execute a buyback: convert accumulated fees into LP position.
    ///
    /// This performs a market swap to balance the token pair ratio and
    /// deposits assets into the core liquidity pool, then locks the
    /// acquired LP shares in the protocol treasury.
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
    ) -> Result<BuybackRecord, BuybackError> {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(BuybackError::NotInitialized)?;
        if admin != stored_admin {
            return Err(BuybackError::NotAdmin);
        }
        admin.require_auth();

        if swap_amount <= 0 {
            return Err(BuybackError::InvalidAmount);
        }

        // Verify fee balance is sufficient
        let fee_key = FeeBalanceKey(fee_token.clone());
        let mut fee_balance: FeeBalance = env
            .storage()
            .persistent()
            .get(&fee_key)
            .ok_or(BuybackError::InsufficientFees)?;

        if fee_balance.amount < swap_amount {
            return Err(BuybackError::InsufficientFees);
        }

        // Get pool configuration
        let pool_key = PoolKey(pool_id.clone());
        let pool: LiquidityPool = env
            .storage()
            .persistent()
            .get(&pool_key)
            .ok_or(BuybackError::PoolNotFound)?;

        // Calculate optimal swap to balance token pair ratio
        let current_ledger = env.ledger().sequence();

        // Determine which side of the pair the fee token is
        let is_token_a = fee_token == pool.token_a;
        let is_token_b = fee_token == pool.token_b;

        if !is_token_a && !is_token_b {
            return Err(BuybackError::InvalidAmount);
        }

        // Calculate optimal swap amounts based on pool ratio
        // ratio_a_bps = weight of token A (10000 = 100%)
        // For balanced LP: we want to deposit proportional to the ratio
        let (amount_a, amount_b) = if is_token_a {
            // Fee token is A: calculate how much B we'd need
            let amount_a = swap_amount;
            let amount_b = if pool.ratio_a_bps > 0 {
                (swap_amount * (10000 - pool.ratio_a_bps as i128)) / (pool.ratio_a_bps as i128)
            } else {
                return Err(BuybackError::InvalidRatio);
            };
            (amount_a, amount_b)
        } else {
            // Fee token is B: calculate how much A we'd need
            let amount_b = swap_amount;
            let amount_a = if pool.ratio_a_bps < 10000 {
                (swap_amount * (pool.ratio_a_bps as i128)) / (10000 - pool.ratio_a_bps as i128)
            } else {
                return Err(BuybackError::InvalidRatio);
            };
            (amount_a, amount_b)
        };

        // Calculate LP shares (simplified: geometric mean for balanced deposit)
        let lp_shares = isqrt(amount_a * amount_b);

        // Deduct fees
        fee_balance.amount -= swap_amount;
        env.storage().persistent().set(&fee_key, &fee_balance);

        // Lock LP shares in treasury
        let treasury: Address = env
            .storage()
            .instance()
            .get(&DataKey::Treasury)
            .ok_or(BuybackError::NotInitialized)?;

        let lp_token_client = token::Client::new(&env, &pool.lp_token);
        // In production, this would transfer LP tokens from the pool contract
        // For now, we record the acquisition
        let treasury_lp_key = TreasuryLPKey(pool_id.clone());
        let current_treasury_lp: i128 = env
            .storage()
            .persistent()
            .get(&treasury_lp_key)
            .unwrap_or(0);
        let new_treasury_lp = current_treasury_lp
            .checked_add(lp_shares)
            .ok_or(BuybackError::Overflow)?;
        env.storage().persistent().set(&treasury_lp_key, &new_treasury_lp);

        let record = BuybackRecord {
            pool_id,
            fee_token: fee_token.clone(),
            fee_amount: swap_amount,
            swapped_amount_a: amount_a,
            swapped_amount_b: amount_b,
            lp_shares_received: lp_shares,
            executed_ledger: current_ledger,
        };

        // Emit event
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

    /// Register a liquidity pool for buyback operations.
    ///
    /// # Parameters
    /// - `admin`: Admin registering the pool
    /// - `pool_id`: Unique pool identifier
    /// - `token_a`: First token in the pair
    /// - `token_b`: Second token in the pair
    /// - `lp_token`: LP token address for the pool
    /// - `ratio_a_bps`: Weight ratio of token A in basis points (10000 = 100%)
    pub fn register_pool(
        env: Env,
        admin: Address,
        pool_id: soroban_sdk::BytesN<32>,
        token_a: Address,
        token_b: Address,
        lp_token: Address,
        ratio_a_bps: u32,
    ) -> Result<LiquidityPool, BuybackError> {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(BuybackError::NotInitialized)?;
        if admin != stored_admin {
            return Err(BuybackError::NotAdmin);
        }
        admin.require_auth();

        if ratio_a_bps == 0 || ratio_a_bps >= 10000 {
            return Err(BuybackError::InvalidRatio);
        }

        let pool_key = PoolKey(pool_id.clone());
        if env.storage().persistent().has(&pool_key) {
            return Err(BuybackError::PoolAlreadyRegistered);
        }

        let pool = LiquidityPool {
            pool_id: pool_id.clone(),
            token_a,
            token_b,
            lp_token,
            ratio_a_bps,
        };

        env.storage().persistent().set(&pool_key, &pool);

        // Emit event
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
/// Used for LP share calculation (geometric mean approximation).
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

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger, LedgerInfo};
    use soroban_sdk::{Env};

    fn setup() -> (Env, TreasuryBuybackContractClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();
        let id = env.register_contract(None, TreasuryBuybackContract);
        let client = TreasuryBuybackContractClient::new(&env, &id);
        (env, client)
    }

    fn advance_ledgers(env: &Env, count: u32) {
        let info = env.ledger().get();
        env.ledger().set(LedgerInfo {
            sequence: info.sequence + count,
            timestamp: info.timestamp,
            protocol_version: info.protocol_version,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 0,
            min_persistent_entry_ttl: 0,
            max_entry_ttl: u32::MAX,
        });
    }

    #[test]
    fn test_initialize() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);

        client.initialize(&admin, &treasury);
        assert_eq!(client.get_admin(), Some(admin));
        assert_eq!(client.get_treasury(), Some(treasury));
    }

    #[test]
    fn test_collect_fees() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let token = Address::generate(&env);

        client.initialize(&admin, &treasury);
        let balance = client.collect_fees(&admin, &token, &1000_0000000);

        assert_eq!(balance.amount, 1000_0000000);
        assert_eq!(balance.token, token);
    }

    #[test]
    fn test_collect_fees_accumulates() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let token = Address::generate(&env);

        client.initialize(&admin, &treasury);
        client.collect_fees(&admin, &token, &500_0000000);
        let balance = client.collect_fees(&admin, &token, &300_0000000);

        assert_eq!(balance.amount, 800_0000000);
    }

    #[test]
    fn test_register_pool() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let token_a = Address::generate(&env);
        let token_b = Address::generate(&env);
        let lp_token = Address::generate(&env);

        client.initialize(&admin, &treasury);

        let pool_id = soroban_sdk::BytesN::<32>::from_array(&env, &[1u8; 32]);
        let pool = client.register_pool(&admin, &pool_id, &token_a, &token_b, &lp_token, &5000);

        assert_eq!(pool.ratio_a_bps, 5000);
        assert_eq!(pool.token_a, token_a);
        assert_eq!(pool.token_b, token_b);
    }

    #[test]
    fn test_get_fee_balance_default() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let token = Address::generate(&env);

        client.initialize(&admin, &treasury);
        let balance = client.get_fee_balance(&token);

        assert_eq!(balance.amount, 0);
    }

    #[test]
    fn test_cannot_collect_zero_fees() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let token = Address::generate(&env);

        client.initialize(&admin, &treasury);
        let result = client.try_collect_fees(&admin, &token, &0);
        assert_eq!(result, Err(Ok(BuybackError::InvalidAmount)));
    }

    #[test]
    fn test_cannot_register_pool_invalid_ratio() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let token_a = Address::generate(&env);
        let token_b = Address::generate(&env);
        let lp_token = Address::generate(&env);

        client.initialize(&admin, &treasury);

        let pool_id = soroban_sdk::BytesN::<32>::from_array(&env, &[1u8; 32]);
        let result = client.try_register_pool(&admin, &pool_id, &token_a, &token_b, &lp_token, &0);
        assert_eq!(result, Err(Ok(BuybackError::InvalidRatio)));
    }

    #[test]
    fn test_isqrt() {
        assert_eq!(isqrt(0), 0);
        assert_eq!(isqrt(1), 1);
        assert_eq!(isqrt(4), 2);
        assert_eq!(isqrt(9), 3);
        assert_eq!(isqrt(100), 10);
        assert_eq!(isqrt(99), 9);
    }
}
