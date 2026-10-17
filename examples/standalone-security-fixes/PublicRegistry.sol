// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title PublicRegistry
/// @notice Standalone example converting uncalled public functions to external.
contract PublicRegistry {
    mapping(address => bool) private _registered;

    function register(address account) external {
        _registered[account] = true;
    }

    function isRegistered(address account) external view returns (bool) {
        return _registered[account];
    }
}
