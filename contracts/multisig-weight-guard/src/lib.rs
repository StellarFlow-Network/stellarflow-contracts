#![no_std]
//! # Multi-Sig Key Weight Rotation Guard (Issue #977)
//!
//! A hardened guard around multi-signature **signer-weight rotation**. In a
//! weighted multi-sig, every signer public key maps to an integer weight and a
//! quorum is reached when the sum of the weights of the approving keys meets a
//! threshold. Silently rewriting those weights is one of the highest-risk
//! operations a contract can perform: a single compromised admin key can use an
//! instant rotation to hand control to keys the rest of the signer set never
//! agreed to.
//!
//! This contract makes weight rotation *slow and observable*:
//!
//! 1. **Mandatory 72-hour cooldown.** Two consecutive weight modifications must
//!    be at least [`WEIGHT_ROTATION_COOLDOWN_SECONDS`] (72h) apart. A rotation
//!    requested too early is rejected with [`ContractError::CooldownNotElapsed`].
//! 2. **The old weights stay valid until the new configuration goes live.**
//!    A rotation is *staged*, not applied immediately. It carries an
//!    `effective_at` timestamp (request time + 72h). Until that timestamp is
//!    reached the *previous* weights and threshold remain the ones enforced for
//!    quorum, so a rotation cannot be used to bypass an in-flight approval.
//! 3. **Full breakdown is emitted.** Every accepted rotation emits a
//!    `SignerWeightsUpdated` event carrying the complete, canonical
//!    `public_key -> weight` mapping, the new threshold and the activation
//!    timestamp, so off-chain monitors can diff key sets without trusting
//!    storage reads.
//!
//! ## Lifecycle
//!
//! ```text
//! initialize ──► active config ──────────────────────────────────────┐
//!                     ▲                                              │
//!                     │                          propose (t0)        │
//!         apply at effective_at ──► pending config (t0 + 72h) ◄──────┘
//!                     │
//!                     └──► active config  (cooldown restarts)
//! ```
//!
//! Between `t0` and `t0 + 72h` the pending configuration exists but is **not
//! yet in force**: [`MultisigWeightGuard::effective_configuration`] keeps
//! returning the old weights. [`MultisigWeightGuard::apply_pending_weights`]
//! finalises the switch once the activation timestamp is reached.

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, Address, BytesN, Env, Map, Symbol, Vec,
};

/// Mandatory cooldown between two consecutive signer-weight modifications: 72
/// hours, expressed in seconds.
pub const WEIGHT_ROTATION_COOLDOWN_SECONDS: u64 = 72 * 60 * 60;

/// Topic under which a weight rotation is announced.
///
/// The payload is a serialised [`SignerWeightsUpdatedEvent`].
pub const SIGNER_WEIGHTS_UPDATED: &str = "SignerWeightsUpdated";

/// Errors returned by the guard.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ContractError {
    /// `initialize` has already been called.
    /// Recovery steps: Inspect the state for AlreadyInitialized and retry with valid inputs or proper conditions.
    AlreadyInitialized = 1,
    /// The contract has not been initialised yet.
    /// Recovery steps: Inspect the state for NotInitialized and retry with valid inputs or proper conditions.
    NotInitialized = 2,
    /// The caller is not the registered admin.
    /// Recovery steps: Inspect the state for NotAdmin and retry with valid inputs or proper conditions.
    NotAdmin = 3,
    /// A modification was requested before the 72h cooldown elapsed.
    /// Recovery steps: Inspect the state for CooldownNotElapsed and retry with valid inputs or proper conditions.
    CooldownNotElapsed = 4,
    /// A rotation is already staged and must be applied first.
    /// Recovery steps: Inspect the state for RotationAlreadyPending and retry with valid inputs or proper conditions.
    RotationAlreadyPending = 5,
    /// No rotation is currently staged.
    /// Recovery steps: Inspect the state for NoPendingConfiguration and retry with valid inputs or proper conditions.
    NoPendingConfiguration = 6,
    /// `apply_pending_weights` was called before `effective_at`.
    /// Recovery steps: Inspect the state for ActivationNotReached and retry with valid inputs or proper conditions.
    ActivationNotReached = 7,
    /// The submitted configuration contains no signers.
    /// Recovery steps: Inspect the state for EmptyWeightSet and retry with valid inputs or proper conditions.
    EmptyWeightSet = 8,
    /// A signer was given a zero weight.
    /// Recovery steps: Inspect the state for ZeroWeight and retry with valid inputs or proper conditions.
    ZeroWeight = 9,
    /// The same public key appears twice in the submitted configuration.
    /// Recovery steps: Inspect the state for DuplicateSigner and retry with valid inputs or proper conditions.
    DuplicateSigner = 10,
    /// `threshold` is zero or exceeds the total configured weight.
    /// Recovery steps: Inspect the state for InvalidThreshold and retry with valid inputs or proper conditions.
    InvalidThreshold = 11,
    /// The approvals did not reach the configured threshold.
    /// Recovery steps: Inspect the state for ThresholdNotMet and retry with valid inputs or proper conditions.
    ThresholdNotMet = 12,
    /// Summing the configured weights overflowed `u32`.
    /// Recovery steps: Inspect the state for Overflow and retry with valid inputs or proper conditions.
    Overflow = 13,
}

/// A single `public_key -> weight` binding.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignerWeight {
    /// Ed25519 public key of the signer.
    pub public_key: BytesN<32>,
    /// Voting weight granted to this key.
    pub weight: u32,
}

/// A complete, self-consistent signer-weight configuration.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WeightConfiguration {
    /// Canonical mapping of signer public key to weight.
    pub weights: Map<BytesN<32>, u32>,
    /// Weight that must be collected to reach quorum.
    pub threshold: u32,
    /// Ledger timestamp at which this configuration becomes (or became) the
    /// one enforced for quorum.
    pub effective_at: u64,
}

/// Emitted whenever a weight rotation is *staged or applied*.
///
/// The `signers` vector is the full public-key breakdown required by issue
/// #977, so consumers never have to reconstruct it from diffs.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignerWeightsUpdatedEvent {
    /// Complete new `public_key -> weight` mapping.
    pub signers: Vec<SignerWeight>,
    /// New weight threshold.
    pub threshold: u32,
    /// `effective_at` of the configuration that was in force when this event
    /// was emitted. Until `effective_at` in this payload is reached, this is
    /// still the configuration used for quorum.
    pub previous_effective_at: u64,
    /// Timestamp at which the new configuration takes over.
    pub effective_at: u64,
    /// Account that authorised (or finalised) the change.
    pub updated_by: Address,
}

/// Storage keys.
#[contracttype]
pub enum DataKey {
    /// Address allowed to stage rotations.
    Admin,
    /// The configuration currently finalised on-chain.
    Active,
    /// A rotation staged but not yet in force.
    Pending,
    /// Timestamp at which the last rotation took effect (0 = never rotated).
    LastRotation,
}

#[contract]
pub struct MultisigWeightGuard;

fn load_admin(env: &Env) -> Result<Address, ContractError> {
    env.storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(ContractError::NotInitialized)
}

fn load_active(env: &Env) -> Result<WeightConfiguration, ContractError> {
    env.storage()
        .instance()
        .get(&DataKey::Active)
        .ok_or(ContractError::NotInitialized)
}

fn load_pending(env: &Env) -> Option<WeightConfiguration> {
    env.storage().instance().get(&DataKey::Pending)
}

fn load_last_rotation(env: &Env) -> u64 {
    env.storage()
        .instance()
        .get(&DataKey::LastRotation)
        .unwrap_or(0u64)
}

/// Validate a caller-supplied configuration and turn it into a canonical
/// `public_key -> weight` map.
///
/// Rules enforced:
/// * at least one signer,
/// * every weight strictly positive,
/// * no duplicate public keys,
/// * `0 < threshold <= total_weight`,
/// * total weight does not overflow `u32`.
fn build_configuration(
    env: &Env,
    signers: &Vec<SignerWeight>,
    threshold: u32,
) -> Result<Map<BytesN<32>, u32>, ContractError> {
    if signers.is_empty() {
        return Err(ContractError::EmptyWeightSet);
    }

    let mut weights: Map<BytesN<32>, u32> = Map::new(env);
    let mut total: u32 = 0;
    for signer in signers.iter() {
        if signer.weight == 0 {
            return Err(ContractError::ZeroWeight);
        }
        if weights.contains_key(signer.public_key.clone()) {
            return Err(ContractError::DuplicateSigner);
        }
        total = total.checked_add(signer.weight).ok_or(ContractError::Overflow)?;
        weights.set(signer.public_key.clone(), signer.weight);
    }

    if threshold == 0 || threshold > total {
        return Err(ContractError::InvalidThreshold);
    }

    Ok(weights)
}

#[contractimpl]
impl MultisigWeightGuard {
    /// Deploy-time setup. Installs the first signer-weight configuration and
    /// makes it immediately effective.
    ///
    /// The initial configuration does **not** consume the 72h cooldown — the
    /// cooldown only governs *consecutive* rotations, and there is no previous
    /// rotation to space away from.
    pub fn initialize(
        env: Env,
        admin: Address,
        signers: Vec<SignerWeight>,
        threshold: u32,
    ) -> Result<(), ContractError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(ContractError::AlreadyInitialized);
        }
        admin.require_auth();

        let weights = build_configuration(&env, &signers, threshold)?;
        let now = env.ledger().timestamp();

        let active = WeightConfiguration {
            weights,
            threshold,
            effective_at: now,
        };

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Active, &active);
        // 0 means "never rotated" so the first rotation is not blocked by a
        // cooldown that refers to no prior modification.
        env.storage().instance().set(&DataKey::LastRotation, &0u64);

        Self::emit_weights_updated(
            &env,
            &signers,
            threshold,
            now,
            now,
            admin,
        );

        Ok(())
    }

    /// Stage a new signer-weight configuration.
    ///
    /// The change is **not** applied immediately. It is stored as the pending
    /// configuration with `effective_at = now + 72h`. Until that timestamp the
    /// previous weights/threshold remain the ones enforced for quorum, which is
    /// the guarantee requested by issue #977.
    ///
    /// Returns the timestamp at which the new configuration becomes effective.
    ///
    /// Fails with [`ContractError::RotationAlreadyPending`] when an unapplied
    /// rotation is still waiting to be finalised (apply it first with
    /// [`Self::apply_pending_weights`]) and with
    /// [`ContractError::CooldownNotElapsed`] when the previous rotation only took
    /// effect less than 72h ago. Because the cooldown is anchored to the moment
    /// a rotation *becomes effective*, the two rules are independent: one
    /// stops overlapping rotations, the other stops rapid-fire ones.
    pub fn propose_weight_rotation(
        env: Env,
        caller: Address,
        signers: Vec<SignerWeight>,
        threshold: u32,
    ) -> Result<u64, ContractError> {
        let admin = load_admin(&env)?;
        if caller != admin {
            return Err(ContractError::NotAdmin);
        }
        caller.require_auth();

        if load_pending(&env).is_some() {
            return Err(ContractError::RotationAlreadyPending);
        }

        let now = env.ledger().timestamp();
        let last = load_last_rotation(&env);
        if last != 0 && now.saturating_sub(last) < WEIGHT_ROTATION_COOLDOWN_SECONDS {
            return Err(ContractError::CooldownNotElapsed);
        }

        let weights = build_configuration(&env, &signers, threshold)?;
        let active = load_active(&env)?;
        let effective_at = now
            .checked_add(WEIGHT_ROTATION_COOLDOWN_SECONDS)
            .ok_or(ContractError::Overflow)?;

        let pending = WeightConfiguration {
            weights,
            threshold,
            effective_at,
        };

        env.storage().instance().set(&DataKey::Pending, &pending);

        Self::emit_weights_updated(
            &env,
            &signers,
            threshold,
            active.effective_at,
            effective_at,
            caller,
        );

        Ok(effective_at)
    }

    /// Finalise a staged rotation once its `effective_at` timestamp is reached.
    ///
    /// This is intentionally permissionless: it only materialises a decision
    /// the admin already authorised, and refuses to run early with
    /// [`ContractError::ActivationNotReached`]. Returns the newly activated
    /// configuration.
    pub fn apply_pending_weights(env: Env) -> Result<WeightConfiguration, ContractError> {
        let pending = load_pending(&env).ok_or(ContractError::NoPendingConfiguration)?;
        let now = env.ledger().timestamp();
        if now < pending.effective_at {
            return Err(ContractError::ActivationNotReached);
        }

        env.storage().instance().set(&DataKey::Active, &pending);
        env.storage().instance().remove(&DataKey::Pending);
        // Anchor the 72h cooldown to the moment the rotation took effect.
        env.storage().instance().set(&DataKey::LastRotation, &now);

        env.events().publish(
            (Symbol::new(&env, "WeightsApplied"),),
            (pending.effective_at, pending.threshold),
        );

        Ok(pending)
    }

    /// The configuration currently **in force** for quorum.
    ///
    /// If a pending rotation has reached its `effective_at` it is returned;
    /// otherwise the last applied configuration is returned, which is exactly
    /// the "old weights remain valid until the configuration timestamp"
    /// guarantee.
    pub fn effective_configuration(env: Env) -> Result<WeightConfiguration, ContractError> {
        let active = load_active(&env)?;
        match load_pending(&env) {
            Some(pending) if env.ledger().timestamp() >= pending.effective_at => Ok(pending),
            _ => Ok(active),
        }
    }

    /// The configuration staged but not yet effective, if any.
    pub fn pending_configuration(env: Env) -> Option<WeightConfiguration> {
        load_pending(&env)
    }

    /// The last finalised configuration (ignoring any pending rotation).
    pub fn active_configuration(env: Env) -> Result<WeightConfiguration, ContractError> {
        load_active(&env)
    }

    /// Seconds still to wait before another rotation may be staged.
    ///
    /// The clock starts when a rotation *takes effect* (see
    /// [`Self::apply_pending_weights`]). Returns `0` once the cooldown has
    /// elapsed, or when no rotation has ever been applied.
    pub fn rotation_cooldown_remaining(env: Env) -> u64 {
        let last = load_last_rotation(&env);
        if last == 0 {
            return 0;
        }
        WEIGHT_ROTATION_COOLDOWN_SECONDS.saturating_sub(env.ledger().timestamp().saturating_sub(last))
    }

    /// Weight granted to `public_key` by the configuration in force, or `0` for
    /// unknown keys.
    pub fn effective_signer_weight(env: Env, public_key: BytesN<32>) -> Result<u32, ContractError> {
        let config = Self::effective_configuration(env)?;
        Ok(config.weights.get(public_key).unwrap_or(0))
    }

    /// Weight threshold enforced by the configuration in force.
    pub fn effective_threshold(env: Env) -> Result<u32, ContractError> {
        Ok(Self::effective_configuration(env)?.threshold)
    }

    /// Sum the weights of the (deduplicated) approving public keys under the
    /// configuration currently in force and check it against the threshold.
    ///
    /// Unknown keys are ignored rather than rejected so that a key removed by a
    /// rotation cannot keep a stale approval alive, and duplicate approvals are
    /// only counted once. Returns the collected weight.
    pub fn verify_quorum(env: Env, approvals: Vec<BytesN<32>>) -> Result<u32, ContractError> {
        let config = Self::effective_configuration(env.clone())?;

        let mut seen: Map<BytesN<32>, ()> = Map::new(&env);
        let mut collected: u32 = 0;
        for key in approvals.iter() {
            if seen.contains_key(key.clone()) {
                continue;
            }
            seen.set(key.clone(), ());
            if let Some(weight) = config.weights.get(key.clone()) {
                collected = collected.checked_add(weight).ok_or(ContractError::Overflow)?;
            }
        }

        if collected < config.threshold {
            return Err(ContractError::ThresholdNotMet);
        }

        Ok(collected)
    }

    fn emit_weights_updated(
        env: &Env,
        signers: &Vec<SignerWeight>,
        threshold: u32,
        previous_effective_at: u64,
        effective_at: u64,
        updated_by: Address,
    ) {
        let event = SignerWeightsUpdatedEvent {
            signers: signers.clone(),
            threshold,
            previous_effective_at,
            effective_at,
            updated_by,
        };
        env.events()
            .publish((Symbol::new(env, SIGNER_WEIGHTS_UPDATED),), event);
    }
}

#[cfg(test)]
mod test {
    use super::{
        ContractError, MultisigWeightGuard, MultisigWeightGuardClient, SignerWeight,
        SignerWeightsUpdatedEvent, WEIGHT_ROTATION_COOLDOWN_SECONDS,
    };
    use soroban_sdk::{
        testutils::{Address as _, Events as _, Ledger as _},
        vec, Address, BytesN, Env, Map, Symbol, TryFromVal, Vec,
    };

    /// Realistic base epoch so that "never rotated" (last = 0) is genuinely in
    /// the past.
    const T0: u64 = 1_700_000_000;

    fn setup() -> (Env, MultisigWeightGuardClient<'static>, Address) {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|li| li.timestamp = T0);
        let id = env.register_contract(None, MultisigWeightGuard);
        let client = MultisigWeightGuardClient::new(&env, &id);
        let admin = Address::generate(&env);
        (env, client, admin)
    }

    fn key(env: &Env, byte: u8) -> BytesN<32> {
        BytesN::from_array(env, &[byte; 32])
    }

    fn signers(env: &Env, entries: &[(BytesN<32>, u32)]) -> Vec<SignerWeight> {
        let mut out = Vec::new(env);
        for (public_key, weight) in entries.iter() {
            out.push_back(SignerWeight {
                public_key: public_key.clone(),
                weight: *weight,
            });
        }
        out
    }

    /// A typical 3-of-5 style set: 3 keys of weight 1, 2 keys of weight 2.
    fn initial_set(env: &Env) -> Vec<SignerWeight> {
        signers(
            env,
            &[
                (key(env, 1), 1),
                (key(env, 2), 1),
                (key(env, 3), 1),
                (key(env, 4), 2),
                (key(env, 5), 2),
            ],
        )
    }

    fn advance(env: &Env, seconds: u64) {
        let now = env.ledger().timestamp();
        env.ledger().with_mut(|li| li.timestamp = now + seconds);
    }

    #[test]
    fn initialize_installs_active_configuration() {
        let (env, client, admin) = setup();
        client.initialize(&admin, &initial_set(&env), &4);

        let active = client.active_configuration();
        assert_eq!(active.threshold, 4);
        assert_eq!(active.effective_at, T0);
        assert_eq!(client.effective_threshold(), 4);
        assert_eq!(client.effective_signer_weight(&key(&env, 4)), 2);
        assert_eq!(client.effective_signer_weight(&key(&env, 9)), 0);
        assert!(client.pending_configuration().is_none());
    }

    #[test]
    fn initialize_cannot_run_twice() {
        let (env, client, admin) = setup();
        client.initialize(&admin, &initial_set(&env), &4);
        let res = client.try_initialize(&admin, &initial_set(&env), &4);
        assert_eq!(res, Err(Ok(ContractError::AlreadyInitialized)));
    }

    #[test]
    fn first_rotation_is_not_blocked_by_cooldown() {
        let (env, client, admin) = setup();
        client.initialize(&admin, &initial_set(&env), &4);
        assert_eq!(client.rotation_cooldown_remaining(), 0);

        let proposed = signers(&env, &[(key(&env, 7), 3), (key(&env, 8), 3)]);
        let effective_at = client.propose_weight_rotation(&admin, &proposed, &5);
        assert_eq!(effective_at, T0 + WEIGHT_ROTATION_COOLDOWN_SECONDS);
    }

    #[test]
    fn old_weights_stay_effective_until_configuration_timestamp() {
        let (env, client, admin) = setup();
        client.initialize(&admin, &initial_set(&env), &4);

        let proposed = signers(&env, &[(key(&env, 7), 5)]);
        client.propose_weight_rotation(&admin, &proposed, &5);

        // Well inside the cooldown window the pending config exists but the
        // *previous* weights are still the ones enforced.
        advance(&env, WEIGHT_ROTATION_COOLDOWN_SECONDS - 1);
        let in_force = client.effective_configuration();
        assert_eq!(in_force.effective_at, T0);
        assert_eq!(client.effective_signer_weight(&key(&env, 1)), 1);
        assert_eq!(client.effective_signer_weight(&key(&env, 7)), 0);
        assert_eq!(client.effective_threshold(), 4);

        // One second later the staged configuration takes over.
        advance(&env, 1);
        let in_force = client.effective_configuration();
        assert_eq!(in_force.effective_at, T0 + WEIGHT_ROTATION_COOLDOWN_SECONDS);
        assert_eq!(client.effective_signer_weight(&key(&env, 7)), 5);
        assert_eq!(client.effective_signer_weight(&key(&env, 1)), 0);
        assert_eq!(client.effective_threshold(), 5);
    }

    #[test]
    fn apply_before_activation_is_rejected() {
        let (env, client, admin) = setup();
        client.initialize(&admin, &initial_set(&env), &4);
        client.propose_weight_rotation(&admin, &signers(&env, &[(key(&env, 7), 3)]), &3);

        advance(&env, WEIGHT_ROTATION_COOLDOWN_SECONDS - 1);
        let res = client.try_apply_pending_weights();
        assert_eq!(res, Err(Ok(ContractError::ActivationNotReached)));
    }

    #[test]
    fn apply_at_activation_switches_configuration_and_clears_pending() {
        let (env, client, admin) = setup();
        client.initialize(&admin, &initial_set(&env), &4);
        client.propose_weight_rotation(&admin, &signers(&env, &[(key(&env, 7), 3)]), &3);

        advance(&env, WEIGHT_ROTATION_COOLDOWN_SECONDS);
        let applied = client.apply_pending_weights();
        assert_eq!(applied.threshold, 3);
        assert_eq!(
            applied.effective_at,
            T0 + WEIGHT_ROTATION_COOLDOWN_SECONDS
        );

        assert!(client.pending_configuration().is_none());
        let active = client.active_configuration();
        assert_eq!(active.threshold, 3);
        assert_eq!(client.effective_signer_weight(&key(&env, 7)), 3);
    }

    #[test]
    fn apply_without_pending_is_rejected() {
        let (env, client, admin) = setup();
        client.initialize(&admin, &initial_set(&env), &4);
        let res = client.try_apply_pending_weights();
        assert_eq!(res, Err(Ok(ContractError::NoPendingConfiguration)));
    }

    #[test]
    fn consecutive_rotation_within_cooldown_is_rejected() {
        let (env, client, admin) = setup();
        client.initialize(&admin, &initial_set(&env), &4);

        client.propose_weight_rotation(&admin, &signers(&env, &[(key(&env, 7), 3)]), &3);
        advance(&env, WEIGHT_ROTATION_COOLDOWN_SECONDS);
        client.apply_pending_weights();

        // Cooldown is anchored to the moment the rotation took effect, so a
        // second rotation requested right away is rejected.
        assert_eq!(
            client.rotation_cooldown_remaining(),
            WEIGHT_ROTATION_COOLDOWN_SECONDS
        );
        let res = client.try_propose_weight_rotation(
            &admin,
            &signers(&env, &[(key(&env, 8), 3)]),
            &3,
        );
        assert_eq!(res, Err(Ok(ContractError::CooldownNotElapsed)));

        // Once the cooldown elapses the next rotation is accepted and is itself
        // staged a further 72h out.
        advance(&env, WEIGHT_ROTATION_COOLDOWN_SECONDS);
        let effective_at = client.propose_weight_rotation(
            &admin,
            &signers(&env, &[(key(&env, 8), 3)]),
            &3,
        );
        assert_eq!(effective_at, T0 + 3 * WEIGHT_ROTATION_COOLDOWN_SECONDS);
    }

    #[test]
    fn pending_rotation_blocks_staging_another() {
        let (env, client, admin) = setup();
        client.initialize(&admin, &initial_set(&env), &4);
        client.propose_weight_rotation(&admin, &signers(&env, &[(key(&env, 7), 3)]), &3);

        // A second rotation cannot be staged while one is still pending: it has
        // to be finalised first so the change cannot be silently overwritten.
        let res = client.try_propose_weight_rotation(
            &admin,
            &signers(&env, &[(key(&env, 8), 3)]),
            &3,
        );
        assert_eq!(res, Err(Ok(ContractError::RotationAlreadyPending)));

        advance(&env, WEIGHT_ROTATION_COOLDOWN_SECONDS);
        client.apply_pending_weights();
        assert!(client.pending_configuration().is_none());
    }

    #[test]
    fn cooldown_remaining_counts_down() {
        let (env, client, admin) = setup();
        client.initialize(&admin, &initial_set(&env), &4);
        client.propose_weight_rotation(&admin, &signers(&env, &[(key(&env, 7), 3)]), &3);
        advance(&env, WEIGHT_ROTATION_COOLDOWN_SECONDS);
        client.apply_pending_weights();

        assert_eq!(
            client.rotation_cooldown_remaining(),
            WEIGHT_ROTATION_COOLDOWN_SECONDS
        );
        advance(&env, 10);
        assert_eq!(
            client.rotation_cooldown_remaining(),
            WEIGHT_ROTATION_COOLDOWN_SECONDS - 10
        );
    }

    #[test]
    fn non_admin_cannot_stage_a_rotation() {
        let (env, client, admin) = setup();
        client.initialize(&admin, &initial_set(&env), &4);
        let intruder = Address::generate(&env);
        let res = client.try_propose_weight_rotation(
            &intruder,
            &signers(&env, &[(key(&env, 7), 3)]),
            &3,
        );
        assert_eq!(res, Err(Ok(ContractError::NotAdmin)));
    }

    #[test]
    fn quorum_uses_old_weights_until_activation_then_new_ones() {
        let (env, client, admin) = setup();
        client.initialize(&admin, &initial_set(&env), &4);

        // Stage a rotation that drops key #1 and #4 and keeps only key #7.
        client.propose_weight_rotation(&admin, &signers(&env, &[(key(&env, 7), 5)]), &5);

        // Before activation the old quorum still works: {4,5} = 2 + 2 = 4.
        let collected = client.verify_quorum(&vec![&env, key(&env, 4), key(&env, 5)]);
        assert_eq!(collected, 4);

        // The new key is not yet counted.
        let res = client.try_verify_quorum(&vec![&env, key(&env, 7)]);
        assert_eq!(res, Err(Ok(ContractError::ThresholdNotMet)));

        // After activation only the new key counts and the old ones are gone.
        advance(&env, WEIGHT_ROTATION_COOLDOWN_SECONDS);
        assert_eq!(client.verify_quorum(&vec![&env, key(&env, 7)]), 5);
        let res = client.try_verify_quorum(&vec![&env, key(&env, 4), key(&env, 5)]);
        assert_eq!(res, Err(Ok(ContractError::ThresholdNotMet)));
    }

    #[test]
    fn quorum_ignores_duplicates_and_unknown_keys() {
        let (env, client, admin) = setup();
        client.initialize(&admin, &initial_set(&env), &4);

        // key #4 counted twice must still only contribute weight 2 -> fails,
        // and an unknown key is ignored entirely.
        let res = client.try_verify_quorum(&vec![&env, key(&env, 4), key(&env, 4), key(&env, 9)]);
        assert_eq!(res, Err(Ok(ContractError::ThresholdNotMet)));

        let collected = client.verify_quorum(&vec![&env, key(&env, 4), key(&env, 4), key(&env, 1), key(&env, 2)]);
        assert_eq!(collected, 4);
    }

    #[test]
    fn invalid_configurations_are_rejected() {
        let (env, client, admin) = setup();
        client.initialize(&admin, &initial_set(&env), &4);

        // Empty set.
        let res = client.try_propose_weight_rotation(&admin, &Vec::new(&env), &1);
        assert_eq!(res, Err(Ok(ContractError::EmptyWeightSet)));

        // Zero weight.
        let res =
            client.try_propose_weight_rotation(&admin, &signers(&env, &[(key(&env, 7), 0)]), &1);
        assert_eq!(res, Err(Ok(ContractError::ZeroWeight)));

        // Duplicate signer.
        let res = client.try_propose_weight_rotation(
            &admin,
            &signers(&env, &[(key(&env, 7), 1), (key(&env, 7), 1)]),
            &1,
        );
        assert_eq!(res, Err(Ok(ContractError::DuplicateSigner)));

        // Threshold zero.
        let res =
            client.try_propose_weight_rotation(&admin, &signers(&env, &[(key(&env, 7), 1)]), &0);
        assert_eq!(res, Err(Ok(ContractError::InvalidThreshold)));

        // Threshold larger than the total weight.
        let res =
            client.try_propose_weight_rotation(&admin, &signers(&env, &[(key(&env, 7), 1)]), &2);
        assert_eq!(res, Err(Ok(ContractError::InvalidThreshold)));
    }

    #[test]
    fn initialize_rejects_invalid_configuration() {
        let (env, client, admin) = setup();
        let res = client.try_initialize(&admin, &signers(&env, &[(key(&env, 1), 1)]), &0);
        assert_eq!(res, Err(Ok(ContractError::InvalidThreshold)));
    }

    #[test]
    fn signer_weights_updated_event_carries_full_breakdown() {
        let (env, client, admin) = setup();
        client.initialize(&admin, &initial_set(&env), &4);

        let proposed = signers(&env, &[(key(&env, 7), 3), (key(&env, 8), 1)]);
        let effective_at = client.propose_weight_rotation(&admin, &proposed, &3);

        let events = env.events().all();
        let (_, topics, data) = events.get(events.len() - 1).unwrap();

        let topic: Symbol = Symbol::try_from_val(&env, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic, Symbol::new(&env, "SignerWeightsUpdated"));

        let decoded = SignerWeightsUpdatedEvent::try_from_val(&env, &data).unwrap();
        assert_eq!(decoded.threshold, 3);
        assert_eq!(decoded.previous_effective_at, T0);
        assert_eq!(decoded.effective_at, effective_at);
        assert_eq!(decoded.updated_by, admin);
        assert_eq!(decoded.signers.len(), 2);

        let mut seen: Map<BytesN<32>, u32> = Map::new(&env);
        for entry in decoded.signers.iter() {
            seen.set(entry.public_key.clone(), entry.weight);
        }
        assert_eq!(seen.get(key(&env, 7)), Some(3));
        assert_eq!(seen.get(key(&env, 8)), Some(1));
    }

    #[test]
    fn configuration_event_emitted_on_initialize() {
        let (env, client, admin) = setup();
        client.initialize(&admin, &initial_set(&env), &4);
        assert_eq!(env.events().all().len(), 1);
    }
}
