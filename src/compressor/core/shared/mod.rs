//! Primitives every quality of this encoder shares.
//!
//! Two layers live here. The lower one is dictated by RFC 7932 and by the
//! pinned reference encoder — the bit writer, the Huffman builders, the
//! match-length scan, the reference logarithms and the format tables — so it is
//! identical whichever quality asks for it. The upper one is the shape of a
//! compressed meta-block: commands, histograms, block splits, context modes,
//! the distance alphabet, the ring buffer, the static dictionary and the
//! writer that turns all of it into bytes.
//!
//! What is *not* here is any decision: which match to take, where to split, how
//! many contexts to model. Those belong to the quality that makes them, which
//! is what lets the fast, greedy and high-quality encoders share this layer
//! without depending on each other.

pub(crate) mod bit_cost;
pub(crate) mod bits;
pub(crate) mod bitstream;
pub(crate) mod block_split;
pub(crate) mod command;
pub(crate) mod constants;
pub(crate) mod dictionary;
pub(crate) mod distance;
pub(crate) mod fast_log;
pub(crate) mod format;
pub(crate) mod histogram;
pub(crate) mod huffman;
pub(crate) mod match_len;
pub(crate) mod metablock;
pub(crate) mod ringbuffer;
pub(crate) mod score;
pub(crate) mod tables;
