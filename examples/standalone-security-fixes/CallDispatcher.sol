// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

contract CallDispatcher {
    address public immutable owner;

    constructor(address _owner) {
        require(_owner != address(0), "Invalid owner");
        owner = _owner;
    }

    function execute(address target, bytes calldata data)
        external
        returns (bytes memory returnData)
    {
        require(msg.sender == owner, "Unauthorized");
        require(target.code.length > 0, "Target contract absent");

        (bool success, bytes memory result) = target.call(data);
        if (!success) {
            assembly ("memory-safe") {
                revert(add(result, 0x20), mload(result))
            }
        }

        return result;
    }
}