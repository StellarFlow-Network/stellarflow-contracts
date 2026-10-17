// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "../src/TokenBridge.sol";

interface Vm {
    function deal(address who, uint256 newBalance) external;
    function prank(address who) external;
    function expectRevert(bytes4 revertData) external;
    function expectEmit(bool, bool, bool, bool) external;
}

contract MockERC20 is IERC20 {
    string public name = "Mock Token";
    string public symbol = "MCK";
    uint8 public decimals = 18;

    uint256 public override totalSupply;
    mapping(address => uint256) public override balanceOf;
    mapping(address => mapping(address => uint256)) public override allowance;

    function mint(address to, uint256 amount) external {
        totalSupply += amount;
        balanceOf[to] += amount;
    }

    function transfer(address recipient, uint256 amount) external override returns (bool) {
        require(balanceOf[msg.sender] >= amount, "ERC20: insufficient balance");
        balanceOf[msg.sender] -= amount;
        balanceOf[recipient] += amount;
        return true;
    }

    function approve(address spender, uint256 amount) external override returns (bool) {
        allowance[msg.sender][spender] = amount;
        return true;
    }

    function transferFrom(address sender, address recipient, uint256 amount) external override returns (bool) {
        require(balanceOf[sender] >= amount, "ERC20: insufficient balance");
        if (allowance[sender][msg.sender] != type(uint256).max) {
            require(allowance[sender][msg.sender] >= amount, "ERC20: insufficient allowance");
            allowance[sender][msg.sender] -= amount;
        }
        balanceOf[sender] -= amount;
        balanceOf[recipient] += amount;
        return true;
    }
}

contract TokenBridgeTest {
    Vm private constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    TokenBridge public bridge;
    MockERC20 public token;

    address public owner = address(this);
    address public relayer = address(0x2222);
    address public user = address(0x3333);
    address public recipient = address(0x4444);

    uint32 public constant DEST_CHAIN_ID = 2;

    function setUp() public {
        bridge = new TokenBridge(relayer);
        token = new MockERC20();

        token.mint(user, 1000 ether);
        token.mint(address(bridge), 1000 ether);

        vm.prank(user);
        token.approve(address(bridge), type(uint256).max);
    }

    function test_BridgeTokensSuccessDirectMsgSender() public {
        uint256 bridgeAmount = 100 ether;
        bytes32 targetRecipient = bytes32(uint256(uint160(recipient)));

        vm.prank(user);
        bytes32 messageId = bridge.bridgeTokens(address(token), bridgeAmount, DEST_CHAIN_ID, targetRecipient);

        require(messageId != bytes32(0), "MessageId should not be zero");
        require(token.balanceOf(user) == 900 ether, "User balance not decremented");
        require(token.balanceOf(address(bridge)) == 1100 ether, "Bridge balance not incremented");
        require(bridge.depositOwners(messageId) == user, "Deposit owner mismatch");
        require(bridge.refundableBalances(messageId) == bridgeAmount, "Refundable balance mismatch");
    }

    function test_ReleaseTokensSuccess() public {
        bytes32 messageId = keccak256("unique-message-id-1");
        uint256 releaseAmount = 50 ether;

        vm.prank(relayer);
        bridge.releaseTokens(messageId, address(token), recipient, releaseAmount);

        require(token.balanceOf(recipient) == releaseAmount, "Recipient did not receive tokens");
        require(bridge.processedMessages(messageId) == true, "Message not marked as processed");
    }

    function test_ReleaseTokensRevertsIfAlreadyProcessed() public {
        bytes32 messageId = keccak256("unique-message-id-replay");
        uint256 releaseAmount = 50 ether;

        vm.prank(relayer);
        bridge.releaseTokens(messageId, address(token), recipient, releaseAmount);

        vm.prank(relayer);
        vm.expectRevert(abi.encodeWithSelector(TokenBridge.MessageAlreadyProcessed.selector, messageId));
        bridge.releaseTokens(messageId, address(token), recipient, releaseAmount);
    }

    function test_ReleaseTokensRevertsUnauthorizedCaller() public {
        bytes32 messageId = keccak256("unique-message-id-unauth");
        uint256 releaseAmount = 50 ether;

        // Calling from non-relayer
        vm.prank(user);
        vm.expectRevert(TokenBridge.Unauthorized.selector);
        bridge.releaseTokens(messageId, address(token), recipient, releaseAmount);
    }

    function test_ClaimRefundDirectMsgSender() public {
        uint256 bridgeAmount = 75 ether;
        bytes32 targetRecipient = bytes32(uint256(uint160(recipient)));

        vm.prank(user);
        bytes32 messageId = bridge.bridgeTokens(address(token), bridgeAmount, DEST_CHAIN_ID, targetRecipient);

        uint256 userBalBefore = token.balanceOf(user);

        vm.prank(user);
        bridge.claimRefund(messageId, address(token));

        require(token.balanceOf(user) == userBalBefore + bridgeAmount, "User did not receive refund");
        require(bridge.refundableBalances(messageId) == 0, "Refundable balance not cleared");
        require(bridge.processedMessages(messageId) == true, "Message not marked processed");
    }

    function test_ClaimRefundRevertsIfUnauthorizedCaller() public {
        uint256 bridgeAmount = 50 ether;
        bytes32 targetRecipient = bytes32(uint256(uint160(recipient)));

        vm.prank(user);
        bytes32 messageId = bridge.bridgeTokens(address(token), bridgeAmount, DEST_CHAIN_ID, targetRecipient);

        // Another address attempts to claim user's refund
        vm.prank(recipient);
        vm.expectRevert(TokenBridge.Unauthorized.selector);
        bridge.claimRefund(messageId, address(token));
    }

    function test_BridgeTokensRevertsWhenPaused() public {
        bridge.setPaused(true);

        vm.prank(user);
        vm.expectRevert(TokenBridge.BridgePaused.selector);
        bridge.bridgeTokens(address(token), 10 ether, DEST_CHAIN_ID, bytes32(uint256(1)));
    }

    function test_AdminFunctionsAccessControl() public {
        address newRelayer = address(0x9999);

        // Non-owner attempts to update relayer
        vm.prank(user);
        vm.expectRevert(TokenBridge.Unauthorized.selector);
        bridge.setRelayer(newRelayer);

        // Owner updates relayer successfully
        bridge.setRelayer(newRelayer);
        require(bridge.relayer() == newRelayer, "Relayer address mismatch");
    }
}
