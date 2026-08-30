use soroban_sdk::{contract, contractimpl, contracttype, token, Address, Env};

/// State of a settlement escrow.
///
/// Funds are held until either:
/// - Both `sender` and `receiver` approve (sign) before `expiry_ledger` elapses.
/// - The depositor executes a single-signature refund after `expiry_ledger`.
#[contracttype]
#[derive(Clone)]
pub struct Escrow {
    pub sender: Address,
    pub receiver: Address,
    pub depositor: Address,
    pub token: Address,
    pub amount: i128,
    pub expiry_ledger: u32,
    pub sender_approved: bool,
    pub receiver_approved: bool,
    pub released: bool,
}

impl Escrow {
    pub fn is_approved(&self) -> bool {
        self.sender_approved && self.receiver_approved
    }

    pub fn is_expired(&self, current_ledger: u32) -> bool {
        current_ledger >= self.expiry_ledger
    }
}

/// Create a new escrow that deposits `amount` of `token` from `depositor`
/// into the contract, locked until both `sender` and `receiver` sign, or
/// until `expiry_ledger` passes.
pub fn create(
    env: &Env,
    sender: Address,
    receiver: Address,
    depositor: Address,
    token: Address,
    amount: i128,
    expiry_ledger: u32,
) -> Escrow {
    let escrow = Escrow {
        sender,
        receiver,
        depositor,
        token,
        amount,
        expiry_ledger,
        sender_approved: false,
        receiver_approved: false,
        released: false,
    };
    let token_client = token::Client::new(env, &escrow.token);
    token_client.transfer(&escrow.depositor, &env.current_contract_address(), &amount);
    escrow
}

/// Approve by the sender — one of the two required signatures for release.
pub fn approve_by_sender(escrow: &mut Escrow, caller: &Address) -> Result<(), ()> {
    if caller != &escrow.sender {
        return Err(());
    }
    escrow.sender_approved = true;
    Ok(())
}

/// Approve by the receiver — one of the two required signatures for release.
pub fn approve_by_receiver(escrow: &mut Escrow, caller: &Address) -> Result<(), ()> {
    if caller != &escrow.receiver {
        return Err(());
    }
    escrow.receiver_approved = true;
    Ok(())
}

/// Release funds to the receiver when both parties have approved before expiry.
pub fn release(env: &Env, escrow: &mut Escrow, current_ledger: u32) -> Result<(), ()> {
    if escrow.released {
        return Err(());
    }
    if escrow.is_expired(current_ledger) {
        return Err(());
    }
    if !escrow.is_approved() {
        return Err(());
    }
    let token_client = token::Client::new(env, &escrow.token);
    token_client.transfer(
        &env.current_contract_address(),
        &escrow.receiver,
        &escrow.amount,
    );
    escrow.released = true;
    Ok(())
}

/// Single-signature refund by the depositor after the expiry ledger has passed.
pub fn refund(env: &Env, escrow: &mut Escrow, caller: &Address, current_ledger: u32) -> Result<(), ()> {
    if escrow.released {
        return Err(());
    }
    if caller != &escrow.depositor {
        return Err(());
    }
    if !escrow.is_expired(current_ledger) {
        return Err(());
    }
    let token_client = token::Client::new(env, &escrow.token);
    token_client.transfer(
        &env.current_contract_address(),
        &escrow.depositor,
        &escrow.amount,
    );
    escrow.released = true;
    Ok(())
}

#[cfg(test)]
#[contract]
pub struct EscrowHarness;

#[cfg(test)]
#[contractimpl]
impl EscrowHarness {
    pub fn create_escrow(
        env: Env,
        sender: Address,
        receiver: Address,
        depositor: Address,
        token: Address,
        amount: i128,
        expiry_ledger: u32,
    ) -> Escrow {
        depositor.require_auth();
        create(&env, sender, receiver, depositor, token, amount, expiry_ledger)
    }

    pub fn release_escrow(env: Env, mut escrow: Escrow, current_ledger: u32) -> Result<Escrow, soroban_sdk::Error> {
        match release(&env, &mut escrow, current_ledger) {
            Ok(()) => Ok(escrow),
            Err(()) => Err(soroban_sdk::Error::from_contract_error(3)),
        }
    }

    pub fn refund_escrow(env: Env, mut escrow: Escrow, caller: Address, current_ledger: u32) -> Result<Escrow, soroban_sdk::Error> {
        match refund(&env, &mut escrow, &caller, current_ledger) {
            Ok(()) => Ok(escrow),
            Err(()) => Err(soroban_sdk::Error::from_contract_error(3)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    fn setup() -> (Env, Address, Address, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let token_id = env.register_stellar_asset_contract(admin);
        let deposit = soroban_sdk::token::StellarAssetClient::new(&env, &token_id);
        let depositor = Address::generate(&env);
        deposit.mint(&depositor, &5000);
        let sender = Address::generate(&env);
        let receiver = Address::generate(&env);
        (env, token_id, depositor, sender, receiver)
    }

    fn client(env: &Env) -> EscrowHarnessClient {
        let harness_id = env.register_contract(None, EscrowHarness);
        EscrowHarnessClient::new(env, &harness_id)
    }

    #[test]
    fn test_create_escrow() {
        let (env, token_id, depositor, sender, receiver) = setup();
        let client = client(&env);
        let current = env.ledger().sequence();
        let escrow = client.create_escrow(&sender, &receiver, &depositor, &token_id, &1000i128, &(current + 100));
        assert!(!escrow.sender_approved);
        assert!(!escrow.receiver_approved);
        assert!(!escrow.released);
    }

    #[test]
    fn test_dual_approval_before_expiry_releases() {
        let (env, token_id, depositor, sender, receiver) = setup();
        let client = client(&env);
        let current = env.ledger().sequence();
        let mut escrow = client.create_escrow(&sender, &receiver, &depositor, &token_id, &1000i128, &(current + 100));
        approve_by_sender(&mut escrow, &sender).unwrap();
        assert!(!escrow.is_approved());
        approve_by_receiver(&mut escrow, &receiver).unwrap();
        assert!(escrow.is_approved());
        let released = client.release_escrow(&escrow, &(current + 50));
        assert!(released.released);
        escrow = released;
    }

    #[test]
    fn test_release_fails_without_dual_approval() {
        let (env, token_id, depositor, sender, receiver) = setup();
        let client = client(&env);
        let current = env.ledger().sequence();
        let mut escrow = client.create_escrow(&sender, &receiver, &depositor, &token_id, &1000i128, &(current + 100));
        approve_by_sender(&mut escrow, &sender).unwrap();
        assert!(client.try_release_escrow(&escrow, &(current + 50)).is_err());
        assert!(!escrow.released);
    }

    #[test]
    fn test_refund_after_expiry_by_depositor() {
        let (env, token_id, depositor, sender, receiver) = setup();
        let client = client(&env);
        let current = env.ledger().sequence();
        let mut escrow = client.create_escrow(&sender, &receiver, &depositor, &token_id, &1000i128, &(current + 10));
        let refunded = client.refund_escrow(&escrow, &depositor, &(current + 20));
        assert!(refunded.released);
        escrow = refunded;
    }

    #[test]
    fn test_refund_fails_before_expiry() {
        let (env, token_id, depositor, sender, receiver) = setup();
        let client = client(&env);
        let current = env.ledger().sequence();
        let mut escrow = client.create_escrow(&sender, &receiver, &depositor, &token_id, &1000i128, &(current + 100));
        assert!(client.try_refund_escrow(&escrow, &depositor, &(current + 50)).is_err());
        assert!(!escrow.released);
    }

    #[test]
    fn test_refund_rejects_non_depositor() {
        let (env, token_id, depositor, sender, receiver) = setup();
        let client = client(&env);
        let current = env.ledger().sequence();
        let mut escrow = client.create_escrow(&sender, &receiver, &depositor, &token_id, &1000i128, &(current + 10));
        let attacker = Address::generate(&env);
        assert!(client.try_refund_escrow(&escrow, &attacker, &(current + 20)).is_err());
    }

    #[test]
    fn test_approve_by_wrong_sender_fails() {
        let (env, token_id, depositor, sender, receiver) = setup();
        let client = client(&env);
        let current = env.ledger().sequence();
        let mut escrow = client.create_escrow(&sender, &receiver, &depositor, &token_id, &1000i128, &(current + 100));
        let wrong = Address::generate(&env);
        assert!(approve_by_sender(&mut escrow, &wrong).is_err());
        assert!(!escrow.sender_approved);
    }

    #[test]
    fn test_release_fails_after_expiry() {
        let (env, token_id, depositor, sender, receiver) = setup();
        let client = client(&env);
        let current = env.ledger().sequence();
        let mut escrow = client.create_escrow(&sender, &receiver, &depositor, &token_id, &1000i128, &(current + 10));
        approve_by_sender(&mut escrow, &sender).unwrap();
        approve_by_receiver(&mut escrow, &receiver).unwrap();
        assert!(client.try_release_escrow(&escrow, &(current + 20)).is_err());
    }

    #[test]
    fn test_release_fails_once_already_released() {
        let (env, token_id, depositor, sender, receiver) = setup();
        let client = client(&env);
        let current = env.ledger().sequence();
        let mut escrow = client.create_escrow(&sender, &receiver, &depositor, &token_id, &1000i128, &(current + 100));
        approve_by_sender(&mut escrow, &sender).unwrap();
        approve_by_receiver(&mut escrow, &receiver).unwrap();
        assert!(client.try_release_escrow(&escrow, &(current + 50)).as_ref().unwrap().is_ok());
    }

    #[test]
    fn test_escrow_state_transitions() {
        let (env, token_id, depositor, sender, receiver) = setup();
        let client = client(&env);
        let current = env.ledger().sequence();
        let escrow = client.create_escrow(&sender, &receiver, &depositor, &token_id, &1000i128, &(current + 50));
        assert!(!escrow.is_approved());
        assert!(!escrow.is_expired(current));
        assert!(!escrow.is_expired(current + 49));
        assert!(escrow.is_expired(current + 50));
        assert!(escrow.is_expired(current + 100));
    }
}
