// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/**
 * @title FundRouter
 * @notice Processes native Ether distributions and routing with validated low-level transfers.
 * @dev Addresses Issue #1125: Unchecked low-level .call() return value.
 *      Validates boolean success return value and reverts with custom error ETHTransferFailed()
 *      on transfer failures to prevent state desynchronization.
 */
contract FundRouter {
    // ── Custom Errors ────────────────────────────────────────────────────────
    /// @notice Thrown when a low-level native ETH transfer fails.
    error ETHTransferFailed();

    /// @notice Thrown when a recipient is the zero address.
    error InvalidRecipient();

    /// @notice Thrown when the transfer amount is zero.
    error InvalidAmount();

    /// @notice Thrown when contract does not have sufficient Ether to complete distribution.
    error InsufficientBalance(uint256 requested, uint256 available);

    /// @notice Thrown when recipient and amount arrays have mismatched lengths.
    error ArrayLengthMismatch();

    /// @notice Thrown when reentrancy is detected.
    error ReentrancyGuardReentrantCall();

    // ── Events ───────────────────────────────────────────────────────────────
    /// @notice Emitted when Ether is successfully routed to a recipient.
    event EtherDistributed(address indexed recipient, uint256 amount);

    /// @notice Emitted when a batch Ether distribution completes successfully.
    event BatchEtherDistributed(uint256 totalAmount, uint256 recipientCount);

    /// @notice Emitted when native Ether is deposited into the router.
    event EtherReceived(address indexed sender, uint256 amount);

    // ── Reentrancy Guard State ───────────────────────────────────────────────
    uint256 private constant _NOT_ENTERED = 1;
    uint256 private constant _ENTERED = 2;
    uint256 private _status;

    modifier nonReentrant() {
        if (_status == _ENTERED) revert ReentrancyGuardReentrantCall();
        _status = _ENTERED;
        _;
        _status = _NOT_ENTERED;
    }

    constructor() {
        _status = _NOT_ENTERED;
    }

    // ── External Functions ───────────────────────────────────────────────────

    /**
     * @notice Distributes Ether to a single recipient.
     * @param recipient The target address receiving Ether.
     * @param amount The amount of wei to send.
     */
    function routeEther(address payable recipient, uint256 amount) external nonReentrant {
        _distributeEther(recipient, amount);
    }

    /**
     * @notice Alias for routeEther for backwards compatibility.
     * @param recipient The target address receiving Ether.
     * @param amount The amount of wei to send.
     */
    function distributeEther(address payable recipient, uint256 amount) external nonReentrant {
        _distributeEther(recipient, amount);
    }

    /**
     * @notice Distributes Ether to multiple recipients in a single transaction.
     * @param recipients Array of recipient addresses.
     * @param amounts Array of wei amounts corresponding to each recipient.
     */
    function batchRouteEther(
        address payable[] calldata recipients,
        uint256[] calldata amounts
    ) external nonReentrant {
        uint256 len = recipients.length;
        if (len != amounts.length) revert ArrayLengthMismatch();

        uint256 totalAmount = 0;
        for (uint256 i = 0; i < len; ) {
            totalAmount += amounts[i];
            unchecked {
                ++i;
            }
        }

        if (address(this).balance < totalAmount) {
            revert InsufficientBalance(totalAmount, address(this).balance);
        }

        for (uint256 i = 0; i < len; ) {
            address payable recipient = recipients[i];
            uint256 amount = amounts[i];

            if (recipient == address(0)) revert InvalidRecipient();
            if (amount == 0) revert InvalidAmount();

            // Secure low-level call: capture boolean return value and validate
            (bool success, ) = recipient.call{value: amount}("");
            if (!success) revert ETHTransferFailed();

            emit EtherDistributed(recipient, amount);

            unchecked {
                ++i;
            }
        }

        emit BatchEtherDistributed(totalAmount, len);
    }

    // ── Internal Helpers ─────────────────────────────────────────────────────

    /**
     * @dev Internal routine to perform validated low-level transfer.
     */
    function _distributeEther(address payable recipient, uint256 amount) internal {
        if (recipient == address(0)) revert InvalidRecipient();
        if (amount == 0) revert InvalidAmount();
        if (address(this).balance < amount) {
            revert InsufficientBalance(amount, address(this).balance);
        }

        // Issue #1125 Fix:
        // Capture boolean return value and revert on failure to avoid silent failures
        (bool success, ) = recipient.call{value: amount}("");
        if (!success) revert ETHTransferFailed();

        emit EtherDistributed(recipient, amount);
    }

    // ── Receive / Fallback ───────────────────────────────────────────────────

    /// @notice Allows the contract to receive native Ether.
    receive() external payable {
        emit EtherReceived(msg.sender, msg.value);
    }

    /// @notice Fallback function to accept Ether.
    fallback() external payable {
        emit EtherReceived(msg.sender, msg.value);
    }
}
