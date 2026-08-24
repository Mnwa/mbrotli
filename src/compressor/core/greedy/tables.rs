//! Constant tables translated from Google's Brotli reference encoder.
//!
//! Source: <https://github.com/google/brotli/tree/028fb5a> (v1.2.0), files
//! `c/common/context.c`, `c/common/constants.c`, `c/enc/command.c` and
//! `c/enc/encode.c`. Distributed by Google under the MIT licence; see
//! `brotli-ffi/vendor/brotli/LICENSE`.

/// Context lookup table for `CONTEXT_UTF8`, the only mode these qualities use.
///
/// The context of a literal is `LUT[p1] | LUT[256 + p2]`, where `p1` and `p2`
/// are the two preceding bytes. `ChooseContextMode` only returns
/// `CONTEXT_SIGNED` at quality ten and above, so the other three tables of
/// `_kBrotliContextLookupTable` are unreachable from the greedy qualities.
pub(crate) const CONTEXT_LUT_UTF8: [u8; 512] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 4, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    8, 12, 16, 12, 12, 20, 12, 16, 24, 28, 12, 12, 32, 12, 36, 12, 44, 44, 44, 44, 44, 44, 44, 44,
    44, 44, 32, 32, 24, 40, 28, 12, 12, 48, 52, 52, 52, 48, 52, 52, 52, 48, 52, 52, 52, 52, 52, 48,
    52, 52, 52, 52, 52, 48, 52, 52, 52, 52, 52, 24, 12, 28, 12, 12, 12, 56, 60, 60, 60, 56, 60, 60,
    60, 56, 60, 60, 60, 60, 60, 56, 60, 60, 60, 60, 60, 56, 60, 60, 60, 60, 60, 24, 12, 28, 12, 0,
    0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1,
    0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1,
    2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3,
    2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1,
    1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1,
    1, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 1, 1, 1, 1, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
];

/// Numeric value of `CONTEXT_UTF8`, as written into the meta-block header.
pub(crate) const CONTEXT_MODE_UTF8: u64 = 2;

/// Number of insert-and-copy length codes (`BROTLI_NUM_INS_COPY_CODES`).
pub(crate) const NUM_INS_COPY_CODES: usize = 24;

/// Insert-length code bases (`kBrotliInsBase`).
pub(crate) const INS_BASE: [u32; NUM_INS_COPY_CODES] = [
    0, 1, 2, 3, 4, 5, 6, 8, 10, 14, 18, 26, 34, 50, 66, 98, 130, 194, 322, 578, 1090, 2114, 6210,
    22594,
];

/// Insert-length code extra-bit counts (`kBrotliInsExtra`).
pub(crate) const INS_EXTRA: [u32; NUM_INS_COPY_CODES] = [
    0, 0, 0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 7, 8, 9, 10, 12, 14, 24,
];

/// Copy-length code bases (`kBrotliCopyBase`).
pub(crate) const COPY_BASE: [u32; NUM_INS_COPY_CODES] = [
    2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 14, 18, 22, 30, 38, 54, 70, 102, 134, 198, 326, 582, 1094, 2118,
];

/// Copy-length code extra-bit counts (`kBrotliCopyExtra`).
pub(crate) const COPY_EXTRA: [u32; NUM_INS_COPY_CODES] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 7, 8, 9, 10, 24,
];

/// Number of block-length symbols (`BROTLI_NUM_BLOCK_LEN_SYMBOLS`).
pub(crate) const NUM_BLOCK_LEN_SYMBOLS: usize = 26;

/// Block-length prefix code ranges (`_kBrotliPrefixCodeRanges`).
///
/// Each entry is `(offset, nbits)`: the code covers
/// `offset..offset + (1 << nbits)`.
pub(crate) const PREFIX_CODE_RANGES: [(u32, u32); NUM_BLOCK_LEN_SYMBOLS] = [
    (1, 2),
    (5, 2),
    (9, 2),
    (13, 2),
    (17, 3),
    (25, 3),
    (33, 3),
    (41, 3),
    (49, 4),
    (65, 4),
    (81, 4),
    (97, 4),
    (113, 5),
    (145, 5),
    (177, 5),
    (209, 5),
    (241, 6),
    (305, 6),
    (369, 7),
    (497, 8),
    (753, 9),
    (1265, 10),
    (2289, 11),
    (4337, 12),
    (8433, 13),
    (16625, 24),
];

/// Two-context map over UTF-8 prefixes (`kStaticContextMapSimpleUTF8`).
pub(crate) const STATIC_CONTEXT_MAP_SIMPLE_UTF8: [u32; 64] = [
    0, 0, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

/// Three-context map over UTF-8 prefixes (`kStaticContextMapContinuation`).
pub(crate) const STATIC_CONTEXT_MAP_CONTINUATION: [u32; 64] = [
    1, 1, 2, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

/// Number of contexts the complex static map distinguishes.
pub(crate) const MAX_STATIC_CONTEXTS: usize = 13;

/// Thirteen-context map over UTF-8 prefixes (`kStaticContextMapComplexUTF8`).
///
/// The rows group the source classes: special, line feed, space, punctuation,
/// quotes, percent, opening and closing brackets, colons, full stop, greater
/// than, digits, upper case and lower case.
pub(crate) const STATIC_CONTEXT_MAP_COMPLEX_UTF8: [u32; 64] = [
    11, 11, 12, 12, //
    0, 0, 0, 0, //
    1, 1, 9, 9, //
    2, 2, 2, 2, //
    1, 1, 1, 1, //
    8, 3, 3, 3, //
    1, 1, 1, 1, //
    2, 2, 2, 2, //
    8, 4, 4, 4, //
    8, 7, 4, 4, //
    8, 0, 0, 0, //
    3, 3, 3, 3, //
    5, 5, 10, 5, //
    5, 5, 10, 5, //
    6, 6, 6, 6, //
    6, 6, 6, 6, //
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_lut_matches_the_reference_checksum() {
        assert_eq!(CONTEXT_LUT_UTF8.len(), 512);
        assert_eq!(
            CONTEXT_LUT_UTF8.iter().map(|&v| u32::from(v)).sum::<u32>(),
            4394
        );
        assert_eq!(CONTEXT_LUT_UTF8[usize::from(b'a')], 56);
        assert_eq!(CONTEXT_LUT_UTF8[usize::from(b'b')], 60);
        assert_eq!(CONTEXT_LUT_UTF8[256 + usize::from(b'a')], 3);
        assert_eq!(CONTEXT_LUT_UTF8[usize::from(b' ')], 8);
    }

    #[test]
    fn command_tables_have_the_reference_lengths_and_checksums() {
        assert_eq!(INS_BASE.iter().sum::<u32>(), 33_577);
        assert_eq!(COPY_BASE.iter().sum::<u32>(), 4_866);
        assert_eq!(INS_EXTRA.iter().sum::<u32>(), 120);
        assert_eq!(COPY_EXTRA.iter().sum::<u32>(), 94);
    }

    #[test]
    fn prefix_code_ranges_are_contiguous() {
        let mut next = 1u32;
        for &(offset, nbits) in &PREFIX_CODE_RANGES {
            assert_eq!(offset, next);
            next = offset + (1u32 << nbits);
        }
    }

    #[test]
    fn static_context_maps_stay_within_their_context_counts() {
        assert!(STATIC_CONTEXT_MAP_SIMPLE_UTF8.iter().all(|&v| v < 2));
        assert!(STATIC_CONTEXT_MAP_CONTINUATION.iter().all(|&v| v < 3));
        assert!(
            STATIC_CONTEXT_MAP_COMPLEX_UTF8
                .iter()
                .all(|&v| (v as usize) < MAX_STATIC_CONTEXTS)
        );
    }
}
