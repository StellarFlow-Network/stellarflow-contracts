// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/**
 * @dev Minimal ERC-20 interface required for TokenBridge operations.
 */
interface IERC20 {
    function totalSupply() external view returns (uint256);
    function balanceOf(address account) external view returns (uint256);
    function transfer(address recipient, uint256 amount) external returns (bool);
    function allowance(address owner, address spender) external view returns (uint256);
    function approve(address spender, uint256 amount) external returns (bool);
    function transferFrom(address sender, address recipient, uint256 amount) external returns (bool);
}

/**
 * @title TokenBridge
 * @notice Cross-chain token bridge facilitating token lock, release, and refund operations.
 * @dev Addresses Issue #1191: Gas Optimization - Use `msg.sender` Directly Instead of Caching in Local Variable.
 *      Removes redundant local variable allocations (`address sender = msg.sender;`) in functions where
 *      the caller address is referenced only once. Direct access to `msg.sender` compiles directly
 *      to the EVM `CALLER` opcode (2 gas), avoiding redundant stack manipulation opcodes (DUP, SWAP, POP)
 *      and keeping the stack depth lean.
 */
contract TokenBridge {
    // ── Custom Errors ────────────────────────────────────────────────────────
    /// @notice Thrown when an input address is address(0).
    error ZeroAddress();

    /// @notice Thrown when an input amount is zero.
    error ZeroAmount();

    /// @notice Thrown when bridging operations are paused.
    error BridgePaused();

    /// @notice Thrown when the caller is not authorized for the requested action.
    error Unauthorized();

    /// @notice Thrown when a cross-chain message has already been processed.
    error MessageAlreadyProcessed(bytes32 messageId);

    /// @notice Thrown when a token transfer or transferFrom returns false.
    error TransferFailed();

    /// @notice Thrown when reentrancy is detected.
    error ReentrancyGuardReentrantCall();

    /// @notice Thrown when destination chain ID is invalid or identical to origin chain.
    error InvalidDestinationChain();

    // ── Events ───────────────────────────────────────────────────────────────
    /// @notice Emitted when tokens are locked to initiate cross-chain bridging.
    event TokensLocked(
        bytes32 indexed messageId,
        address indexed token,
        address indexed sender,
        uint256 amount,
        uint32 destinationChain,
        bytes32 recipient
    );

    /// @notice Emitted when tokens are released to a recipient on the target chain.
    event TokensReleased(
        bytes32 indexed messageId,
        address indexed token,
        address indexed recipient,
        uint256 amount
    );

    /// @notice Emitted when a failed bridge deposit is refunded.
    event BridgeRefundClaimed(
        bytes32 indexed messageId,
        address indexed token,
        address indexed recipient,
        uint256 amount
    );

    /// @notice Emitted when the bridge pause state is updated.
    event PauseStateChanged(bool isPaused);

    /// @notice Emitted when the authorized relayer address is updated.
    event RelayerUpdated(address indexed oldRelayer, address indexed newRelayer);

    /// @notice Emitted when contract ownership is transferred.
    event OwnershipTransferred(address indexed previousOwner, address indexed newOwner);

    // ── State Variables ──────────────────────────────────────────────────────
    address public owner;
    address public relayer;
    bool public paused;
    uint256 public nonce;

    /// @dev Tracks processed cross-chain message IDs to prevent replay attacks.
    mapping(bytes32 => bool) public processedMessages;

    /// @dev Tracks refundable bridge deposits by messageId.
    mapping(bytes32 => uint256) public refundableBalances;

    /// @dev Tracks deposit ownership for refunds.
    mapping(bytes32 => address) public depositOwners;

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

    modifier whenNotPaused() {
        if (paused) revert BridgePaused();
        _;
    }

    modifier onlyOwner() {
        // Issue #1191 Gas Optimization: Use msg.sender directly instead of caching
        if (msg.sender != owner) revert Unauthorized();
        _;
    }

    modifier onlyRelayer() {
        // Issue #1191 Gas Optimization: Use msg.sender directly instead of caching
        if (msg.sender != relayer) revert Unauthorized();
        _;
    }

    // ── Constructor ──────────────────────────────────────────────────────────
    constructor(address _relayer) {
        if (_relayer == address(0)) revert ZeroAddress();
        owner = msg.sender;
        relayer = _relayer;
        _status = _NOT_ENTERED;
        emit OwnershipTransferred(address(0), msg.sender);
        emit RelayerUpdated(address(0), _relayer);
    }

    // ── External Functions ───────────────────────────────────────────────────

    /**
     * @notice Locks tokens in the bridge to be transferred to another chain.
     * @dev Gas Optimized: `msg.sender` is accessed directly rather than cached in a local variable,
     *      saving stack allocation and opcode overhead.
     * @param token Address of the ERC-20 token being bridged.
     * @param amount Amount of tokens to bridge.
     * @param destinationChain Identifier of the destination blockchain.
     * @param recipient Target recipient identifier encoded as bytes32.
     * @return messageId Unique hash identifying this cross-chain transfer.
     */
    function bridgeTokens(
        address token,
        uint256 amount,
        uint32 destinationChain,
        bytes32 recipient
    ) external nonReentrant whenNotPaused returns (bytes32 messageId) {
        if (token == address(0)) revert ZeroAddress();
        if (amount == 0) revert ZeroAmount();
        if (destinationChain == 0) revert InvalidDestinationChain();
        if (recipient == bytes32(0)) revert ZeroAddress();

        uint256 currentNonce = ++nonce;
        // Issue #1191 Fix: Direct usage of msg.sender without redundant `address sender = msg.sender;`
        messageId = keccak256(
            abi.encodePacked(
                block.chainid,
                destinationChain,
                token,
                amount,
                msg.sender,
                recipient,
                currentNonce
            )
        );

        depositOwners[messageId] = msg.sender;
        refundableBalances[messageId] = amount;

        // Perform token transfer from caller directly
        bool success = IERC20(token).transferFrom(msg.sender, address(this), amount);
        if (!success) revert TransferFailed();

        emit TokensLocked(
            messageId,
            token,
            msg.sender,
            amount,
            destinationChain,
            recipient
        );
    }

    /**
     * @notice Releases tokens to a recipient upon validation by the authorized relayer.
     * @dev Implements Checks-Effects-Interactions (CEI) to protect against reentrancy.
     *      `msg.sender` is validated directly via `onlyRelayer`.
     * @param messageId Unique message ID from the origin chain.
     * @param token Token contract address on this chain.
     * @param recipient Address receiving the released tokens.
     * @param amount Number of tokens to unlock.
     */
    function releaseTokens(
        bytes32 messageId,
        address token,
        address recipient,
        uint256 amount
    ) external onlyRelayer nonReentrant whenNotPaused {
        if (messageId == bytes32(0)) revert ZeroAddress();
        if (token == address(0)) revert ZeroAddress();
        if (recipient == address(0)) revert ZeroAddress();
        if (amount == 0) revert ZeroAmount();
        if (processedMessages[messageId]) revert MessageAlreadyProcessed(messageId);

        // CEI: Mark message as processed before external token transfer
        processedMessages[messageId] = true;

        bool success = IERC20(token).transfer(recipient, amount);
        if (!success) revert TransferFailed();

        emit TokensReleased(messageId, token, recipient, amount);
    }

    /**
     * @notice Claims a refund for a previously initiated bridge transaction that could not be relayed.
     * @dev Gas Optimized: `msg.sender` is checked and used directly without local variable caching.
     * @param messageId Unique identifier of the refundable bridge transaction.
     * @param token Address of the token to refund.
     */
    function claimRefund(bytes32 messageId, address token) external nonReentrant {
        if (messageId == bytes32(0)) revert ZeroAddress();
        if (token == address(0)) revert ZeroAddress();
        if (processedMessages[messageId]) revert MessageAlreadyProcessed(messageId);

        // Issue #1191 Fix: Direct reference to msg.sender
        if (depositOwners[messageId] != msg.sender) revert Unauthorized();

        uint256 refundAmount = refundableBalances[messageId];
        if (refundAmount == 0) revert ZeroAmount();

        // State update before external transfer
        refundableBalances[messageId] = 0;
        processedMessages[messageId] = true;

        bool success = IERC20(token).transfer(msg.sender, refundAmount);
        if (!success) revert TransferFailed();

        emit BridgeRefundClaimed(messageId, token, msg.sender, refundAmount);
    }

    // ── Admin Functions ──────────────────────────────────────────────────────

    /**
     * @notice Sets the active pause state of the bridge.
     * @param _paused True to pause bridging, false to resume.
     */
    function setPaused(bool _paused) external onlyOwner {
        paused = _paused;
        emit PauseStateChanged(_paused);
    }

    /**
     * @notice Updates the authorized relayer address.
     * @param newRelayer Address of the new relayer.
     */
    function setRelayer(address newRelayer) external onlyOwner {
        if (newRelayer == address(0)) revert ZeroAddress();
        address oldRelayer = relayer;
        relayer = newRelayer;
        emit RelayerUpdated(oldRelayer, newRelayer);
    }

    /**
     * @notice Transfers ownership of the TokenBridge contract.
     * @param newOwner Address of the new contract owner.
     */
    function transferOwnership(address newOwner) external onlyOwner {
        if (newOwner == address(0)) revert ZeroAddress();
        address oldOwner = owner;
        owner = newOwner;
        emit OwnershipTransferred(oldOwner, newOwner);
    }
}
