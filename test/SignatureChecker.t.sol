// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "../src/utils/SignatureChecker.sol";

interface Vm {
    function expectRevert(bytes4 revertData) external;
}

contract SignatureCheckerTest {
    Vm private constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    bytes32 private constant SECP256K1_N =
        0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141;

    // Test wrapper calling library functions
    function recoverSignature(bytes32 hash, bytes memory signature) external pure returns (address) {
        return SignatureChecker.recover(hash, signature);
    }

    function recoverExplicit(bytes32 hash, uint8 v, bytes32 r, bytes32 s) external pure returns (address) {
        return SignatureChecker.recover(hash, v, r, s);
    }

    function test_RejectMalleableSignatureWithHighS() public {
        bytes32 hash = keccak256("StellarFlow Test Digest");
        bytes32 r = bytes32(uint256(0x123456789));
        // s > n/2 is in the malleable upper half
        bytes32 malleableS = bytes32(uint256(0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A1));
        uint8 v = 27;

        // Issue #1226: Must revert with InvalidSignatureS()
        vm.expectRevert(SignatureChecker.InvalidSignatureS.selector);
        this.recoverExplicit(hash, v, r, malleableS);
    }

    function test_RejectInvalidV() public {
        bytes32 hash = keccak256("StellarFlow Test Digest");
        bytes32 r = bytes32(uint256(0x123456789));
        bytes32 canonicalS = bytes32(uint256(0x1234));
        uint8 invalidV = 29;

        vm.expectRevert(SignatureChecker.InvalidSignatureV.selector);
        this.recoverExplicit(hash, invalidV, r, canonicalS);
    }

    function test_RejectInvalidLength() public {
        bytes32 hash = keccak256("StellarFlow Test Digest");
        bytes memory shortSig = new bytes(64);

        vm.expectRevert(SignatureChecker.InvalidSignatureLength.selector);
        this.recoverSignature(hash, shortSig);
    }
}
