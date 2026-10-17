// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "../src/RewardPool.sol";

/**
 * @dev Mock ERC20 token for staking and reward testing.
 */
contract MockERC20 is IERC20 {
    string public name;
    string public symbol;
    uint8 public decimals = 18;
    uint256 private _totalSupply;
    mapping(address => uint256) private _balances;
    mapping(address => mapping(address => uint256)) private _allowances;

    constructor(string memory _name, string memory _symbol) {
        name = _name;
        symbol = _symbol;
    }

    function mint(address account, uint256 amount) external {
        _totalSupply += amount;
        _balances[account] += amount;
        emit Transfer(address(0), account, amount);
    }

    function totalSupply() external view override returns (uint256) {
        return _totalSupply;
    }

    function balanceOf(address account) external view override returns (uint256) {
        return _balances[account];
    }

    function transfer(address recipient, uint256 amount) external override returns (bool) {
        require(_balances[msg.sender] >= amount, "ERC20: transfer amount exceeds balance");
        _balances[msg.sender] -= amount;
        _balances[recipient] += amount;
        emit Transfer(msg.sender, recipient, amount);
        return true;
    }

    function allowance(address owner, address spender) external view override returns (uint256) {
        return _allowances[owner][spender];
    }

    function approve(address spender, uint256 amount) external override returns (bool) {
        _allowances[msg.sender][spender] = amount;
        emit Approval(msg.sender, spender, amount);
        return true;
    }

    function transferFrom(address sender, address recipient, uint256 amount) external override returns (bool) {
        require(_balances[sender] >= amount, "ERC20: transfer amount exceeds balance");
        require(_allowances[sender][msg.sender] >= amount, "ERC20: transfer amount exceeds allowance");
        _allowances[sender][msg.sender] -= amount;
        _balances[sender] -= amount;
        _balances[recipient] += amount;
        emit Transfer(sender, recipient, amount);
        return true;
    }
}

interface Vm {
    function warp(uint256 newTimestamp) external;
    function expectRevert(bytes4 revertData) external;
    function expectRevert(bytes calldata revertData) external;
    function prank(address msgSender) external;
}

contract RewardPoolTest {
    Vm private constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    MockERC20 public stakingToken;
    MockERC20 public rewardToken;
    RewardPool public pool;

    address public alice = address(0xAAAA);
    address public bob = address(0xBBBB);

    uint256 public constant DURATION = 7 days;
    uint256 public constant TOTAL_REWARD = 700 ether; // 100 ether per day

    function setUp() public {
        stakingToken = new MockERC20("Staking Token", "STK");
        rewardToken = new MockERC20("Reward Token", "RWD");

        pool = new RewardPool(address(stakingToken), address(rewardToken), DURATION);

        // Fund test users with staking tokens
        stakingToken.mint(alice, 1000 ether);
        stakingToken.mint(bob, 1000 ether);

        // Fund RewardPool with rewards and start distribution
        rewardToken.mint(address(pool), TOTAL_REWARD);
        pool.notifyRewardAmount(TOTAL_REWARD);
    }

    function test_AutoClaimOnTotalWithdrawal() public {
        // Alice stakes 100 ether
        vm.prank(alice);
        stakingToken.approve(address(pool), 100 ether);
        vm.prank(alice);
        pool.stake(100 ether);

        // Fast forward 1 day (should accrue ~100 ether of rewards)
        vm.warp(block.timestamp + 1 days);

        uint256 earnedBefore = pool.earned(alice);
        require(earnedBefore >= 99 ether && earnedBefore <= 101 ether, "Earned calculation incorrect");

        uint256 aliceRewardBalanceBefore = rewardToken.balanceOf(alice);
        require(aliceRewardBalanceBefore == 0, "Alice should have 0 reward tokens before");

        // Alice withdraws ENTIRE staked balance WITHOUT calling claimReward()
        vm.prank(alice);
        pool.withdraw(100 ether);

        // Verify Issue #1234 remediation:
        // 1. Staked tokens are completely refunded
        require(pool.balanceOf(alice) == 0, "Pool balance should be 0");
        require(stakingToken.balanceOf(alice) == 1000 ether, "Alice should have all staked tokens back");

        // 2. Earned rewards were automatically claimed and transferred during withdraw()
        uint256 aliceRewardBalanceAfter = rewardToken.balanceOf(alice);
        require(aliceRewardBalanceAfter == earnedBefore, "Rewards should have been auto-claimed on withdraw");
        require(aliceRewardBalanceAfter > 0, "Alice should have received non-zero reward payout");

        // 3. User reward accumulator is settled and does not retain unpaid phantom balance
        require(pool.rewards(alice) == 0, "Pool rewards for Alice should be zeroed post-claim");
    }

    function test_ExitWithdrawsStakeAndClaimsAllRewards() public {
        vm.prank(alice);
        stakingToken.approve(address(pool), 200 ether);
        vm.prank(alice);
        pool.stake(200 ether);

        vm.warp(block.timestamp + 3 days);

        uint256 expectedReward = pool.earned(alice);
        require(expectedReward > 0, "Expected non-zero reward");

        // Calling exit() unstakes total balance and auto-claims rewards
        vm.prank(alice);
        pool.exit();

        require(pool.balanceOf(alice) == 0, "Alice staked balance should be 0");
        require(stakingToken.balanceOf(alice) == 1000 ether, "Alice stake returned");
        require(rewardToken.balanceOf(alice) == expectedReward, "Alice reward paid out");
    }

    function test_PartialWithdrawAutoClaimsAccruedRewards() public {
        vm.prank(alice);
        stakingToken.approve(address(pool), 200 ether);
        vm.prank(alice);
        pool.stake(200 ether);

        vm.warp(block.timestamp + 2 days);

        uint256 expectedRewardDay2 = pool.earned(alice);

        // Withdraw half of staked tokens
        vm.prank(alice);
        pool.withdraw(100 ether);

        require(pool.balanceOf(alice) == 100 ether, "Remaining staked balance is 100");
        require(rewardToken.balanceOf(alice) == expectedRewardDay2, "Day 2 rewards paid out on partial withdraw");

        // Fast forward another day; remaining 100 ether continues to earn rewards
        vm.warp(block.timestamp + 1 days);
        uint256 earnedDay3 = pool.earned(alice);
        require(earnedDay3 > 0, "Alice earns rewards on remaining stake");

        // Withdraw the remaining half
        vm.prank(alice);
        pool.withdraw(100 ether);

        require(pool.balanceOf(alice) == 0, "Alice fully unstaked");
        require(rewardToken.balanceOf(alice) >= expectedRewardDay2 + earnedDay3, "Total rewards collected");
    }

    function test_RevertOnWithdrawZeroOrExcessAmount() public {
        vm.prank(alice);
        stakingToken.approve(address(pool), 50 ether);
        vm.prank(alice);
        pool.stake(50 ether);

        // Zero amount withdraw reverts
        vm.expectRevert(RewardPool.ZeroAmount.selector);
        vm.prank(alice);
        pool.withdraw(0);

        // Excess amount withdraw reverts with InsufficientBalance
        vm.expectRevert(abi.encodeWithSelector(RewardPool.InsufficientBalance.selector, 100 ether, 50 ether));
        vm.prank(alice);
        pool.withdraw(100 ether);
    }
}
