# Pull Request: Delegate Vote Implementation for Sector Specialists

## Summary

Implemented the `delegate_vote` function to allow high-level admins to delegate their voting power for specific asset groups to "Sector Specialists" (e.g., a specialist for West African currencies).

## Changes Overview

| Component | Files Modified | Lines Added |
|-----------|----------------|-------------|
| Storage Types | `types.rs` | +45 |
| Contract Interface | `lib.rs` | +130 |
| Tests | `test.rs` | +95 |
| **Total** | **3 files** | **~270 lines** |

---

## Technical Implementation

### 1. New Data Types (`types.rs`)

```rust
// Storage keys for delegation tracking
DelegateInfo(Address),    // Delegation info for an admin
DelegatedVotes(Address),  // Delegated votes for a delegate

// Data structures
pub struct DelegateInfo {
    pub delegator: Address,    // The admin granting delegation
    pub delegate: Address,     // The Sector Specialist receiving power
    pub asset_group: Symbol,   // Asset group (e.g., "NGN", "GHS")
    pub delegated_at: u64,     // Timestamp of delegation
    pub is_active: bool,       // Active status
}

pub struct DelegatedVote {
    pub delegator: Address,
    pub asset_group: Symbol,
    pub delegated_at: u64,
}
```

### 2. New Functions (`lib.rs`)

| Function | Description | Access |
|----------|-------------|--------|
| `delegate_vote(delegator, delegate, asset_group)` | Delegate voting power to Sector Specialist | Admin only |
| `revoke_delegation(delegator, asset_group)` | Revoke previously delegated vote | Delegator only |
| `get_delegation(delegator, asset_group)` | Query delegation info | Public |
| `get_delegate_votes(delegate)` | Get all votes for a delegate | Public |

### 3. Security Features

- ✅ **Admin-only delegation**: Only authorized admins can delegate voting power
- ✅ **Self-delegation prevention**: Cannot delegate to oneself
- ✅ **Ownership verification**: Only original delegator can revoke
- ✅ **Event logging**: Emits `VoteDelegated` and `DelegationRevoked` events

---

## Usage Example

```rust
// Admin delegates voting power for West African currencies to a specialist
let delegator = Address::from_string("GA...ADMIN");
let delegate = Address::from_string("GA...SPECIALIST");
let asset_group = Symbol::new(&env, "NGN");

// Delegate voting power
contract.delegate_vote(&delegator, &delegate, &asset_group);

// Query delegation
let delegation = contract.get_delegation(&delegator, &asset_group);
assert!(delegation.unwrap().is_active);

// Revoke delegation later
contract.revoke_delegation(&delegator, &asset_group);
```

---

## Test Coverage

| Test Name | Description | Status |
|-----------|-------------|--------|
| `test_delegate_vote_success` | Valid admin delegates to specialist | ✅ |
| `test_delegate_vote_unauthorized_non_admin` | Non-admin cannot delegate | ✅ |
| `test_delegate_vote_cannot_delegate_to_self` | Self-delegation rejected | ✅ |
| `test_revoke_delegation_success` | Delegator can revoke their delegation | ✅ |
| `test_revoke_delegation_unauthorized` | Non-delegator cannot revoke | ✅ |
| `test_get_delegate_votes_returns_delegations` | Query delegate's vote list | ✅ |
| `test_delegate_vote_multiple_assets_same_delegate` | Multiple asset groups to same delegate | ✅ |

---

## Build & Test Commands

```bash
# Navigate to contract directory
cd contracts/price-oracle

# Build the contract
cargo build --target wasm32-unknown-unknown --release

# Run all tests
cargo test

# Run only delegate tests
cargo test delegate
```

---

## Related Documentation

- [PR_DESCRIPTION.md](../../PR_DESCRIPTION.md) - Original feature requirements
- [QUICK_REFERENCE.md](../../QUICK_REFERENCE.md) - API quick reference
- [CALLBACK_INTERFACE.md](../../CALLBACK_INTERFACE.md) - Related callback docs

---

## Checklist

- [x] Implement `delegate_vote` function
- [x] Implement `revoke_delegation` function  
- [x] Add getter functions for delegation queries
- [x] Add storage keys and data types
- [x] Write comprehensive tests
- [x] Emit appropriate events
- [x] Add documentation comments

---

## Notes

- The implementation uses `ledger().timestamp()` for accurate time tracking
- Delegations are stored per (delegator, asset_group) combination
- Multiple delegations to the same delegate are tracked in a vote list
- The contract follows the existing auth pattern from `crate::auth`