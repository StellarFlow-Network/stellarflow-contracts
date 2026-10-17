// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title Validation
/// @notice Standalone example showing the gas-optimized zero validation pattern.
contract Validation {
    /// @dev Uses a bitwise OR to collapse multiple zero checks into one comparison.
    /// This avoids repeated logical AND branch checks and reduces bytecode/ops.
    function validateZeroInputs(uint256 a, uint256 b, uint256 c) external pure {
        require((a | b | c) == 0, "Non-zero input");
    }
}
