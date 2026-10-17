// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

interface ICollateralPriceFeed {
    function decimals() external view returns (uint8);

    function latestRoundData()
        external
        view
        returns (
            uint80 roundId,
            int256 answer,
            uint256 startedAt,
            uint256 updatedAt,
            uint80 answeredInRound
        );
}

contract CollateralVault {
    uint256 public constant MAX_ORACLE_DELAY = 1 hours;

    ICollateralPriceFeed public immutable priceFeed;

    constructor(ICollateralPriceFeed _priceFeed) {
        require(address(_priceFeed) != address(0), "Invalid price feed");
        priceFeed = _priceFeed;
    }

    function collateralValue(uint256 collateralAmount) external view returns (uint256) {
        uint256 price = _validatedPrice();
        uint8 feedDecimals = priceFeed.decimals();
        require(feedDecimals <= 77, "Unsupported feed decimals");

        return (collateralAmount * price) / (10 ** uint256(feedDecimals));
    }

    function _validatedPrice() internal view returns (uint256) {
        (uint80 roundId, int256 answer,, uint256 updatedAt, uint80 answeredInRound) =
            priceFeed.latestRoundData();

        require(answer > 0, "Invalid oracle answer");
        require(updatedAt > 0 && updatedAt <= block.timestamp, "Invalid update time");
        require(answeredInRound >= roundId, "Incomplete oracle round");
        require(block.timestamp - updatedAt < MAX_ORACLE_DELAY, "Stale oracle price");

        return uint256(answer);
    }
}