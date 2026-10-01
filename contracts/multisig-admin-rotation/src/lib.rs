#![no_std]

soroban-sdk::contractimport!();

use soroban-sdk::{contract, contractimpl, contracttype, Address, Env, Vec, Bytes};

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    AdminKeys,
    PendingRotation,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingAdminRotation {
    pub new_admin_keys: Vec<Address>,
    pub effective_at: u64,
}

#[contract]
pub struct MultisigAdminRotationContract;

#[contractimpl]
impl MultisigAdminRotationContract {
    pub fn initialize(env: Env, admin_keys: Vec<Address>) {
        if env.storage().persistent().has(&DataKey::AdminKeys) {
            panic!("already initialized");
        }
        if admin_keys.is_empty() {
            panic!("admin keys cannot be empty");
        }
        env.storage().persistent().set(&DataKey::AdminKeys, &admin_keys);
    }

    pub fn propose_rotation(env: Env, approvers: Vec<Address>, new_admin_keys: Vec<Address>) {
        let current_keys: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::AdminKeys)
            .expect("not initialized");

        if new_admin_keys.is_empty() {
            panic!("new admin keys cannot be empty");
        }

        // Require 100% threshold signature approval of the current master key set
        if approvers.len() != current_keys.len() {
            panic!("100% threshold approval required: approvers count mismatch");
        }

        for key in current_keys.iter() {
            key.require_auth();
            let mut found = false;
            for approver in approvers.iter() {
                if approver == key {
                    found = true;
                    break;
                }
            }
            if !found {
                panic!("100% threshold approval required: missing master key approval");
            }
        }

        let current_timestamp = env.ledger().timestamp();
        let effective_at = current_timestamp + 86400; // 24-hour timelock delay

        let pending = PendingAdminRotation {
            new_admin_keys,
            effective_at,
        };

        env.storage().persistent().set(&DataKey::PendingRotation, &pending);
    }

    pub fn apply_rotation(env: Env) {
        let pending: PendingAdminRotation = env
            .storage()
            .persistent()
            .get(&DataKey::PendingRotation)
            .expect("no pending rotation");

        let current_timestamp = env.ledger().timestamp();
        if current_timestamp < pending.effective_at {
            panic!("24-hour timelock delay has not expired");
        }

        env.storage().persistent().set(&DataKey::AdminKeys, &pending.new_admin_keys);
        env.storage().persistent().remove(&DataKey::PendingRotation);

        // Emit AdminKeysRotated system audit event
        env.events().publish(
            (Bytes::from_slice(&env, b"AdminKeysRotated"),),
            pending.new_admin_keys,
        );
    }

    pub fn get_admin_keys(env: Env) -> Vec<Address> {
        env.storage()
            .persistent()
            .get(&DataKey::AdminKeys)
            .expect("not initialized");
        env.storage().persistent().get(&DataKey::AdminKeys).unwrap()
    }

    pub fn get_pending_rotation(env: Env) -> Option<PendingAdminRotation> {
        env.storage().persistent().get(&DataKey::PendingRotation)
    }
}

#[cfg(test)]mod test;
