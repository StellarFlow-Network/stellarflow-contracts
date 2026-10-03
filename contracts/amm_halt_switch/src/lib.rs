#![no_std]
use soroban_sdk::{contract, contractimpl, contracttype, Address, Env, Vec};

#[derive(Clone)]
@contracttype
pub enum DataKey {
    Admin,
    Paused,
    PrevLiquidity,
    MultiSigSigners,
    RequiredApprovals,
    Approvals(Address),
}

@contract
pub struct AmmHaltSwitchContract;

@contractimpl
impl AmmHaltSwitchContract {
    /// Initialize contract with admin, signers, and liquidity baseline
    pub fn initialize(env: Env, admin: Address, signers: Vec<Address>, required_approvals: u32) {
        admin.require_auth();
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("already initialized");
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Paused, &false);
        env.storage().instance().set(&DataKey::PrevLiquidity, &1000_000_000i128); // Initial baseline
        env.storage().instance().set(&DataKey::MultiSigSigners, &signers);
        env.storage().instance().set(&DataKey::RequiredApprovals, &required_approvals);
    }

    /// Check liquidity and trigger emergency pause if drop exceeds 30% (Delta L > 0.30)
    pub fn update_liquidity(env: Env, current_liquidity: i128) {
        let prev_liquidity: i128 = env.storage().instance().get(&DataKey::PrevLiquidity).unwrap_or(current_liquidity);
        
        if prev_liquidity > 0 {
            // Delta L = (L_prev - L_curr) / L_prev
            // Represented with fixed precision scaled by 1000: drop_ratio_scaled >= 300 (30.0%)
            let diff = prev_liquidity - current_liquidity;
            if diff > 0 {
                let drop_scaled = (diff * 1000) / prev_liquidity;
                if drop_scaled > 300 {
                    env.storage().instance().set(&DataKey::Paused, &true);
                }
            }
        }

        env.storage().instance().set(&DataKey::PrevLiquidity, &current_liquidity);
    }

    /// Multi-sig approval to resume standard trading operations
    pub fn approve_resume(env: Env, signer: Address) {
        signer.require_auth();
        let signers: Vec<Address> = env.storage().instance().get(&DataKey::MultiSigSigners).unwrap();
        if !signers.contains(&signer) {
            panic!("unauthorized signer");
        }

        let key = DataKey::Approvals(signer.clone());
        let approved: bool = env.storage().instance().get(&key).unwrap_or(false);
        if approved {
            panic!("already approved by signer");
        }

        env.storage().instance().set(&key, &true);

        // Count approvals
        let required: u32 = env.storage().instance().get(&DataKey::RequiredApprovals).unwrap();
        let mut current_approvals = 0u32;
        for s in signers.iter() {
            let a: bool = env.storage().instance().get(&DataKey::Approvals(s)).unwrap_or(false);
            if a {
                current_approvals += 1;
            }
        }

        if current_approvals >= required {
            env.storage().instance().set(&DataKey::Paused, &false);
        }
    }

    pub fn is_paused(env: Env) -> bool {
        env.storage().instance().get(&DataKey::Paused).unwrap_or(false)
    }
}
