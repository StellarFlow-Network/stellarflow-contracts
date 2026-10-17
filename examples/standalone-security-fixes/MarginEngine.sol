// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title MarginEngine
/// @notice Standalone example enforcing liquidation debt coverage checks.
contract MarginEngine {
    struct Position {
        uint256 collateral;
        uint256 debt;
    }

    mapping(address => Position) public positions;

    event BadDebtRealized(address indexed position, uint256 amount);

    function liquidate(address position, uint256 collateralToSeize) external {
        Position storage pos = positions[position];
        require(pos.collateral > 0, "No position");

        uint256 debtCovered = collateralToSeize;
        if (debtCovered < pos.debt) {
            emit BadDebtRealized(position, pos.debt - debtCovered);
            revert("Insufficient liquidation coverage");
        }

        pos.collateral = 0;
        pos.debt = 0;
    }
}
