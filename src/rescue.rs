//! ── Timelocked Protocol Treasury Emergency Rescue Handler ─────────────────
//!
//! Purpose:
//! Allow governance to recover mis-sent non-protocol tokens stuck in contract
//! addresses after a mandatory timelock delay.
//!
//! Acceptance Criteria:
//! 1. Require governance proposal to queue token rescue action.
//! 2. Restrict rescue actions: cannot extract primary pool or vault reserve assets.
//! 3. Execute token transfer to treasury address once timelock expires.

use soroban_sdk::{contracttype, symbol_short, Address, Env, Map, Symbol};
use crate::{ContractData, ContractError, DATA_KEY};

/// Mandatory delay between queueing a token rescue action and allowing execution (48 hours).
pub const RESCUE_TIMELOCK_DELAY: u64 = 48 * 60 * 60;

/// Storage key prefix for indexed rescue proposals: (RESCUE_PROPOSAL_KEY, proposal_id)
pub(crate) const RESCUE_PROPOSAL_KEY: Symbol = symbol_short!("RSCPROP");

/// Storage key for the proposal counter (monotonically increasing u64).
pub(crate) const RESCUE_COUNTER_KEY: Symbol = symbol_short!("RSCCNT");

/// Storage key for the set of protected primary pool and vault reserve asset addresses.
pub(crate) const PROTECTED_ASSETS_KEY: Symbol = symbol_short!("PRTASST");

/// Event topic constants for emergency token rescue
pub const EV_TOKEN_RESCUE_QUEUED: Symbol = symbol_short!("rsc_queue");
pub const EV_TOKEN_RESCUE_EXECUTED: Symbol = symbol_short!("rsc_exec");
pub const EV_TOKEN_RESCUE_CANCELLED: Symbol = symbol_short!("rsc_canc");
pub const EV_PROTECTED_ASSET_SET: Symbol = symbol_short!("prt_asset");

/// Status lifecycle of a token rescue proposal.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RescueProposalStatus {
    /// Action is queued and waiting for timelock delay to elapse.
    Pending,
    /// Action was executed after timelock delay expired.
    Executed,
    /// Action was cancelled during timelock window.
    Cancelled,
}

/// A queued token rescue proposal.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RescueProposal {
    /// Unique proposal ID.
    pub proposal_id: u64,
    /// Address of the token to be rescued.
    pub token: Address,
    /// Amount of tokens to recover.
    pub amount: i128,
    /// Destination address (protocol treasury).
    pub recipient: Address,
    /// Proposer address (governance / admin).
    pub proposer: Address,
    /// Ledger timestamp when proposal was queued.
    pub staged_at: u64,
    /// Earliest ledger timestamp when proposal can be executed.
    pub execute_at: u64,
    /// Lifecycle status.
    pub status: RescueProposalStatus,
}

/// Register a token as a protected asset (primary pool token or vault reserve asset).
/// Protected assets CANNOT be extracted via emergency rescue.
///
/// Only contract admin can register protected assets.
pub fn register_protected_asset(
    env: &Env,
    caller: Address,
    asset: Address,
) -> Result<(), ContractError> {
    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;

    if data.admin != caller {
        return Err(ContractError::NotAdmin);
    }
    caller.require_auth();

    let mut protected_map: Map<Address, bool> = env
        .storage()
        .instance()
        .get(&PROTECTED_ASSETS_KEY)
        .unwrap_or_else(|| Map::new(env));

    protected_map.set(asset.clone(), true);
    env.storage().instance().set(&PROTECTED_ASSETS_KEY, &protected_map);
    crate::kernel::instance::bump_instance_ttl(env);

    env.events().publish(
        (EV_PROTECTED_ASSET_SET,),
        (caller, asset),
    );

    Ok(())
}

/// Check if a token address is a protected primary pool or vault reserve asset.
pub fn is_protected_asset(env: &Env, asset: &Address) -> bool {
    let protected_map: Option<Map<Address, bool>> = env.storage().instance().get(&PROTECTED_ASSETS_KEY);
    match protected_map {
        Some(map) => map.get(asset.clone()).unwrap_or(false),
        None => false,
    }
}

/// Queue a governance proposal for recovering mis-sent tokens.
///
/// Acceptance Criterion 1 & 2:
/// - Requires governance/admin caller authentication.
/// - Restricts rescue actions: cannot extract primary pool or vault reserve assets.
pub fn queue_token_rescue(
    env: &Env,
    proposer: Address,
    token: Address,
    amount: i128,
    recipient: Address,
) -> Result<u64, ContractError> {
    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;

    if data.admin != proposer {
        return Err(ContractError::NotAdmin);
    }
    proposer.require_auth();

    if amount <= 0 {
        return Err(ContractError::AmountTooLow);
    }

    // Acceptance Criterion 2: Restrict rescue actions: cannot extract primary pool or vault reserve assets.
    if is_protected_asset(env, &token) {
        return Err(ContractError::ProtectedAssetNotRescueable);
    }

    let counter: u64 = env
        .storage()
        .instance()
        .get(&RESCUE_COUNTER_KEY)
        .unwrap_or(0u64);
    let proposal_id = counter.checked_add(1).ok_or(ContractError::Overflow)?;
    env.storage().instance().set(&RESCUE_COUNTER_KEY, &proposal_id);

    let staged_at = env.ledger().timestamp();
    let execute_at = staged_at
        .checked_add(RESCUE_TIMELOCK_DELAY)
        .ok_or(ContractError::Overflow)?;

    let proposal = RescueProposal {
        proposal_id,
        token: token.clone(),
        amount,
        recipient: recipient.clone(),
        proposer: proposer.clone(),
        staged_at,
        execute_at,
        status: RescueProposalStatus::Pending,
    };

    env.storage().instance().set(&(RESCUE_PROPOSAL_KEY, proposal_id), &proposal);
    crate::kernel::instance::bump_instance_ttl(env);

    env.events().publish(
        (EV_TOKEN_RESCUE_QUEUED, proposal_id),
        (proposer, token, amount, recipient, execute_at),
    );

    Ok(proposal_id)
}

/// Execute token transfer to treasury address once timelock expires.
///
/// Acceptance Criterion 3:
/// - Execute token transfer to treasury address once timelock expires.
pub fn execute_token_rescue(
    env: &Env,
    executor: Address,
    proposal_id: u64,
) -> Result<(), ContractError> {
    executor.require_auth();

    let mut proposal: RescueProposal = env
        .storage()
        .instance()
        .get(&(RESCUE_PROPOSAL_KEY, proposal_id))
        .ok_or(ContractError::RescueProposalNotFound)?;

    if proposal.status != RescueProposalStatus::Pending {
        return Err(ContractError::RescueProposalNotPending);
    }

    let now = env.ledger().timestamp();
    if now < proposal.execute_at {
        return Err(ContractError::RescueTimelockNotExpired);
    }

    // Safety re-check: verify token is still not a protected asset
    if is_protected_asset(env, &proposal.token) {
        return Err(ContractError::ProtectedAssetNotRescueable);
    }

    // Execute token transfer to treasury/recipient address using soroban_sdk token Client
    let token_client = soroban_sdk::token::Client::new(env, &proposal.token);
    token_client.transfer(&env.current_contract_address(), &proposal.recipient, &proposal.amount);

    proposal.status = RescueProposalStatus::Executed;
    env.storage().instance().set(&(RESCUE_PROPOSAL_KEY, proposal_id), &proposal);
    crate::kernel::instance::bump_instance_ttl(env);

    env.events().publish(
        (EV_TOKEN_RESCUE_EXECUTED, proposal_id),
        (executor, proposal.token.clone(), proposal.amount, proposal.recipient.clone()),
    );

    Ok(())
}

/// Cancel a pending token rescue proposal during its timelock window.
pub fn cancel_token_rescue(
    env: &Env,
    canceller: Address,
    proposal_id: u64,
) -> Result<(), ContractError> {
    let data: ContractData = env
        .storage()
        .instance()
        .get(&DATA_KEY)
        .ok_or(ContractError::NotInitialized)?;

    if data.admin != canceller {
        return Err(ContractError::NotAdmin);
    }
    canceller.require_auth();

    let mut proposal: RescueProposal = env
        .storage()
        .instance()
        .get(&(RESCUE_PROPOSAL_KEY, proposal_id))
        .ok_or(ContractError::RescueProposalNotFound)?;

    if proposal.status != RescueProposalStatus::Pending {
        return Err(ContractError::RescueProposalNotPending);
    }

    proposal.status = RescueProposalStatus::Cancelled;
    env.storage().instance().set(&(RESCUE_PROPOSAL_KEY, proposal_id), &proposal);
    crate::kernel::instance::bump_instance_ttl(env);

    env.events().publish(
        (EV_TOKEN_RESCUE_CANCELLED, proposal_id),
        (canceller,),
    );

    Ok(())
}

/// Get details of a rescue proposal by ID.
pub fn get_rescue_proposal(env: &Env, proposal_id: u64) -> Option<RescueProposal> {
    env.storage().instance().get(&(RESCUE_PROPOSAL_KEY, proposal_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger, LedgerInfo};
    use soroban_sdk::Env;

    fn setup_env() -> (Env, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);

        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        env.as_contract(&contract_id, || {
            let data = ContractData {
                admin: admin.clone(),
                value: 0,
                max_fee_ceiling: 10_000,
            };
            env.storage().instance().set(&DATA_KEY, &data);
        });

        (env, admin, treasury)
    }

    fn advance_timestamp(env: &Env, delta_seconds: u64) {
        let ts = env.ledger().timestamp();
        env.ledger().set(LedgerInfo {
            timestamp: ts + delta_seconds,
            protocol_version: env.ledger().protocol_version(),
            sequence_number: env.ledger().sequence() + 1,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 0,
            min_persistent_entry_ttl: 0,
            max_entry_ttl: u32::MAX,
        });
    }

    #[test]
    fn test_register_and_check_protected_asset() {
        let (env, admin, _treasury) = setup_env();
        let pool_asset = Address::generate(&env);

        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        env.as_contract(&contract_id, || {
            assert!(!is_protected_asset(&env, &pool_asset));
            assert!(register_protected_asset(&env, admin.clone(), pool_asset.clone()).is_ok());
            assert!(is_protected_asset(&env, &pool_asset));
        });
    }

    #[test]
    fn test_non_admin_cannot_register_protected_asset() {
        let (env, _admin, _treasury) = setup_env();
        let non_admin = Address::generate(&env);
        let pool_asset = Address::generate(&env);

        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        env.as_contract(&contract_id, || {
            let res = register_protected_asset(&env, non_admin, pool_asset);
            assert_eq!(res, Err(ContractError::NotAdmin));
        });
    }

    #[test]
    fn test_cannot_queue_rescue_for_protected_asset() {
        let (env, admin, treasury) = setup_env();
        let pool_asset = Address::generate(&env);

        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        env.as_contract(&contract_id, || {
            register_protected_asset(&env, admin.clone(), pool_asset.clone()).unwrap();

            let res = queue_token_rescue(&env, admin, pool_asset, 1_000, treasury);
            assert_eq!(res, Err(ContractError::ProtectedAssetNotRescueable));
        });
    }

    #[test]
    fn test_queue_and_get_rescue_proposal() {
        let (env, admin, treasury) = setup_env();
        let mis_sent_token = Address::generate(&env);

        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        env.as_contract(&contract_id, || {
            let pid = queue_token_rescue(&env, admin.clone(), mis_sent_token.clone(), 5_000, treasury.clone()).unwrap();
            assert_eq!(pid, 1);

            let proposal = get_rescue_proposal(&env, pid).unwrap();
            assert_eq!(proposal.proposal_id, 1);
            assert_eq!(proposal.token, mis_sent_token);
            assert_eq!(proposal.amount, 5_000);
            assert_eq!(proposal.recipient, treasury);
            assert_eq!(proposal.status, RescueProposalStatus::Pending);
            assert_eq!(proposal.execute_at, proposal.staged_at + RESCUE_TIMELOCK_DELAY);
        });
    }

    #[test]
    fn test_cannot_execute_before_timelock_expires() {
        let (env, admin, treasury) = setup_env();
        let mis_sent_token = Address::generate(&env);

        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        env.as_contract(&contract_id, || {
            let pid = queue_token_rescue(&env, admin.clone(), mis_sent_token, 5_000, treasury).unwrap();

            advance_timestamp(&env, RESCUE_TIMELOCK_DELAY - 10);
            let res = execute_token_rescue(&env, admin, pid);
            assert_eq!(res, Err(ContractError::RescueTimelockNotExpired));
        });
    }

    #[test]
    fn test_cancel_rescue_proposal() {
        let (env, admin, treasury) = setup_env();
        let mis_sent_token = Address::generate(&env);

        let contract_id = env.register_contract(None, crate::TimeLockedUpgradeContract);
        env.as_contract(&contract_id, || {
            let pid = queue_token_rescue(&env, admin.clone(), mis_sent_token, 5_000, treasury).unwrap();

            assert!(cancel_token_rescue(&env, admin.clone(), pid).is_ok());

            let proposal = get_rescue_proposal(&env, pid).unwrap();
            assert_eq!(proposal.status, RescueProposalStatus::Cancelled);

            advance_timestamp(&env, RESCUE_TIMELOCK_DELAY + 10);
            let res = execute_token_rescue(&env, admin, pid);
            assert_eq!(res, Err(ContractError::RescueProposalNotPending));
        });
    }
}
