//! Protocol Revenue Distribution Vault Handler (Issue #938).
//!
//! Automatically splits incoming protocol fee revenue between $veFLOW$ stakers
//! (70%) and the development reserve (30%), enforcing fixed allocation scalars
//! in contract state and emitting a structured `RevenueDistributed` event.

use soroban_sdk::{contracttype, symbol_short, token, Address, Env};

use crate::ContractError;

/// Fixed allocation scalar for $veFLOW$ stakers: 70% (7_000 basis points out of 10_000).
pub const STAKERS_ALLOCATION_BPS: u32 = 7_000;

/// Fixed allocation scalar for the development reserve: 30% (3_000 basis points out of 10_000).
pub const DEV_ALLOCATION_BPS: u32 = 3_000;

/// Basis point denominator (10_000 = 100%).
pub const BPS_DENOMINATOR: u32 = 10_000;

/// Storage keys for the revenue distribution vault configuration.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RevenueVaultKey {
    /// Token used to settle protocol fee revenue.
    RevenueToken,
    /// Address of the $veFLOW$ staker reward pool.
    StakersPool,
    /// Address of the development reserve account.
    DevReserve,
    /// Cumulative total revenue ever distributed.
    TotalDistributed,
}

/// Result breakdown emitted with the `RevenueDistributed` event.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RevenueDistributedEvent {
    /// Total revenue amount distributed in this call.
    pub total_amount: i128,
    /// Amount transferred to $veFLOW$ stakers (70%).
    pub stakers_share: i128,
    /// Amount transferred to the development reserve (30%).
    pub dev_share: i128,
    /// Ledger timestamp at distribution time.
    pub timestamp: u64,
}

/// Configure the revenue distribution vault.
///
/// Only the contract admin may call this.
pub fn initialize_vault(
    env: &Env,
    admin: Address,
    revenue_token: Address,
    stakers_pool: Address,
    dev_reserve: Address,
) {
    admin.require_auth();
    env.storage().persistent().set(&RevenueVaultKey::RevenueToken, &revenue_token);
    env.storage().persistent().set(&RevenueVaultKey::StakersPool, &stakers_pool);
    env.storage().persistent().set(&RevenueVaultKey::DevReserve, &dev_reserve);
    env.storage().persistent().set(&RevenueVaultKey::TotalDistributed, &0i128);
}

/// Distribute `total_revenue` of protocol fee revenue.
///
/// Splits the incoming amount using the fixed allocation scalars:
/// - $d_{stakers} = 0.70$ → `stakers_share = total_revenue * 7_000 / 10_000`
/// - $d_{dev} = 0.30$ → `dev_share = total_revenue * 3_000 / 10_000`
///
/// Emits a structured `RevenueDistributed` event with the full split breakdown.
///
/// # Errors
/// Returns [`ContractError::NotInitialized`] if the vault has not been configured.
/// Returns [`ContractError::AmountTooLow`] if `total_revenue` is zero.
pub fn distribute_revenue(
    env: &Env,
    caller: Address,
    total_revenue: i128,
) -> Result<RevenueDistributedEvent, ContractError> {
    caller.require_auth();

    if total_revenue <= 0 {
        return Err(ContractError::AmountTooLow);
    }

    let revenue_token: Address = env
        .storage()
        .persistent()
        .get(&RevenueVaultKey::RevenueToken)
        .ok_or(ContractError::NotInitialized)?;
    let stakers_pool: Address = env
        .storage()
        .persistent()
        .get(&RevenueVaultKey::StakersPool)
        .ok_or(ContractError::NotInitialized)?;
    let dev_reserve: Address = env
        .storage()
        .persistent()
        .get(&RevenueVaultKey::DevReserve)
        .ok_or(ContractError::NotInitialized)?;

    // Compute split using fixed allocation scalars.
    let stakers_share = total_revenue
        .checked_mul(STAKERS_ALLOCATION_BPS as i128)
        .ok_or(ContractError::Overflow)?
        / BPS_DENOMINATOR as i128;
    let dev_share = total_revenue
        .checked_mul(DEV_ALLOCATION_BPS as i128)
        .ok_or(ContractError::Overflow)?
        / BPS_DENOMINATOR as i128;

    // Verify the split sums to the total (guards against rounding drift).
    if stakers_share + dev_share != total_revenue {
        return Err(ContractError::FeeDistributionMismatch);
    }

    // Transfer to stakers pool and development reserve.
    let token_client = token::Client::new(env, &revenue_token);
    token_client.transfer(&caller, &stakers_pool, &stakers_share);
    token_client.transfer(&caller, &dev_reserve, &dev_share);

    // Update cumulative distributed total.
    let prev_total: i128 = env
        .storage()
        .persistent()
        .get(&RevenueVaultKey::TotalDistributed)
        .unwrap_or(0i128);
    let new_total = prev_total
        .checked_add(total_revenue)
        .ok_or(ContractError::Overflow)?;
    env.storage().persistent().set(&RevenueVaultKey::TotalDistributed, &new_total);

    let event = RevenueDistributedEvent {
        total_amount: total_revenue,
        stakers_share,
        dev_share,
        timestamp: env.ledger().timestamp(),
    };

    // Emit structured RevenueDistributed event.
    env.events().publish(
        (symbol_short!("RevDist"), caller),
        event.clone(),
    );

    Ok(event)
}

/// Return the cumulative total revenue distributed to date.
pub fn get_total_distributed(env: &Env) -> i128 {
    env.storage()
        .persistent()
        .get(&RevenueVaultKey::TotalDistributed)
        .unwrap_or(0i128)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_scalars_sum_to_bps_denominator() {
        assert_eq!(STAKERS_ALLOCATION_BPS + DEV_ALLOCATION_BPS, BPS_DENOMINATOR);
    }

    #[test]
    fn split_calculation_is_correct() {
        let total: i128 = 1_000_000;
        let stakers = total * STAKERS_ALLOCATION_BPS as i128 / BPS_DENOMINATOR as i128;
        let dev = total * DEV_ALLOCATION_BPS as i128 / BPS_DENOMINATOR as i128;
        assert_eq!(stakers, 700_000);
        assert_eq!(dev, 300_000);
        assert_eq!(stakers + dev, total);
    }

    #[test]
    fn stakers_allocation_is_seventy_percent() {
        assert_eq!(STAKERS_ALLOCATION_BPS, 7_000);
    }

    #[test]
    fn dev_allocation_is_thirty_percent() {
        assert_eq!(DEV_ALLOCATION_BPS, 3_000);
    }
}
