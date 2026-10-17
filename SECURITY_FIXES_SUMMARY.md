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

## 3. Issue #1154: Non-Standard ERC-20 Calls in Token Transfer

### 3.1 Vulnerability Overview
ERC-20 declares that `transfer` and `transferFrom` return a boolean, but widely
used tokens do not follow that convention. USDT, for example, returns no data at
all, while some other tokens return `false` instead of reverting. Calling the raw
`IERC20` methods therefore either reverts while decoding a missing return value
or silently ignores a `false` result.

### 3.2 Impact
- **Interoperability failure:** transfers of non-standard tokens (e.g. USDT) revert.
- **Silent failures:** a `false` return is ignored, so accounting advances while no tokens move.

### 3.3 Remediation
1. **Safe wrappers:** every token movement goes through `SafeERC20.safeTransfer`
   and `SafeERC20.safeTransferFrom`; no raw `IERC20.transfer`/`transferFrom` call
   remains.
2. **Optional return value:** empty return data is accepted, so USDT-style
   tokens succeed.
3. **False / malformed results:** a `false` return (or data that is not a
   32-byte boolean) reverts with `SafeERC20FailedOperation(token)`.
4. **Revert bubbling:** the token's own revert reason is propagated unchanged.
5. **Input validation:** zero token/recipient/sender addresses and zero amounts
   are rejected before any external call.

### 3.4 Implemented Files & Tests
- **Safe wrapper library:** [`src/utils/SafeERC20.sol`](src/utils/SafeERC20.sol)
  (self-contained, OpenZeppelin-compatible; the repository has no Solidity
  dependency manager).
- **Transfer abstraction:** [`src/TokenTransfer.sol`](src/TokenTransfer.sol)
- **Solidity Test Suite:** [`test/TokenTransfer.t.sol`](test/TokenTransfer.t.sol)
  - `test_TransferStandardToken` / `test_TransferFromStandardTokenSucceeds`:
    normal boolean-returning tokens still work.
  - `test_TransferNonStandardTokenSucceeds` /
    `test_TransferFromNonStandardTokenSucceeds`: USDT-style empty return data succeeds.
  - `test_TransferReturningFalseReverts` /
    `test_TransferFromReturningFalseReverts`: `false` returns revert explicitly.
  - `test_TransferBubblesTokenRevertReason` /
    `test_TransferFromBubblesTokenRevertReason`: token revert reasons are preserved.
  - `test_ZeroTokenAddressReverts`, `test_ZeroRecipientReverts`,
    `test_ZeroAmountReverts`, `test_TransferFromZeroSenderReverts`: input validation.

---

## 4. Cross-Platform Architectural Summary

| Issue | Vulnerability | Remediation in Solidity | Soroban / Rust Counterpart |
| :--- | :--- | :--- | :--- |
| **#1125** | Unchecked low-level `.call()` return value | `(bool success, ) = recipient.call{value: amount}(""); if (!success) revert ETHTransferFailed();` | Explicit balance checks & non-silent error returns in `src/router/fund_router.rs` (`ETHTransferFailed`) |
| **#1226** | Signature malleability via unrestricted `s` | `if (uint256(s) > uint256(SECP256K1_N_DIV_2)) revert InvalidSignatureS();` | Big-endian curve order bounds check in `src/security/signature_checker.rs` (`MalleableSignature`) |
| **#1154** | Non-standard ERC-20 return values | `SafeERC20.safeTransfer/safeTransferFrom` accept empty data and revert on `false` or malformed data (`SafeERC20FailedOperation`) | N/A — `soroban_sdk::token::Client` traps on failure and has no boolean return value |
