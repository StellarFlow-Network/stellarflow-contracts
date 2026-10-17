// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Initializable} from "@openzeppelin/contracts-upgradeable/proxy/utils/Initializable.sol";

contract UpgradeableVault is Initializable {
    address public owner;
    uint256 public configuration;

    constructor() {
        _disableInitializers();
    }

    function initialize(address _owner, uint256 _configuration) external initializer {
        require(_owner != address(0), "Invalid owner");
        owner = _owner;
        configuration = _configuration;
    }
}