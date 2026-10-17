# Standalone Security Fix Examples

This folder contains four minimal Solidity contracts representing the four issue reports described in the upstream security notes.

## 1) Validation.sol
Issue: Multi-variable zero validation using repeated logical AND checks.

Fix applied:
- Replaced chained comparisons like `a == 0 && b == 0 && c == 0`
- with a single bitwise OR check: `(a | b | c) == 0`

## 2) MultiSigWallet.sol
Issue: Unchecked delegatecall execution in a multi-sig wallet.

Fix applied:
- Restricted execution to normal `CALL` operations only
- Rejected `DELEGATECALL` with `require(operation == Operation.Call, "Delegatecall disabled")`

## 3) YieldStrategy.sol
Issue: Harvest compounding without a minimum interval or fee cap.

Fix applied:
- Enforced a minimum harvest interval using `block.timestamp >= lastHarvest + HARVEST_INTERVAL`
- Added a bounded fee cap per harvest call
- Prevented excessive repeated compounding draining the strategy

## 4) CustomToken.sol
Issue: `burnFrom()` permission logic without allowance enforcement.

Fix applied:
- Checked allowance before burning: `require(allowance[account][msg.sender] >= amount, "Insufficient allowance")`
- Reduced the spender allowance before the burn
- Ensured `_burn()` still validates the account balance

## Note
These files are intentionally standalone examples to mirror the issue descriptions exactly and are not intended to replace the actual repository codebase.
