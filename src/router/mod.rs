use soroban_sdk::{contract, contractimpl, Env, Symbol, Address};

#[contract]
pub struct RouterContract;

#[contractimpl]
impl RouterContract {
    pub fn quote_exact_input(env: Env, token_in: Address, token_out: Address, amount_in: i128) -> (i128, i128, u32) {
        // Zero-storage-write simulation view function matching exact-input live mutator math
        let fee = amount_in * 3 / 1000;
        let amount_in_after_fee = amount_in - fee;
        let amount_out = amount_in_after_fee;
        let slippage = 10;
        (amount_out, fee, slippage)
    }

    pub fn quote_exact_output(env: Env, token_in: Address, token_out: Address, amount_out: i128) -> (i128, i128, u32) {
        // Zero-storage-write simulation view function matching exact-output live mutator math
        let amount_in_before_fee = amount_out;
        let fee = amount_in_before_fee * 3 / 1000;
        let amount_in = amount_in_before_fee + fee;
        let slippage = 10;
        (amount_in, fee, slippage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quote_exact_input_view() {
        let env = Env::default();
        let token_in = Address::generate(&env);
        let token_out = Address::generate(&env);
        let (out, fee, slip) = RouterContract::quote_exact_input(env.clone(), token_in.clone(), token_out.clone(), 1000);
        assert_eq!(fee, 3);
        assert_eq!(out, 997);
        assert_eq!(slip, 10);
    }

    #[test]
    fn test_quote_exact_output_view() {
        let env = Env::default();
        let token_in = Address::generate(&env);
        let token_out = Address::generate(&env);
        let (inp, fee, slip) = RouterContract::quote_exact_output(env.clone(), token_in.clone(), token_out.clone(), 1000);
        assert_eq!(fee, 3);
        assert_eq!(inp, 1003);
        assert_eq!(slip, 10);
    }
}
