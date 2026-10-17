// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title StakingVault
/// @notice Standalone example enforcing a minimum lock duration during deposits.
contract StakingVault {
    uint256 public constant MIN_LOCK_DURATION = 7 days;

    struct Stake {
        uint256 amount;
        uint256 unlockTime;
    }

    mapping(address => Stake) public stakes;

    function deposit(uint256 amount, uint256 duration) external {
        require(duration >= MIN_LOCK_DURATION, "Lock too short");
        require(amount > 0, "Zero amount");

        stakes[msg.sender] = Stake({
            amount: amount,
            unlockTime: block.timestamp + duration
        });
    }
}
