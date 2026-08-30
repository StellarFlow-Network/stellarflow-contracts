//! Shielded Remittance Escrow — Deposit Note Nullifier Verifier
//!
//! Implements double-spend protection for private cross-border remittance
//! withdrawals via zero-knowledge nullifiers, following the standard
//! shielded-pool pattern (nullifier = deterministic hash derived from the
//! spent note + spender's secret, revealed only at withdrawal time so it
//! can never be linked back to the original deposit).
//!
//! Responsibilities are split into three layers:
//!   - `storage`   : persistent nullifier-set access (single responsibility)
//!   - `verifier`  : pure verification logic (no I/O side effects)
//!   - `contract`  : public entrypoint orchestrating verify -> record -> emit
//!
//! Event emission follows this repo's existing convention (see
//! stop-loss-trigger's `trig_reg` event) of `env.events().publish((topics),
//! data)` rather than the newer `#[contractevent]` derive macro, since this
//! workspace pins `soroban-sdk = "=20.0.0"`.

#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, BytesN, Env,
};

/// A nullifier is the output of the note's ZK circuit — a 32-byte field
/// element. It is unlinkable to the deposit note but unique per spend.
pub type Nullifier = BytesN<32>;

#[contracttype]
#[derive(Clone)]
enum DataKey {
    /// Maps a spent nullifier -> unit marker. Presence = spent.
    Nullifier(Nullifier),
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum NullifierError {
    /// The nullifier has already been recorded — this note was already withdrawn.
    AlreadySpent = 1,
    /// The supplied ZK proof did not verify against the nullifier/public inputs.
    InvalidProof = 2,
}

/// ---- Storage layer ---------------------------------------------------
/// Isolated so the persistence mechanism (instance vs persistent storage,
/// TTL policy, key layout) can change without touching verification logic.
mod storage {
    use super::*;

    const NULLIFIER_TTL_LEDGERS: u32 = 6_312_000; // ~1 year at 5s/ledger
    const NULLIFIER_TTL_THRESHOLD: u32 = 1_000_000;

    pub fn is_spent(env: &Env, nullifier: &Nullifier) -> bool {
        env.storage()
            .persistent()
            .has(&DataKey::Nullifier(nullifier.clone()))
    }

    pub fn mark_spent(env: &Env, nullifier: &Nullifier) {
        let key = DataKey::Nullifier(nullifier.clone());
        env.storage().persistent().set(&key, &true);
        env.storage().persistent().extend_ttl(
            &key,
            NULLIFIER_TTL_THRESHOLD,
            NULLIFIER_TTL_LEDGERS,
        );
    }
}

/// ---- Verification layer -----------------------------------------------
/// Pure(ish) checks — no storage writes happen here, only reads plus proof
/// verification, so this can be unit tested independently of contract state
/// transitions.
mod verifier {
    use super::*;

    /// Checks the nullifier hasn't been spent before AND that the caller's
    /// ZK proof is valid for the given public inputs. Order matters: fail
    /// fast on the cheap check (storage read) before the expensive one
    /// (proof verification).
    pub fn verify_withdrawal(
        env: &Env,
        nullifier: &Nullifier,
        proof: &BytesN<256>,
        public_inputs: &BytesN<32>,
    ) -> Result<(), NullifierError> {
        if storage::is_spent(env, nullifier) {
            return Err(NullifierError::AlreadySpent);
        }

        if !verify_zk_proof(proof, nullifier, public_inputs) {
            return Err(NullifierError::InvalidProof);
        }

        Ok(())
    }

    /// Placeholder for the actual proof system integration (e.g. Groth16 /
    /// PLONK verifier over BLS12-381). Wire this up to your circuit's
    /// verifying key before deploying — this stub always rejects so the
    /// contract fails closed rather than silently accepting unverified
    /// withdrawals.
    fn verify_zk_proof(
        _proof: &BytesN<256>,
        _nullifier: &Nullifier,
        _public_inputs: &BytesN<32>,
    ) -> bool {
        // TODO: integrate real verifying key + pairing check.
        false
    }
}

#[contract]
pub struct NullifierVerifier;

#[contractimpl]
impl NullifierVerifier {
    /// Executes a shielded withdrawal:
    ///   1. Verify the nullifier is unspent and the proof is valid.
    ///   2. Record the nullifier so it can never be replayed.
    ///   3. Emit an anonymous payout event for indexers.
    ///
    /// `proof` and `public_inputs` are opaque to this module — they're
    /// handed to the ZK verifying key. `recipient`/`amount` are the only
    /// non-anonymous data in the whole flow, by design (someone has to
    /// receive the funds).
    pub fn withdraw(
        env: Env,
        nullifier: Nullifier,
        proof: BytesN<256>,
        public_inputs: BytesN<32>,
        recipient: Address,
        amount: i128,
    ) -> Result<(), NullifierError> {
        verifier::verify_withdrawal(&env, &nullifier, &proof, &public_inputs)?;

        storage::mark_spent(&env, &nullifier);

        // Anonymous payout event: topics carry only the event tag and the
        // nullifier (spend-uniqueness marker, unlinkable to the deposit).
        // Data carries recipient + amount — the only fields that must be
        // public for the payout to be indexable at all.
        env.events().publish(
            (symbol_short!("payout"), nullifier),
            (recipient, amount),
        );

        Ok(())
    }

    /// Read-only check exposed for off-chain callers / indexers who want to
    /// pre-flight a nullifier before submitting a withdrawal tx.
    pub fn is_nullifier_spent(env: Env, nullifier: Nullifier) -> bool {
        storage::is_spent(&env, &nullifier)
    }
}
