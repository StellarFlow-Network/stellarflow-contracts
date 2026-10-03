//! Structural validation for uploaded Groth16 proving keys on BN254.
//!
//! The payload schema is the canonical uncompressed Groth16 proving-key
//! layout: fixed alpha/beta/delta elements and the A, B, H, and L query vectors.
//! G1 coordinates use 32-byte big-endian field elements; G2 coordinates use
//! two 32-byte field elements. The circuit's public-input and constraint
//! counts determine the exact payload size.

use soroban_sdk::{contracttype, Bytes, BytesN};

use crate::ContractError;

/// Uncompressed BN254 G1 point size: two 32-byte coordinates.
const G1_POINT_BYTES: u32 = 64;
/// Uncompressed BN254 G2 point size: four 32-byte coordinates.
const G2_POINT_BYTES: u32 = 128;

/// A proving-key upload with the curve generator claimed by the producer.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UploadedProvingKey {
    /// Claimed BN254 G1 generator X coordinate, big-endian.
    pub generator_x: BytesN<32>,
    /// Claimed BN254 G1 generator Y coordinate, big-endian.
    pub generator_y: BytesN<32>,
    /// Serialized Groth16 proving key in the documented schema.
    pub payload: Bytes,
}

/// Circuit dimensions used to calculate the exact canonical key payload size.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProvingKeySchema {
    /// Number of public inputs to the circuit.
    pub public_input_count: u32,
    /// Number of R1CS variables, including the constant-one variable.
    pub variable_count: u32,
    /// Number of constraints in the circuit's R1CS.
    pub constraint_count: u32,
}

/// Return the expected byte length of the canonical uncompressed Groth16
/// proving-key payload for `schema`.
///
/// The key contains alpha/beta/delta G1, beta/delta G2, plus A, B-G1, B-G2,
/// H, and L query vectors. Gamma and IC are verification-key elements and are
/// deliberately excluded. Malformed dimensions and overflow fail.
pub fn expected_payload_len(schema: &ProvingKeySchema) -> Result<u32, ContractError> {
    let public_inputs_plus_constant = schema
        .public_input_count
        .checked_add(1)
        .ok_or(ContractError::InvalidProvingKey)?;
    if schema.variable_count < public_inputs_plus_constant || schema.constraint_count == 0 {
        return Err(ContractError::InvalidProvingKey);
    }

    // Fixed proving-key points: alpha/beta/delta G1, beta/delta G2.
    let fixed_bytes = 3 * G1_POINT_BYTES + 2 * G2_POINT_BYTES;
    let a_query = schema.variable_count.checked_mul(G1_POINT_BYTES);
    let b_g1_query = schema.variable_count.checked_mul(G1_POINT_BYTES);
    let b_g2_query = schema.variable_count.checked_mul(G2_POINT_BYTES);
    let h_query = schema
        .constraint_count
        .checked_sub(1)
        .and_then(|count| count.checked_mul(G1_POINT_BYTES));
    let l_query = schema
        .variable_count
        .checked_sub(public_inputs_plus_constant)
        .and_then(|count| count.checked_mul(G1_POINT_BYTES));

    [a_query, b_g1_query, b_g2_query, h_query, l_query]
        .into_iter()
        .try_fold(fixed_bytes, |total, item| {
            total
                .checked_add(item.ok_or(ContractError::InvalidProvingKey)?)
                .ok_or(ContractError::InvalidProvingKey)
        })
}

/// Validate the claimed BN254 G1 generator and exact payload schema length.
///
/// BN254's standard G1 generator is `(1, 2)` in the base field. Any malformed
/// dimensions, generator mismatch, or payload-length mismatch returns
/// `ContractError::InvalidProvingKey`.
pub fn validate_proving_key(
    key: &UploadedProvingKey,
    schema: &ProvingKeySchema,
) -> Result<(), ContractError> {
    let mut expected_x = [0u8; 32];
    expected_x[31] = 1;
    let mut expected_y = [0u8; 32];
    expected_y[31] = 2;

    if key.generator_x.to_array() != expected_x || key.generator_y.to_array() != expected_y {
        return Err(ContractError::InvalidProvingKey);
    }

    if key.payload.len() != expected_payload_len(schema)? {
        return Err(ContractError::InvalidProvingKey);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::Env;

    fn valid_key(env: &Env, schema: &ProvingKeySchema) -> UploadedProvingKey {
        let mut generator_x = [0u8; 32];
        generator_x[31] = 1;
        let mut generator_y = [0u8; 32];
        generator_y[31] = 2;
        let mut payload = Bytes::new(env);
        for _ in 0..expected_payload_len(schema).unwrap() {
            payload.push_back(0xA5);
        }

        UploadedProvingKey {
            generator_x: BytesN::from_array(env, &generator_x),
            generator_y: BytesN::from_array(env, &generator_y),
            payload,
        }
    }

    fn sample_schema() -> ProvingKeySchema {
        ProvingKeySchema {
            public_input_count: 1,
            variable_count: 3,
            constraint_count: 3,
        }
    }

    #[test]
    fn computes_exact_groth16_payload_length() {
        assert_eq!(expected_payload_len(&sample_schema()), Ok(1_408));
    }

    #[test]
    fn accepts_standard_bn254_generator_and_exact_payload() {
        let env = Env::default();
        let schema = sample_schema();
        assert!(validate_proving_key(&valid_key(&env, &schema), &schema).is_ok());
    }

    #[test]
    fn rejects_nonstandard_generator_parameters() {
        let env = Env::default();
        let schema = sample_schema();
        let mut key = valid_key(&env, &schema);
        let mut wrong_y = [0u8; 32];
        wrong_y[31] = 3;
        key.generator_y = BytesN::from_array(&env, &wrong_y);

        assert_eq!(
            validate_proving_key(&key, &schema),
            Err(ContractError::InvalidProvingKey)
        );
    }

    #[test]
    fn rejects_payload_length_mismatch() {
        let env = Env::default();
        let schema = sample_schema();
        let mut key = valid_key(&env, &schema);
        key.payload.push_back(0);

        assert_eq!(
            validate_proving_key(&key, &schema),
            Err(ContractError::InvalidProvingKey)
        );
    }

    #[test]
    fn rejects_impossible_schema_dimensions() {
        let schema = ProvingKeySchema {
            public_input_count: 4,
            variable_count: 4,
            constraint_count: 1,
        };
        assert_eq!(
            expected_payload_len(&schema),
            Err(ContractError::InvalidProvingKey)
        );
    }
}
