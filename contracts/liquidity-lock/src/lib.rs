#![no_std]

use soroban_sdk::{contract, contractimpl, contracttype, token, Address, Env};

const SCHEDULE_LEDGERS: u32 = 3000;

#[derive(Clone)]
#[contracttype]
pub enum DataKey {
    Admin,
    Token,
    Stream(Address), // Maps recipient address to their StreamData
    /// Total tokens ever deposited into the vault (gross, not net of claims).
    TotalDeposited,
    /// Optional hard cap on TVL. When set, new deposits that would push
    /// TotalDeposited past this value are rejected.  `None` means no cap.
    MaxTvlCap,
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
        // TotalDeposited starts at zero; MaxTvlCap is absent (no cap) by default.
        env.storage().instance().set(&DataKey::TotalDeposited, &0_i128);
    }

    /// Build a time-locked distribution pipeline that releases accrued validator
    /// rewards gradually over a 3,000-ledger linear schedule.
    ///
    /// Reverts if the post-deposit TVL would exceed the configured `MaxTvlCap`.
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

        // ── TVL cap enforcement ───────────────────────────────────────────────
        let current_tvl: i128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalDeposited)
            .unwrap_or(0);

        let post_deposit_tvl = current_tvl
            .checked_add(amount)
            .expect("TVL overflow");

        if let Some(cap) = env
            .storage()
            .instance()
            .get::<DataKey, i128>(&DataKey::MaxTvlCap)
        {
            if post_deposit_tvl > cap {
                panic!("deposit exceeds TVL cap");
            }
        }
        // ─────────────────────────────────────────────────────────────────────

        // Invariant check: verify balance consistency before state change
        Self::assert_balance_invariant(&env);

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

        // Update running TVL total
        env.storage()
            .instance()
            .set(&DataKey::TotalDeposited, &post_deposit_tvl);

        // Invariant check: verify balance consistency after state change
        Self::assert_balance_invariant(&env);
    }

    /// Provide a public inspection method that calculates claimable token
    /// allocations based on the elapsed ledger duration.
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

    /// Claims the currently unlocked tokens from the stream.
    pub fn claim(env: Env, recipient: Address) -> i128 {
        recipient.require_auth();

        // Invariant check: verify balance consistency before state change
        Self::assert_balance_invariant(&env);

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

        // Invariant check: verify balance consistency after state change
        Self::assert_balance_invariant(&env);

        claimable
    }

    /// Governance: set or update the maximum TVL cap.
    ///
    /// Pass `new_cap = 0` to remove the cap entirely (no limit).
    /// Only the admin may call this.
    pub fn set_tvl_cap(env: Env, admin: Address, new_cap: i128) {
        admin.require_auth();
        let stored_admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        if admin != stored_admin {
            panic!("not admin");
        }
        if new_cap < 0 {
            panic!("cap must be non-negative");
        }

        if new_cap == 0 {
            // Remove the cap — vault is no longer restricted.
            env.storage().instance().remove(&DataKey::MaxTvlCap);
        } else {
            env.storage().instance().set(&DataKey::MaxTvlCap, &new_cap);
        }
    }

    /// Read the current TVL cap. Returns `None` when no cap is active.
    pub fn get_tvl_cap(env: Env) -> Option<i128> {
        env.storage()
            .instance()
            .get::<DataKey, i128>(&DataKey::MaxTvlCap)
    }

    /// Read the current total deposited TVL.
    pub fn get_total_deposited(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::TotalDeposited)
            .unwrap_or(0)
    }

    /// Invariant check: assert token reserves are non-negative.
    fn assert_balance_invariant(env: &Env) {
        let token_addr: Address = match env.storage().instance().get(&DataKey::Token) {
            Some(addr) => addr,
            None => return, // Not initialized yet
        };

        let token_client = token::Client::new(env, &token_addr);
        let actual_balance = token_client.balance(&env.current_contract_address());

        assert!(
            actual_balance >= 0,
            "Balance invariant violated: actual balance is negative"
        );
    }
}
