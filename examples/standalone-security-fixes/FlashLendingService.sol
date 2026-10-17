// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title FlashLendingService
/// @notice Standalone example resolving unbounded array iteration in flash-loan borrower tracking.
contract FlashLendingService {
    mapping(address => bool) public activeBorrowers;

    event FlashLoanExecuted(address indexed borrower);

    function markActive(address borrower) external {
        activeBorrowers[borrower] = true;
    }

    function clearActive(address borrower) external {
        activeBorrowers[borrower] = false;
    }

    function executeFlashLoan(address borrower) external {
        require(activeBorrowers[borrower], "Inactive borrower");
        activeBorrowers[borrower] = false;
        emit FlashLoanExecuted(borrower);
    }
}
