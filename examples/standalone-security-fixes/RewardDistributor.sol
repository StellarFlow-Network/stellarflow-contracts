// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title RewardDistributor
/// @notice Standalone example caching mapping reads in loop iterations.
contract RewardDistributor {
    mapping(address => uint256) public userRewards;

    function distribute(address[] calldata users, uint256[] calldata amounts) external {
        require(users.length == amounts.length, "Length mismatch");

        for (uint256 i = 0; i < users.length; i++) {
            address user = users[i];
            uint256 cachedReward = userRewards[user];
            uint256 updatedReward = cachedReward + amounts[i];
            userRewards[user] = updatedReward;
        }
    }
}
