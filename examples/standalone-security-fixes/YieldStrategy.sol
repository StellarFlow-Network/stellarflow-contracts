// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title YieldStrategy
/// @notice Standalone example implementing a harvest interval and fee cap.
contract YieldStrategy {
    uint256 public constant HARVEST_INTERVAL = 1 days;
    uint256 public constant MAX_PERFORMANCE_FEE_BPS = 2000; // 20%
    uint256 public constant BPS = 10_000;
    uint256 public constant MAX_FEE_PER_HARVEST = 1 ether;

    uint256 public lastHarvest;
    uint256 public totalAssets;
    uint256 public performanceFeeBps = 200; // 2%
    uint256 public accruedFees;
    address public feeRecipient;

    event Harvest(uint256 yield, uint256 fee, uint256 compounded, uint256 newTotalAssets);

    constructor(address _feeRecipient) {
        feeRecipient = _feeRecipient;
        lastHarvest = block.timestamp;
    }

    function harvest(uint256 yieldAmount) external {
        require(block.timestamp >= lastHarvest + HARVEST_INTERVAL, "Harvest too frequent");
        require(yieldAmount > 0, "Zero yield");

        uint256 fee = (yieldAmount * performanceFeeBps) / BPS;
        if (fee > MAX_FEE_PER_HARVEST) {
            fee = MAX_FEE_PER_HARVEST;
        }

        uint256 compounded = yieldAmount - fee;
        totalAssets += compounded;
        accruedFees += fee;
        lastHarvest = block.timestamp;

        emit Harvest(yieldAmount, fee, compounded, totalAssets);
    }
}
