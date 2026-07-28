use soroban_sdk::{Env, String};

use crate::ContractError;

pub const MAX_INPUT_STRING_BYTES: u32 = 256;

pub fn sanitize_string_input(_env: &Env, value: &String) -> Result<String, ContractError> {
    unimplemented!("string sanitization helper will be implemented")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_strings_longer_than_256_bytes() {
        let env = Env::default();
        let input = String::from_str(&env, &"a".repeat(257));

        assert_eq!(sanitize_string_input(&env, &input), Err(ContractError::InvalidInput));
    }

    #[test]
    fn accepts_strings_up_to_256_bytes() {
        let env = Env::default();
        let input = String::from_str(&env, &"a".repeat(256));

        assert_eq!(sanitize_string_input(&env, &input), Ok(input.clone()));
    }
}
