// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "../src/MetaTxHandler.sol";

contract MockTarget {
    uint256 public counter;
    address public lastCaller;

    event Executed(address caller, uint256 count);

    function increment() external payable {
        counter++;
        lastCaller = msg.sender;
        emit Executed(msg.sender, counter);
    }
}

interface Vm {
    function warp(uint256 newTimestamp) external;
    function expectRevert(bytes calldata revertData) external;
    function expectRevert(bytes4 revertData) external;
    function sign(uint256 privateKey, bytes32 digest) external pure returns (uint8 v, bytes32 r, bytes32 s);
    function addr(uint256 privateKey) external pure returns (address);
}

contract MetaTxHandlerTest {
    Vm private constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    MetaTxHandler public handler;
    MockTarget public target;

    uint256 private alicePrivateKey = 0x1111111111111111111111111111111111111111111111111111111111111111;
    address public alice;

    function setUp() public {
        handler = new MetaTxHandler();
        target = new MockTarget();
        alice = vm.addr(alicePrivateKey);
    }

    function _signMetaTx(
        address from,
        address to,
        uint256 value,
        bytes memory data,
        uint256 nonce,
        uint256 deadline,
        uint256 privateKey
    ) internal view returns (bytes memory) {
        bytes32 digest = handler.getDigest(from, to, value, data, nonce, deadline);
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(privateKey, digest);
        return abi.encodePacked(r, s, v);
    }

    function test_ValidMetaTransactionWithDeadlinePasses() public {
        bytes memory callData = abi.encodeWithSelector(MockTarget.increment.selector);
        uint256 nonce = handler.getNonce(alice);
        uint256 deadline = block.timestamp + 1 hours;

        bytes memory signature = _signMetaTx(
            alice,
            address(target),
            0,
            callData,
            nonce,
            deadline,
            alicePrivateKey
        );

        handler.executeMetaTransaction(
            alice,
            address(target),
            0,
            callData,
            nonce,
            deadline,
            signature
        );

        require(target.counter() == 1, "Counter should have incremented");
        require(handler.getNonce(alice) == nonce + 1, "Nonce should have incremented");
    }

    function test_RevertWhenSignatureExpired() public {
        bytes memory callData = abi.encodeWithSelector(MockTarget.increment.selector);
        uint256 nonce = handler.getNonce(alice);
        uint256 deadline = block.timestamp + 10 minutes;

        bytes memory signature = _signMetaTx(
            alice,
            address(target),
            0,
            callData,
            nonce,
            deadline,
            alicePrivateKey
        );

        // Fast forward time past the deadline
        vm.warp(deadline + 1 seconds);

        // Issue #1253: Require block.timestamp <= deadline, revert with "Signature expired"
        vm.expectRevert(bytes("Signature expired"));
        handler.executeMetaTransaction(
            alice,
            address(target),
            0,
            callData,
            nonce,
            deadline,
            signature
        );
    }

    function test_RevertOnReplayedNonce() public {
        bytes memory callData = abi.encodeWithSelector(MockTarget.increment.selector);
        uint256 nonce = handler.getNonce(alice);
        uint256 deadline = block.timestamp + 1 hours;

        bytes memory signature = _signMetaTx(
            alice,
            address(target),
            0,
            callData,
            nonce,
            deadline,
            alicePrivateKey
        );

        // First execution succeeds
        handler.executeMetaTransaction(
            alice,
            address(target),
            0,
            callData,
            nonce,
            deadline,
            signature
        );

        // Replaying the identical transaction must revert with InvalidNonce
        vm.expectRevert(abi.encodeWithSelector(MetaTxHandler.InvalidNonce.selector, 1, 0));
        handler.executeMetaTransaction(
            alice,
            address(target),
            0,
            callData,
            nonce,
            deadline,
            signature
        );
    }

    function test_RevertOnTamperedDeadline() public {
        bytes memory callData = abi.encodeWithSelector(MockTarget.increment.selector);
        uint256 nonce = handler.getNonce(alice);
        uint256 signedDeadline = block.timestamp + 10 minutes;

        // Alice signs with 10 minute deadline
        bytes memory signature = _signMetaTx(
            alice,
            address(target),
            0,
            callData,
            nonce,
            signedDeadline,
            alicePrivateKey
        );

        // Relayer attempts to extend deadline to 1 day without Alice re-signing
        uint256 forgedDeadline = block.timestamp + 1 days;

        // Digest changes, so recovered signer will not match Alice
        vm.expectRevert();
        handler.executeMetaTransaction(
            alice,
            address(target),
            0,
            callData,
            nonce,
            forgedDeadline,
            signature
        );
    }
}
