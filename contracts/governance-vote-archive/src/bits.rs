//! Compact bit-field codecs for governance vote registers, plus the
//! evidence-first gate that decides when a value may be packed at all.
//!
//! Two independent packings live here:
//!
//! 1. [`pack_vote_options`] — boolean vote options are packed one bit per
//!    option into 64-bit storage words (`option 0 -> bit 0`, `option 1 ->
//!    bit 1`, ...). Bits fill a word from the least-significant end and words
//!    are emitted in ascending order, so the encoding is deterministic and
//!    free of padding. The field is a `Vec<u64>` and therefore has **no
//!    arbitrary upper bound**: a register with `n` eligible voters costs
//!    `ceil(n / 64)` words. The earlier draft of this module rejected ballots
//!    longer than 128 options for no reason tied to the governance model, which
//!    silently capped registers at 128 voters; that cap is intentionally gone.
//! 2. [`pack_weights`] — vote weights are packed as 7-bit fields, nine fields
//!    per 64-bit word (9 * 7 = 63 bits used). The repository's governance
//!    weights are whole integers in `1..=100` (`AdminWeight`, see
//!    `contracts/price-oracle/src/auth.rs` and issue #264), which fit a 7-bit
//!    field exactly, so the packing is lossless: no rounding, no clamping and
//!    no change to the precision of the stored value.
//!
//! ## Evidence-first discovery gate
//!
//! Packing a value is only sound while the repository's vote model matches the
//! assumptions the encoding was designed for. [`OBSERVED_VOTE_MODEL`] records
//! those assumptions as data — minimum weight, maximum weight, default weight
//! and whether the model carries sub-unit (fractional) precision — and
//! [`VoteModelEvidence::weights_are_packable`] evaluates them. The gate is
//! consulted for every vote by [`encoding_for`], so a future model with
//! fractional weights (or a wider range) switches the register to the detailed
//! representation instead of silently truncating precision.
//!
//! ## Fallback for unsupported packing
//!
//! When the gate says a weight is not packable, [`encoding_for`] returns
//! [`Encoding::DetailedFallback`] and the caller
//! ([`crate::register::cast_vote`]) records that one vote in the existing
//! detailed register (`ActionVotes` + `AdminWeight`) rather than rounding it,
//! rejecting it, or corrupting the packed field. The tally sums both layouts,
//! so governance semantics are unchanged; see [`crate::register`] for the
//! storage model.

use alloc::vec::Vec;
use soroban_sdk::{Env, Vec as SVec};

use crate::ArchiveError;

/// Bits reserved per packed weight field.
pub const WEIGHT_BITS: u32 = 7;

/// Packed weight fields per 64-bit word (9 * 7 = 63 bits used, 1 bit spare).
pub const WEIGHTS_PER_WORD: u32 = 9;

/// Smallest representable vote weight.
pub const MIN_VOTE_WEIGHT: u32 = 1;

/// Largest representable vote weight (mirrors the repository's `AdminWeight`
/// ceiling from price-oracle issue #264).
pub const MAX_VOTE_WEIGHT: u32 = 100;

/// Weight used when a voter never had one configured. Matches the default the
/// price-oracle `auth` module applies (`unwrap_or(1)`).
pub const DEFAULT_VOTE_WEIGHT: u32 = 1;

/// Largest value a single packed weight field can physically hold (2^7 - 1).
pub const WEIGHT_FIELD_MAX: u32 = (1 << WEIGHT_BITS) - 1;

const WORD_BITS: u32 = 64;
const WEIGHT_MASK: u64 = (1u64 << WEIGHT_BITS) - 1;

// ─── Evidence-first discovery gate ──────────────────────────────────────────

/// Evidence about the governance vote model that packing depends on.
///
/// This is intentionally explicit data rather than a comment: the gate can be
/// asserted in tests and re-evaluated if the repository's model changes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VoteModelEvidence {
    /// Smallest weight the model can assign.
    pub weight_min: u32,
    /// Largest weight the model can assign.
    pub weight_max: u32,
    /// Weight applied when none has been configured.
    pub weight_default: u32,
    /// `true` when the model stores sub-unit / fractional precision.
    pub has_fractional_weights: bool,
}

/// The vote model observed in this repository as of issue #978.
///
/// Evidence: `contracts/price-oracle/src/auth.rs` stores `AdminWeight(Address)`
/// as a whole `u32`, the public `set_admin_weight` entrypoint rejects anything
/// outside `1..=100`, and `_get_admin_weight` defaults to `1`. There is no
/// fractional representation anywhere in the governance path.
pub const OBSERVED_VOTE_MODEL: VoteModelEvidence = VoteModelEvidence {
    weight_min: MIN_VOTE_WEIGHT,
    weight_max: MAX_VOTE_WEIGHT,
    weight_default: DEFAULT_VOTE_WEIGHT,
    has_fractional_weights: false,
};

impl VoteModelEvidence {
    /// Whether every weight the model allows fits a [`WEIGHT_BITS`]-wide field
    /// without loss.
    ///
    /// A fractional model can never satisfy this: a sub-unit value would have
    /// to be rounded, which would change voting power.
    pub const fn weights_are_packable(&self) -> bool {
        !self.has_fractional_weights
            && self.weight_min >= MIN_VOTE_WEIGHT
            && self.weight_max <= WEIGHT_FIELD_MAX
    }
}

/// Which on-chain representation a single vote should be written to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Encoding {
    /// Lossless: the observed weight fits the packed 7-bit field.
    Packed,
    /// Fallback: the weight cannot be packed losslessly, so the vote is
    /// recorded in the detailed register instead of being rounded or rejected.
    DetailedFallback,
}

/// Decide how a vote of `weight` should be stored.
///
/// Returns [`Encoding::Packed`] only when both the observed vote model and the
/// concrete weight permit a lossless 7-bit encoding.
pub fn encoding_for(weight: u32) -> Encoding {
    if OBSERVED_VOTE_MODEL.weights_are_packable() && pack_weight(weight).is_ok() {
        Encoding::Packed
    } else {
        Encoding::DetailedFallback
    }
}

// ─── Word-count helpers ─────────────────────────────────────────────────────

/// Number of words required to hold `len` fields of `per_word` fields each.
#[inline]
fn word_count(len: u32, per_word: u32) -> u32 {
    if len == 0 {
        0
    } else {
        (len - 1) / per_word + 1
    }
}

/// Number of 64-bit words required to hold `len` packed boolean options.
#[inline]
pub fn option_word_count(len: u32) -> u32 {
    word_count(len, WORD_BITS)
}

/// Number of 64-bit words required to hold `len` packed 7-bit weights.
#[inline]
pub fn weight_word_count(len: u32) -> u32 {
    word_count(len, WEIGHTS_PER_WORD)
}

// ─── Boolean option packing ─────────────────────────────────────────────────

/// Pack a sequence of boolean vote options into a deterministic bit-field.
///
/// Option `i` occupies bit `i % 64` of word `i / 64`, little-endian within the
/// word. Identical logical option sets always produce identical bytes, and the
/// encoding grows with the ballot: there is no fixed-width limit to exceed.
pub fn pack_vote_options(env: &Env, options: &[bool]) -> SVec<u64> {
    let mut words = alloc::vec![0u64; option_word_count(options.len() as u32) as usize];
    for (i, opted) in options.iter().enumerate() {
        if *opted {
            words[i / WORD_BITS as usize] |= 1u64 << (i % WORD_BITS as usize);
        }
    }
    words_to_svec(env, &words)
}

/// Decode a packed option set back into its original boolean sequence.
pub fn unpack_vote_options(
    env: &Env,
    words: &SVec<u64>,
    len: u32,
) -> Result<Vec<bool>, ArchiveError> {
    if words.len() < option_word_count(len) {
        return Err(ArchiveError::InvalidPackedLength);
    }
    let mut out = Vec::with_capacity(len as usize);
    for i in 0..len {
        out.push(is_bit_set(words, i));
    }
    let _ = env;
    Ok(out)
}

/// Return `true` when option (or voter) slot `index` is set.
///
/// Reading past the end of the packed field is `false`: an empty register has
/// no votes recorded.
pub fn is_bit_set(words: &SVec<u64>, index: u32) -> bool {
    let word = index / WORD_BITS;
    let shift = index % WORD_BITS;
    match words.get(word) {
        Some(w) => (w >> shift) & 1 == 1,
        None => false,
    }
}

/// Set option (or voter) slot `index`, growing the packed field as needed.
pub fn set_packed_bit(words: &mut SVec<u64>, index: u32) {
    let word = index / WORD_BITS;
    let shift = index % WORD_BITS;
    ensure_words(words, word + 1);
    let current = words.get(word).unwrap_or(0);
    words.set(word, current | (1u64 << shift));
}

/// Clear option (or voter) slot `index`. Clearing a slot that is already clear
/// is a no-op.
pub fn clear_packed_bit(words: &mut SVec<u64>, index: u32) {
    let word = index / WORD_BITS;
    let shift = index % WORD_BITS;
    if let Some(current) = words.get(word) {
        words.set(word, current & !(1u64 << shift));
    }
}

/// Count how many of the first `len` slots are set.
pub fn true_bit_count(words: &SVec<u64>, len: u32) -> u32 {
    let mut count = 0;
    for i in 0..len {
        if is_bit_set(words, i) {
            count += 1;
        }
    }
    count
}

// ─── Vote weight packing ────────────────────────────────────────────────────

/// Validate and represent a single vote weight as its 7-bit field value.
///
/// `1..=100` is representable in 7 bits without loss (`0..=127` is in range),
/// so the returned value equals the input exactly. Weights outside that range
/// are rejected here; callers that must still record such a vote use the
/// detailed fallback (see [`encoding_for`]).
pub fn pack_weight(weight: u32) -> Result<u64, ArchiveError> {
    if weight < MIN_VOTE_WEIGHT || weight > MAX_VOTE_WEIGHT {
        return Err(ArchiveError::InvalidVoteWeight);
    }
    Ok(weight as u64)
}

/// Pack a sequence of vote weights into deterministic 7-bit fields.
///
/// Field `j` occupies bits `(j % 9) * 7 ..= (j % 9) * 7 + 6` of word `j / 9`.
/// Any out-of-range weight rejects the whole batch, so a partially encoded
/// register can never be stored.
pub fn pack_weights(env: &Env, weights: &[u32]) -> Result<SVec<u64>, ArchiveError> {
    let mut words = alloc::vec![0u64; weight_word_count(weights.len() as u32) as usize];
    for (j, weight) in weights.iter().enumerate() {
        let value = pack_weight(*weight)?;
        let word = j as u32 / WEIGHTS_PER_WORD;
        let shift = (j as u32 % WEIGHTS_PER_WORD) * WEIGHT_BITS;
        words[word as usize] |= value << shift;
    }
    Ok(words_to_svec(env, &words))
}

/// Decode `len` packed 7-bit weights back into whole vote weights.
pub fn unpack_weights(env: &Env, words: &SVec<u64>, len: u32) -> Result<Vec<u32>, ArchiveError> {
    let _ = env;
    if words.len() < weight_word_count(len) {
        return Err(ArchiveError::InvalidPackedLength);
    }
    let mut out = Vec::with_capacity(len as usize);
    for j in 0..len {
        out.push(packed_weight(words, j)?);
    }
    Ok(out)
}

/// Read the packed weight stored in field `index`.
pub fn packed_weight(words: &SVec<u64>, index: u32) -> Result<u32, ArchiveError> {
    let word = index / WEIGHTS_PER_WORD;
    let shift = (index % WEIGHTS_PER_WORD) * WEIGHT_BITS;
    let packed = words.get(word).ok_or(ArchiveError::InvalidPackedLength)?;
    Ok(((packed >> shift) & WEIGHT_MASK) as u32)
}

/// Write `weight` into packed weight field `index`, growing the field as needed.
///
/// The target field is cleared before writing so repeated writes are stable
/// and never leak bits from a previous value.
pub fn set_packed_weight(
    words: &mut SVec<u64>,
    index: u32,
    weight: u32,
) -> Result<(), ArchiveError> {
    let value = pack_weight(weight)?;
    let word = index / WEIGHTS_PER_WORD;
    let shift = (index % WEIGHTS_PER_WORD) * WEIGHT_BITS;
    ensure_words(words, word + 1);
    let current = words.get(word).unwrap_or(0);
    let cleared = current & !(WEIGHT_MASK << shift);
    words.set(word, cleared | (value << shift));
    Ok(())
}

/// Sum the 7-bit weight fields for every set slot in `0..len`.
///
/// This is the packed-register tally: it is an exact integer sum of the same
/// whole weights the detailed register stores, so it cannot drift from the
/// pre-optimization result.
pub fn packed_weight_sum(words: &SVec<u64>, weights: &SVec<u64>, len: u32) -> u64 {
    let mut total: u64 = 0;
    for i in 0..len {
        if is_bit_set(words, i) {
            if let Ok(weight) = packed_weight(weights, i) {
                total = total.saturating_add(weight as u64);
            }
        }
    }
    total
}

fn ensure_words(words: &mut SVec<u64>, needed: u32) {
    while words.len() < needed {
        words.push_back(0u64);
    }
}

fn words_to_svec(env: &Env, words: &[u64]) -> SVec<u64> {
    let mut out = SVec::new(env);
    for word in words {
        out.push_back(*word);
    }
    out
}
