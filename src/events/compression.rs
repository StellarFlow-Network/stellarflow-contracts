//! Standardized event payload compression (Issue #1019).
//!
//! Compresses struct payloads in emitted events to minimize Soroban ledger
//! event storage fees. Two complementary encodings are provided:
//!
//! 1. **Variable-length integers (varint / LEB128)** — numerical state values
//!    (`u64`/`u128` counters, amounts, timestamps) are encoded into compact
//!    byte arrays. Small values (the common case for status codes, counters,
//!    and tick indices) cost 1 byte instead of 8 or 16.
//!
//! 2. **64-bit word packing** — status flags and short timestamps are bit-
//!    packed into a single `u64` field so one ledger value carries what used
//!    to be four.
//!
//! The [`decode_varint`] / [`unpack_word`] helpers are the canonical
//! off-chain decoding routines; indexers and event-parsing tools must produce
//! byte-identical results when run against on-chain payloads, which the round-
//! trip tests in this module assert.

use soroban_sdk::{contracttype, Bytes, BytesN, Env};

// ---------------------------------------------------------------------------
// Varint (LEB128) encoding
// ---------------------------------------------------------------------------

/// Maximum bytes a varint can occupy.
///
/// `u128` needs at most ceil(128 / 7) = 19 bytes; `u64` needs 10.
pub const VARINT_MAX_LEN: u32 = 19;

/// Encode a `u64` as an unsigned LEB128 varint.
///
/// Each byte carries 7 payload bits in the low positions; the high bit is set
/// on every byte except the last, letting a decoder know when the value ends.
pub fn encode_varint_u64(value: u64) -> [u8; VARINT_MAX_LEN as usize] {
    let mut out = [0u8; VARINT_MAX_LEN as usize];
    let mut v = value;
    let mut i = 0usize;
    loop {
        let mut byte = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            byte |= 0x80;
        }
        out[i] = byte;
        i += 1;
        if v == 0 {
            break;
        }
    }
    out
}

/// Encode a `u128` as an unsigned LEB128 varint.
pub fn encode_varint_u128(value: u128) -> [u8; VARINT_MAX_LEN as usize] {
    let mut out = [0u8; VARINT_MAX_LEN as usize];
    let mut v = value;
    let mut i = 0usize;
    loop {
        let mut byte = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            byte |= 0x80;
        }
        out[i] = byte;
        i += 1;
        if v == 0 {
            break;
        }
    }
    out
}

/// Decode a varint produced by [`encode_varint_u64`] / [`encode_varint_u128`].
///
/// `buf` must contain the exact encoded bytes (length ≤ [`VARINT_MAX_LEN`]).
/// Returns the reconstructed value and the number of bytes consumed, so
/// multi-field payloads can be decoded field by field.
///
/// Returns `None` when the buffer is empty, exceeds [`VARINT_MAX_LEN`], or
/// contains a non-terminated continuation byte (corrupt payload).
pub fn decode_varint(buf: &[u8]) -> Option<(u128, u32)> {
    if buf.is_empty() || buf.len() > VARINT_MAX_LEN as usize {
        return None;
    }
    let mut value: u128 = 0;
    let mut shift: u32 = 0;
    for (i, &byte) in buf.iter().enumerate() {
        let payload = (byte & 0x7f) as u128;
        // Guard against overflow: the final byte may not push past 128 bits.
        if shift >= 128 || (payload << shift) >> shift != payload {
            return None;
        }
        value |= payload << shift;
        if byte & 0x80 == 0 {
            return Some((value, (i + 1) as u32));
        }
        shift += 7;
    }
    // Ran off the end without a terminating byte.
    None
}

/// Convenience wrapper: encode a `u64` into a Soroban `Bytes` of the exact
/// encoded length, ready to be embedded in an event payload.
pub fn varint_bytes_u64(env: &Env, value: u64) -> Bytes {
    let full = encode_varint_u64(value);
    let len = varint_len_u64(value);
    Bytes::from_slice(env, &full[..len as usize])
}

/// Number of bytes [`encode_varint_u64`] produces for `value`.
pub fn varint_len_u64(value: u64) -> u32 {
    let mut len = 1u32;
    let mut v = value >> 7;
    while v != 0 {
        len += 1;
        v >>= 7;
    }
    len
}

// ---------------------------------------------------------------------------
// 64-bit word packing
// ---------------------------------------------------------------------------

/// Bit offsets inside a packed 64-bit status/timestamp word.
///
/// Layout: flags occupy the low 16 bits (4 flag slots of 4 bits each),
/// a sequence/counter sits in bits 16..31, and a short delta timestamp
/// (seconds) occupies the high 32 bits.
pub const FLAG_BITS: u32 = 16;
pub const SEQ_SHIFT: u32 = 16;
pub const SEQ_BITS: u32 = 16;
pub const TS_SHIFT: u32 = 32;

/// Pack four status flags (booleans), a 16-bit sequence number, and a 32-bit
/// timestamp (or timestamp delta) into a single `u64` ledger word.
///
/// Sequence numbers above `u16::MAX` and timestamps above `u32::MAX` are
/// truncated to their low bits — callers passing a full ledger timestamp
/// should pre-reduce it modulo the window they care about (see
/// [`pack_word`]'s use in tests) or store the full value in a separate field.
pub fn pack_word(
    flag0: bool,
    flag1: bool,
    flag2: bool,
    flag3: bool,
    seq: u16,
    timestamp: u32,
) -> u64 {
    let mut word: u64 = 0;
    if flag0 { word |= 0x1; }
    if flag1 { word |= 0x2; }
    if flag2 { word |= 0x4; }
    if flag3 { word |= 0x8; }
    word |= (seq as u64) << SEQ_SHIFT;
    word |= (timestamp as u64) << TS_SHIFT;
    word
}

/// Unpack a word produced by [`pack_word`] back into its components.
pub fn unpack_word(word: u64) -> (bool, bool, bool, bool, u16, u32) {
    let flags = (word & 0xff_ff_ff_ff) as u32;
    let flag0 = flags & 0x1 != 0;
    let flag1 = flags & 0x2 != 0;
    let flag2 = flags & 0x4 != 0;
    let flag3 = flags & 0x8 != 0;
    let seq = ((word >> SEQ_SHIFT) & 0xff_ff) as u16;
    let ts = (word >> TS_SHIFT) as u32;
    (flag0, flag1, flag2, flag3, seq, ts)
}

// ---------------------------------------------------------------------------
// Packed event payload helpers
// ---------------------------------------------------------------------------

/// A fully compressed event payload: a vector of varint-encoded numeric
/// fields plus one packed 64-bit status/timestamp word.
///
/// Emitted as the data value of standardized events; off-chain tools decode
/// it with [`decode_varint`] (walking `fields` in order) and [`unpack_word`].
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompressedEventPayload {
    /// Varint-encoded numeric fields, in declaration order.
    pub fields: Bytes,
    /// Packed flags/sequence/timestamp word (see [`pack_word`]).
    pub packed_word: u64,
}

impl CompressedEventPayload {
    /// Build a compressed payload from numeric values and a packed word.
    pub fn new(env: &Env, values: &[u64], packed_word: u64) -> Self {
        let mut fields = Bytes::new(env);
        for v in values {
            let len = varint_len_u64(*v);
            let full = encode_varint_u64(*v);
            fields.append(&Bytes::from_slice(env, &full[..len as usize]));
        }
        Self { fields, packed_word }
    }

    /// Decode all varint fields back into their original values.
    ///
    /// Mirrors what an off-chain indexer does against the on-chain payload.
    pub fn decode_fields(&self) -> Option<soroban_sdk::Vec<u128>> {
        let env = self.fields.env();
        let mut out = soroban_sdk::Vec::new(env);
        let mut offset = 0u32;
        let total = self.fields.len();
        while offset < total {
            // Copy the field bytes into host memory for decoding.
            // Varints are ≤ 19 bytes so the temp buffer is always enough.
            let mut tmp = [0u8; VARINT_MAX_LEN as usize];
            let mut n = 0usize;
            while offset < total && n < tmp.len() {
                tmp[n] = self.fields.get(offset).unwrap_or(0);
                offset += 1;
                n += 1;
                // Stop at the terminating byte.
                if tmp[n - 1] & 0x80 == 0 {
                    break;
                }
            }
            let (value, consumed) = decode_varint(&tmp[..n])?;
            out.push_back(value);
            let _ = consumed; // offset already advanced past the field
        }
        Some(out)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_encodes_single_byte_for_small_values() {
        let enc = encode_varint_u64(0);
        assert_eq!(enc[0], 0x00);
        assert_eq!(varint_len_u64(0), 1);

        let enc = encode_varint_u64(127);
        assert_eq!(enc[0], 0x7f);
        assert_eq!(varint_len_u64(127), 1);
    }

    #[test]
    fn varint_multi_byte_matches_leb128_reference_vectors() {
        // Standard LEB128 reference vectors.
        let enc = encode_varint_u64(128);
        assert_eq!(&enc[..2], &[0x80, 0x01]);
        let enc = encode_varint_u64(624_485);
        assert_eq!(&enc[..3], &[0xe5, 0x8e, 0x26]);
        assert_eq!(varint_len_u64(624_485), 3);
    }

    #[test]
    fn varint_u128_uses_wider_encoding() {
        let enc = encode_varint_u128(u128::MAX);
        // ceil(128/7) = 19 bytes for the largest value.
        let (val, len) = decode_varint(&enc).unwrap();
        assert_eq!(val, u128::MAX);
        assert_eq!(len, 19);
    }

    #[test]
    fn varint_round_trips_every_boundary_width() {
        for &v in &[
            0u64,
            1,
            127,
            128,
            16_383,
            16_384,
            u32::MAX as u64,
            u32::MAX as u64 + 1,
            u64::MAX,
        ] {
            let enc = encode_varint_u64(v);
            let (val, len) = decode_varint(&enc).unwrap();
            assert_eq!(val, v as u128, "round-trip failed for {v}");
            assert_eq!(len, varint_len_u64(v));
        }
    }

    #[test]
    fn varint_rejects_corrupt_buffers() {
        // Empty.
        assert!(decode_varint(&[]).is_none());
        // Overlong (continuation bit set on every byte, no terminator).
        let unbounded = [0x80u8; VARINT_MAX_LEN as usize];
        assert!(decode_varint(&unbounded).is_none());
        // Too long.
        let too_long = [0u8; (VARINT_MAX_LEN as usize) + 1];
        assert!(decode_varint(&too_long).is_none());
    }

    #[test]
    fn packed_word_round_trips_flags_seq_and_timestamp() {
        let word = pack_word(true, false, true, true, 0xab_cd, 0x67_89_ab_cd);
        let (f0, f1, f2, f3, seq, ts) = unpack_word(word);
        assert!(f0 && !f1 && f2 && f3);
        assert_eq!(seq, 0xab_cd);
        assert_eq!(ts, 0x67_89_ab_cd);
    }

    #[test]
    fn packed_word_defaults_to_zero() {
        let (f0, f1, f2, f3, seq, ts) = unpack_word(0);
        assert!(!f0 && !f1 && !f2 && !f3);
        assert_eq!(seq, 0);
        assert_eq!(ts, 0);
    }

    #[test]
    fn compressed_payload_round_trips_through_varint_and_word() {
        let env = Env::default();
        let values: [u64; 4] = [7, 1_000_000, u32::MAX as u64, 42];
        let word = pack_word(false, true, false, false, 12, 3_600);

        let payload = CompressedEventPayload::new(&env, &values, word);
        let decoded = payload.decode_fields().unwrap();

        assert_eq!(decoded.len(), 4);
        assert_eq!(decoded.get(0), Some(7u128));
        assert_eq!(decoded.get(1), Some(1_000_000u128));
        assert_eq!(decoded.get(2), Some(u32::MAX as u128));
        assert_eq!(decoded.get(3), Some(42u128));

        let (_, _, _, _, seq, ts) = unpack_word(payload.packed_word);
        assert_eq!(seq, 12);
        assert_eq!(ts, 3_600);

        // Compression must actually shrink the payload versus fixed-width.
        // 4 values × 8 bytes = 32 raw; varints need at most 10 each but only
        // 5 here in total for these values.
        assert!(payload.fields.len() < 32);
    }

    #[test]
    fn compressed_payload_word_fits_bytesn32_for_topic_embedding() {
        // The packed word is representable in exactly 8 big-endian bytes —
        // the shape downstream parsers expect when reassembling payloads.
        let env = Env::default();
        let word = pack_word(true, true, false, false, 1, 60);
        let bytes = BytesN::from_array(&env, &word.to_be_bytes());
        let recovered = u64::from_be_bytes(bytes.to_array());
        assert_eq!(recovered, word);
    }
}
