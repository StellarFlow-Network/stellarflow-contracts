// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title FlashMintVault
/// @notice Standalone example enforcing a bounded flash mint limit.
contract FlashMintVault {
    uint256 public constant MAX_FLASH_LOAN = 1_000_000e18;
    uint256 public totalSupply;

    function maxFlashLoan(address token) external view returns (uint256) {
        require(token == address(0), "Unsupported token");
        return MAX_FLASH_LOAN;
    }

    function mint(uint256 amount) external {
        require(amount <= MAX_FLASH_LOAN, "Flash mint exceeds cap");
        totalSupply += amount;
    }
}
