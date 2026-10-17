// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/**
 * @title MetaTxHandler
 * @notice Processes off-chain EIP-712 authorized meta-transactions for gasless user execution.
 * @dev Addresses Issue #1253: Missing Expiry Timestamp in Off-Chain EIP-712 Meta-Transaction Payloads.
 *
 *      Vulnerability:
 *      Previously, meta-transaction schemas omitted an explicit `deadline` parameter.
 *      Signed meta-transactions remained valid indefinitely, allowing relayers or malicious actors
 *      to hold and execute transactions arbitrarily far in the future or during adverse market conditions.
 *
 *      Remediation:
 *      1. Added `uint256 deadline` to the EIP-712 `MetaTransaction` struct and type hash.
 *      2. Enforced strict timestamp validation:
 *         `require(block.timestamp <= deadline, "Signature expired");`
 *         during meta-transaction processing before nonce consumption and dispatch.
 */
contract MetaTxHandler {
    // ── EIP-712 Type Definitions ─────────────────────────────────────────────

    /// @notice EIP-712 Domain Separator structure typehash.
    bytes32 public constant EIP712_DOMAIN_TYPEHASH = keccak256(
        "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"
    );

    /// @notice EIP-712 MetaTransaction structure typehash including the deadline parameter.
    bytes32 public constant META_TRANSACTION_TYPEHASH = keccak256(
        "MetaTransaction(address from,address to,uint256 value,bytes data,uint256 nonce,uint256 deadline)"
    );

    /// @dev Secp256k1 curve order / 2 for strict signature malleability prevention.
    bytes32 internal constant SECP256K1_N_DIV_2 =
        0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0;

    // ── Structs ──────────────────────────────────────────────────────────────

    /// @notice Canonical representation of an EIP-712 meta-transaction payload.
    struct MetaTransaction {
        address from;
        address to;
        uint256 value;
        bytes data;
        uint256 nonce;
        uint256 deadline;
    }

    // ── Custom Errors ────────────────────────────────────────────────────────
    /// @notice Thrown when signature has expired beyond the authorized deadline.
    error SignatureExpired(uint256 deadline, uint256 currentTimestamp);

    /// @notice Thrown when the nonce in the meta-transaction payload does not match expected nonce.
    error InvalidNonce(uint256 expected, uint256 provided);

    /// @notice Thrown when recovered signer does not match the payload's `from` address.
    error InvalidSigner(address recovered, address expected);

    /// @notice Thrown when the signature is structurally invalid or zero address is recovered.
    error InvalidSignature();

    /// @notice Thrown when signature s-value exceeds the secp256k1 lower half order (malleability).
    error InvalidSignatureS();

    /// @notice Thrown when signature v-value is not 27 or 28.
    error InvalidSignatureV();

    /// @notice Thrown when raw signature bytes length is not exactly 65 bytes.
    error InvalidSignatureLength();

    /// @notice Thrown when target contract execution fails.
    error ExecutionFailed(bytes returnData);

    /// @notice Thrown when forwarded msg.value is less than required by the meta-transaction.
    error InsufficientValue(uint256 required, uint256 available);

    /// @notice Thrown when an address parameter is address(0).
    error ZeroAddress();

    /// @notice Thrown on reentrancy detection.
    error ReentrancyGuardReentrantCall();

    // ── Events ───────────────────────────────────────────────────────────────
    /// @notice Emitted when a meta-transaction executes successfully.
    event MetaTransactionExecuted(
        address indexed from,
        address indexed to,
        uint256 value,
        uint256 nonce,
        uint256 deadline
    );

    // ── State Variables ──────────────────────────────────────────────────────
    string public constant NAME = "StellarFlowMetaTxHandler";
    string public constant VERSION = "1";

    bytes32 public immutable DOMAIN_SEPARATOR;
    uint256 private immutable _initialChainId;

    mapping(address => uint256) public nonces;

    // ── Reentrancy Guard State ───────────────────────────────────────────────
    uint256 private constant _NOT_ENTERED = 1;
    uint256 private constant _ENTERED = 2;
    uint256 private _reentrancyStatus;

    modifier nonReentrant() {
        if (_reentrancyStatus == _ENTERED) revert ReentrancyGuardReentrantCall();
        _reentrancyStatus = _ENTERED;
        _;
        _reentrancyStatus = _NOT_ENTERED;
    }

    constructor() {
        _initialChainId = block.chainid;
        DOMAIN_SEPARATOR = _buildDomainSeparator(block.chainid, address(this));
        _reentrancyStatus = _NOT_ENTERED;
    }

    receive() external payable {}

    // ── View Functions ───────────────────────────────────────────────────────

    /**
     * @notice Computes domain separator, taking chain forks into account.
     */
    function getDomainSeparator() public view returns (bytes32) {
        if (block.chainid == _initialChainId) {
            return DOMAIN_SEPARATOR;
        }
        return _buildDomainSeparator(block.chainid, address(this));
    }

    /**
     * @notice Returns the current nonce for a given signer address.
     * @param user The address to query.
     */
    function getNonce(address user) external view returns (uint256) {
        return nonces[user];
    }

    /**
     * @notice Computes the EIP-712 struct hash for a meta-transaction payload.
     */
    function hashMetaTransaction(
        address from,
        address to,
        uint256 value,
        bytes calldata data,
        uint256 nonce,
        uint256 deadline
    ) public pure returns (bytes32) {
        return keccak256(
            abi.encode(
                META_TRANSACTION_TYPEHASH,
                from,
                to,
                value,
                keccak256(data),
                nonce,
                deadline
            )
        );
    }

    /**
     * @notice Computes the final 32-byte EIP-712 digest to be signed.
     */
    function getDigest(
        address from,
        address to,
        uint256 value,
        bytes calldata data,
        uint256 nonce,
        uint256 deadline
    ) public view returns (bytes32) {
        bytes32 structHash = hashMetaTransaction(from, to, value, data, nonce, deadline);
        return keccak256(abi.encodePacked("\x19\x01", getDomainSeparator(), structHash));
    }

    // ── Meta-Transaction Execution ───────────────────────────────────────────

    /**
     * @notice Executes an EIP-712 meta-transaction with struct payload and raw signature bytes.
     * @param metaTx The meta-transaction parameters including the deadline.
     * @param signature The 65-byte ECDSA signature (r, s, v).
     * @return returnData Return data from the executed call.
     */
    function executeMetaTransaction(
        MetaTransaction calldata metaTx,
        bytes calldata signature
    ) external payable nonReentrant returns (bytes memory) {
        return _processMetaTransaction(
            metaTx.from,
            metaTx.to,
            metaTx.value,
            metaTx.data,
            metaTx.nonce,
            metaTx.deadline,
            signature
        );
    }

    /**
     * @notice Executes an EIP-712 meta-transaction with flattened parameters and raw signature bytes.
     * @param from Signer and authorizer of the transaction.
     * @param to Target contract or recipient address.
     * @param value Native Ether amount to forward with the call.
     * @param data Calldata payload to execute on the target.
     * @param nonce Expected replay protection sequence number.
     * @param deadline Expiry timestamp after which this transaction cannot be executed.
     * @param signature 65-byte signature.
     */
    function executeMetaTransaction(
        address from,
        address to,
        uint256 value,
        bytes calldata data,
        uint256 nonce,
        uint256 deadline,
        bytes calldata signature
    ) external payable nonReentrant returns (bytes memory) {
        return _processMetaTransaction(from, to, value, data, nonce, deadline, signature);
    }

    /**
     * @notice Executes an EIP-712 meta-transaction with decomposed (r, s, v) signature components.
     */
    function executeMetaTransaction(
        address from,
        address to,
        uint256 value,
        bytes calldata data,
        uint256 nonce,
        uint256 deadline,
        bytes32 r,
        bytes32 s,
        uint8 v
    ) external payable nonReentrant returns (bytes memory) {
        bytes memory signature = abi.encodePacked(r, s, v);
        return _processMetaTransaction(from, to, value, data, nonce, deadline, signature);
    }

    // ── Internal Helpers ─────────────────────────────────────────────────────

    function _processMetaTransaction(
        address from,
        address to,
        uint256 value,
        bytes calldata data,
        uint256 nonce,
        uint256 deadline,
        bytes memory signature
    ) internal returns (bytes memory) {
        // ── 1. Issue #1253 Remediation: Expiry Validation ────────────────────
        // Revert immediately if block timestamp has passed the signed deadline
        require(block.timestamp <= deadline, "Signature expired");

        if (from == address(0) || to == address(0)) revert ZeroAddress();
        if (msg.value < value) revert InsufficientValue(value, msg.value);

        // ── 2. Nonce Validation & Consumption ────────────────────────────────
        if (nonces[from] != nonce) {
            revert InvalidNonce(nonces[from], nonce);
        }
        nonces[from] = nonce + 1;

        // ── 3. EIP-712 Digest & Signature Verification ───────────────────────
        bytes32 digest = getDigest(from, to, value, data, nonce, deadline);
        address signer = _recoverSigner(digest, signature);

        if (signer == address(0) || signer != from) {
            revert InvalidSigner(signer, from);
        }

        // ── 4. Target Execution ──────────────────────────────────────────────
        (bool success, bytes memory returnData) = to.call{value: value}(
            abi.encodePacked(data, from)
        );

        if (!success) {
            revert ExecutionFailed(returnData);
        }

        emit MetaTransactionExecuted(from, to, value, nonce, deadline);
        return returnData;
    }

    function _recoverSigner(bytes32 digest, bytes memory signature) internal pure returns (address) {
        if (signature.length != 65) revert InvalidSignatureLength();

        bytes32 r;
        bytes32 s;
        uint8 v;

        assembly {
            r := mload(add(signature, 0x20))
            s := mload(add(signature, 0x40))
            v := byte(0, mload(add(signature, 0x60)))
        }

        // Enforce strict upper bound on s to prevent signature malleability
        if (uint256(s) > uint256(SECP256K1_N_DIV_2)) {
            revert InvalidSignatureS();
        }

        // Enforce canonical v recovery value
        if (v != 27 && v != 28) {
            revert InvalidSignatureV();
        }

        address signer = ecrecover(digest, v, r, s);
        if (signer == address(0)) revert InvalidSignature();

        return signer;
    }

    function _buildDomainSeparator(uint256 chainId, address contractAddr) internal pure returns (bytes32) {
        return keccak256(
            abi.encode(
                EIP712_DOMAIN_TYPEHASH,
                keccak256(bytes(NAME)),
                keccak256(bytes(VERSION)),
                chainId,
                contractAddr
            )
        );
    }
}
