// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "../src/FundRouter.sol";

interface Vm {
    function deal(address who, uint256 newBalance) external;
    function expectRevert(bytes4 revertData) external;
    function expectEmit(bool, bool, bool, bool) external;
}

contract RevertingReceiver {
    // Reverts on receiving Ether to test transfer failure handling
    receive() external payable {
        revert("Rejecting ETH transfer");
    }
}

contract AcceptingReceiver {
    uint256 public received;

    receive() external payable {
        received += msg.value;
    }
}

contract FundRouterTest {
    Vm private constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    FundRouter public router;
    AcceptingReceiver public goodReceiver;
    RevertingReceiver public badReceiver;

    event EtherDistributed(address indexed recipient, uint256 amount);
    event BatchEtherDistributed(uint256 totalAmount, uint256 recipientCount);

    function setUp() public {
        router = new FundRouter();
        goodReceiver = new AcceptingReceiver();
        badReceiver = new RevertingReceiver();
        vm.deal(address(router), 100 ether);
    }

    function test_SingleTransferSuccess() public {
        uint256 amount = 1 ether;
        router.routeEther(payable(address(goodReceiver)), amount);
        require(address(goodReceiver).balance == amount, "Good receiver balance mismatch");
    }

    function test_SingleTransferRevertsOnFailedCall() public {
        uint256 amount = 1 ether;
        // Issue #1125: low-level call fails, must revert with custom error ETHTransferFailed()
        vm.expectRevert(FundRouter.ETHTransferFailed.selector);
        router.routeEther(payable(address(badReceiver)), amount);
    }

    function test_BatchTransferSuccess() public {
        address payable[] memory recipients = new address payable[](2);
        recipients[0] = payable(address(goodReceiver));
        recipients[1] = payable(address(0x1111));

        uint256[] memory amounts = new uint256[](2);
        amounts[0] = 2 ether;
        amounts[1] = 3 ether;

        router.batchRouteEther(recipients, amounts);
        require(address(goodReceiver).balance == 2 ether, "Receiver 0 balance mismatch");
        require(address(0x1111).balance == 3 ether, "Receiver 1 balance mismatch");
    }

    function test_BatchTransferRevertsAtomicallyIfAnyFails() public {
        address payable[] memory recipients = new address payable[](2);
        recipients[0] = payable(address(goodReceiver));
        recipients[1] = payable(address(badReceiver));

        uint256[] memory amounts = new uint256[](2);
        amounts[0] = 1 ether;
        amounts[1] = 1 ether;

        vm.expectRevert(FundRouter.ETHTransferFailed.selector);
        router.batchRouteEther(recipients, amounts);
    }
}
