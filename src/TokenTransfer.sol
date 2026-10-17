// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {IERC20, SafeERC20} from "./utils/SafeERC20.sol";

/**
 * @title TokenTransfer
 * @notice Token transfer abstraction that never calls ERC-20 methods directly.
 * @dev Addresses Issue #1154: Wrap Non-Standard ERC-20 Calls with SafeERC20.
 *
 *      Every outbound token movement goes through
 *      `SafeERC20.safeTransfer` / `SafeERC20.safeTransferFrom`, so tokens that
 *      do not return a boolean (e.g. USDT) keep working, while tokens that
 *      return `false` or revert cause an atomic, explicit failure instead of a
 *      silent no-op.
 */
contract TokenTransfer {
    using SafeERC20 for IERC20;

    /// @notice Thrown when the token or a participant address is the zero address.
    error ZeroAddress();

    /// @notice Thrown when the transfer amount is zero.
    error ZeroAmount();

    /// @notice Emitted after a successful token transfer.
    event TokenTransferred(
        address indexed token,
        address indexed from,
        address indexed to,
        uint256 amount
    );

    /**
     * @notice Transfers `amount` tokens held by this contract to `to`.
     * @param token The ERC-20 token contract.
     * @param to The recipient address.
     * @param amount The amount of tokens to transfer.
     */
    function transferToken(IERC20 token, address to, uint256 amount) external {
        _validate(token, to, amount);
        token.safeTransfer(to, amount);
        emit TokenTransferred(address(token), address(this), to, amount);
    }

    /**
     * @notice Transfers `amount` tokens from `from` to `to` using the caller's allowance.
     * @param token The ERC-20 token contract.
     * @param from The address tokens are taken from.
     * @param to The recipient address.
     * @param amount The amount of tokens to transfer.
     */
    function transferTokenFrom(IERC20 token, address from, address to, uint256 amount) external {
        _validate(token, to, amount);
        if (from == address(0)) revert ZeroAddress();
        token.safeTransferFrom(from, to, amount);
        emit TokenTransferred(address(token), from, to, amount);
    }

    /**
     * @dev Rejects zero addresses and zero amounts before any external call.
     */
    function _validate(IERC20 token, address to, uint256 amount) private pure {
        if (address(token) == address(0) || to == address(0)) revert ZeroAddress();
        if (amount == 0) revert ZeroAmount();
    }
}
