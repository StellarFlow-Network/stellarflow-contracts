use soroban_sdk::{contract, contracterror, contractimpl, contracttype, Address, Env, Symbol};

const MAX_ORACLE_AGE: u64 = 300;
const MAX_REASONABLE_PRICE: i128 = 1_000_000_000_000;

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct OracleResponse {
    pub price: i128,
    pub timestamp: u64,
    pub valid: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub enum FailureMode {
    Fresh,
    Stale,
    Corrupted,
    Malicious,
}

#[contract]
pub struct FailureMatrixOracle;

#[contractimpl]
impl FailureMatrixOracle {
    pub fn set_mode(env: Env, mode: FailureMode) {
        env.storage()
            .instance()
            .set(&Symbol::new(&env, "mode"), &mode);
    }

    pub fn get_response(env: Env, _asset: Symbol) -> OracleResponse {
        let mode: FailureMode = env
            .storage()
            .instance()
            .get(&Symbol::new(&env, "mode"))
            .unwrap_or(FailureMode::Fresh);
        let now = env.ledger().timestamp();

        match mode {
            FailureMode::Fresh => OracleResponse {
                price: 100,
                timestamp: now,
                valid: true,
            },
            FailureMode::Stale => OracleResponse {
                price: 100,
                timestamp: now.saturating_sub(MAX_ORACLE_AGE + 1),
                valid: true,
            },
            FailureMode::Corrupted => OracleResponse {
                price: 100,
                timestamp: now,
                valid: false,
            },
            FailureMode::Malicious => OracleResponse {
                price: MAX_REASONABLE_PRICE + 1,
                timestamp: now,
                valid: true,
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub enum ProtocolDataKey {
    Processed(Symbol),
    Paused,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracterror]
pub enum ProtocolError {
    OracleRejected = 1,
    EmergencyPaused = 2,
}

#[contract]
pub struct OracleFailureMatrixProtocol;

#[contractimpl]
impl OracleFailureMatrixProtocol {
    pub fn borrow(
        env: Env,
        oracle: Address,
        asset: Symbol,
        amount: i128,
    ) -> Result<bool, ProtocolError> {
        Self::process(&env, oracle, asset, amount)
    }

    pub fn swap(
        env: Env,
        oracle: Address,
        asset: Symbol,
        amount: i128,
    ) -> Result<bool, ProtocolError> {
        Self::process(&env, oracle, asset, amount)
    }

    pub fn liquidate(
        env: Env,
        oracle: Address,
        asset: Symbol,
        amount: i128,
    ) -> Result<bool, ProtocolError> {
        Self::process(&env, oracle, asset, amount)
    }

    pub fn processed(env: Env, operation: Symbol) -> i128 {
        env.storage()
            .persistent()
            .get(&ProtocolDataKey::Processed(operation))
            .unwrap_or(0)
    }

    pub fn is_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&ProtocolDataKey::Paused)
            .unwrap_or(false)
    }

    fn process(
        env: &Env,
        oracle: Address,
        asset: Symbol,
        amount: i128,
    ) -> Result<bool, ProtocolError> {
        if Self::is_paused(env) {
            return Err(ProtocolError::EmergencyPaused);
        }

        let response = FailureMatrixOracleClient::new(env, &oracle).get_response(&asset);
        let now = env.ledger().timestamp();
        let safe = response.valid
            && response.price > 0
            && response.price <= MAX_REASONABLE_PRICE
            && response.timestamp <= now
            && now.saturating_sub(response.timestamp) <= MAX_ORACLE_AGE;

        if !safe {
            env.storage()
                .instance()
                .set(&ProtocolDataKey::Paused, &true);
            return Ok(false);
        }

        let operation = Symbol::new(env, "operation");
        let current = Self::processed(env, operation.clone());
        env.storage()
            .persistent()
            .set(&ProtocolDataKey::Processed(operation), &(current + amount));
        Ok(true)
    }
}

pub struct FailureMatrixFixture {
    pub oracle: Address,
    pub protocol: Address,
}

pub fn setup_failure_matrix(env: &Env, mode: FailureMode) -> FailureMatrixFixture {
    env.mock_all_auths();
    env.ledger().with_mut(|ledger| ledger.timestamp = 1_000);

    let oracle = env.register_contract(None, FailureMatrixOracle);
    let protocol = env.register_contract(None, OracleFailureMatrixProtocol);
    FailureMatrixOracleClient::new(env, &oracle).set_mode(&mode);

    FailureMatrixFixture { oracle, protocol }
}
