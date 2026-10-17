// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title VaultWithdrawal
/// @notice Standalone example enforcing a minimum output threshold on withdrawals.
contract VaultWithdrawal {
    uint256 public totalAssets;

    function deposit(uint256 amount) external {
        totalAssets += amount;
    }

    function withdraw(uint256 amount, uint256 minAmountOut) external {
        require(amount <= totalAssets, "Insufficient vault balance");
        require(amount >= minAmountOut, "Slippage too high");

        totalAssets -= amount;
    }
}
