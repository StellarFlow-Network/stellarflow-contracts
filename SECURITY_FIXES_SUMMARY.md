# Security Fixes Summary: Issues #1125 & #1226

This document summarizes the security vulnerabilities identified, remediated, and tested across both the Solidity contract layer and the Rust / Soroban contract architecture in `stellarflow-contracts`.

---

## 1. Issue #1125: Unchecked Low-Level `.call()` Return Value in Fund Router

### 1.1 Vulnerability Overview
The fund router contract processes native Ether distributions via low-level address calls (`address.call{value: amount}("")`). In the previous implementation, the boolean `success` return value from the low-level call was not captured or validated.

### 1.2 Impact
- **Silent Transfer Failures:** If the recipient address is a contract whose fallback/receive function consumes all gas, intentionally reverts, or rejects incoming transfers, the transfer fails silently without reverting execution.
- **State Desynchronization:** Internal accounting ledgers credit or debit participants without underlying native tokens moving.
- **Lost Funds & Misattribution:** Senders assume funds have been distributed, leading to financial discrepancies.

### 1.3 Remediation
1. **Explicit Return Value Capture:** Capture the boolean return value:
   ```solidity
   (bool success, ) = recipient.call{value: amount}("");
   ```
2. **Atomic Revert with Custom Error:** Immediately validate the success flag and revert with custom error `ETHTransferFailed()`:
   ```solidity
   if (!success) revert ETHTransferFailed();
   ```
3. **Comprehensive Validations:**
   - Enforce non-zero recipient addresses (`InvalidRecipient`).
   - Enforce positive transfer amounts (`InvalidAmount`).
   - Check available contract balance (`InsufficientBalance`).
   - Protect distribution functions against reentrancy (`nonReentrant`).
   - Validate array length parity in batch distributions (`ArrayLengthMismatch`).

### 1.4 Implemented Files & Tests
- **Solidity Contract:** [`src/FundRouter.sol`](src/FundRouter.sol)
- **Solidity Test Suite:** [`test/FundRouter.t.sol`](test/FundRouter.t.sol)
  - `test_SingleTransferSuccess`: Confirms normal transfer succeeds and balances update.
  - `test_SingleTransferRevertsOnFailedCall`: Asserts `ETHTransferFailed` when recipient reverts.
  - `test_BatchTransferSuccess`: Verifies multi-recipient distribution.
  - `test_BatchTransferRevertsAtomicallyIfAnyFails`: Asserts atomic rollback if any transfer in the batch fails.
- **Soroban / Rust Equivalent:** [`src/router/fund_router.rs`](src/router/fund_router.rs) & `ContractError::ETHTransferFailed` alias in [`src/lib.rs`](src/lib.rs).

---

## 2. Issue #1226: Signature Malleability via Unrestricted `s` Value in `SignatureChecker.sol`

### 2.1 Vulnerability Overview
ECDSA signatures on the secp256k1 curve consist of two scalar values `(r, s)` and a recovery identifier `v`. For every valid signature `(r, s, v)`, the signature `(r, n - s, v ^ 1)` is also mathematically valid for the exact same message hash, where `n` is the secp256k1 group order:
```
n = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141
```
When a verification routine uses raw `ecrecover` without restricting `s` to the lower half of the group order (`s <= n / 2`), an attacker or third party can manipulate an existing valid signature into an alternate valid signature without possessing the private key.

### 2.2 Impact
- **Signature Malleability:** Attackers can alter `s` and `v` to create a new signature hash that remains valid.
- **Replay & Front-Running Attacks:** In systems that authorize actions via signature hashes or nonce-less authorizations, malleable signatures can bypass duplicate detection, causing unauthorized duplicate payouts or replay actions.

### 2.3 Remediation
1. **Enforce Lower Half Curve Order Bound:**
   Enforce that `s` does not exceed `n / 2`:
   ```solidity
   bytes32 internal constant SECP256K1_N_DIV_2 =
       0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0;

   if (uint256(s) > uint256(SECP256K1_N_DIV_2)) {
       revert InvalidSignatureS();
   }
   ```
2. **Canonical Recovery Byte Enforcement:**
   Ensure `v` is strictly `27` or `28`:
   ```solidity
   if (v != 27 && v != 28) {
       revert InvalidSignatureV();
   }
   ```
3. **Length Validation:**
   Reject any signature with length not equal to 65 bytes (`InvalidSignatureLength()`).
4. **Zero Address Protection:**
   Ensure `ecrecover` does not return `address(0)`.

### 2.4 Implemented Files & Tests
- **Solidity Library:** [`src/utils/SignatureChecker.sol`](src/utils/SignatureChecker.sol)
- **Solidity Test Suite:** [`test/SignatureChecker.t.sol`](test/SignatureChecker.t.sol)
  - `test_RejectMalleableSignatureWithHighS`: Asserts revert with `InvalidSignatureS` when `s > n / 2`.
  - `test_RejectInvalidV`: Reverts with `InvalidSignatureV` when `v` is outside `{27, 28}`.
  - `test_RejectInvalidLength`: Reverts with `InvalidSignatureLength` for malformed signatures.
- **Soroban / Rust Equivalent:** [`src/security/signature_checker.rs`](src/security/signature_checker.rs)
  - `verify_signature_s_bounds`: Byte-level bound checker comparing `s` against `SECP256K1_N_HALF_ORDER`.
  - `validate_secp256k1_signature`: Complete ECDSA signature validator.
  - Rust unit tests confirming rejection of malleable upper-half `s` and acceptance of lower-half `s`.
  - `ContractError::MalleableSignature` alias in [`src/lib.rs`](src/lib.rs).

---

## 3. Issue #1191: Gas Optimization — Direct `msg.sender` Usage in Token Bridge

### 3.1 Optimization Overview
Functions in `src/TokenBridge.sol` previously declared intermediate stack variables (e.g., `address sender = msg.sender;`) when the caller address was only referenced a single time.

### 3.2 Impact & Opcode Analysis
- **Unnecessary Stack Allocation:** Declaring an intermediate local variable consumes an extra stack slot, triggering redundant `DUP` / `SWAP` and stack cleanup opcodes (`POP`).
- **Gas Overhead:** In EVM, `msg.sender` compiles directly to the low-cost `CALLER` opcode (2 gas). Caching it in a local variable that is only used once introduces net gas overhead and increases contract bytecode size.

### 3.3 Remediation
1. **Eliminate Redundant Local Variable Allocation:** Remove `address sender = msg.sender;` across contract functions.
2. **Direct `msg.sender` Resolution:** Directly invoke `msg.sender` at each invocation site:
   - In `bridgeTokens(...)`: passed directly to `transferFrom(msg.sender, address(this), amount)` and emitted in `TokensLocked`.
   - In `claimRefund(...)`: verified and transferred directly to `msg.sender`.
   - In `onlyOwner` & `onlyRelayer` modifiers: evaluated directly against authorization state.

### 3.4 Implemented Files & Tests
- **Solidity Contract:** [`src/TokenBridge.sol`](src/TokenBridge.sol)
- **Solidity Test Suite:** [`test/TokenBridge.t.sol`](test/TokenBridge.t.sol)
  - `test_BridgeTokensSuccessDirectMsgSender`: Validates token bridging with direct `msg.sender` usage.
  - `test_ReleaseTokensSuccess`: Confirms authorized relayer release.
  - `test_ReleaseTokensRevertsIfAlreadyProcessed`: CEI re-entry and replay guard verification.
  - `test_ClaimRefundDirectMsgSender`: Asserts refund mechanism directs assets to `msg.sender`.
  - `test_AdminFunctionsAccessControl`: Verifies direct authorization checks on administrative setters.

---

## 4. Cross-Platform Architectural Summary

| Issue | Vulnerability / Optimization | Remediation in Solidity | Soroban / Rust Counterpart |
| :--- | :--- | :--- | :--- |
| **#1125** | Unchecked low-level `.call()` return value | `(bool success, ) = recipient.call{value: amount}(""); if (!success) revert ETHTransferFailed();` | Explicit balance checks & non-silent error returns in `src/router/fund_router.rs` (`ETHTransferFailed`) |
| **#1226** | Signature malleability via unrestricted `s` | `if (uint256(s) > uint256(SECP256K1_N_DIV_2)) revert InvalidSignatureS();` | Big-endian curve order bounds check in `src/security/signature_checker.rs` (`MalleableSignature`) |
| **#1191** | Redundant local caching of `msg.sender` | Remove `address sender = msg.sender;` and use `msg.sender` directly | Lean stack variable usage in `src/bridge/` |

