// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {TokenTransfer} from "../src/TokenTransfer.sol";
import {IERC20, SafeERC20} from "../src/utils/SafeERC20.sol";

interface Vm {
    function expectRevert(bytes4 revertData) external;

    function expectRevert(bytes calldata revertData) external;

    function expectEmit(bool, bool, bool, bool) external;

    function prank(address sender) external;
}

/// @dev Standard ERC-20 that returns a boolean and can be told to misbehave.
contract MockERC20 {
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    bool public returnFalse;
    bool public revertOnTransfer;

    function mint(address to, uint256 amount) external {
        balanceOf[to] += amount;
    }

    function approve(address spender, uint256 amount) external {
        allowance[msg.sender][spender] = amount;
    }

    function setReturnFalse(bool value) external {
        returnFalse = value;
    }

    function setRevertOnTransfer(bool value) external {
        revertOnTransfer = value;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        if (revertOnTransfer) revert("transfer reverted");
        if (returnFalse) return false;
        _move(msg.sender, to, amount);
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        if (revertOnTransfer) revert("transferFrom reverted");
        if (returnFalse) return false;
        _spendAllowance(from, amount);
        _move(from, to, amount);
        return true;
    }

    function _spendAllowance(address from, uint256 amount) internal {
        uint256 allowed = allowance[from][msg.sender];
        require(allowed >= amount, "insufficient allowance");
        if (allowed != type(uint256).max) {
            allowance[from][msg.sender] = allowed - amount;
        }
    }

    function _move(address from, address to, uint256 amount) internal {
        require(balanceOf[from] >= amount, "insufficient balance");
        balanceOf[from] -= amount;
        balanceOf[to] += amount;
    }
}

/// @dev USDT-style token: `transfer`/`transferFrom` return no data at all.
contract NonStandardERC20 {
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    function mint(address to, uint256 amount) external {
        balanceOf[to] += amount;
    }

    function approve(address spender, uint256 amount) external {
        allowance[msg.sender][spender] = amount;
    }

    function transfer(address to, uint256 amount) external {
        _move(msg.sender, to, amount);
    }

    function transferFrom(address from, address to, uint256 amount) external {
        uint256 allowed = allowance[from][msg.sender];
        require(allowed >= amount, "insufficient allowance");
        if (allowed != type(uint256).max) {
            allowance[from][msg.sender] = allowed - amount;
        }
        _move(from, to, amount);
    }

    function _move(address from, address to, uint256 amount) internal {
        require(balanceOf[from] >= amount, "insufficient balance");
        balanceOf[from] -= amount;
        balanceOf[to] += amount;
    }
}

contract TokenTransferTest {
    Vm private constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    address private constant ALICE = address(0x1111);
    address private constant BOB = address(0x2222);

    TokenTransfer private transferContract;
    MockERC20 private token;
    NonStandardERC20 private nonStandard;

    event TokenTransferred(
        address indexed token,
        address indexed from,
        address indexed to,
        uint256 amount
    );

    function setUp() public {
        transferContract = new TokenTransfer();
        token = new MockERC20();
        nonStandard = new NonStandardERC20();
    }

    function test_TransferStandardToken() public {
        token.mint(address(transferContract), 100);

        vm.expectEmit(true, true, true, true);
        emit TokenTransferred(address(token), address(transferContract), BOB, 40);

        transferContract.transferToken(IERC20(address(token)), BOB, 40);

        assert(token.balanceOf(address(transferContract)) == 60);
        assert(token.balanceOf(BOB) == 40);
    }

    /// @dev Issue #1154: a missing boolean return must not revert the transfer.
    function test_TransferNonStandardTokenSucceeds() public {
        nonStandard.mint(address(transferContract), 100);

        vm.expectEmit(true, true, true, true);
        emit TokenTransferred(address(nonStandard), address(transferContract), BOB, 40);

        transferContract.transferToken(IERC20(address(nonStandard)), BOB, 40);

        assert(nonStandard.balanceOf(address(transferContract)) == 60);
        assert(nonStandard.balanceOf(BOB) == 40);
    }

    /// @dev Issue #1154: transferFrom must tolerate a missing boolean return.
    function test_TransferFromNonStandardTokenSucceeds() public {
        nonStandard.mint(ALICE, 100);

        vm.prank(ALICE);
        nonStandard.approve(address(transferContract), 40);

        transferContract.transferTokenFrom(IERC20(address(nonStandard)), ALICE, BOB, 40);

        assert(nonStandard.balanceOf(ALICE) == 60);
        assert(nonStandard.balanceOf(BOB) == 40);
        assert(nonStandard.allowance(ALICE, address(transferContract)) == 0);
    }

    function test_TransferFromStandardTokenSucceeds() public {
        token.mint(ALICE, 100);

        vm.prank(ALICE);
        token.approve(address(transferContract), type(uint256).max);

        transferContract.transferTokenFrom(IERC20(address(token)), ALICE, BOB, 25);

        assert(token.balanceOf(ALICE) == 75);
        assert(token.balanceOf(BOB) == 25);
    }

    function test_TransferReturningFalseReverts() public {
        token.mint(address(transferContract), 100);
        token.setReturnFalse(true);

        vm.expectRevert(
            abi.encodeWithSelector(
                SafeERC20.SafeERC20FailedOperation.selector,
                address(token)
            )
        );
        transferContract.transferToken(IERC20(address(token)), BOB, 10);
    }

    function test_TransferFromReturningFalseReverts() public {
        token.mint(ALICE, 100);

        vm.prank(ALICE);
        token.approve(address(transferContract), 10);

        token.setReturnFalse(true);

        vm.expectRevert(
            abi.encodeWithSelector(
                SafeERC20.SafeERC20FailedOperation.selector,
                address(token)
            )
        );
        transferContract.transferTokenFrom(IERC20(address(token)), ALICE, BOB, 10);
    }

    function test_TransferBubblesTokenRevertReason() public {
        token.mint(address(transferContract), 100);
        token.setRevertOnTransfer(true);

        vm.expectRevert(bytes("transfer reverted"));
        transferContract.transferToken(IERC20(address(token)), BOB, 10);
    }

    function test_TransferFromBubblesTokenRevertReason() public {
        token.mint(ALICE, 100);

        vm.prank(ALICE);
        token.approve(address(transferContract), 10);

        token.setRevertOnTransfer(true);

        vm.expectRevert(bytes("transferFrom reverted"));
        transferContract.transferTokenFrom(IERC20(address(token)), ALICE, BOB, 10);
    }

    function test_ZeroTokenAddressReverts() public {
        vm.expectRevert(TokenTransfer.ZeroAddress.selector);
        transferContract.transferToken(IERC20(address(0)), BOB, 1);
    }

    function test_ZeroRecipientReverts() public {
        token.mint(address(transferContract), 1);

        vm.expectRevert(TokenTransfer.ZeroAddress.selector);
        transferContract.transferToken(IERC20(address(token)), address(0), 1);
    }

    function test_ZeroAmountReverts() public {
        token.mint(address(transferContract), 1);

        vm.expectRevert(TokenTransfer.ZeroAmount.selector);
        transferContract.transferToken(IERC20(address(token)), BOB, 0);
    }

    function test_TransferFromZeroSenderReverts() public {
        token.mint(address(transferContract), 1);

        vm.expectRevert(TokenTransfer.ZeroAddress.selector);
        transferContract.transferTokenFrom(IERC20(address(token)), address(0), BOB, 1);
    }
}
