// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

interface IConversionPriceFeed {
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

contract OracleConverter {
    uint256 private constant _TARGET_DECIMALS = 18;
    uint256 private constant _TARGET_SCALE = 1e18;

    function priceRatio(address baseFeed, address quoteFeed) external view returns (uint256) {
        uint256 normalizedBasePrice = _normalizedPrice(baseFeed);
        uint256 normalizedQuotePrice = _normalizedPrice(quoteFeed);
        require(normalizedQuotePrice > 0, "Zero quote price");

        return (normalizedBasePrice * _TARGET_SCALE) / normalizedQuotePrice;
    }

    function _normalizedPrice(address feedAddress) internal view returns (uint256) {
        require(feedAddress.code.length > 0, "Oracle feed absent");

        IConversionPriceFeed feed = IConversionPriceFeed(feedAddress);
        (, int256 answer,,,) = feed.latestRoundData();
        require(answer > 0, "Invalid oracle answer");

        uint8 feedDecimals = feed.decimals();
        require(feedDecimals <= 77, "Unsupported feed decimals");

        uint256 price = uint256(answer);
        if (feedDecimals < _TARGET_DECIMALS) {
            return price * (10 ** (_TARGET_DECIMALS - uint256(feedDecimals)));
        }
        if (feedDecimals > _TARGET_DECIMALS) {
            return price / (10 ** (uint256(feedDecimals) - _TARGET_DECIMALS));
        }
        return price;
    }
}