//! Logarithms with the exact rounding the reference encoder relies on.
//!
//! `FastLog2` (`c/enc/fast_log.h`) reads a table of single-precision
//! logarithms for small arguments and falls back to `log2` above it. Cost
//! comparisons in the block splitter and the histogram builders are decided on
//! these values, so the rounding is part of the bitstream contract rather than
//! an implementation detail.

use super::tables::LOG2_TABLE;

/// Returns `floor(log2(value))` for a non-zero `value`.
#[inline(always)]
pub(crate) const fn log2_floor_non_zero(value: usize) -> u32 {
    (usize::BITS - 1) - value.leading_zeros()
}

/// Reference logarithm with `log2(0) == 0` (`FastLog2`).
#[inline]
pub(crate) fn fast_log2(value: usize) -> f64 {
    match LOG2_TABLE.get(value) {
        Some(&entry) => entry,
        None => (value as f64).log2(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_log2_matches_the_bit_width() {
        assert_eq!(log2_floor_non_zero(1), 0);
        assert_eq!(log2_floor_non_zero(2), 1);
        assert_eq!(log2_floor_non_zero(3), 1);
        assert_eq!(log2_floor_non_zero(255), 7);
        assert_eq!(log2_floor_non_zero(256), 8);
    }

    #[test]
    fn fast_log2_reads_the_table_below_it_and_computes_above() {
        assert_eq!(fast_log2(0), 0.0);
        assert_eq!(fast_log2(1), 0.0);
        assert_eq!(fast_log2(2), 1.0);
        assert_eq!(fast_log2(255), LOG2_TABLE[255]);
        assert_eq!(fast_log2(256), 8.0);
        assert_eq!(fast_log2(1024), 10.0);
    }
}
