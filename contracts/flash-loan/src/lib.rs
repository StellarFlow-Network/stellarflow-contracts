#![no_std]

//! Uncollateralized flash loan provider contract.
//!
//! Allows any caller to borrow an asset for the duration of a single
//! transaction provided the borrowed amount plus the protocol fee is returned
//! to the contract before the transaction completes.
//!
//! Flow:
//! 1. The borrower calls [`FlashLoan::flash_loan`].
//! 2. The requested asset balance is transferred to the target (borrowing)
//!    contract address.
//! 3. The provider invokes `exec_flash_loan_callback(asset, amount, fee)` on
//!    the borrowing contract, which may use the funds freely.
//! 4. On return from the callback the provider asserts its asset balance has
//!    grown by at least `principal + flash_loan_fee` (default `0.09%`,
//!    9 basis points). The whole operation reverts on any deficit.

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, token, Address, Env,
    IntoVal, Symbol,
};

/// Denominator for basis-point fees.
pub const BPS_DENOMINATOR: u64 = 10_000;

/// Default flash loan fee: 0.09% = 9 basis points.
pub const DEFAULT_FLASH_LOAN_FEE_BPS: u64 = 9;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum FlashLoanError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    NotAdmin = 3,
    InvalidAmount = 4,
    CallbackFailed = 5,
    DebtNotRepaid = 6,
    InsufficientLiquidity = 7,
    AlreadyInFlight = 8,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlashLoanResult {
    pub amount: i128,
    pub fee: i128,
    pub fee_bps: u64,
}

#[contracttype]
pub enum DataKey {
    Admin,
    Asset,
    FeeBps,
    AccruedFees,
    InFlight,
}

#[contract]
pub struct FlashLoan;

#[contractimpl]
impl FlashLoan {
    /// Initialize the provider with an admin and the lendable asset.
    ///
    /// When `fee_bps` is zero the default fee of 0.09% (9 bps) is used.
    pub fn initialize(
        env: Env,
        admin: Address,
        asset: Address,
        fee_bps: u64,
    ) -> Result<(), FlashLoanError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(FlashLoanError::AlreadyInitialized);
        }
        admin.require_auth();
        let fee_bps = if fee_bps == 0 {
            DEFAULT_FLASH_LOAN_FEE_BPS
        } else {
            fee_bps
        };
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Asset, &asset);
        env.storage().instance().set(&DataKey::FeeBps, &fee_bps);
        Ok(())
    }

    /// Execute a flash loan.
    ///
    /// Transfers `amount` of the configured asset to `borrower`, invokes
    /// `exec_flash_loan_callback(asset, amount, fee)` on it, then verifies the
    /// contract's asset balance covers the principal plus the protocol fee.
    ///
    /// Reverts with [`FlashLoanError::DebtNotRepaid`] if the borrowed asset is
    /// not returned (with fee) before the transaction completes.
    pub fn flash_loan(
        env: Env,
        borrower: Address,
        amount: i128,
    ) -> Result<FlashLoanResult, FlashLoanError> {
        if amount <= 0 {
            return Err(FlashLoanError::InvalidAmount);
        }
        borrower.require_auth();
        let asset = Self::asset(&env)?;
        let fee_bps = Self::fee_bps(&env);
        let fee = Self::compute_fee(amount, fee_bps);

        if env.storage().instance().get::<DataKey, bool>(&DataKey::InFlight) == Some(true) {
            return Err(FlashLoanError::AlreadyInFlight);
        }
        env.storage().instance().set(&DataKey::InFlight, &true);

        let token = token::Client::new(&env, &asset);
        let balance_before: i128 = token.balance(&env.current_contract_address());
        if balance_before < amount {
            env.storage().instance().set(&DataKey::InFlight, &false);
            return Err(FlashLoanError::InsufficientLiquidity);
        }

        token.transfer(&env.current_contract_address(), &borrower, &amount);

        let callback_args = soroban_sdk::vec![
            &env,
            asset.to_val(),
            amount.into_val(&env),
            fee.into_val(&env)
        ];
        let _: () = env.invoke_contract(
            &borrower,
            &Symbol::new(&env, "exec_flash_loan_callback"),
            callback_args,
        );

        let balance_after: i128 = token.balance(&env.current_contract_address());
        env.storage().instance().set(&DataKey::InFlight, &false);

        if balance_after < balance_before.checked_add(fee).ok_or(FlashLoanError::DebtNotRepaid)? {
            return Err(FlashLoanError::DebtNotRepaid);
        }

        let total_fees: i128 = env
            .storage()
            .instance()
            .get(&DataKey::AccruedFees)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::AccruedFees, &total_fees.checked_add(fee).ok_or(FlashLoanError::DebtNotRepaid)?);

        env.events().publish(
            (symbol_short!("fl_loan"),),
            (borrower, amount, fee),
        );

        Ok(FlashLoanResult {
            amount,
            fee,
            fee_bps,
        })
    }

    /// Update the flash loan fee (in basis points) charged to borrowers.
    pub fn set_fee_bps(env: Env, admin: Address, fee_bps: u64) -> Result<u64, FlashLoanError> {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(FlashLoanError::NotInitialized)?;
        if admin != stored_admin {
            return Err(FlashLoanError::NotAdmin);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::FeeBps, &fee_bps);
        Ok(fee_bps)
    }

    /// Have the admin sweep accrued flash loan fees to a recipient.
    ///
    /// Transfers the recorded (not yet withdrawn) fee balance out of the
    /// contract. The principal lent out is never part of the sweep.
    pub fn sweep_fees(
        env: Env,
        admin: Address,
        recipient: Address,
    ) -> Result<i128, FlashLoanError> {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(FlashLoanError::NotInitialized)?;
        if admin != stored_admin {
            return Err(FlashLoanError::NotAdmin);
        }
        admin.require_auth();

        let accrued: i128 = env
            .storage()
            .instance()
            .get(&DataKey::AccruedFees)
            .unwrap_or(0);
        if accrued > 0 {
            let asset = Self::asset(&env)?;
            token::Client::new(&env, &asset).transfer(
                &env.current_contract_address(),
                &recipient,
                &accrued,
            );
            env.storage().instance().set(&DataKey::AccruedFees, &0i128);
        }
        Ok(accrued)
    }

    /// Compute `amount * fee_bps / 10_000`.
    pub fn compute_fee(amount: i128, fee_bps: u64) -> i128 {
        amount.saturating_mul(fee_bps as i128) / (BPS_DENOMINATOR as i128)
    }

    pub fn get_admin(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Admin)
    }

    pub fn get_asset(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Asset)
    }

    pub fn get_fee_bps(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::FeeBps)
            .unwrap_or(DEFAULT_FLASH_LOAN_FEE_BPS)
    }

    pub fn get_accrued_fees(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::AccruedFees)
            .unwrap_or(0)
        }
}

impl FlashLoan {
    fn asset(env: &Env) -> Result<Address, FlashLoanError> {
        env.storage()
            .instance()
            .get(&DataKey::Asset)
            .ok_or(FlashLoanError::NotInitialized)
    }

    fn fee_bps(env: &Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::FeeBps)
            .unwrap_or(DEFAULT_FLASH_LOAN_FEE_BPS)
    }
}

/// Mock borrowing contract used in tests.
///
/// It implements the `exec_flash_loan_callback` interface expected by the
/// provider. Depending on the stored `MockRepay` flag it either repays the
/// full `amount + fee` back to the provider or keeps the funds (simulating a
/// borrower that fails to repay).
#[cfg(test)]
#[contract]
struct MockBorrower;

#[cfg(test)]
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
enum MockKey {
    Repay,
    Provider,
}

#[cfg(test)]
#[contractimpl]
impl MockBorrower {
    pub fn set_repay(env: Env, repay: bool) {
        env.storage().instance().set(&MockKey::Repay, &repay);
    }

    pub fn set_provider(env: Env, provider: Address) {
        env.storage().instance().set(&MockKey::Provider, &provider);
    }

    pub fn exec_flash_loan_callback(env: Env, asset: Address, amount: i128, fee: i128) {
        let repay: bool = env
            .storage()
            .instance()
            .get(&MockKey::Repay)
            .unwrap_or(true);
        if repay {
            let provider: Address = env
                .storage()
                .instance()
                .get(&MockKey::Provider)
                .unwrap_or_else(|| env.current_contract_address());
            soroban_sdk::token::Client::new(&env, &asset)
                .transfer(&env.current_contract_address(), &provider, &(amount + fee));
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::token::{Client as TokenClient, StellarAssetClient};
    use soroban_sdk::Env;

    fn setup() -> (Env, FlashLoanClient<'static>, Address, StellarAssetClient<'static>, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let token_admin = Address::generate(&env);
        let asset_id = env.register_stellar_asset_contract(token_admin.clone());
        let stellar = StellarAssetClient::new(&env, &asset_id);

        let contract_id = env.register_contract(None, FlashLoan);
        let client = FlashLoanClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        client.initialize(&admin, &asset_id, &0);
        (env, client, asset_id, stellar, admin)
    }

    fn register_mock(env: &Env) -> Address {
        env.register_contract(None, MockBorrower)
    }

    #[test]
    fn test_initialize_uses_default_fee() {
        let (_env, client, asset_id, _stellar, admin) = setup();
        assert_eq!(client.get_admin(), Some(admin));
        assert_eq!(client.get_asset(), Some(asset_id));
        assert_eq!(client.get_fee_bps(), DEFAULT_FLASH_LOAN_FEE_BPS);
    }

    #[test]
    fn test_flash_loan_succeeded_with_repayment() {
        let (env, client, asset_id, stellar, admin) = setup();
        stellar.mint(&client.address, &100_000);

        let borrower = register_mock(&env);
        let mock = MockBorrowerClient::new(&env, &borrower);
        mock.set_repay(&true);
        mock.set_provider(&client.address);
        // Give the borrower the funds needed to repay principal + fee (the
        // mock has no external treasury to draw arbitrage profits from).
        stellar.mint(&borrower, &10_009);

        let result = client.flash_loan(&borrower, &10_000);
        assert_eq!(result.amount, 10_000);
        // 0.09% fee on 10_000 = 9.
        assert_eq!(result.fee, 9);
        assert_eq!(result.fee_bps, DEFAULT_FLASH_LOAN_FEE_BPS);
        assert_eq!(client.get_accrued_fees(), 9);

        // Provider balance returned to ~original + fee.
        let token = TokenClient::new(&env, &asset_id);
        assert_eq!(token.balance(&client.address), 100_009);

        // Sweep the accrued fees.
        let swept = client.sweep_fees(&admin, &admin);
        assert_eq!(swept, 9);
        assert_eq!(client.get_accrued_fees(), 0);
    }

    #[test]
    fn test_flash_loan_reverts_when_borrower_does_not_repay() {
        let (env, client, _asset_id, stellar, _admin) = setup();
        stellar.mint(&client.address, &100_000);

        let borrower = register_mock(&env);
        MockBorrowerClient::new(&env, &borrower).set_repay(&false);

        let result = client.try_flash_loan(&borrower, &10_000);
        assert_eq!(result, Err(Ok(FlashLoanError::DebtNotRepaid)));
    }

    #[test]
    fn test_flash_loan_reverts_on_insufficient_liquidity() {
        let (env, client, _asset_id, _stellar, _admin) = setup();

        let borrower = register_mock(&env);
        MockBorrowerClient::new(&env, &borrower).set_repay(&true);

        // Provider holds no funds, so the loan cannot be extended.
        let result = client.try_flash_loan(&borrower, &10_000);
        assert_eq!(result, Err(Ok(FlashLoanError::InsufficientLiquidity)));
    }

    #[test]
    fn test_fee_computation_is_bps_based() {
        assert_eq!(FlashLoan::compute_fee(10_000, 9), 9);
        assert_eq!(FlashLoan::compute_fee(1_000_000, 9), 900);
        assert_eq!(FlashLoan::compute_fee(1_000, 50), 5);
        assert_eq!(FlashLoan::compute_fee(0, 9), 0);
    }
}