// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title Utils
/// @notice Standalone example fixing variable shadowing by renaming parameters with underscores.
contract Utils {
    address public owner;

    constructor(address _owner) {
        owner = _owner;
    }

    function setOwner(address _owner) external {
        owner = _owner;
    }
}
