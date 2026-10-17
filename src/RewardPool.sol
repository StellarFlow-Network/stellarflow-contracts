// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/**
 * @dev Minimal ERC20 interface for staking and reward token interactions.
 */
interface IERC20 {
    function totalSupply() external view returns (uint256);
    function balanceOf(address account) external view returns (uint256);
    function transfer(address recipient, uint256 amount) external returns (bool);
    function allowance(address owner, address spender) external view returns (uint256);
    function approve(address spender, uint256 amount) external returns (bool);
    function transferFrom(address sender, address recipient, uint256 amount) external returns (bool);

    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);
}

/**
 * @title RewardPool
 * @notice Staking and reward distribution pool for StellarFlow network participants.
 * @dev Addresses Issue #1234: Loss of Unclaimed Rewards on User Stake Withdrawal.
 *
 *      Vulnerability:
 *      Withdrawing entire staked balances previously updated accumulators and reduced staked
 *      balances to zero without atomically paying out accrued rewards. If a user did not call
 *      claimReward() prior to withdraw(), their earned rewards could be lost or inaccessible.
 *
 *      Remediation:
 *      Automatically triggers `_claimReward(msg.sender)` inside `withdraw()` prior to balance
 *      updates. This guarantees that all accrued rewards earned during the staking period are
 *      transferred immediately and safely upon withdrawal.
 */
contract RewardPool {
    // ── Custom Errors ────────────────────────────────────────────────────────
    /// @notice Thrown when an address parameter is address(0).
    error ZeroAddress();

    /// @notice Thrown when a deposit, withdrawal, or reward amount is zero.
    error ZeroAmount();

    /// @notice Thrown when attempting to withdraw more than the staked balance.
    error InsufficientBalance(uint256 requested, uint256 available);

    /// @notice Thrown when transfer of reward tokens fails.
    error RewardTransferFailed();

    /// @notice Thrown when transfer of staking tokens fails.
    error StakingTransferFailed();

    /// @notice Thrown when caller is not the owner/administrator.
    error OnlyOwner();

    /// @notice Thrown when reward duration is modified while active reward period is running.
    error RewardDurationNotFinished(uint256 finishTime);

    /// @notice Thrown when provided reward rate exceeds contract balance.
    error RewardRateTooHigh(uint256 provided, uint256 available);

    /// @notice Thrown on reentrancy detection.
    error ReentrancyGuardReentrantCall();

    // ── Events ───────────────────────────────────────────────────────────────
    /// @notice Emitted when a user stakes tokens.
    event Staked(address indexed user, uint256 amount);

    /// @notice Emitted when a user withdraws staked tokens.
    event Withdrawn(address indexed user, uint256 amount);

    /// @notice Emitted when a user claims accrued rewards.
    event RewardPaid(address indexed user, uint256 reward);

    /// @notice Emitted when a new reward amount is notified.
    event RewardAdded(uint256 reward);

    /// @notice Emitted when the reward duration is updated.
    event RewardsDurationUpdated(uint256 newDuration);

    // ── State Variables ──────────────────────────────────────────────────────
    IERC20 public immutable stakingToken;
    IERC20 public immutable rewardToken;
    address public immutable owner;

    uint256 public rewardRate;
    uint256 public rewardsDuration;
    uint256 public periodFinish;
    uint256 public lastUpdateTime;
    uint256 public rewardPerTokenStored;

    mapping(address => uint256) public userRewardPerTokenPaid;
    mapping(address => uint256) public rewards;

    uint256 private _totalSupply;
    mapping(address => uint256) private _balances;

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

    modifier onlyOwner() {
        if (msg.sender != owner) revert OnlyOwner();
        _;
    }

    modifier updateReward(address account) {
        rewardPerTokenStored = rewardPerToken();
        lastUpdateTime = lastTimeRewardApplicable();
        if (account != address(0)) {
            rewards[account] = earned(account);
            userRewardPerTokenPaid[account] = rewardPerTokenStored;
        }
        _;
    }

    /**
     * @notice Initializes the RewardPool contract.
     * @param _stakingToken Address of the token to stake.
     * @param _rewardToken Address of the token to distribute as reward.
     * @param _rewardsDuration Duration in seconds over which notified rewards are distributed.
     */
    constructor(
        address _stakingToken,
        address _rewardToken,
        uint256 _rewardsDuration
    ) {
        if (_stakingToken == address(0) || _rewardToken == address(0)) revert ZeroAddress();
        if (_rewardsDuration == 0) revert ZeroAmount();

        stakingToken = IERC20(_stakingToken);
        rewardToken = IERC20(_rewardToken);
        owner = msg.sender;
        rewardsDuration = _rewardsDuration;
        _reentrancyStatus = _NOT_ENTERED;
    }

    // ── View Functions ───────────────────────────────────────────────────────

    /// @notice Returns the total amount of tokens currently staked in the pool.
    function totalSupply() external view returns (uint256) {
        return _totalSupply;
    }

    /// @notice Returns the staked token balance of an account.
    function balanceOf(address account) external view returns (uint256) {
        return _balances[account];
    }

    /// @notice Returns the timestamp applicable for current reward calculations.
    function lastTimeRewardApplicable() public view returns (uint256) {
        return block.timestamp < periodFinish ? block.timestamp : periodFinish;
    }

    /// @notice Calculates the accumulated reward per staked token up to current timestamp.
    function rewardPerToken() public view returns (uint256) {
        if (_totalSupply == 0) {
            return rewardPerTokenStored;
        }
        return rewardPerTokenStored + (
            (lastTimeRewardApplicable() - lastUpdateTime) * rewardRate * 1e18
        ) / _totalSupply;
    }

    /// @notice Returns the total pending reward accrued for an account.
    function earned(address account) public view returns (uint256) {
        return (
            _balances[account] * (rewardPerToken() - userRewardPerTokenPaid[account])
        ) / 1e18 + rewards[account];
    }

    // ── Mutating Functions ───────────────────────────────────────────────────

    /**
     * @notice Stakes a specified amount of stakingToken into the reward pool.
     * @param amount The quantity of stakingToken to deposit.
     */
    function stake(uint256 amount) external nonReentrant updateReward(msg.sender) {
        if (amount == 0) revert ZeroAmount();

        _totalSupply += amount;
        _balances[msg.sender] += amount;

        bool success = stakingToken.transferFrom(msg.sender, address(this), amount);
        if (!success) revert StakingTransferFailed();

        emit Staked(msg.sender, amount);
    }

    /**
     * @notice Withdraws staked tokens from the pool.
     * @dev Remediation for Issue #1234:
     *      Automatically triggers `_claimReward(msg.sender)` prior to balance updates.
     *      This prevents forfeiture of earned rewards when unstaking partial or entire balances.
     * @param amount The quantity of stakingToken to withdraw.
     */
    function withdraw(uint256 amount) public nonReentrant updateReward(msg.sender) {
        if (amount == 0) revert ZeroAmount();
        if (_balances[msg.sender] < amount) {
            revert InsufficientBalance(amount, _balances[msg.sender]);
        }

        // ── 1. Automatically trigger reward payout prior to balance updates ────
        _claimReward(msg.sender);

        // ── 2. Update staking balances ─────────────────────────────────────────
        _balances[msg.sender] -= amount;
        _totalSupply -= amount;

        // ── 3. Transfer staked tokens back to the user ─────────────────────────
        bool success = stakingToken.transfer(msg.sender, amount);
        if (!success) revert StakingTransferFailed();

        emit Withdrawn(msg.sender, amount);
    }

    /**
     * @notice Manually claims pending earned reward tokens.
     */
    function claimReward() public nonReentrant updateReward(msg.sender) {
        _claimReward(msg.sender);
    }

    /**
     * @notice Convenience function to withdraw entire staked balance and claim rewards.
     */
    function exit() external {
        withdraw(_balances[msg.sender]);
    }

    /**
     * @dev Internal helper that transfers pending rewards to `account`.
     *      State is cleared before token transfer to respect Checks-Effects-Interactions.
     */
    function _claimReward(address account) internal {
        uint256 reward = rewards[account];
        if (reward > 0) {
            rewards[account] = 0;
            bool success = rewardToken.transfer(account, reward);
            if (!success) revert RewardTransferFailed();
            emit RewardPaid(account, reward);
        }
    }

    // ── Administrative Functions ─────────────────────────────────────────────

    /**
     * @notice Notifies the contract of a newly added reward distribution amount.
     * @param reward The amount of rewardToken added for distribution.
     */
    function notifyRewardAmount(uint256 reward) external onlyOwner updateReward(address(0)) {
        if (block.timestamp >= periodFinish) {
            rewardRate = reward / rewardsDuration;
        } else {
            uint256 remaining = periodFinish - block.timestamp;
            uint256 leftover = remaining * rewardRate;
            rewardRate = (reward + leftover) / rewardsDuration;
        }

        uint256 contractBalance = rewardToken.balanceOf(address(this));
        if (stakingToken == rewardToken) {
            contractBalance -= _totalSupply;
        }
        if (rewardRate * rewardsDuration > contractBalance) {
            revert RewardRateTooHigh(rewardRate * rewardsDuration, contractBalance);
        }

        lastUpdateTime = block.timestamp;
        periodFinish = block.timestamp + rewardsDuration;
        emit RewardAdded(reward);
    }

    /**
     * @notice Updates the rewards distribution duration. Can only be set when period has ended.
     * @param _rewardsDuration The new rewards duration in seconds.
     */
    function setRewardsDuration(uint256 _rewardsDuration) external onlyOwner {
        if (block.timestamp < periodFinish) {
            revert RewardDurationNotFinished(periodFinish);
        }
        if (_rewardsDuration == 0) revert ZeroAmount();
        rewardsDuration = _rewardsDuration;
        emit RewardsDurationUpdated(_rewardsDuration);
    }
}
