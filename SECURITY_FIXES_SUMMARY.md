# Security Fixes Summary: Issues #1234 & #1253

This document details the security vulnerabilities identified, remediated, and verified across both the Solidity contract ecosystem and the Soroban / Rust architecture within `stellarflow-contracts`.

---

## 1. Issue #1234: Loss of Unclaimed Rewards on User Stake Withdrawal in `RewardPool.sol`

### 1.1 Vulnerability Overview
In staking accumulator systems (Synthetix-style reward pools), participant earnings are calculated using an accumulated global `rewardPerToken` index and the staker's tracked `userRewardPerTokenPaid`. When a staker withdraws their staked tokens, the contract updates user reward indices and reduces their staked balance.

In the previous implementation:
- Calling `withdraw()` directly reduced the user's staked balance `_balances[msg.sender]` to zero without atomically triggering payout of the accrued rewards.
- If a user unstaked their full balance before manually invoking `claimReward()`, the accumulator state was reset or rendered inaccessible, permanently forfeiting the earned rewards.

### 1.2 Security Impact
- **Loss of Unclaimed Rewards (Critical):** Users who withdraw their staked capital without first calling `claimReward()` permanently lose their earned yield.
- **Fairness & State Machine Inconsistency:** Protocol stakers providing liquidity and taking price risk are unfairly penalized upon exit.

### 1.3 Remediation
1. **Automated Atomic Reward Payout:**
   Updated `withdraw(uint256 amount)` in `src/RewardPool.sol` to automatically invoke `_claimReward(msg.sender)` **prior** to updating staked balances:
   ```solidity
   function withdraw(uint256 amount) public nonReentrant updateReward(msg.sender) {
       if (amount == 0) revert ZeroAmount();
       if (_balances[msg.sender] < amount) {
           revert InsufficientBalance(amount, _balances[msg.sender]);
       }

       // Remediation Step 1: Automatically trigger _claimReward(msg.sender) inside withdraw() prior to balance updates
       _claimReward(msg.sender);

       // Balance updates performed AFTER pending rewards are claimed and transferred
       _balances[msg.sender] -= amount;
       _totalSupply -= amount;

       bool success = stakingToken.transfer(msg.sender, amount);
       if (!success) revert StakingTransferFailed();

       emit Withdrawn(msg.sender, amount);
   }
   ```
2. **Persistence and Checks-Effects-Interactions:**
   In `_claimReward(address account)`, state is zeroed before the external transfer to prevent reentrancy attacks, and `RewardPaid` is emitted.
3. **Comprehensive Test Coverage:**
   Created Foundry test suite in `test/RewardPool.t.sol` verifying:
   - Total unstake automatically transfers accrued rewards without requiring a separate `claimReward()` call.
   - Partial unstake claims accumulated rewards while remaining stake continues to accrue rewards.
   - Reverts for zero amount and excess withdrawal attempts.

### 1.4 Implemented Files & Tests
- **Solidity Contract:** [`src/RewardPool.sol`](src/RewardPool.sol)
- **Solidity Test Suite:** [`test/RewardPool.t.sol`](test/RewardPool.t.sol)
- **Soroban / Rust Equivalent:** [`src/vaults/reward_pool.rs`](src/vaults/reward_pool.rs) with automated yield settlement in `withdraw_tokens`.
- **Rust Error Mapping:** `ContractError::RewardTransferFailed` in [`src/lib.rs`](src/lib.rs).

---

## 2. Issue #1253: Missing Expiry Timestamp in Off-Chain EIP-712 Meta-Transaction Payloads

### 2.1 Vulnerability Overview
Off-chain meta-transactions allow users to authorize state transitions off-chain using cryptographic signatures (EIP-712 typed structured data), which relayers submit on-chain.

In the previous meta-transaction schema:
- The typed structured data omitted an explicit `deadline` parameter.
- Once signed, authorizations remained valid indefinitely in transaction memory pools and off-chain relayer databases.

### 2.2 Security Impact
- **Indefinite Validity / Relayer Griefing (Medium):** Signed authorizations could be withheld and broadcasted at arbitrary future times when market conditions, contract state, or user intentions have shifted unfavorably.
- **Unbounded Authorization Window:** Users had no mechanism to invalidate an unmined meta-transaction other than burning nonces with on-chain transactions.

### 2.3 Remediation
1. **Added `uint256 deadline` to EIP-712 Typed Data Schemas:**
   ```solidity
   struct MetaTransaction {
       address from;
       address to;
       uint256 value;
       bytes data;
       uint256 nonce;
       uint256 deadline;
   }

   bytes32 public constant META_TRANSACTION_TYPEHASH = keccak256(
       "MetaTransaction(address from,address to,uint256 value,bytes data,uint256 nonce,uint256 deadline)"
   );
   ```
2. **Enforced On-Chain Expiry Validation:**
   Validated the timestamp requirement before nonce consumption and dispatch:
   ```solidity
   require(block.timestamp <= deadline, "Signature expired");
   ```
3. **Replay Protection & Cryptographic Rigor:**
   - Sequential nonce tracking per signer (`nonces[from]`).
   - Strict `s`-value bounds check (`s <= SECP256K1_N_DIV_2`) to prevent signature malleability.
   - Exact 65-byte signature length validation.

### 2.4 Implemented Files & Tests
- **Solidity Contract:** [`src/MetaTxHandler.sol`](src/MetaTxHandler.sol)
- **Solidity Test Suite:** [`test/MetaTxHandler.t.sol`](test/MetaTxHandler.t.sol)
  - `test_ValidMetaTransactionWithDeadlinePasses`: Valid signature with future deadline succeeds.
  - `test_RevertWhenSignatureExpired`: Transactions executed after deadline revert with `"Signature expired"`.
  - `test_RevertOnReplayedNonce`: Repeated execution with consumed nonce reverts with `InvalidNonce`.
  - `test_RevertOnTamperedDeadline`: Forging deadline alters EIP-712 digest and reverts.
- **Soroban / Rust Equivalent:** [`src/auth/meta_tx.rs`](src/auth/meta_tx.rs) validating `current_timestamp <= payload.deadline` with unit tests.
- **Rust Error Mapping:** `ContractError::SignatureExpired` in [`src/lib.rs`](src/lib.rs).

---

## 3. Cross-Platform Architectural Summary

| Issue | Severity | Core Vulnerability | Solidity Remediation (`src/`) | Soroban / Rust Counterpart |
| :--- | :--- | :--- | :--- | :--- |
| **#1234** | **CRITICAL** | Unclaimed rewards lost on full stake withdrawal | `_claimReward(msg.sender)` inside `withdraw()` prior to balance updates in `RewardPool.sol` | Yield auto-settlement in `src/vaults/reward_pool.rs` (`withdraw_tokens`) |
| **#1253** | **MEDIUM** | Indefinitely valid meta-tx without expiry deadline | `uint256 deadline` in EIP-712 struct + `require(block.timestamp <= deadline, "Signature expired")` in `MetaTxHandler.sol` | Expiry guard `validate_meta_tx_execution` in `src/auth/meta_tx.rs` (`SignatureExpired`) |
