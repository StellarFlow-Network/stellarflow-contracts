// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/**
 * @title IERC20
 * @notice Minimal ERC-20 interface used by the SafeERC20 wrappers.
 */
interface IERC20 {
    function transfer(address to, uint256 value) external returns (bool);

    function transferFrom(address from, address to, uint256 value) external returns (bool);
}

/**
 * @title SafeERC20
 * @notice Safe wrappers around ERC-20 operations.
 * @dev Addresses Issue #1154: Non-standard ERC-20 tokens.
 *
 *      The ERC-20 standard says `transfer`/`transferFrom` return a boolean, but
 *      several widely used tokens (notably USDT) return no data at all, and
 *      some return `false` instead of reverting. Calling the raw interface
 *      methods makes a missing return value fail ABI decoding and revert the
 *      whole transaction, and makes a `false` return silently ignored.
 *
 *      `_callOptionalReturn` accepts both behaviours:
 *        - empty return data  -> success (non-standard, e.g. USDT);
 *        - 32-byte `true`     -> success;
 *        - `false` or malformed data -> revert with SafeERC20FailedOperation;
 *        - a revert -> the original revert data is bubbled up unchanged.
 *
 *      This is a self-contained, OpenZeppelin-compatible implementation. The
 *      repository has no Solidity dependency manager, so the library is kept
 *      local (mirroring `src/FundRouter.sol` and `src/utils/SignatureChecker.sol`)
 *      instead of adding an external OpenZeppelin dependency.
 */
library SafeERC20 {
    /// @notice Thrown when a token call fails or returns `false`.
    error SafeERC20FailedOperation(address token);

    /**
     * @notice Safely transfers `value` tokens to `to`.
     * @param token The ERC-20 token contract.
     * @param to The recipient address.
     * @param value The amount of tokens to transfer.
     */
    function safeTransfer(IERC20 token, address to, uint256 value) internal {
        _callOptionalReturn(token, abi.encodeCall(IERC20.transfer, (to, value)));
    }

    /**
     * @notice Safely transfers `value` tokens from `from` to `to`.
     * @param token The ERC-20 token contract.
     * @param from The address tokens are taken from.
     * @param to The recipient address.
     * @param value The amount of tokens to transfer.
     */
    function safeTransferFrom(IERC20 token, address from, address to, uint256 value) internal {
        _callOptionalReturn(token, abi.encodeCall(IERC20.transferFrom, (from, to, value)));
    }

    /**
     * @dev Performs the call and validates the optional boolean return value.
     */
    function _callOptionalReturn(IERC20 token, bytes memory data) private {
        (bool success, bytes memory returndata) = address(token).call(data);

        if (!success) {
            // Bubble up the token's own revert reason when it provided one.
            if (returndata.length != 0) {
                assembly ("memory-safe") {
                    revert(add(returndata, 0x20), mload(returndata))
                }
            }
            revert SafeERC20FailedOperation(address(token));
        }

        // Empty return data is accepted: tokens such as USDT return nothing.
        if (returndata.length != 0) {
            // A well-formed boolean is exactly 32 bytes; anything else is unsafe.
            if (returndata.length != 32 || !abi.decode(returndata, (bool))) {
                revert SafeERC20FailedOperation(address(token));
            }
        }
    }
}
