// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title YieldStrategyVault
/// @notice Standalone example enforcing a maximum fee cap on strategy manager fees.
contract YieldStrategyVault {
    uint256 public constant MAX_FEE_BPS = 2000; // 20%
    uint256 public feeBps;

    event FeeUpdated(uint256 oldFeeBps, uint256 newFeeBps);

    function setFeeBps(uint256 newFeeBps) external {
        require(newFeeBps <= MAX_FEE_BPS, "Fee above max bound");
        uint256 oldFeeBps = feeBps;
        feeBps = newFeeBps;
        emit FeeUpdated(oldFeeBps, newFeeBps);
    }
}
