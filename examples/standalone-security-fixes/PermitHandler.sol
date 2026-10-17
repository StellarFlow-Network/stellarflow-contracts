// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title PermitHandler
/// @notice Standalone example ensuring recovered signers are never the zero address.
contract PermitHandler {
    address public owner;

    constructor(address _owner) {
        owner = _owner;
    }

    function verifyPermit(address recoveredSigner, address expectedOwner) external pure {
        require(recoveredSigner != address(0), "Invalid signature");
        require(recoveredSigner == expectedOwner, "Signer mismatch");
    }
}
