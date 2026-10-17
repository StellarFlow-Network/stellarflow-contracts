//! Cryptographic signature verification and malleability protections (Issue #1226).
//!
//! Secp256k1 curve order s-value constraint:
//! To prevent signature malleability, any valid signature must enforce that its `s` parameter
//! lies in the lower half of the curve order:
//! s <= 0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0.

use crate::ContractError;

/// Half the secp256k1 group order n:
/// n = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141
/// n / 2 = 0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0
pub const SECP256K1_N_HALF_ORDER: [u8; 32] = [
    0x7F, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0x5D, 0x57, 0x6E, 0x73, 0x57, 0xA4, 0x50, 0x1D,
    0xDF, 0xE9, 0x2F, 0x46, 0x68, 0x1B, 0x20, 0xA0,
];

/// Validates that an `s` parameter does not exceed `SECP256K1_N_HALF_ORDER`.
///
/// Returns `Ok(())` if non-malleable, or `Err(ContractError::MalleableSignature)` if malleable.
pub fn verify_signature_s_bounds(s: &[u8; 32]) -> Result<(), ContractError> {
    for i in 0..32 {
        if s[i] < SECP256K1_N_HALF_ORDER[i] {
            return Ok(());
        }
        if s[i] > SECP256K1_N_HALF_ORDER[i] {
            return Err(ContractError::MalleableSignature);
        }
    }
    // Exactly equal to n/2 is valid
    Ok(())
}

/// Validates an ECDSA signature's recovery byte and s-bounds.
pub fn validate_secp256k1_signature(v: u8, _r: &[u8; 32], s: &[u8; 32]) -> Result<(), ContractError> {
    if v != 27 && v != 28 {
        return Err(ContractError::InvalidSaltSignature);
    }
    verify_signature_s_bounds(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_canonical_s_passes() {
        let mut s = [0u8; 32];
        s[31] = 42;
        assert!(verify_signature_s_bounds(&s).is_ok());
    }

    #[test]
    fn test_exact_half_order_passes() {
        assert!(verify_signature_s_bounds(&SECP256K1_N_HALF_ORDER).is_ok());
    }

    #[test]
    fn test_malleable_s_is_rejected() {
        let mut malleable_s = SECP256K1_N_HALF_ORDER;
        malleable_s[31] += 1;
        assert_eq!(
            verify_signature_s_bounds(&malleable_s),
            Err(ContractError::MalleableSignature)
        );
    }
}
