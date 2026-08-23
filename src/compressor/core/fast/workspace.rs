//! Reusable scratch state for the fast encoders.
//!
//! Both arenas mirror `BrotliOnePassArena` and `BrotliTwoPassArena` from the
//! pinned reference. They are allocated once per encoder and reused for every
//! block, so no allocation happens inside a match scan or a command replay.

use super::constants::{NUM_COMMAND_SYMBOLS, NUM_LITERAL_SYMBOLS};
use super::huffman::{HuffmanNode, tree_capacity};
use super::tables::{
    DEFAULT_COMMAND_BITS, DEFAULT_COMMAND_CODE, DEFAULT_COMMAND_CODE_NUM_BITS,
    DEFAULT_COMMAND_DEPTHS,
};

/// Size of the buffer holding the pre-compressed next-block command code.
const COMMAND_CODE_CAPACITY: usize = 512;

/// Scratch state of the quality 0 one-pass encoder.
pub(crate) struct OnePassArena {
    /// Bit depths of the current literal prefix code.
    pub(crate) lit_depth: [u8; NUM_LITERAL_SYMBOLS],
    /// Bit patterns of the current literal prefix code.
    pub(crate) lit_bits: [u16; NUM_LITERAL_SYMBOLS],
    /// Bit depths of the command and distance prefix codes.
    pub(crate) cmd_depth: [u8; 128],
    /// Bit patterns of the command and distance prefix codes.
    pub(crate) cmd_bits: [u16; 128],
    /// Command and distance statistics gathered for the next block.
    pub(crate) cmd_histo: [u32; 128],
    /// Pre-compressed command and distance code for the next fragment.
    pub(crate) cmd_code: [u8; COMMAND_CODE_CAPACITY],
    /// Number of valid bits in [`OnePassArena::cmd_code`].
    pub(crate) cmd_code_numbits: usize,
    /// Node pool shared by every prefix-code build.
    pub(crate) tree: Vec<HuffmanNode>,
    /// Literal histogram, reused as the block-merge sample histogram.
    pub(crate) histogram: [u32; NUM_LITERAL_SYMBOLS],
    /// Scratch depths for the reordered command alphabet.
    pub(crate) tmp_depth: [u8; NUM_COMMAND_SYMBOLS],
    /// Scratch bit patterns for the reordered command alphabet.
    pub(crate) tmp_bits: [u16; 64],
}

impl Default for OnePassArena {
    /// Creates an arena primed with the reference first-block command code.
    fn default() -> Self {
        let mut cmd_code = [0u8; COMMAND_CODE_CAPACITY];
        cmd_code[..DEFAULT_COMMAND_CODE.len()].copy_from_slice(&DEFAULT_COMMAND_CODE);
        Self {
            lit_depth: [0; NUM_LITERAL_SYMBOLS],
            lit_bits: [0; NUM_LITERAL_SYMBOLS],
            cmd_depth: DEFAULT_COMMAND_DEPTHS,
            cmd_bits: DEFAULT_COMMAND_BITS,
            cmd_histo: [0; 128],
            cmd_code,
            cmd_code_numbits: DEFAULT_COMMAND_CODE_NUM_BITS,
            tree: vec![HuffmanNode::default(); tree_capacity(NUM_LITERAL_SYMBOLS)],
            histogram: [0; NUM_LITERAL_SYMBOLS],
            tmp_depth: [0; NUM_COMMAND_SYMBOLS],
            tmp_bits: [0; 64],
        }
    }
}

/// Scratch state of the quality 1 two-pass encoder.
pub(crate) struct TwoPassArena {
    /// Exact literal histogram of the current block.
    pub(crate) lit_histo: [u32; NUM_LITERAL_SYMBOLS],
    /// Bit depths of the current literal prefix code.
    pub(crate) lit_depth: [u8; NUM_LITERAL_SYMBOLS],
    /// Bit patterns of the current literal prefix code.
    pub(crate) lit_bits: [u16; NUM_LITERAL_SYMBOLS],
    /// Exact command histogram of the current block.
    pub(crate) cmd_histo: [u32; 128],
    /// Bit depths of the command and distance prefix codes.
    pub(crate) cmd_depth: [u8; 128],
    /// Bit patterns of the command and distance prefix codes.
    pub(crate) cmd_bits: [u16; 128],
    /// Node pool shared by every prefix-code build.
    pub(crate) tmp_tree: Vec<HuffmanNode>,
    /// Scratch depths for the reordered command alphabet.
    pub(crate) tmp_depth: [u8; NUM_COMMAND_SYMBOLS],
    /// Scratch bit patterns for the reordered command alphabet.
    pub(crate) tmp_bits: [u16; 64],
}

impl Default for TwoPassArena {
    fn default() -> Self {
        Self {
            lit_histo: [0; NUM_LITERAL_SYMBOLS],
            lit_depth: [0; NUM_LITERAL_SYMBOLS],
            lit_bits: [0; NUM_LITERAL_SYMBOLS],
            cmd_histo: [0; 128],
            cmd_depth: [0; 128],
            cmd_bits: [0; 128],
            tmp_tree: vec![HuffmanNode::default(); tree_capacity(NUM_LITERAL_SYMBOLS)],
            tmp_depth: [0; NUM_COMMAND_SYMBOLS],
            tmp_bits: [0; 64],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_pass_arena_starts_from_the_reference_command_code() {
        let arena = OnePassArena::default();
        assert_eq!(arena.cmd_code_numbits, 448);
        assert_eq!(&arena.cmd_code[..57], &DEFAULT_COMMAND_CODE);
        assert!(arena.cmd_code[57..].iter().all(|&b| b == 0));
        assert_eq!(arena.cmd_depth, DEFAULT_COMMAND_DEPTHS);
        assert_eq!(arena.cmd_bits, DEFAULT_COMMAND_BITS);
        assert_eq!(arena.tree.len(), 513);
    }

    #[test]
    fn two_pass_arena_starts_cleared() {
        let arena = TwoPassArena::default();
        assert!(arena.lit_histo.iter().all(|&c| c == 0));
        assert!(arena.cmd_histo.iter().all(|&c| c == 0));
        assert_eq!(arena.tmp_tree.len(), 513);
    }
}
