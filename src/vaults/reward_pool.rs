//! Staking and reward distribution pool state machine (Issue #1234).
//!
//! Addresses loss of unclaimed rewards on user stake withdrawal.
//! When withdrawing staked balances, accrued rewards must be claimed automatically
//! prior to balance updates so users never forfeit earned incentives.

use soroban_sdk::{contracttype, Address, Env};
use crate::ContractError;

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct StakerPosition {
    pub staked_balance: i128,
    pub reward_per_token_paid: i128,
    pub accrued_rewards: i128,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct RewardPoolState {
    pub total_staked: i128,
    pub reward_rate: i128,
    pub last_update_time: u64,
    pub reward_per_token_stored: i128,
    pub period_finish: u64,
}

impl RewardPoolState {
    pub fn new(reward_rate: i128, duration: u64, start_time: u64) -> Self {
        Self {
            total_staked: 0,
            reward_rate,
            last_update_time: start_time,
            reward_per_token_stored: 0,
            period_finish: start_time + duration,
        }
    }

    pub fn last_time_applicable(&self, current_time: u64) -> u64 {
        if current_time < self.period_finish {
            current_time
        } else {
            self.period_finish
        }
    }

    pub fn update_reward_index(&mut self, current_time: u64) {
        let applicable_time = self.last_time_applicable(current_time);
        if self.total_staked > 0 && applicable_time > self.last_update_time {
            let elapsed = (applicable_time - self.last_update_time) as i128;
            let added_reward_per_token = (elapsed * self.reward_rate * 1_000_000_000_000_000_000) / self.total_staked;
            self.reward_per_token_stored += added_reward_per_token;
        }
        self.last_update_time = applicable_time;
    }
}

pub fn update_user_reward(pool: &RewardPoolState, pos: &mut StakerPosition) {
    if pos.staked_balance > 0 {
        let delta = pool.reward_per_token_stored - pos.reward_per_token_paid;
        let earned = (pos.staked_balance * delta) / 1_000_000_000_000_000_000;
        pos.accrued_rewards += earned;
    }
    pos.reward_per_token_paid = pool.reward_per_token_stored;
}

/// Stakes an amount into the pool.
pub fn stake_tokens(
    pool: &mut RewardPoolState,
    pos: &mut StakerPosition,
    amount: i128,
    current_time: u64,
) -> Result<(), ContractError> {
    if amount <= 0 {
        return Err(ContractError::AmountTooLow);
    }
    pool.update_reward_index(current_time);
    update_user_reward(pool, pos);

    pos.staked_balance += amount;
    pool.total_staked += amount;
    Ok(())
}

/// Withdraws staked tokens from the pool.
///
/// Remediation for Issue #1234:
/// Accrued rewards are automatically claimed and returned *before* balance updates
/// so the user never forfeits earned yield even upon full unstake.
pub fn withdraw_tokens(
    pool: &mut RewardPoolState,
    pos: &mut StakerPosition,
    amount: i128,
    current_time: u64,
) -> Result<(i128, i128), ContractError> {
    if amount <= 0 {
        return Err(ContractError::AmountTooLow);
    }
    if pos.staked_balance < amount {
        return Err(ContractError::InsufficientReserveBalance);
    }

    pool.update_reward_index(current_time);
    update_user_reward(pool, pos);

    // ── Issue #1234: Auto-claim accrued rewards prior to balance updates ────
    let claimed_rewards = pos.accrued_rewards;
    pos.accrued_rewards = 0;

    pos.staked_balance -= amount;
    pool.total_staked -= amount;

    Ok((amount, claimed_rewards))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auto_claim_rewards_on_unstaking() {
        let mut pool = RewardPoolState::new(10, 100, 1000);
        let mut pos = StakerPosition {
            staked_balance: 0,
            reward_per_token_paid: 0,
            accrued_rewards: 0,
        };

        // User stakes 100 tokens at t=1000
        assert!(stake_tokens(&mut pool, &mut pos, 100, 1000).is_ok());

        // Fast forward to t=1050 (50 seconds elapsed, 50 * 10 = 500 reward accrued)
        // User unstakes their ENTIRE balance
        let res = withdraw_tokens(&mut pool, &mut pos, 100, 1050);
        assert!(res.is_ok());

        let (withdrawn, claimed) = res.unwrap();
        assert_eq!(withdrawn, 100);
        assert_eq!(claimed, 500);
        assert_eq!(pos.staked_balance, 0);
        assert_eq!(pos.accrued_rewards, 0);
    }

    #[test]
    fn test_revert_on_excess_withdraw() {
        let mut pool = RewardPoolState::new(10, 100, 1000);
        let mut pos = StakerPosition {
            staked_balance: 50,
            reward_per_token_paid: 0,
            accrued_rewards: 0,
        };

        let res = withdraw_tokens(&mut pool, &mut pos, 100, 1010);
        assert!(res.is_err());
    }
}
