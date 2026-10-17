// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title CDPEngine
/// @notice Standalone example preventing unsafe position creation below initial health thresholds.
contract CDPEngine {
    uint256 public constant MIN_INITIAL_HEALTH_FACTOR = 1.5e18; // 150%

    struct Position {
        uint256 collateral;
        uint256 debt;
    }

    mapping(address => Position) public positions;

    error UnsafePositionCreation();

    function openPosition(uint256 collateral, uint256 debt) external {
        require(collateral > 0, "Zero collateral");

        uint256 healthFactor = (collateral * 1e18) / debt;
        if (healthFactor < MIN_INITIAL_HEALTH_FACTOR) {
            revert UnsafePositionCreation();
        }

        positions[msg.sender] = Position({collateral: collateral, debt: debt});
    }
}
