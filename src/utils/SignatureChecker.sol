// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/**
 * @title SignatureChecker
 * @notice Cryptographic signature verification with strict protection against signature malleability.
 * @dev Addresses Issue #1226: Signature Malleability via Unrestricted s Value.
 *      Enforces strict secp256k1 lower-half curve order check:
 *      s <= 0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0.
 *      Malleable signatures (with s in upper half) are strictly rejected with custom error InvalidSignatureS().
 */
library SignatureChecker {
    // ── Custom Errors ────────────────────────────────────────────────────────
    /// @notice Thrown when signature's s-value exceeds the lower half of the secp256k1 curve order.
    error InvalidSignatureS();

    /// @notice Thrown when the recovery id (v) is not 27 or 28.
    error InvalidSignatureV();

    /// @notice Thrown when the signature byte length is not exactly 65 bytes.
    error InvalidSignatureLength();

    /// @notice Thrown when ecrecover yields the zero address or fails.
    error InvalidSignature();

    // ── Constants ────────────────────────────────────────────────────────────
    /// @dev Half the secp256k1 group order n. Any s > n/2 is considered malleable and rejected.
    /// secp256k1 n = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141
    /// n / 2 = 0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0
    bytes32 internal constant SECP256K1_N_DIV_2 =
        0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0;

    // ── Recovery Routines ────────────────────────────────────────────────────

    /**
     * @notice Recovers the signer address from a 65-byte signature with strict s range checking.
     * @param hash The 32-byte digest of the signed payload.
     * @param signature The 65-byte packed signature (r[32] || s[32] || v[1]).
     * @return signer The verified signer address.
     */
    function recover(bytes32 hash, bytes memory signature) internal pure returns (address) {
        if (signature.length != 65) {
            revert InvalidSignatureLength();
        }

        bytes32 r;
        bytes32 s;
        uint8 v;

        assembly {
            r := mload(add(signature, 0x20))
            s := mload(add(signature, 0x40))
            v := byte(0, mload(add(signature, 0x60)))
        }

        return recover(hash, v, r, s);
    }

    /**
     * @notice Recovers the signer address from explicit (v, r, s) components with strict range checks.
     * @param hash The 32-byte digest of the signed payload.
     * @param v The recovery byte (must be 27 or 28).
     * @param r The r curve scalar.
     * @param s The s curve scalar (must be <= SECP256K1_N_DIV_2).
     * @return signer The verified signer address.
     */
    function recover(
        bytes32 hash,
        uint8 v,
        bytes32 r,
        bytes32 s
    ) internal pure returns (address signer) {
        // Issue #1226 Fix:
        // Enforce strict upper bound on s to reject malleable signatures.
        if (uint256(s) > uint256(SECP256K1_N_DIV_2)) {
            revert InvalidSignatureS();
        }

        // Enforce canonical v range
        if (v != 27 && v != 28) {
            revert InvalidSignatureV();
        }

        signer = ecrecover(hash, v, r, s);
        if (signer == address(0)) {
            revert InvalidSignature();
        }
    }

    /**
     * @notice Verifies whether a signature is valid for a given signer address.
     * @param signer The expected signer address.
     * @param hash The 32-byte payload digest.
     * @param signature The 65-byte packed signature.
     * @return valid True if valid and non-malleable, false otherwise.
     */
    function isValidSignatureNow(
        address signer,
        bytes32 hash,
        bytes memory signature
    ) internal pure returns (bool) {
        if (signature.length != 65) {
            return false;
        }

        bytes32 r;
        bytes32 s;
        uint8 v;

        assembly {
            r := mload(add(signature, 0x20))
            s := mload(add(signature, 0x40))
            v := byte(0, mload(add(signature, 0x60)))
        }

        if (uint256(s) > uint256(SECP256K1_N_DIV_2)) {
            return false;
        }

        if (v != 27 && v != 28) {
            return false;
        }

        address recovered = ecrecover(hash, v, r, s);
        return recovered != address(0) && recovered == signer;
    }
}
