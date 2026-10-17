// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title FeeCalculator
/// @notice Standalone example caching state into stack variables to cut repeated SLOADs.
contract FeeCalculator {
    uint256 public feeRate;

    function calculate(uint256 amount) external view returns (uint256) {
        uint256 _feeRate = feeRate;
        uint256 rateMultiplier = amount * _feeRate;
        return rateMultiplier / 1e4;
    }
}
