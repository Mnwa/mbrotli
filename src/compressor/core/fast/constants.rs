//! Normative constants shared by the quality 0 and quality 1 fast encoders.
//!
//! Every value here is fixed by Google's Brotli reference encoder
//! (`google/brotli` v1.2.0, commit `028fb5a`, MIT licence) and by RFC 7932.
//! Changing any of them changes the emitted bitstream, so they are asserted in
//! [`tests`] rather than being treated as tunables.

/// Multiplier used by both fast hash functions (`c/enc/hash_base.h`).
pub(crate) const HASH_MUL32: u32 = 0x1E35_A7BD;

/// Window size, in bits, the fast path always advertises in the stream header.
pub(crate) const WINDOW_BITS_FAST: usize = 18;

/// Bytes of the sliding window reserved as a margin (`BROTLI_WINDOW_GAP`).
pub(crate) const WINDOW_GAP: usize = 16;

/// Largest backward distance the fast path may emit.
pub(crate) const MAX_BACKWARD_DISTANCE: usize = (1 << WINDOW_BITS_FAST) - WINDOW_GAP;

/// Largest hash table quality 0 may use, in entries.
pub(crate) const Q0_MAX_HASH_ENTRIES: usize = 1 << 15;

/// Largest hash table quality 1 may use, in entries.
pub(crate) const Q1_MAX_HASH_ENTRIES: usize = 1 << 17;

/// Smallest hash table either quality may use, in entries.
pub(crate) const MIN_HASH_ENTRIES: usize = 256;

/// Fixed match length quality 0 probes for.
pub(crate) const Q0_MIN_MATCH: usize = 5;

/// Fixed match length quality 1 probes for with table bits `8..=15`.
pub(crate) const Q1_MIN_MATCH_SMALL: usize = 4;

/// Fixed match length quality 1 probes for with table bits `16..=17`.
pub(crate) const Q1_MIN_MATCH_LARGE: usize = 6;

/// Size of the first meta-block quality 0 opens.
pub(crate) const Q0_FIRST_BLOCK_SIZE: usize = 3 << 15;

/// Chunk quality 0 appends when it decides to extend a meta-block.
pub(crate) const Q0_MERGE_BLOCK_SIZE: usize = 1 << 16;

/// Largest meta-block quality 0 may grow to by merging.
pub(crate) const Q0_MAX_MERGED_BLOCK_SIZE: usize = 1 << 20;

/// Block size quality 1 processes in one two-pass round.
pub(crate) const Q1_BLOCK_SIZE: usize = 1 << 17;

/// Sampling stride of the quality 0 literal histogram for long inputs.
pub(crate) const LITERAL_SAMPLE_RATE: usize = 29;

/// Sampling stride of the block-entropy estimates used by both qualities.
pub(crate) const BLOCK_SAMPLE_RATE: usize = 43;

/// Quality 0 uncompressed-mode threshold, in millibytes per literal.
pub(crate) const Q0_MIN_RATIO: usize = 980;

/// Quality 1 compressibility threshold, as a fraction of eight bits per byte.
pub(crate) const Q1_MIN_RATIO: f64 = 0.98;

/// Largest insert length quality 0 encodes through the short insert path.
pub(crate) const SHORT_INSERT_LIMIT: usize = 6210;

/// Largest insert length encoded with fourteen extra bits.
pub(crate) const LONG_INSERT_LIMIT: usize = 22594;

/// Number of symbols in the full Brotli command alphabet.
pub(crate) const NUM_COMMAND_SYMBOLS: usize = 704;

/// Number of symbols in the Brotli literal alphabet.
pub(crate) const NUM_LITERAL_SYMBOLS: usize = 256;

/// Number of symbols in the Brotli code-length alphabet.
pub(crate) const CODE_LENGTH_CODES: usize = 18;

/// Code-length symbol that repeats the previous non-zero length.
pub(crate) const REPEAT_PREVIOUS_CODE_LENGTH: usize = 16;

/// Code-length symbol that repeats a run of zero lengths.
pub(crate) const REPEAT_ZERO_CODE_LENGTH: usize = 17;

/// Code length assumed to precede the first emitted length.
pub(crate) const INITIAL_REPEATED_CODE_LENGTH: u8 = 8;

/// Bytes of slack a fast-path output buffer needs beyond `2 * len + 503`.
///
/// The bit writer materialises up to eight bytes around the current bit
/// position, so the scratch buffer keeps one machine word of headroom.
pub(crate) const OUTPUT_SLACK: usize = 8;

/// Constant term of the per-block output reservation, `2 * len + 503`.
pub(crate) const OUTPUT_RESERVE_CONST: usize = 503;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_the_reference_encoder() {
        assert_eq!(HASH_MUL32, 0x1E35_A7BD);
        assert_eq!(WINDOW_BITS_FAST, 18);
        assert_eq!(WINDOW_GAP, 16);
        assert_eq!(MAX_BACKWARD_DISTANCE, 262_128);
        assert_eq!(Q0_MAX_HASH_ENTRIES, 32_768);
        assert_eq!(Q1_MAX_HASH_ENTRIES, 131_072);
        assert_eq!(Q0_FIRST_BLOCK_SIZE, 98_304);
        assert_eq!(Q0_MERGE_BLOCK_SIZE, 65_536);
        assert_eq!(Q0_MAX_MERGED_BLOCK_SIZE, 1_048_576);
        assert_eq!(Q1_BLOCK_SIZE, 131_072);
        assert_eq!(LITERAL_SAMPLE_RATE, 29);
        assert_eq!(BLOCK_SAMPLE_RATE, 43);
        assert_eq!(SHORT_INSERT_LIMIT, 6_210);
        assert_eq!(LONG_INSERT_LIMIT, 22_594);
        assert_eq!(NUM_COMMAND_SYMBOLS, 704);
        assert_eq!(CODE_LENGTH_CODES, 18);
    }
}
