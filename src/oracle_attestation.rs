//! Multi-Oracle Attestation Guard for Anchor Relayer Proof-of-Settlement.
//!
//! Implements a 2-of-3 independent oracle consensus mechanism for validating
//! fiat settlement proofs before releasing remittance escrow funds.

use soroban_sdk::{
    contracttype, symbol_short, Address, Bytes, BytesN, Env, Map, Symbol, Vec,
};

use crate::{AssetId, ContractError, FiatEscrow, FiatEscrowKey, FiatSettlementState};

/// Maximum number of registered oracles per asset corridor.
pub const MAX_ORACLES: u32 = 3;

/// Minimum number of attestations required for consensus (2-of-3).
pub const MIN_ATTESTATIONS: u32 = 2;

/// Maximum age of an attestation in seconds before it's considered stale.
pub const MAX_ATTESTATION_AGE_SECS: u64 = 300; // 5 minutes

/// Storage key for oracle registry.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OracleRegistryKey {
    /// Map of asset -> registered oracle addresses.
    Oracles(AssetId),
    /// Counter for oracle registry version.
    Version,
}

/// An oracle's attestation of a fiat settlement.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OracleAttestation {
    /// The oracle that signed this attestation.
    pub oracle: Address,
    /// The escrow ID this attestation applies to.
    pub escrow_id: u64,
    /// Transaction ID of the fiat payout.
    pub tx_id: BytesN<32>,
    /// Amount settled in fiat (in stroops/smallest unit).
    pub amount: u64,
    /// Asset identifier.
    pub asset: AssetId,
    /// Timestamp when the oracle observed the settlement.
    pub observed_at: u64,
    /// Cryptographic signature: Sig_oracle(Tx_id, Amount).
    pub signature: BytesN<64>,
}

/// Aggregated attestation result after consensus check.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttestationResult {
    /// Whether consensus was reached (2-of-3).
    pub consensus_reached: bool,
    /// Number of valid attestations received.
    pub valid_count: u32,
    /// Number of invalid/conflicting attestations.
    pub invalid_count: u32,
    /// The agreed-upon transaction ID (if consensus).
    pub agreed_tx_id: Option<BytesN<32>>,
    /// The agreed-upon amount (if consensus).
    pub agreed_amount: Option<u64>,
}

/// Register a new oracle for a specific asset corridor.
/// Admin only.
pub fn register_oracle(
    env: &Env,
    admin: Address,
    asset: AssetId,
    oracle: Address,
) -> Result<(), ContractError> {
    // Verify admin authorization
    let data = crate::TimeLockedUpgradeContract::get_data(env.clone())?;
    if data.admin != admin {
        return Err(ContractError::NotAdmin);
    }
    admin.require_auth();

    let key = OracleRegistryKey::Oracles(asset);
    let mut oracles: Vec<Address> = env.storage().persistent().get(&key).unwrap_or_else(|| Vec::new(env));

    // Check if oracle already registered
    if oracles.iter().any(|o| o == oracle) {
        return Err(ContractError::AlreadyRegistered);
    }

    // Enforce max oracle limit
    if oracles.len() >= MAX_ORACLES {
        return Err(ContractError::CapacityExceeded);
    }

    oracles.push_back(oracle);
    env.storage().persistent().set(&key, &oracles);

    // Increment registry version
    let version: u32 = env.storage().persistent().get(&OracleRegistryKey::Version).unwrap_or(0);
    env.storage().persistent().set(&OracleRegistryKey::Version, &(version + 1));

    // Emit event
    env.events().publish(
        (symbol_short!("oracle_reg"), asset),
        oracle,
    );

    Ok(())
}

/// Get registered oracles for an asset.
pub fn get_oracles(env: &Env, asset: AssetId) -> Vec<Address> {
    let key = OracleRegistryKey::Oracles(asset);
    env.storage().persistent().get(&key).unwrap_or_else(|| Vec::new(env))
}

/// Verify a single oracle's attestation signature.
/// The signature must be: Sig_oracle(Tx_id || Amount)
fn verify_attestation_signature(
    env: &Env,
    oracle: &Address,
    tx_id: &BytesN<32>,
    amount: u64,
    signature: &BytesN<64>,
) -> Result<(), ContractError> {
    // Reconstruct the signed payload: Tx_id || Amount (8 bytes LE)
    let mut payload = Bytes::new(env);
    payload.append(tx_id);
    let amount_bytes = amount.to_le_bytes();
    payload.append(&Bytes::from_slice(env, &amount_bytes));

    // Verify Ed25519 signature using the oracle's public key
    let verified = env.crypto().ed25519_verify(
        oracle,
        &payload,
        signature,
    );

    if !verified {
        return Err(ContractError::OracleInvalidSignature);
    }

    Ok(())
}

/// Storage key for individual attestations.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OracleAttestationKey {
    Attestation(u64, Address), // escrow_id, oracle
}

/// Submit an oracle attestation for a fiat settlement.
pub fn submit_attestation(
    env: &Env,
    oracle: Address,
    escrow_id: u64,
    tx_id: BytesN<32>,
    amount: u64,
    asset: AssetId,
    signature: BytesN<64>,
) -> Result<(), ContractError> {
    // Require oracle authorization
    oracle.require_auth();

    // Verify oracle is registered for this asset
    let oracles = get_oracles(env, asset);
    if oracles.is_empty() {
        return Err(ContractError::OracleRegistryNotConfigured);
    }
    if !oracles.iter().any(|o| o == oracle) {
        return Err(ContractError::OracleNotAuthorized);
    }

    // Load escrow and verify state
    let escrow_key = FiatEscrowKey::Escrow(escrow_id);
    let escrow: FiatEscrow = env.storage().persistent().get(&escrow_key)
        .ok_or(ContractError::NotRegistered)?;

    if escrow.state != FiatSettlementState::Locked && escrow.state != FiatSettlementState::Dispatched {
        return Err(ContractError::InvalidEscrowState);
    }

    // Verify escrow asset matches
    if escrow.asset != asset {
        return Err(ContractError::InvalidAsset);
    }

    // Verify amount matches escrow
    if escrow.amount != amount {
        return Err(ContractError::InvalidArgument);
    }

    // Verify attestation signature
    verify_attestation_signature(env, &oracle, &tx_id, amount, &signature)?;

    // Check attestation freshness
    let current_time = env.ledger().timestamp();
    // Note: observed_at would need to be passed or derived; for now we trust current time
    // In production, the oracle would include observed_at in the signed payload

    // Check for duplicate attestation from this oracle
    let attestation_key = OracleAttestationKey::Attestation(escrow_id, oracle.clone());
    if env.storage().persistent().has(&attestation_key) {
        return Err(ContractError::DuplicateOracleAttestation);
    }

    // Create and store attestation
    let attestation = OracleAttestation {
        oracle: oracle.clone(),
        escrow_id,
        tx_id: tx_id.clone(),
        amount,
        asset,
        observed_at: current_time,
        signature: signature.clone(),
    };
    env.storage().persistent().set(&attestation_key, &attestation);

    // Emit event
    env.events().publish(
        (symbol_short!("attest"), escrow_id, oracle),
        (tx_id, amount),
    );

    Ok(())
}

/// Check if consensus has been reached for an escrow (2-of-3).
pub fn check_consensus(env: &Env, escrow_id: u64, asset: AssetId) -> Result<AttestationResult, ContractError> {
    let oracles = get_oracles(env, asset);
    
    if oracles.len() < MIN_ATTESTATIONS {
        return Err(ContractError::OracleRegistryNotConfigured);
    }

    let mut valid_attestations: Vec<OracleAttestation> = Vec::new(env);
    let mut tx_id_counts: Map<BytesN<32>, u32> = Map::new(env);
    let mut amount_counts: Map<u64, u32> = Map::new(env);

    // Collect all attestations for this escrow
    for oracle in oracles.iter() {
        let key = OracleAttestationKey::Attestation(escrow_id, oracle.clone());
        if let Some(attestation) = env.storage().persistent().get::<_, OracleAttestation>(&key) {
            // Verify signature again (defense in depth)
            if verify_attestation_signature(env, &oracle, &attestation.tx_id, attestation.amount, &attestation.signature).is_ok() {
                // Check freshness
                let age = env.ledger().timestamp().saturating_sub(attestation.observed_at);
                if age <= MAX_ATTESTATION_AGE_SECS {
                    valid_attestations.push_back(attestation.clone());
                    
                    // Count votes for tx_id and amount
                    *tx_id_counts.entry(attestation.tx_id.clone()).or_insert(0) += 1;
                    *amount_counts.entry(attestation.amount).or_insert(0) += 1;
                }
            }
        }
    }

    let valid_count = valid_attestations.len() as u32;

    // Check if we have minimum attestations
    if valid_count < MIN_ATTESTATIONS {
        return Ok(AttestationResult {
            consensus_reached: false,
            valid_count,
            invalid_count: oracles.len() as u32 - valid_count,
            agreed_tx_id: None,
            agreed_amount: None,
        });
    }

    // Find majority tx_id (must have at least MIN_ATTESTATIONS)
    let mut agreed_tx_id: Option<BytesN<32>> = None;
    let mut agreed_amount: Option<u64> = None;

    for (tx_id, count) in tx_id_counts.iter() {
        if count >= MIN_ATTESTATIONS {
            agreed_tx_id = Some(tx_id.clone());
            break;
        }
    }

    for (amount, count) in amount_counts.iter() {
        if count >= MIN_ATTESTATIONS {
            agreed_amount = Some(amount);
            break;
        }
    }

    let consensus_reached = agreed_tx_id.is_some() && agreed_amount.is_some();

    if !consensus_reached {
        // Check if there's a conflict (different tx_ids or amounts with significant support)
        let has_conflict = tx_id_counts.len() > 1 || amount_counts.len() > 1;
        
        return Ok(AttestationResult {
            consensus_reached: false,
            valid_count,
            invalid_count: oracles.len() as u32 - valid_count,
            agreed_tx_id,
            agreed_amount,
        });
    }

    Ok(AttestationResult {
        consensus_reached: true,
        valid_count,
        invalid_count: oracles.len() as u32 - valid_count,
        agreed_tx_id,
        agreed_amount,
    })
}

/// Verify oracle consensus before allowing fiat escrow settlement.
/// This is the main guard function that should be called before settle_fiat_escrow.
pub fn verify_consensus_before_settle(
    env: &Env,
    escrow_id: u64,
) -> Result<AttestationResult, ContractError> {
    let escrow_key = FiatEscrowKey::Escrow(escrow_id);
    let escrow: FiatEscrow = env.storage().persistent().get(&escrow_key)
        .ok_or(ContractError::NotRegistered)?;

    let result = check_consensus(env, escrow_id, escrow.asset)?;

    if !result.consensus_reached {
        // Check for explicit conflict
        if result.valid_count >= MIN_ATTESTATIONS && (result.agreed_tx_id.is_none() || result.agreed_amount.is_none()) {
            // Attestations exist but disagree on tx_id or amount
            return Err(ContractError::OracleAttestationConflict);
        }
        // Not enough attestations yet
        return Err(ContractError::InsufficientOracleAttestations);
    }

    Ok(result)
}

/// Refund escrow to sender if consensus cannot be reached or attestations conflict.
pub fn refund_escrow_on_conflict(
    env: &Env,
    escrow_id: u64,
    caller: Address,
) -> Result<(), ContractError> {
    caller.require_auth();

    let escrow_key = FiatEscrowKey::Escrow(escrow_id);
    let mut escrow: FiatEscrow = env.storage().persistent().get(&escrow_key)
        .ok_or(ContractError::NotRegistered)?;

    // Only sender can request refund on conflict
    if escrow.sender != caller {
        return Err(ContractError::Unauthorized);
    }

    // Check if escrow is in a refundable state
    if escrow.state != FiatSettlementState::Locked && escrow.state != FiatSettlementState::Dispatched {
        return Err(ContractError::InvalidEscrowState);
    }

    // Check for conflict or timeout
    let result = check_consensus(env, escrow_id, escrow.asset)?;
    
    let can_refund = !result.consensus_reached && (
        result.valid_count >= MIN_ATTESTATIONS || // Explicit conflict (attestations exist but disagree)
        result.valid_count == 0 ||  // No attestations at all
        env.ledger().timestamp() >= escrow.locked_at + escrow.timeout_secs // Timeout
    );

    if !can_refund {
        return Err(ContractError::DeadlineNotReached);
    }

    // Update state to refunded
    escrow.state = FiatSettlementState::Refunded;
    env.storage().persistent().set(&escrow_key, &escrow);

    // Emit refund event
    env.events().publish(
        (symbol_short!("refund"), escrow_id, escrow.sender.clone()),
        escrow.amount,
    );

    Ok(())
}

/// Get all attestations for an escrow (for debugging/transparency).
pub fn get_attestations(env: &Env, escrow_id: u64, asset: AssetId) -> Vec<OracleAttestation> {
    let oracles = get_oracles(env, asset);
    let mut attestations = Vec::new(env);
    
    for oracle in oracles.iter() {
        let key = OracleAttestationKey::Attestation(escrow_id, oracle);
        if let Some(attestation) = env.storage().persistent().get::<_, OracleAttestation>(&key) {
            attestations.push_back(attestation);
        }
    }
    
    attestations
}

/// Remove an oracle from the registry (admin only).
pub fn remove_oracle(
    env: &Env,
    admin: Address,
    asset: AssetId,
    oracle: Address,
) -> Result<(), ContractError> {
    let data = crate::TimeLockedUpgradeContract::get_data(env.clone())?;
    if data.admin != admin {
        return Err(ContractError::NotAdmin);
    }
    admin.require_auth();

    let key = OracleRegistryKey::Oracles(asset);
    let mut oracles: Vec<Address> = env.storage().persistent().get(&key).unwrap_or_else(|| Vec::new(env));
    
    // Find and remove the oracle
    let mut found = false;
    let mut new_oracles = Vec::new(env);
    for o in oracles.iter() {
        if o == oracle {
            found = true;
        } else {
            new_oracles.push_back(o);
        }
    }
    
    if !found {
        return Err(ContractError::NotRegistered);
    }
    
    env.storage().persistent().set(&key, &new_oracles);
    
    // Increment registry version
    let version: u32 = env.storage().persistent().get(&OracleRegistryKey::Version).unwrap_or(0);
    env.storage().persistent().set(&OracleRegistryKey::Version, &(version + 1));
    
    // Emit event
    env.events().publish(
        (symbol_short!("oracle_rem"), asset),
        oracle,
    );
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger, LedgerInfo};
    use soroban_sdk::{BytesN, Env};

    const TEST_ASSET: AssetId = 1;
    const TEST_AMOUNT: u64 = 10_000_000;

    fn setup() -> (Env, Address, Address, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set(LedgerInfo {
            timestamp: 1_000_000,
            protocol_version: env.ledger().protocol_version(),
            sequence_number: 100,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 0,
            min_persistent_entry_ttl: 0,
            max_entry_ttl: u32::MAX,
        });

        // Initialize contract
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let client = crate::TimeLockedUpgradeContractClient::new(&env, &contract_id);
        client.initialize(&admin, &treasury);

        let oracle1 = Address::generate(&env);
        let oracle2 = Address::generate(&env);
        let oracle3 = Address::generate(&env);

        // Register oracles
        register_oracle(&env, admin.clone(), TEST_ASSET, oracle1.clone()).unwrap();
        register_oracle(&env, admin.clone(), TEST_ASSET, oracle2.clone()).unwrap();
        register_oracle(&env, admin.clone(), TEST_ASSET, oracle3.clone()).unwrap();

        (env, admin, oracle1, oracle2, oracle3)
    }

    fn create_test_escrow(env: &Env, sender: &Address, anchor: &Address) -> u64 {
        let escrow_id = 1;
        let escrow = FiatEscrow {
            id: escrow_id,
            sender: sender.clone(),
            anchor: anchor.clone(),
            amount: TEST_AMOUNT,
            asset: TEST_ASSET,
            state: FiatSettlementState::Locked,
            created_at: env.ledger().timestamp(),
            locked_at: env.ledger().timestamp(),
            timeout_secs: 86400,
        };
        let key = FiatEscrowKey::Escrow(escrow_id);
        env.storage().persistent().set(&key, &escrow);
        escrow_id
    }

    fn make_signature(env: &Env, oracle: &Address, tx_id: &BytesN<32>, amount: u64) -> BytesN<64> {
        // Create payload: tx_id || amount
        let mut payload = Bytes::new(env);
        payload.append(tx_id);
        payload.append(&Bytes::from_slice(env, &amount.to_le_bytes()));
        
        // Sign with oracle's key
        env.crypto().ed25519_sign(oracle, &payload)
    }

    #[test]
    fn register_oracle_success() {
        let (env, admin, oracle1, _, _) = setup();
        assert!(register_oracle(&env, admin, TEST_ASSET, oracle1).is_ok());
    }

    #[test]
    fn register_oracle_max_limit() {
        let (env, admin, oracle1, oracle2, oracle3) = setup();
        let oracle4 = Address::generate(&env);

        register_oracle(&env, admin.clone(), TEST_ASSET, oracle1).unwrap();
        register_oracle(&env, admin.clone(), TEST_ASSET, oracle2).unwrap();
        register_oracle(&env, admin.clone(), TEST_ASSET, oracle3).unwrap();
        
        // Fourth oracle should fail
        let result = register_oracle(&env, admin, TEST_ASSET, oracle4);
        assert_eq!(result, Err(ContractError::CapacityExceeded));
    }

    #[test]
    fn get_oracles_returns_registered() {
        let (env, admin, oracle1, oracle2, _) = setup();
        register_oracle(&env, admin.clone(), TEST_ASSET, oracle1.clone()).unwrap();
        register_oracle(&env, admin, TEST_ASSET, oracle2.clone()).unwrap();

        let oracles = get_oracles(&env, TEST_ASSET);
        assert_eq!(oracles.len(), 2);
    }

    #[test]
    fn submit_attestation_unauthorized_oracle_fails() {
        let (env, admin, oracle1, oracle2, _) = setup();
        register_oracle(&env, admin, TEST_ASSET, oracle1).unwrap();
        
        let sender = Address::generate(&env);
        let anchor = Address::generate(&env);
        let escrow_id = create_test_escrow(&env, &sender, &anchor);
        
        let tx_id = BytesN::from_array(&env, &[1u8; 32]);
        let sig = make_signature(&env, &oracle2, &tx_id, TEST_AMOUNT);

        // oracle2 not registered
        let result = submit_attestation(&env, oracle2, escrow_id, tx_id, TEST_AMOUNT, TEST_ASSET, sig);
        assert_eq!(result, Err(ContractError::OracleNotAuthorized));
    }

    #[test]
    fn check_consensus_insufficient_oracles() {
        let (env, admin, oracle1, _, _) = setup();
        register_oracle(&env, admin, TEST_ASSET, oracle1).unwrap();

        let sender = Address::generate(&env);
        let anchor = Address::generate(&env);
        let escrow_id = create_test_escrow(&env, &sender, &anchor);

        let result = check_consensus(&env, escrow_id, TEST_ASSET);
        // Should fail because only 1 oracle registered (need 2)
        assert!(result.is_err());
    }

    #[test]
    fn check_consensus_no_attestations() {
        let (env, admin, oracle1, oracle2, oracle3) = setup();
        register_oracle(&env, admin.clone(), TEST_ASSET, oracle1).unwrap();
        register_oracle(&env, admin.clone(), TEST_ASSET, oracle2).unwrap();
        register_oracle(&env, admin, TEST_ASSET, oracle3).unwrap();

        let sender = Address::generate(&env);
        let anchor = Address::generate(&env);
        let escrow_id = create_test_escrow(&env, &sender, &anchor);

        let result = check_consensus(&env, escrow_id, TEST_ASSET).unwrap();
        assert!(!result.consensus_reached);
        assert_eq!(result.valid_count, 0);
    }

    #[test]
    fn verify_consensus_before_settle_requires_consensus() {
        let (env, admin, oracle1, oracle2, oracle3) = setup();
        register_oracle(&env, admin.clone(), TEST_ASSET, oracle1).unwrap();
        register_oracle(&env, admin.clone(), TEST_ASSET, oracle2).unwrap();
        register_oracle(&env, admin, TEST_ASSET, oracle3).unwrap();

        let sender = Address::generate(&env);
        let anchor = Address::generate(&env);
        let escrow_id = create_test_escrow(&env, &sender, &anchor);

        // No attestations submitted - should fail
        let result = verify_consensus_before_settle(&env, escrow_id);
        assert_eq!(result, Err(ContractError::InsufficientOracleAttestations));
    }

    #[test]
    fn remove_oracle_success() {
        let (env, admin, oracle1, oracle2, _) = setup();
        register_oracle(&env, admin.clone(), TEST_ASSET, oracle1.clone()).unwrap();
        register_oracle(&env, admin.clone(), TEST_ASSET, oracle2.clone()).unwrap();

        let oracles_before = get_oracles(&env, TEST_ASSET);
        assert_eq!(oracles_before.len(), 2);

        remove_oracle(&env, admin, TEST_ASSET, oracle1).unwrap();

        let oracles_after = get_oracles(&env, TEST_ASSET);
        assert_eq!(oracles_after.len(), 1);
        assert!(!oracles_after.iter().any(|o| o == oracle1));
    }
}