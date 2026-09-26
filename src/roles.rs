//! Time-boxed, revocable role delegation for administrative permissions
//! (Issue #703).
//!
//! Roles such as `PauseAdmin` or `FeeAdmin` are granted with an explicit
//! `expiration_ledger`. Once the current ledger sequence reaches that value
//! the grant is treated as expired everywhere (`has_role` / `require_role`)
//! without requiring any further transaction — access is gated purely by
//! comparing ledger sequences at read time. The admin may also strip a grant
//! early via `revoke_role`, e.g. in response to a compromised delegate key.

use soroban_sdk::{contracttype, Address, BytesN, Env, Symbol, Vec};

use crate::{ContractData, ContractError, DATA_KEY};

/// Delegable administrative permissions.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    /// Legacy role names retained for compatibility with existing tests and APIs.
    PauseAdmin,
    FeeAdmin,
    UpgradeAdmin,
    TreasuryAdmin,
    /// Granular RBAC names required by the administrative access-control issue.
    ROLE_PAUSER,
    ROLE_FEE_SETTER,
    ROLE_ORACLE_MANAGER,
    ROLE_ADMIN,
}

/// A single role grant, scoped to one `(grantee, role)` pair.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct RoleGrant {
    pub role: Role,
    pub role_hash: BytesN<32>,
    pub grantee: Address,
    pub granted_by: Address,
    pub granted_at_ledger: u32,
    /// Ledger sequence at/after which this grant is no longer valid.
    pub expiration_ledger: u32,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleAuditEvent {
    pub event_name: Symbol,
    pub role: Role,
    pub role_hash: BytesN<32>,
    pub actor: Address,
    pub grantee: Address,
    pub ledger: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RoleStorageKey {
    Grant(Address, Role),
    ActiveRoleHash(Address, BytesN<32>),
    AuditLog,
}

pub fn role_hash(env: &Env, role: Role) -> BytesN<32> {
    let name = match role {
        Role::PauseAdmin | Role::ROLE_PAUSER => "ROLE_PAUSER",
        Role::FeeAdmin | Role::ROLE_FEE_SETTER => "ROLE_FEE_SETTER",
        Role::UpgradeAdmin => "ROLE_UPGRADE_ADMIN",
        Role::TreasuryAdmin => "ROLE_TREASURY_ADMIN",
        Role::ROLE_ORACLE_MANAGER => "ROLE_ORACLE_MANAGER",
        Role::ROLE_ADMIN => "ROLE_ADMIN",
    };
    let bytes = name.as_bytes();
    env.crypto().sha256(bytes)
}

fn append_role_audit_event(env: &Env, event_name: Symbol, grant: &RoleGrant, actor: &Address) {
    let event = RoleAuditEvent {
        event_name: event_name.clone(),
        role: grant.role,
        role_hash: grant.role_hash,
        actor: actor.clone(),
        grantee: grant.grantee.clone(),
        ledger: env.ledger().sequence(),
    };

    let mut log: Vec<RoleAuditEvent> = env
        .storage()
        .instance()
        .get(&RoleStorageKey::AuditLog)
        .unwrap_or_else(|| Vec::new(env));
    log.push_back(event);
    env.storage()
        .instance()
        .set(&RoleStorageKey::AuditLog, &log);
}

pub fn get_role_audit_events(env: &Env) -> Vec<RoleAuditEvent> {
    env.storage()
        .instance()
        .get(&RoleStorageKey::AuditLog)
        .unwrap_or_else(|| Vec::new(env))
}

fn require_admin(env: &Env, caller: &Address) -> Result<(), ContractError> {
    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;
    if &data.admin == caller || has_role(env, caller, Role::ROLE_ADMIN) {
        caller.require_auth();
        return Ok(());
    }
    Err(ContractError::NotAdmin)
}

/// Grant `role` to `grantee`, valid up until (but excluding) `expiration_ledger`.
/// Admin-only. Overwrites any existing grant for the same `(grantee, role)` pair.
pub fn grant_role(
    env: &Env,
    admin: Address,
    grantee: Address,
    role: Role,
    expiration_ledger: u32,
) -> Result<RoleGrant, ContractError> {
    require_admin(env, &admin)?;

    let current_ledger = env.ledger().sequence();
    if expiration_ledger <= current_ledger {
        return Err(ContractError::RoleExpirationInPast);
    }

    let role_id = role_hash(env, role);
    let grant = RoleGrant {
        role,
        role_hash: role_id,
        grantee: grantee.clone(),
        granted_by: admin.clone(),
        granted_at_ledger: current_ledger,
        expiration_ledger,
    };

    let key = RoleStorageKey::Grant(grantee.clone(), role);
    env.storage().persistent().set(&key, &grant);
    env.storage().persistent().extend_ttl(
        &key,
        crate::storage::PERSISTENT_TTL_THRESHOLD,
        crate::storage::PERSISTENT_TTL_THRESHOLD,
    );
    env.storage().instance().set(
        &RoleStorageKey::ActiveRoleHash(grantee.clone(), role_id),
        &true,
    );

    env.events().publish(
        (Symbol::new(env, "RoleGranted"), role, grantee.clone()),
        (admin.clone(), current_ledger),
    );
    append_role_audit_event(env, Symbol::new(env, "RoleGranted"), &grant, &admin);

    Ok(grant)
}

/// Explicit admin override entrypoint: revoke a role before its natural
/// expiration (e.g. because the delegate's key is suspected compromised).
pub fn revoke_role(
    env: &Env,
    admin: Address,
    grantee: Address,
    role: Role,
) -> Result<(), ContractError> {
    require_admin(env, &admin)?;

    let key = RoleStorageKey::Grant(grantee.clone(), role);
    let grant = env
        .storage()
        .persistent()
        .get::<_, RoleGrant>(&key)
        .ok_or(ContractError::RoleNotFound)?;

    env.storage().persistent().remove(&key);
    env.storage()
        .instance()
        .remove(&RoleStorageKey::ActiveRoleHash(
            grantee.clone(),
            grant.role_hash,
        ));

    env.events().publish(
        (Symbol::new(env, "RoleRevoked"), grant.role, grantee.clone()),
        (admin.clone(), env.ledger().sequence()),
    );
    append_role_audit_event(env, Symbol::new(env, "RoleRevoked"), &grant, &admin);
    Ok(())
}

/// Returns `true` only when a live (non-expired, non-revoked) grant exists
/// for `(grantee, role)`.
pub fn has_role(env: &Env, grantee: &Address, role: Role) -> bool {
    let key = RoleStorageKey::Grant(grantee.clone(), role);
    match env.storage().persistent().get::<_, RoleGrant>(&key) {
        Some(grant) => {
            let active = env.ledger().sequence() < grant.expiration_ledger;
            active
                && env
                    .storage()
                    .instance()
                    .has(&RoleStorageKey::ActiveRoleHash(
                        grantee.clone(),
                        grant.role_hash,
                    ))
        }
        None => false,
    }
}

/// Enforcing guard for call sites that require `grantee` to currently hold a
/// live `role`. Other modules (e.g. the pause/fee entrypoints) should call
/// this at the top of the guarded function.
pub fn require_role(env: &Env, grantee: &Address, role: Role) -> Result<(), ContractError> {
    if has_role(env, grantee, role) {
        Ok(())
    } else {
        Err(ContractError::RoleExpiredOrMissing)
    }
}

/// Read the raw grant record, including already-expired grants that have not
/// been explicitly revoked/pruned. Callers wanting enforcement should use
/// `has_role` / `require_role` instead, since those apply the expiration
/// check that this read-only accessor deliberately does not.
pub fn get_role_grant(env: &Env, grantee: Address, role: Role) -> Option<RoleGrant> {
    let key = RoleStorageKey::Grant(grantee, role);
    env.storage().persistent().get(&key)
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BridgeValidatorSet {
    pub sequence: u32,
    pub validator_hashes: Vec<BytesN<32>>,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BridgeStorageKey {
    ValidatorSet,
}

/// Returns the current bridge validator set, defaulting to sequence 0 and no
/// validators.
pub fn get_bridge_validator_set(env: &Env) -> BridgeValidatorSet {
    env.storage()
        .instance()
        .get(&BridgeStorageKey::ValidatorSet)
        .unwrap_or(BridgeValidatorSet {
            sequence: 0,
            validator_hashes: Vec::new(env),
        })
}

/// Rotate the cross-chain bridge validator public-key hashes.
///
/// Only the contract admin (typically a governance multi-sig) may invoke this.
/// The rotation sequence is incremented and recorded alongside the new set,
/// preventing replay of attestations signed by a stale validator set.
pub fn update_bridge_validators(
    env: &Env,
    admin: Address,
    validator_hashes: Vec<BytesN<32>>,
) -> Result<u32, ContractError> {
    require_admin(env, &admin)?;

    let previous = get_bridge_validator_set(env);
    let sequence = previous.sequence + 1;
    let new_set = BridgeValidatorSet {
        sequence,
        validator_hashes: validator_hashes.clone(),
    };

    env.storage()
        .instance()
        .set(&BridgeStorageKey::ValidatorSet, &new_set);

    env.events().publish(
        (Symbol::new(env, "BridgeValidatorsUpdated"), sequence),
        validator_hashes,
    );

    Ok(sequence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger, LedgerInfo};

    fn setup() -> (
        Env,
        crate::TimeLockedUpgradeContractClient<'static>,
        Address,
    ) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        let client = crate::TimeLockedUpgradeContractClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        client.initialize(&admin, &treasury);
        (env, client, admin)
    }

    fn set_sequence(env: &Env, sequence: u32) {
        env.ledger().set(LedgerInfo {
            timestamp: env.ledger().timestamp(),
            protocol_version: env.ledger().protocol_version(),
            sequence_number: sequence,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 0,
            min_persistent_entry_ttl: 0,
            max_entry_ttl: u32::MAX,
        });
    }

    #[test]
    fn grant_role_succeeds_for_admin_and_is_active() {
        let (env, client, admin) = setup();
        let grantee = Address::generate(&env);
        let expiry = env.ledger().sequence() + 100;
        client.grant_role(&admin, &grantee, &Role::PauseAdmin, &expiry);
        assert!(client.has_role(&grantee, &Role::PauseAdmin));
    }

    #[test]
    fn role_expires_after_expiration_ledger() {
        let (env, client, admin) = setup();
        let grantee = Address::generate(&env);
        let expiry = env.ledger().sequence() + 10;
        client.grant_role(&admin, &grantee, &Role::FeeAdmin, &expiry);
        assert!(client.has_role(&grantee, &Role::FeeAdmin));
        set_sequence(&env, expiry);
        assert!(!client.has_role(&grantee, &Role::FeeAdmin));
    }

    #[test]
    fn non_admin_cannot_grant_role() {
        let (env, client, _admin) = setup();
        let attacker = Address::generate(&env);
        let grantee = Address::generate(&env);
        let expiry = env.ledger().sequence() + 10;
        let result = client.try_grant_role(&attacker, &grantee, &Role::PauseAdmin, &expiry);
        assert_eq!(result, Err(Ok(ContractError::NotAdmin)));
    }

    #[test]
    fn grant_role_rejects_expiration_in_the_past() {
        let (env, client, admin) = setup();
        let grantee = Address::generate(&env);
        let now = env.ledger().sequence();
        let result = client.try_grant_role(&admin, &grantee, &Role::PauseAdmin, &now);
        assert_eq!(result, Err(Ok(ContractError::RoleExpirationInPast)));
    }

    #[test]
    fn revoke_role_removes_access_immediately() {
        let (env, client, admin) = setup();
        let grantee = Address::generate(&env);
        let expiry = env.ledger().sequence() + 1000;
        client.grant_role(&admin, &grantee, &Role::TreasuryAdmin, &expiry);
        assert!(client.has_role(&grantee, &Role::TreasuryAdmin));
        client.revoke_role(&admin, &grantee, &Role::TreasuryAdmin);
        assert!(!client.has_role(&grantee, &Role::TreasuryAdmin));
    }

    #[test]
    fn revoke_role_fails_when_no_grant_exists() {
        let (env, client, admin) = setup();
        let grantee = Address::generate(&env);
        let result = client.try_revoke_role(&admin, &grantee, &Role::UpgradeAdmin);
        assert_eq!(result, Err(Ok(ContractError::RoleNotFound)));
    }

    #[test]
    fn distinct_roles_for_same_grantee_are_independent() {
        let (env, client, admin) = setup();
        let grantee = Address::generate(&env);
        let expiry = env.ledger().sequence() + 50;
        client.grant_role(&admin, &grantee, &Role::PauseAdmin, &expiry);
        assert!(client.has_role(&grantee, &Role::PauseAdmin));
        assert!(!client.has_role(&grantee, &Role::FeeAdmin));
    }

    #[test]
    fn named_rbac_roles_are_active_and_audited() {
        let (env, client, admin) = setup();
        let pauser = Address::generate(&env);
        let fee_setter = Address::generate(&env);
        let oracle_manager = Address::generate(&env);
        let expiry = env.ledger().sequence() + 120;

        client.grant_role(&admin, &pauser, &Role::ROLE_PAUSER, &expiry);
        client.grant_role(&admin, &fee_setter, &Role::ROLE_FEE_SETTER, &expiry);
        client.grant_role(&admin, &oracle_manager, &Role::ROLE_ORACLE_MANAGER, &expiry);

        assert!(client.has_role(&pauser, &Role::ROLE_PAUSER));
        assert!(client.has_role(&fee_setter, &Role::ROLE_FEE_SETTER));
        assert!(client.has_role(&oracle_manager, &Role::ROLE_ORACLE_MANAGER));

        client.revoke_role(&admin, &pauser, &Role::ROLE_PAUSER);
        assert!(!client.has_role(&pauser, &Role::ROLE_PAUSER));

        let events = env.events().all();
        let event_debug = format!("{:?}", events);
        assert!(event_debug.contains("RoleGranted"));
        assert!(event_debug.contains("RoleRevoked"));
    }
}
