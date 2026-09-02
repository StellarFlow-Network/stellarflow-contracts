use soroban_sdk::{contract, contractimpl, contracttype, symbol_short, Address, Env, Symbol, token};

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    Escrow(u64), // ID-based escrow storage
    EscrowId,    // Counter for new IDs
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Escrow {
    pub sender: Address,
    pub recipient: Address,
    pub token: Address,
    pub amount: i128,
}

#[contract]
pub struct RemittanceContract;

#[contractimpl]
impl RemittanceContract {
    /// Creates a new remittance escrow.
    /// Requirements: Enforce sender authorization and emit EscrowCreated.
    pub fn create_escrow(
        e: Env,
        sender: Address,
        recipient: Address,
        token: Address,
        amount: i128,
    ) -> u64 {
        // Requirement 1: Enforce authorization check on sender
        sender.require_auth();

        // Transfer tokens from sender to this contract
        let token_client = token::Client::new(&e, &token);
        token_client.transfer(&sender, &e.current_contract_address(), &amount);

        // Increment and get new ID
        let mut id: u64 = e.storage().persistent().get(&DataKey::EscrowId).unwrap_or(0);
        id += 1;
        e.storage().persistent().set(&DataKey::EscrowId, &id);

        // Store escrow details
        let escrow = Escrow {
            sender: sender.clone(),
            recipient: recipient.clone(),
            token: token.clone(),
            amount,
        };
        e.storage().persistent().set(&DataKey::Escrow(id), &escrow);

        // Requirement 3: Emit structured EscrowCreated event
        e.events().publish(
            (symbol_short!("created"), sender, recipient),
            (id, token, amount),
        );

        id
    }

    /// Settles and releases an escrow to the recipient.
    /// Requirements: Enforce recipient authorization and emit EscrowClaimed.
    pub fn claim_escrow(e: Env, escrow_id: u64) {
        let key = DataKey::Escrow(escrow_id);
        
        // Retrieve escrow or panic if it doesn't exist
        let escrow: Escrow = e.storage().persistent().get(&key).expect("Escrow not found");

        // Requirement 2: Enforce recipient signature verification
        escrow.recipient.require_auth();

        // Release funds to the recipient
        let token_client = token::Client::new(&e, &escrow.token);
        token_client.transfer(&e.current_contract_address(), &escrow.recipient, &escrow.amount);

        // Remove escrow from storage
        e.storage().persistent().remove(&key);

        // Requirement 3: Emit structured EscrowClaimed event
        e.events().publish(
            (symbol_short!("claimed"), escrow.recipient),
            (escrow_id, escrow.token, escrow.amount),
        );
    }
}
