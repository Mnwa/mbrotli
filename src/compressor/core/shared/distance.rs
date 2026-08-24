//! The distance alphabet: its parameters and the constants that bound it.
//!
//! Ports `BrotliInitDistanceParams` from `c/enc/metablock.c` and the distance
//! constants of `c/common/constants.h` from the pinned reference
//! (`google/brotli` v1.2.0, commit `028fb5a`).
//!
//! Every quality from three upwards writes distances through this alphabet;
//! qualities four and above may also tune it per meta-block, which is why the
//! parameters are a value rather than a global.

/// Largest number of postfix bits the format allows (`BROTLI_MAX_NPOSTFIX`).
pub(crate) const MAX_NPOSTFIX: u32 = 3;

/// Largest number of direct distance codes (`BROTLI_MAX_NDIRECT`).
pub(crate) const MAX_NDIRECT: u32 = 120;

/// Number of short distance codes (`BROTLI_NUM_DISTANCE_SHORT_CODES`).
pub(crate) const NUM_DISTANCE_SHORT_CODES: u32 = 16;

/// Largest number of distance bits in RFC 7932 (`BROTLI_MAX_DISTANCE_BITS`).
pub(crate) const MAX_DISTANCE_BITS: u32 = 24;

/// Distance symbols a histogram reserves
/// (`BROTLI_NUM_HISTOGRAM_DISTANCE_SYMBOLS`).
pub(crate) const NUM_HISTOGRAM_DISTANCE_SYMBOLS: usize = 544;

/// Distance alphabet size assuming no postfix or direct codes.
///
/// `MAX_SIMPLE_DISTANCE_ALPHABET_SIZE` in `c/enc/brotli_bit_stream.c`, computed
/// for the large-window bit count even though this encoder never uses it.
pub(crate) const MAX_SIMPLE_DISTANCE_ALPHABET_SIZE: usize = 140;

/// Resolved distance alphabet (`BrotliDistanceParams`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct DistanceParams {
    /// Number of postfix bits, `NPOSTFIX`.
    pub(crate) postfix_bits: u32,
    /// Number of direct distance codes, `NDIRECT`.
    pub(crate) num_direct: u32,
    /// Size of the distance alphabet that is written to the stream.
    pub(crate) alphabet_size_max: u32,
    /// Size of the distance alphabet that can actually occur.
    pub(crate) alphabet_size_limit: u32,
    /// Largest distance this alphabet can express.
    pub(crate) max_distance: u32,
}

impl DistanceParams {
    /// Builds the alphabet for `postfix_bits` and `num_direct`.
    ///
    /// Mirrors `BrotliInitDistanceParams` for the RFC 7932 window sizes; the
    /// large-window extension is not reachable through the public API, so the
    /// limit and the maximum alphabet always coincide.
    pub(crate) const fn new(postfix_bits: u32, num_direct: u32) -> Self {
        let alphabet_size_max =
            NUM_DISTANCE_SHORT_CODES + num_direct + (MAX_DISTANCE_BITS << (postfix_bits + 1));
        let max_distance = num_direct + (1u32 << (MAX_DISTANCE_BITS + postfix_bits + 2))
            - (1u32 << (postfix_bits + 2));
        Self {
            postfix_bits,
            num_direct,
            alphabet_size_max,
            alphabet_size_limit: alphabet_size_max,
            max_distance,
        }
    }
}

impl Default for DistanceParams {
    /// Returns the alphabet with neither postfix nor direct codes.
    fn default() -> Self {
        Self::new(0, 0)
    }
}
