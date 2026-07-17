#![no_std]
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, token, Address, Env, Vec,
};

#[cfg(test)]
mod test;

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Token,
    Oracle,
    Balance(Address),
    Allowance(Address, Address), // (consumer, relayer)
    RelayerFunders(Address),     // relayer -> list of consumers who funded them
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    InvalidAmount = 2,
    InsufficientBalance = 3,
}

#[contract]
pub struct GasTank;

#[contractimpl]
impl GasTank {
    pub fn initialize(env: Env, token: Address, oracle: Address) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Token) {
            return Err(Error::AlreadyInitialized);
        }
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage().instance().set(&DataKey::Oracle, &oracle);
        Ok(())
    }

    pub fn get_token(env: Env) -> Address {
        env.storage().instance().get(&DataKey::Token).unwrap()
    }

    pub fn get_oracle(env: Env) -> Address {
        env.storage().instance().get(&DataKey::Oracle).unwrap()
    }

    pub fn deposit(env: Env, consumer: Address, amount: i128) -> Result<(), Error> {
        consumer.require_auth();
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let token_addr = Self::get_token(env.clone());
        let token_client = token::Client::new(&env, &token_addr);
        token_client.transfer(&consumer, &env.current_contract_address(), &amount);

        let balance_key = DataKey::Balance(consumer.clone());
        let current_balance: i128 = env.storage().persistent().get(&balance_key).unwrap_or(0);
        let new_balance = current_balance
            .checked_add(amount)
            .expect("balance overflow");
        env.storage().persistent().set(&balance_key, &new_balance);

        env.events()
            .publish((symbol_short!("deposit"), consumer.clone()), amount);
        Ok(())
    }

    pub fn withdraw(env: Env, consumer: Address, amount: i128) -> Result<(), Error> {
        consumer.require_auth();
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let balance_key = DataKey::Balance(consumer.clone());
        let current_balance: i128 = env.storage().persistent().get(&balance_key).unwrap_or(0);
        if current_balance < amount {
            return Err(Error::InsufficientBalance);
        }

        let new_balance = current_balance - amount;
        env.storage().persistent().set(&balance_key, &new_balance);

        let token_addr = Self::get_token(env.clone());
        let token_client = token::Client::new(&env, &token_addr);
        token_client.transfer(&env.current_contract_address(), &consumer, &amount);

        env.events()
            .publish((symbol_short!("withdraw"), consumer.clone()), amount);
        Ok(())
    }

    pub fn set_allowance(
        env: Env,
        consumer: Address,
        relayer: Address,
        amount: i128,
    ) -> Result<(), Error> {
        consumer.require_auth();
        if amount < 0 {
            return Err(Error::InvalidAmount);
        }

        let allowance_key = DataKey::Allowance(consumer.clone(), relayer.clone());
        env.storage().persistent().set(&allowance_key, &amount);

        let funders_key = DataKey::RelayerFunders(relayer.clone());
        let mut funders: Vec<Address> = env
            .storage()
            .persistent()
            .get(&funders_key)
            .unwrap_or_else(|| Vec::new(&env));

        if amount > 0 {
            if !funders.iter().any(|f| f == consumer) {
                funders.push_back(consumer.clone());
                env.storage().persistent().set(&funders_key, &funders);
            }
        } else {
            // Remove consumer from list of funders if allowance is set to 0
            let mut new_funders = Vec::new(&env);
            for f in funders.iter() {
                if f != consumer {
                    new_funders.push_back(f);
                }
            }
            env.storage().persistent().set(&funders_key, &new_funders);
        }

        env.events().publish(
            (
                symbol_short!("allowance"),
                consumer.clone(),
                relayer.clone(),
            ),
            amount,
        );
        Ok(())
    }

    pub fn get_balance(env: Env, consumer: Address) -> i128 {
        let balance_key = DataKey::Balance(consumer);
        env.storage().persistent().get(&balance_key).unwrap_or(0)
    }

    pub fn get_allowance(env: Env, consumer: Address, relayer: Address) -> i128 {
        let allowance_key = DataKey::Allowance(consumer, relayer);
        env.storage().persistent().get(&allowance_key).unwrap_or(0)
    }

    pub fn reimburse(env: Env, relayer: Address) {
        // Only the authorized oracle can trigger reimbursement
        let oracle = Self::get_oracle(env.clone());
        oracle.require_auth();

        let funders_key = DataKey::RelayerFunders(relayer.clone());
        let funders: Vec<Address> = env
            .storage()
            .persistent()
            .get(&funders_key)
            .unwrap_or_else(|| Vec::new(&env));

        let token_addr = Self::get_token(env.clone());
        let token_client = token::Client::new(&env, &token_addr);

        // Loop through the funders and pay the relayer from their allowances/balances
        for consumer in funders.iter() {
            let allowance_key = DataKey::Allowance(consumer.clone(), relayer.clone());
            let allowance = env.storage().persistent().get(&allowance_key).unwrap_or(0);
            if allowance <= 0 {
                continue;
            }

            let balance_key = DataKey::Balance(consumer.clone());
            let balance = env.storage().persistent().get(&balance_key).unwrap_or(0);
            if balance <= 0 {
                continue;
            }

            // Charge amount is the minimum of the allowance and the consumer's available balance
            let charge = if allowance < balance {
                allowance
            } else {
                balance
            };
            if charge > 0 {
                // Update consumer's balance
                let new_balance = balance - charge;
                env.storage().persistent().set(&balance_key, &new_balance);

                // Transfer tokens to the relayer
                token_client.transfer(&env.current_contract_address(), &relayer, &charge);

                // Publish reimbursement event
                env.events().publish(
                    (
                        symbol_short!("reimburse"),
                        relayer.clone(),
                        consumer.clone(),
                    ),
                    charge,
                );
            }
        }
    }
}
