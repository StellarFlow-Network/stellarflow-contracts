#![no_std]
//! # Administrative Key Recovery Delay and Dispute Module (Issue #962)
//!
//! Safely handle administrative account key loss through delayed recovery
//! requests.
//!
//! ## Problem
//!
//! When an admin key is lost (or compromised and unrecoverable by its owner),
//! the key itself cannot authorize its own replacement — that is precisely
//! what "lost" means. The replacement must therefore be authorized *without*
//! the old key, which is indistinguishable on-chain from an attacker trying
//! to seize control. Instant replacement would hand the protocol to whoever
//! speaks first.
//!
//! ## Design: optimistic recovery with guardian veto
//!
//! ```text
//! request_recovery ──► pending (7-day dispute window) ──┬─► finalize_recovery (admin replaced)
//!                                                       └──► cancel_recovery (guardian veto)
//! ```
//!
//! * **Initiation is permissionless.** Anyone may file a recovery proposal
//!   naming the replacement admin key. This is necessary, not lax: the
//!   legitimate owner of a lost key has no privileged credential left to
//!   present, so gating initiation on on-chain authority would lock them out
//!   alongside the attacker. Spam is contained because only one proposal may
//!   be active at a time and a proposal grants nothing until finalised.
//! * **The 7-day dispute window is mandatory.**
//!   [`KeyRecovery::finalize_recovery`] refuses to run before
//!   `requested_at + RECOVERY_DELAY_SECONDS` (`RecoveryError::DelayNotElapsed`),
//!   giving the guardian set and off-chain monitors a full week to inspect
//!   the proposal.
//! * **Any active guardian can veto.** During the window, any member of the
//!   current guardian set may call [`KeyRecovery::cancel_recovery`] and kill
//!   the proposal outright. Veto power is intentionally per-guardian rather
//!   than threshold-based: a single guardian sounding the alarm must be
//!   enough to stop a takeover, and a cancelled proposal can simply be
//!   re-filed if it was legitimate.
//! * **Finalisation is permissionless but strict.** Once mature, anyone may
//!   finalise; the call succeeds only if the proposal survived the window
//!   untouched, and it atomically swaps the admin key and clears the proposal.
//!
//! Guardian membership itself stays under admin control (`add_guardian` /
//! `remove_guardian`): a guardian removed before cancelling loses veto power,
//! which is what "active multi-sig guardians" means here.

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, Address, Env, Symbol, Vec,
};

/// Mandatory waiting period between a recovery request and finalisation:
/// 7 days, expressed in seconds.
pub const RECOVERY_DELAY_SECONDS: u64 = 7 * 24 * 60 * 60;

/// Topic emitted when a recovery proposal is filed.
pub const RECOVERY_REQUESTED: &str = "RecoveryRequested";
/// Topic emitted when a guardian vetoes a recovery proposal.
pub const RECOVERY_CANCELLED: &str = "RecoveryCancelled";
/// Topic emitted when a recovery proposal is finalised and the key replaced.
pub const RECOVERY_FINALIZED: &str = "RecoveryFinalized";

/// Errors returned by the recovery module.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum RecoveryError {
    /// `initialize` has already been called.
    AlreadyInitialized = 1,
    /// The contract has not been initialised yet.
    NotInitialized = 2,
    /// The caller is not the registered admin.
    NotAdmin = 3,
    /// The caller is not an active guardian.
    NotGuardian = 4,
    /// A recovery proposal is already active.
    RecoveryAlreadyPending = 5,
    /// No recovery proposal is currently active.
    NoActiveProposal = 6,
    /// The 7-day dispute window has not elapsed yet.
    DelayNotElapsed = 7,
    /// The guardian set would be left empty.
    EmptyGuardianSet = 8,
    /// The address is already a guardian.
    AlreadyGuardian = 9,
    /// The address is not a guardian.
    UnknownGuardian = 10,
    /// The proposed replacement equals the current admin key.
    UnchangedKey = 11,
}

/// A pending administrative key recovery proposal.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryProposal {
    /// Current admin key at the time of the request.
    pub current_admin: Address,
    /// Replacement admin key the proposal will install if unopposed.
    pub proposed_admin: Address,
    /// Account that filed the proposal.
    pub requested_by: Address,
    /// Ledger timestamp at which the proposal was filed.
    pub requested_at: u64,
    /// Earliest ledger timestamp at which finalisation is allowed.
    pub executable_at: u64,
}

/// Storage keys.
#[contracttype]
pub enum DataKey {
    /// Address whose key is recoverable through this module.
    Admin,
    /// Active multi-sig guardian set; any member may veto a proposal.
    Guardians,
    /// The currently pending recovery proposal, if any.
    Proposal,
}

#[contract]
pub struct KeyRecovery;

fn load_admin(env: &Env) -> Result<Address, RecoveryError> {
    env.storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(RecoveryError::NotInitialized)
}

fn require_admin(env: &Env, caller: &Address) -> Result<(), RecoveryError> {
    let admin = load_admin(&env)?;
    if caller != &admin {
        return Err(RecoveryError::NotAdmin);
    }
    caller.require_auth();
    Ok(())
}

fn load_guardians(env: &Env) -> Vec<Address> {
    env.storage()
        .instance()
        .get(&DataKey::Guardians)
        .unwrap_or_else(|| Vec::new(env))
}

fn is_guardian(env: &Env, who: &Address) -> bool {
    load_guardians(env).iter().any(|g| g == *who)
}

#[contractimpl]
impl KeyRecovery {
    /// Deploy-time setup. Installs the recoverable admin key and the initial
    /// guardian set, which must be non-empty — a recovery module with nobody
    /// to dispute is just a delayed takeover machine.
    pub fn initialize(
        env: Env,
        admin: Address,
        guardians: Vec<Address>,
    ) -> Result<(), RecoveryError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(RecoveryError::AlreadyInitialized);
        }
        if guardians.is_empty() {
            return Err(RecoveryError::EmptyGuardianSet);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Guardians, &guardians);
        Ok(())
    }

    /// File a recovery proposal naming the replacement admin key.
    ///
    /// Permissionless by design (see module docs): the legitimate owner of a
    /// lost key has no credential left to present. Only one proposal may be
    /// active at a time. Returns the earliest timestamp at which the proposal
    /// may be finalised (`requested_at + 7 days`).
    pub fn request_recovery(
        env: Env,
        requester: Address,
        proposed_admin: Address,
    ) -> Result<u64, RecoveryError> {
        let admin = load_admin(&env)?;
        requester.require_auth();
        if env.storage().instance().has(&DataKey::Proposal) {
            return Err(RecoveryError::RecoveryAlreadyPending);
        }
        if proposed_admin == admin {
            return Err(RecoveryError::UnchangedKey);
        }

        let now = env.ledger().timestamp();
        let executable_at = now
            .checked_add(RECOVERY_DELAY_SECONDS)
            .ok_or(RecoveryError::DelayNotElapsed)?;

        let proposal = RecoveryProposal {
            current_admin: admin,
            proposed_admin: proposed_admin.clone(),
            requested_by: requester.clone(),
            requested_at: now,
            executable_at,
        };
        env.storage().instance().set(&DataKey::Proposal, &proposal);

        env.events().publish(
            (Symbol::new(&env, RECOVERY_REQUESTED),),
            (proposed_admin, requester, executable_at),
        );

        Ok(executable_at)
    }

    /// Veto a pending recovery proposal during the dispute window.
    ///
    /// Any *active* guardian may cancel: the caller must both authenticate
    /// and belong to the current guardian set. A single veto kills the
    /// proposal — there is nothing left to finalise afterwards.
    pub fn cancel_recovery(env: Env, guardian: Address) -> Result<(), RecoveryError> {
        load_admin(&env)?;
        if !is_guardian(&env, &guardian) {
            return Err(RecoveryError::NotGuardian);
        }
        guardian.require_auth();
        let proposal: RecoveryProposal = env
            .storage()
            .instance()
            .get(&DataKey::Proposal)
            .ok_or(RecoveryError::NoActiveProposal)?;
        env.storage().instance().remove(&DataKey::Proposal);

        env.events().publish(
            (Symbol::new(&env, RECOVERY_CANCELLED),),
            (proposal.proposed_admin, guardian),
        );

        Ok(())
    }

    /// Finalise a mature recovery proposal and replace the admin key.
    ///
    /// Succeeds only once the 7-day dispute window has fully elapsed *and*
    /// the proposal was never vetoed. Permissionless: anyone may execute the
    /// swap on behalf of a mature proposal.
    pub fn finalize_recovery(env: Env) -> Result<Address, RecoveryError> {
        let proposal: RecoveryProposal = env
            .storage()
            .instance()
            .get(&DataKey::Proposal)
            .ok_or(RecoveryError::NoActiveProposal)?;
        if env.ledger().timestamp() < proposal.executable_at {
            return Err(RecoveryError::DelayNotElapsed);
        }

        env.storage().instance().set(&DataKey::Admin, &proposal.proposed_admin);
        env.storage().instance().remove(&DataKey::Proposal);

        env.events().publish(
            (Symbol::new(&env, RECOVERY_FINALIZED),),
            (proposal.current_admin.clone(), proposal.proposed_admin.clone()),
        );

        Ok(proposal.proposed_admin)
    }

    /// Add a guardian. Admin-only.
    pub fn add_guardian(env: Env, caller: Address, guardian: Address) -> Result<(), RecoveryError> {
        require_admin(&env, &caller)?;
        let mut guardians = load_guardians(&env);
        if guardians.iter().any(|g| g == guardian) {
            return Err(RecoveryError::AlreadyGuardian);
        }
        guardians.push_back(guardian);
        env.storage().instance().set(&DataKey::Guardians, &guardians);
        Ok(())
    }

    /// Remove a guardian. Admin-only. The last guardian cannot be removed.
    pub fn remove_guardian(
        env: Env,
        caller: Address,
        guardian: Address,
    ) -> Result<(), RecoveryError> {
        require_admin(&env, &caller)?;
        let guardians = load_guardians(&env);
        if !guardians.iter().any(|g| g == guardian) {
            return Err(RecoveryError::UnknownGuardian);
        }
        if guardians.len() == 1 {
            return Err(RecoveryError::EmptyGuardianSet);
        }
        let mut remaining = Vec::new(&env);
        for g in guardians.iter() {
            if g != guardian {
                remaining.push_back(g);
            }
        }
        env.storage().instance().set(&DataKey::Guardians, &remaining);
        Ok(())
    }

    /// The currently registered admin key.
    pub fn get_admin(env: Env) -> Result<Address, RecoveryError> {
        load_admin(&env)
    }

    /// The active guardian set; every member may veto a pending proposal.
    pub fn get_guardians(env: Env) -> Vec<Address> {
        load_guardians(&env)
    }

    /// The currently pending recovery proposal, if any.
    pub fn get_proposal(env: Env) -> Option<RecoveryProposal> {
        env.storage().instance().get(&DataKey::Proposal)
    }

    /// Seconds remaining before a pending proposal may be finalised, or `0`
    /// when no proposal is pending or the window has elapsed.
    pub fn dispute_remaining(env: Env) -> u64 {
        match env.storage().instance().get(&DataKey::Proposal) {
            Some(proposal) => {
                let proposal: RecoveryProposal = proposal;
                RECOVERY_DELAY_SECONDS.saturating_sub(
                    env.ledger().timestamp().saturating_sub(proposal.requested_at),
                )
            }
            None => 0,
        }
    }
}

#[cfg(test)]
mod test {
    use super::{KeyRecovery, KeyRecoveryClient, RecoveryError, RECOVERY_DELAY_SECONDS};
    use soroban_sdk::{
        testutils::{Address as _, Ledger as _},
        vec, Address, Env, Symbol, TryFromVal, Vec,
    };

    const T0: u64 = 1_700_000_000;

    fn setup() -> (Env, KeyRecoveryClient<'static>, Address, Vec<Address>) {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|li| li.timestamp = T0);
        let id = env.register_contract(None, KeyRecovery);
        let client = KeyRecoveryClient::new(&env, &id);
        let admin = Address::generate(&env);
        let guardians = vec![
            &env,
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
        ];
        client.initialize(&admin, &guardians);
        (env, client, admin, guardians)
    }

    fn advance(env: &Env, seconds: u64) {
        env.ledger().with_mut(|li| li.timestamp += seconds);
    }

    #[test]
    fn initialize_installs_admin_and_guardians() {
        let (env, client, admin, guardians) = setup();
        assert_eq!(client.get_admin(), admin);
        assert_eq!(client.get_guardians(), guardians);
        assert!(client.get_proposal().is_none());
        assert_eq!(client.dispute_remaining(), 0);
        let _ = env;
    }

    #[test]
    fn initialize_rejects_empty_guardian_set() {
        let env = Env::default();
        env.mock_all_auths();
        let id = env.register_contract(None, KeyRecovery);
        let client = KeyRecoveryClient::new(&env, &id);
        let res = client.try_initialize(&Address::generate(&env), &Vec::new(&env));
        assert_eq!(res, Err(Ok(RecoveryError::EmptyGuardianSet)));
    }

    #[test]
    fn initialize_cannot_run_twice() {
        let (_, client, admin, guardians) = setup();
        let res = client.try_initialize(&admin, &guardians);
        assert_eq!(res, Err(Ok(RecoveryError::AlreadyInitialized)));
    }

    #[test]
    fn request_opens_a_seven_day_dispute_window() {
        let (env, client, _, _) = setup();
        let requester = Address::generate(&env);
        let proposed = Address::generate(&env);
        let executable_at = client.request_recovery(&requester, &proposed);
        assert_eq!(executable_at, T0 + RECOVERY_DELAY_SECONDS);

        let proposal = client.get_proposal().unwrap();
        assert_eq!(proposal.proposed_admin, proposed);
        assert_eq!(proposal.requested_by, requester);
        assert_eq!(proposal.requested_at, T0);
        assert_eq!(proposal.executable_at, T0 + RECOVERY_DELAY_SECONDS);
        assert_eq!(client.dispute_remaining(), RECOVERY_DELAY_SECONDS);
    }

    #[test]
    fn request_rejects_a_second_proposal_while_one_is_active() {
        let (env, client, _, _) = setup();
        client.request_recovery(&Address::generate(&env), &Address::generate(&env));
        let res = client.try_request_recovery(&Address::generate(&env), &Address::generate(&env));
        assert_eq!(res, Err(Ok(RecoveryError::RecoveryAlreadyPending)));
    }

    #[test]
    fn request_rejects_replacing_the_key_with_itself() {
        let (env, client, admin, _) = setup();
        let res = client.try_request_recovery(&Address::generate(&env), &admin);
        assert_eq!(res, Err(Ok(RecoveryError::UnchangedKey)));
        let _ = env;
    }

    #[test]
    fn finalize_before_expiry_is_rejected() {
        let (env, client, _, _) = setup();
        client.request_recovery(&Address::generate(&env), &Address::generate(&env));
        advance(&env, RECOVERY_DELAY_SECONDS - 1);
        let res = client.try_finalize_recovery();
        assert_eq!(res, Err(Ok(RecoveryError::DelayNotElapsed)));
        assert_eq!(client.dispute_remaining(), 1);
    }

    #[test]
    fn finalize_after_expiry_replaces_the_key() {
        let (env, client, admin, _) = setup();
        let proposed = Address::generate(&env);
        client.request_recovery(&Address::generate(&env), &proposed);

        advance(&env, RECOVERY_DELAY_SECONDS);
        let new_admin = client.finalize_recovery();
        assert_eq!(new_admin, proposed);
        assert_eq!(client.get_admin(), proposed);
        assert!(client.get_proposal().is_none());

        // The old key no longer administers the module.
        let res = client.try_add_guardian(&admin, &Address::generate(&env));
        assert_eq!(res, Err(Ok(RecoveryError::NotAdmin)));
        let _ = env;
    }

    #[test]
    fn any_active_guardian_can_veto_during_the_window() {
        let (env, client, admin, guardians) = setup();
        client.request_recovery(&Address::generate(&env), &Address::generate(&env));

        // Each guardian holds an independent veto; the last one listed
        // exercises it here.
        client.cancel_recovery(&guardians.get(2).unwrap());
        assert!(client.get_proposal().is_none());
        assert_eq!(client.dispute_remaining(), 0);

        // Nothing left to finalise, and the admin key is untouched.
        assert_eq!(
            client.try_finalize_recovery(),
            Err(Ok(RecoveryError::NoActiveProposal))
        );
        assert_eq!(client.get_admin(), admin);
        let _ = env;
    }

    #[test]
    fn veto_after_expiry_still_blocks_finalization() {
        // A proposal that nobody finalised on time is still vetoable: the
        // window gates *finalisation*, while cancellation only needs an
        // active proposal and an active guardian.
        let (env, client, _, guardians) = setup();
        client.request_recovery(&Address::generate(&env), &Address::generate(&env));
        advance(&env, RECOVERY_DELAY_SECONDS + 1_000);
        client.cancel_recovery(&guardians.get(0).unwrap());
        assert_eq!(
            client.try_finalize_recovery(),
            Err(Ok(RecoveryError::NoActiveProposal))
        );
    }

    #[test]
    fn non_guardians_cannot_veto() {
        let (env, client, _, _) = setup();
        client.request_recovery(&Address::generate(&env), &Address::generate(&env));
        let outsider = Address::generate(&env);
        assert_eq!(
            client.try_cancel_recovery(&outsider),
            Err(Ok(RecoveryError::NotGuardian))
        );
        // The proposal survives the attempt.
        assert!(client.get_proposal().is_some());
        let _ = env;
    }

    #[test]
    fn removed_guardians_lose_veto_power() {
        let (env, client, admin, guardians) = setup();
        let removed = guardians.get(0).unwrap();
        client.remove_guardian(&admin, &removed.clone());
        client.request_recovery(&Address::generate(&env), &Address::generate(&env));
        assert_eq!(
            client.try_cancel_recovery(&removed),
            Err(Ok(RecoveryError::NotGuardian))
        );
        // The remaining guardians can still veto.
        client.cancel_recovery(&guardians.get(1).unwrap());
        assert!(client.get_proposal().is_none());
        let _ = env;
    }

    #[test]
    fn guardian_set_cannot_be_emptied() {
        let (_, client, admin, guardians) = setup();
        client.remove_guardian(&admin, &guardians.get(0).unwrap());
        client.remove_guardian(&admin, &guardians.get(1).unwrap());
        let res = client.try_remove_guardian(&admin, &guardians.get(2).unwrap());
        assert_eq!(res, Err(Ok(RecoveryError::EmptyGuardianSet)));
    }

    #[test]
    fn guardian_management_is_admin_only() {
        let (env, client, _, _) = setup();
        let intruder = Address::generate(&env);
        assert_eq!(
            client.try_add_guardian(&intruder, &Address::generate(&env)),
            Err(Ok(RecoveryError::NotAdmin))
        );
        // Removal by a non-admin fails the admin check first, before the
        // guardian membership check is even reached.
        assert_eq!(
            client.try_remove_guardian(&intruder, &Address::generate(&env)),
            Err(Ok(RecoveryError::NotAdmin))
        );
    }

    #[test]
    fn refile_after_veto_starts_a_fresh_window() {
        let (env, client, _, guardians) = setup();
        client.request_recovery(&Address::generate(&env), &Address::generate(&env));
        client.cancel_recovery(&guardians.get(0).unwrap());

        advance(&env, 100);
        let proposed = Address::generate(&env);
        let executable_at = client.request_recovery(&Address::generate(&env), &proposed);
        assert_eq!(executable_at, T0 + 100 + RECOVERY_DELAY_SECONDS);

        advance(&env, RECOVERY_DELAY_SECONDS);
        assert_eq!(client.finalize_recovery(), proposed);
    }

    #[test]
    fn request_event_carries_proposal_details() {
        use soroban_sdk::testutils::Events as _;

        let (env, client, _, _) = setup();
        let requester = Address::generate(&env);
        let proposed = Address::generate(&env);
        client.request_recovery(&requester, &proposed);

        let events = env.events().all();
        let (_, topics, data) = events.get(events.len() - 1).unwrap();
        let topic: Symbol = Symbol::try_from_val(&env, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic, Symbol::new(&env, "RecoveryRequested"));
        let (p, r, x): (Address, Address, u64) =
            TryFromVal::try_from_val(&env, &data).unwrap();
        assert_eq!((p, r, x), (proposed, requester, T0 + RECOVERY_DELAY_SECONDS));
    }
}
