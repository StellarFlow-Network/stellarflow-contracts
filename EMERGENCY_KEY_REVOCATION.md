# Emergency Key Revocation System

## Overview

This document describes the **Emergency Key Revocation** mechanism implemented in `src/admin.rs` and integrated into the main contract. This system provides a secure, automated way to revoke compromised administrative or coordinator keys without requiring a full contract upgrade.

## Problem Statement

If an administrative or coordinator hot-wallet key becomes compromised, the system previously lacked a secure, automated method to strip its permissions. The original process would require:
- A full contract upgrade (slow, complex)
- Manual intervention (prone to delays during emergencies)
- No instant blocking capability

## Solution Architecture

### Core Components

#### 1. EmergencyRevocationProposal Struct (in `admin.rs`)

```rust
#[contracttype]
#[derive(Clone)]
pub struct EmergencyRevocationProposal {
    /// Address to be revoked (compromised key)
    pub target: Address,
    
    /// Replacement administrator
    pub replacement: Address,
    
    /// Signer that proposed the revocation
    pub proposer: Address,
    
    /// Ledger timestamp when proposed
    pub proposed_at: u64,
    
    /// Votes from authorized signers
    pub votes: Map<Address, ()>,
}
```

#### 2. Storage Keys

- `EMERGENCY_REVOCATION_KEY`: Active emergency revocation proposal
- `REVOKED_SIGNER_KEY`: Set of revoked (blocked) addresses
- `REVOCATION_EXPIRY_SECONDS`: 7-day expiration window for proposals

#### 3. Error Handling

New contract errors:
- `RevokedAddress` (23): Address was revoked and cannot sign
- `EmergencyRevocationAlreadyActive` (24): Emergency revocation already pending
- `NoActiveEmergencyRevocation` (25): No emergency revocation proposal active

## Workflow

### Phase 1: Proposal

**Function:** `propose_emergency_revocation(target, replacement, proposer)`

**Who Can Call:** Admin or any registered signer

**Actions:**
1. Validates caller is authorized (admin or signer)
2. Checks no active revocation exists for target
3. Creates `EmergencyRevocationProposal` with:
   - Target address to be revoked
   - Replacement admin
   - Proposer identity (for audit trail)
   - Current ledger timestamp
   - Empty votes map

**Storage:** Proposal stored at `EMERGENCY_REVOCATION_KEY`

**Constraints:**
- Only one emergency revocation can be active at a time
- Proposals expire after 7 days if not executed

### Phase 2: Multi-Sig Voting

**Function:** `vote_emergency_revocation(voter)`

**Who Can Call:** Admin or any registered signer (except revoked addresses)

**Actions:**
1. Validates voter is authorized and not revoked
2. Checks proposal hasn't expired (7 days)
3. Prevents double-voting
4. Adds vote to proposal's vote map
5. Calculates threshold: `(signers_count + 1) / 2 + 1` (simple majority)
6. **If threshold reached:**
   - Blocks target address in `REVOKED_SIGNER_KEY`
   - Promotes replacement to admin (if not already)
   - Removes proposal from storage
   - **Instantly effective** - no waiting period
7. **If threshold not reached:** Updates proposal with new vote count

**Returns:** Boolean indicating if execution occurred

**Threshold Calculation:**
- Total eligible voters = number of signers + 1 (admin)
- Threshold = (total / 2) + 1 (strict majority)
- Example: 4 signers + admin = 5 eligible voters → 3 votes needed

### Phase 3: Enforcement

**Enforcement Points:**

1. **Signer Registration** (`register_signer`)
   - Prevents registering revoked addresses as signers
   - Returns `RevokedAddress` error

2. **Voting Operations** (`vote_emergency_revocation`, `vote_revocation`)
   - Revoked addresses cannot participate in any voting
   - Returns `RevokedAddress` error

3. **Query Functions** (`is_address_revoked`)
   - Provides boolean status for any address
   - Accessible by any contract participant

## Public API

### In `lib.rs` Contract Implementation

```rust
// Propose emergency revocation
pub fn propose_emergency_revocation(
    env: Env,
    target: Address,           // Address to revoke
    replacement: Address,      // New admin
    proposer: Address,         // Must be signer or admin
) -> Result<(), ContractError>

// Vote on emergency revocation
pub fn vote_emergency_revocation(
    env: Env,
    voter: Address,           // Must be signer or admin
) -> Result<bool, ContractError>  // Returns true if executed

// Get current proposal
pub fn get_emergency_revocation_proposal(
    env: Env,
) -> Option<admin::EmergencyRevocationProposal>

// Get current vote count
pub fn get_emergency_revocation_vote_count(env: Env) -> Option<u32>

// Check if address is revoked
pub fn is_address_revoked(env: Env, addr: Address) -> bool

// Get all revoked addresses (placeholder)
pub fn get_revoked_addresses(env: Env) -> Vec<Address>

// Cancel active proposal (admin only)
pub fn cancel_emergency_revocation(
    env: Env,
    canceller: Address,
) -> Result<(), ContractError>
```

### In `admin.rs` Helper Functions

```rust
// Propose emergency revocation
pub fn propose_emergency_revocation(
    env: &Env,
    target: Address,
    replacement: Address,
    proposer: Address,
    signers: &Map<Address, ()>,
) -> Result<(), ContractError>

// Vote on emergency revocation
pub fn vote_emergency_revocation(
    env: &Env,
    voter: Address,
    signers: &Map<Address, ()>,
    revoked_addrs: &mut Map<Address, ()>,
) -> Result<bool, ContractError>

// Get current proposal status
pub fn get_emergency_revocation_proposal(env: &Env) -> Option<EmergencyRevocationProposal>

// Get vote count
pub fn get_emergency_revocation_vote_count(env: &Env) -> Option<u32>

// Cancel proposal
pub fn cancel_emergency_revocation(
    env: &Env,
    canceller: Address,
) -> Result<(), ContractError>
```

## Security Features

### 1. Multi-Sig Requirement
- Simple majority required (>50% of eligible voters)
- Prevents single-point-of-failure revocations
- Includes admin in voter count

### 2. Instant Execution
- No timelock delay (unlike upgrade mechanism)
- Compromise requires immediate response
- Storage flags updated instantly upon threshold

### 3. Access Control
- Only authorized signers or admin can propose
- Only authorized signers or admin can vote
- Revoked addresses cannot participate in any voting

### 4. Audit Trail
- Proposer identity stored
- Proposal timestamp recorded
- Vote history preserved until execution
- All via ledger-readable storage

### 5. Expiration Window
- Proposals expire after 7 days
- Prevents stale emergency votes from lingering
- Automatic cleanup on vote attempt past expiry

### 6. Prevention of Revoked Address Actions
- Revoked addresses cannot be re-registered as signers
- Revoked addresses cannot vote on any proposals
- Revoked addresses cannot execute contract functions requiring auth

## Usage Example

```rust
use soroban_sdk::{Address, Env};

// Setup: Initialize contract with admin
env.initialize(&admin);

// Register signers for multi-sig
env.register_signer(&signer1, &admin);
env.register_signer(&signer2, &admin);
env.register_signer(&signer3, &admin);

// EMERGENCY SCENARIO: signer2's key is compromised

// Step 1: Propose revocation (by signer1)
env.propose_emergency_revocation(
    &signer2,              // compromised key
    &admin,                // keep current admin
    &signer1,              // proposer
)?;

// Step 2: Signers vote to approve (need 2 out of 4 = 3 votes)
env.vote_emergency_revocation(&signer1)?;  // Returns false (1/3 votes)
env.vote_emergency_revocation(&signer3)?;  // Returns false (2/3 votes)
env.vote_emergency_revocation(&admin)?;    // Returns true (3/3 votes - EXECUTED)

// Step 3: Verify revocation
assert!(env.is_address_revoked(&signer2));
assert!(env.is_address_revoked(&signer1).not());

// Step 4: Attempt to register revoked address fails
assert!(env.register_signer(&signer2, &admin).is_err());
```

## Implementation Details

### Storage Efficiency
- Uses `Map<Address, ()>` for O(1) revocation lookups
- Revoked address set maintained per-contract
- Only stores what's necessary (address presence)

### Threshold Calculation
```rust
let total_eligible = signers.len() + 1; // +1 for admin
let threshold = total_eligible / 2 + 1; // Integer division
```

**Examples:**
- 1 signer + 1 admin = 2 voters → 2 votes needed (100%)
- 2 signers + 1 admin = 3 voters → 2 votes needed (67%)
- 3 signers + 1 admin = 4 voters → 3 votes needed (75%)
- 4 signers + 1 admin = 5 voters → 3 votes needed (60%)

### Vote Execution Flow
1. Validate voter authorization and not revoked
2. Check proposal not expired
3. Prevent double-voting
4. Record vote
5. Count votes
6. If threshold reached:
   - Add target to revoked addresses
   - Promote replacement to admin (if needed)
   - Clean up proposal storage
   - Return `true`
7. If threshold not reached:
   - Store updated proposal with new vote
   - Return `false`

## Deployment Checklist

- [x] `EmergencyRevocationProposal` struct defined
- [x] Error types added (`RevokedAddress`, `EmergencyRevocationAlreadyActive`, `NoActiveEmergencyRevocation`)
- [x] Storage keys defined (`EMERGENCY_REVOCATION_KEY`, `REVOKED_SIGNER_KEY`)
- [x] Proposal function implemented in `admin.rs`
- [x] Voting function implemented in `admin.rs`
- [x] Helper functions in `admin.rs`
- [x] Public contract methods in `lib.rs`
- [x] Revocation checks in sensitive operations
- [x] Error propagation for revoked addresses

## Future Enhancements

1. **Revocation Cooldown**: Add delay before revoked address can be registered again
2. **Revocation History**: Maintain append-only log of all revocations
3. **Partial Revocation**: Revoke specific permissions instead of full address block
4. **Revocation Appeal**: Allow revoked address to propose self-reinstatement
5. **Time-Delayed Revocation**: Allow revocation to take effect after delay (safety check)

## Testing Considerations

### Unit Tests Should Verify:
1. Only authorized addresses can propose revocation
2. Threshold calculation is correct for various signer counts
3. Double-voting prevention works
4. Proposal expiration after 7 days
5. Instant execution upon threshold
6. Revoked addresses cannot vote
7. Revoked addresses cannot be registered as signers
8. Replacement admin is correctly promoted
9. Revocation status persists across storage
10. Multiple concurrent revocation attempts are blocked

## References

- **Storage Model**: Instance storage for fast access, no TTL concerns
- **Auth Model**: Uses `require_auth()` for each signer
- **Error Handling**: Explicit error types for all failure modes
- **Time Source**: Stellar ledger timestamp for proposal expiration
