//! Primitives every quality of this encoder shares.
//!
//! The bit writer, the Huffman builders, the match-length scan, the reference
//! logarithms and the format constants are dictated by RFC 7932 and by the
//! pinned reference encoder, so they are identical whichever quality asks for
//! them. Keeping them here rather than inside one quality's tree is what lets
//! the fast and the greedy encoders share an implementation without depending
//! on each other.

pub(crate) mod bits;
pub(crate) mod constants;
pub(crate) mod fast_log;
pub(crate) mod huffman;
pub(crate) mod match_len;
pub(crate) mod tables;
