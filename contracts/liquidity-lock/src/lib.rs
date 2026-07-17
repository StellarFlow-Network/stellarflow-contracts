#![no_std]

use soroban_sdk::{contract, contractimpl, contracttype, token, Address, Env};

const SCHEDULE_LEDGERS: u32 = 3000;

#[derive(Clone)]
#[contracttype]
pub enum DataKey {
    Admin,
    Token,
    Stream(Address), // Maps recipient address to their StreamData
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct StreamData {
    pub start_ledger: u32,
    pub total_amount: i128,
    pub claimed_amount: i128,
}

#[contract]
pub struct LiquidityLockContract;

#[contractimpl]
impl LiquidityLockContract {
    pub fn initialize(env: Env, admin: Address, token: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("already initialized");
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Token, &token);
    }

    /// Build a time-locked distribution pipeline that releases accrued validator rewards gradually over a 3,000-ledger linear schedule
    pub fn create_stream(env: Env, admin: Address, recipient: Address, amount: i128) {
        admin.require_auth();
        let stored_admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        if admin != stored_admin {
            panic!("not admin");
        }
        if amount <= 0 {
            panic!("amount must be positive");
        }

        let stream_key = DataKey::Stream(recipient.clone());
        if env.storage().instance().has(&stream_key) {
            panic!("stream already exists");
        }

        let current_ledger = env.ledger().sequence();
        let stream = StreamData {
            start_ledger: current_ledger,
            total_amount: amount,
            claimed_amount: 0,
        };

        // Transfer tokens from admin to this contract
        let token_addr: Address = env.storage().instance().get(&DataKey::Token).unwrap();
        let token_client = token::Client::new(&env, &token_addr);
        token_client.transfer(&admin, &env.current_contract_address(), &amount);

        env.storage().instance().set(&stream_key, &stream);
    }

    /// Provide a public inspection method that calculates claimable token allocations based on the elapsed ledger duration.
    pub fn get_claimable(env: Env, recipient: Address) -> i128 {
        let stream_key = DataKey::Stream(recipient.clone());
        if let Some(stream) = env.storage().instance().get::<_, StreamData>(&stream_key) {
            let current_ledger = env.ledger().sequence();
            let elapsed = current_ledger.saturating_sub(stream.start_ledger);

            let unlocked = if elapsed >= SCHEDULE_LEDGERS {
                stream.total_amount
            } else {
                (stream.total_amount * (elapsed as i128)) / (SCHEDULE_LEDGERS as i128)
            };

            unlocked - stream.claimed_amount
        } else {
            0
        }
    }

    /// Claims the currently unlocked tokens from the stream
    pub fn claim(env: Env, recipient: Address) -> i128 {
        recipient.require_auth();

        let stream_key = DataKey::Stream(recipient.clone());
        let mut stream: StreamData = env
            .storage()
            .instance()
            .get(&stream_key)
            .unwrap_or_else(|| panic!("no stream found"));

        let claimable = Self::get_claimable(env.clone(), recipient.clone());
        if claimable <= 0 {
            panic!("nothing to claim");
        }

        stream.claimed_amount += claimable;
        env.storage().instance().set(&stream_key, &stream);

        let token_addr: Address = env.storage().instance().get(&DataKey::Token).unwrap();
        let token_client = token::Client::new(&env, &token_addr);
        token_client.transfer(&env.current_contract_address(), &recipient, &claimable);

        claimable
    }
}
