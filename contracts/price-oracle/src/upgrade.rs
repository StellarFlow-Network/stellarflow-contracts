//! Upgrade helpers for the StellarFlow Oracle contract.
//!
//! This module provides utilities for the governance-gated WASM bytecode upgrade
//! path. The primary entry point is [`parse_wasm_hash_from_hex`], which decodes
//! a 64-character lowercase hex string (as stored in a [`ProposedAction::data`]
//! field) into the [`soroban_sdk::BytesN<32>`] expected by
//! `env.deployer().update_current_contract_wasm()`.

use soroban_sdk::{BytesN, Env, String as SorobanString};

use crate::Error;

/// Decode a 64-character lowercase hex string into a `BytesN<32>` WASM hash.
///
/// The `data` field of a [`crate::types::ProposedAction`] with
/// `action_type = AdminAction::Upgrade` must contain exactly 64 hex characters
/// representing the 32-byte SHA-256 hash of the new WASM bytecode.
///
/// # Errors
/// Returns [`Error::InvalidActionType`] when:
/// - The string length is not exactly 64 characters.
/// - Any character is not a valid lowercase or uppercase hex digit (`0-9`, `a-f`, `A-F`).
///
/// # Example (off-chain)
/// ```text
/// stellar contract upload --wasm target/wasm32-unknown-unknown/release/price_oracle.wasm
/// # → prints the 64-char hex hash; pass that string as `data` when proposing the action
/// ```
pub fn parse_wasm_hash_from_hex(env: &Env, hex: &SorobanString) -> Result<BytesN<32>, Error> {
    // Soroban `String` does not implement `Iterator` or index access directly.
    // We copy the bytes into a fixed-size stack buffer via `copy_into_slice`.
    const HEX_LEN: usize = 64; // 32 bytes × 2 hex digits
    const BYTE_LEN: usize = 32;

    if hex.len() as usize != HEX_LEN {
        return Err(Error::InvalidActionType);
    }

    // Copy the Soroban string bytes into a stack buffer.
    let mut hex_buf = [0u8; HEX_LEN];
    hex.copy_into_slice(&mut hex_buf);

    // Decode each pair of hex digits into a byte.
    let mut out = [0u8; BYTE_LEN];
    for i in 0..BYTE_LEN {
        let hi = hex_nibble(hex_buf[i * 2])?;
        let lo = hex_nibble(hex_buf[i * 2 + 1])?;
        out[i] = (hi << 4) | lo;
    }

    Ok(BytesN::from_array(env, &out))
}

/// Convert a single ASCII hex character to its nibble value (0–15).
///
/// Accepts `0-9`, `a-f`, and `A-F`. Returns [`Error::InvalidActionType`] for
/// any other byte value.
#[inline]
fn hex_nibble(c: u8) -> Result<u8, Error> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(Error::InvalidActionType),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Unit tests
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{contract, contractimpl, Env};

    #[contract]
    struct TestContract;

    #[contractimpl]
    impl TestContract {}

    fn make_env() -> (Env, soroban_sdk::Address) {
        let env = Env::default();
        let id = env.register(TestContract, ());
        (env, id)
    }

    #[test]
    fn test_parse_valid_lowercase_hex() {
        let (env, id) = make_env();
        env.as_contract(&id, || {
            // 64 lowercase hex chars → 32 zero bytes
            let hex = SorobanString::from_str(
                &env,
                "0000000000000000000000000000000000000000000000000000000000000000",
            );
            let result = parse_wasm_hash_from_hex(&env, &hex);
            assert!(result.is_ok());
            let bytes = result.unwrap();
            assert_eq!(bytes, BytesN::from_array(&env, &[0u8; 32]));
        });
    }

    #[test]
    fn test_parse_valid_mixed_case_hex() {
        let (env, id) = make_env();
        env.as_contract(&id, || {
            // All 0xff bytes encoded as "ff" repeated
            let hex = SorobanString::from_str(
                &env,
                "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            );
            let result = parse_wasm_hash_from_hex(&env, &hex);
            assert!(result.is_ok());
            assert_eq!(result.unwrap(), BytesN::from_array(&env, &[0xffu8; 32]));
        });
    }

    #[test]
    fn test_parse_valid_uppercase_hex() {
        let (env, id) = make_env();
        env.as_contract(&id, || {
            let hex = SorobanString::from_str(
                &env,
                "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF",
            );
            let result = parse_wasm_hash_from_hex(&env, &hex);
            assert!(result.is_ok());
            assert_eq!(result.unwrap(), BytesN::from_array(&env, &[0xffu8; 32]));
        });
    }

    #[test]
    fn test_parse_known_pattern() {
        let (env, id) = make_env();
        env.as_contract(&id, || {
            // "0102...1f20" — bytes 1 through 32
            let hex = SorobanString::from_str(
                &env,
                "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20",
            );
            let result = parse_wasm_hash_from_hex(&env, &hex);
            assert!(result.is_ok());
            let expected: [u8; 32] = [
                0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
                0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a,
                0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x20,
            ];
            assert_eq!(result.unwrap(), BytesN::from_array(&env, &expected));
        });
    }

    #[test]
    fn test_parse_too_short_returns_error() {
        let (env, id) = make_env();
        env.as_contract(&id, || {
            let hex = SorobanString::from_str(&env, "deadbeef");
            let result = parse_wasm_hash_from_hex(&env, &hex);
            assert_eq!(result, Err(Error::InvalidActionType));
        });
    }

    #[test]
    fn test_parse_too_long_returns_error() {
        let (env, id) = make_env();
        env.as_contract(&id, || {
            // 66 chars — one byte too long
            let hex = SorobanString::from_str(
                &env,
                "000000000000000000000000000000000000000000000000000000000000000000",
            );
            let result = parse_wasm_hash_from_hex(&env, &hex);
            assert_eq!(result, Err(Error::InvalidActionType));
        });
    }

    #[test]
    fn test_parse_invalid_char_returns_error() {
        let (env, id) = make_env();
        env.as_contract(&id, || {
            // 'g' is not a valid hex digit
            let hex = SorobanString::from_str(
                &env,
                "gg00000000000000000000000000000000000000000000000000000000000000",
            );
            let result = parse_wasm_hash_from_hex(&env, &hex);
            assert_eq!(result, Err(Error::InvalidActionType));
        });
    }
}
