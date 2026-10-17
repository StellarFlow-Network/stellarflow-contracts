// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title MultiSigWallet
/// @notice Standalone example of a multisig wallet that forbids delegatecalls.
contract MultiSigWallet {
    enum Operation {
        Call,
        DelegateCall
    }

    mapping(address => bool) public isOwner;
    address[] public owners;
    uint256 public threshold;

    event Execution(address indexed target, uint256 value, bytes data);

    modifier onlyOwner() {
        require(isOwner[msg.sender], "Not owner");
        _;
    }

    constructor(address[] memory _owners, uint256 _threshold) {
        require(_owners.length > 0, "No owners");
        require(_threshold > 0 && _threshold <= _owners.length, "Invalid threshold");

        for (uint256 i = 0; i < _owners.length; i++) {
            address owner = _owners[i];
            require(owner != address(0), "Zero owner");
            require(!isOwner[owner], "Duplicate owner");
            isOwner[owner] = true;
            owners.push(owner);
        }

        threshold = _threshold;
    }

    /// @dev Executes a transaction only with a standard CALL.
    /// DELEGATECALL is explicitly rejected to prevent storage corruption.
    function execute(
        address target,
        uint256 value,
        bytes calldata data,
        Operation operation
    ) external onlyOwner {
        require(operation == Operation.Call, "Delegatecall disabled");
        (bool success, ) = target.call{value: value}(data);
        require(success, "Call failed");

        emit Execution(target, value, data);
    }
}
