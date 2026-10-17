// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title FlashMintVault
/// @notice Standalone example enforcing a bounded flash mint limit.
contract FlashMintVault {
    uint256 public constant MAX_FLASH_LOAN = 1_000_000e18;
    uint256 public constant MAX_TOTAL_SUPPLY = 10_000_000e18;
    uint256 public totalSupply;

    function maxFlashLoan(address token) external view returns (uint256) {
        require(token == address(0), "Unsupported token");
        if (totalSupply >= MAX_TOTAL_SUPPLY) {
            return 0;
        }

        uint256 remainingSupply = MAX_TOTAL_SUPPLY - totalSupply;
        return remainingSupply < MAX_FLASH_LOAN ? remainingSupply : MAX_FLASH_LOAN;
    }

    function mint(uint256 amount) external {
        require(amount <= MAX_FLASH_LOAN, "Flash mint exceeds cap");
        require(
            totalSupply <= MAX_TOTAL_SUPPLY && amount <= MAX_TOTAL_SUPPLY - totalSupply,
            "Total supply cap exceeded"
        );
        totalSupply += amount;
    }
}
