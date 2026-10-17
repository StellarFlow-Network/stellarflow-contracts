// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title CustomToken
/// @notice Standalone example of a burnFrom() implementation with allowance enforcement.
contract CustomToken {
    string public name = "Example Token";
    string public symbol = "EXT";
    uint8 public decimals = 18;

    uint256 public totalSupply;

    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);
    event Burn(address indexed from, uint256 value);

    function mint(address to, uint256 amount) external {
        require(to != address(0), "Zero address");
        totalSupply += amount;
        balanceOf[to] += amount;
        emit Transfer(address(0), to, amount);
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        require(to != address(0), "Zero address");
        require(balanceOf[msg.sender] >= amount, "Insufficient balance");

        balanceOf[msg.sender] -= amount;
        balanceOf[to] += amount;

        emit Transfer(msg.sender, to, amount);
        return true;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        emit Approval(msg.sender, spender, amount);
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        require(from != address(0), "Zero address");
        require(to != address(0), "Zero address");
        require(balanceOf[from] >= amount, "Insufficient balance");
        require(allowance[from][msg.sender] >= amount, "Insufficient allowance");

        allowance[from][msg.sender] -= amount;
        balanceOf[from] -= amount;
        balanceOf[to] += amount;

        emit Transfer(from, to, amount);
        return true;
    }

    function burn(uint256 amount) external {
        _burn(msg.sender, amount);
    }

    function burnFrom(address account, uint256 amount) external {
        require(account != address(0), "Zero address");
        require(allowance[account][msg.sender] >= amount, "Insufficient allowance");

        allowance[account][msg.sender] -= amount;
        _burn(account, amount);
    }

    function _burn(address account, uint256 amount) internal {
        require(balanceOf[account] >= amount, "Insufficient balance");

        balanceOf[account] -= amount;
        totalSupply -= amount;

        emit Burn(account, amount);
        emit Transfer(account, address(0), amount);
    }
}
