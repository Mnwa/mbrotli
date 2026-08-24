//! Upper bound on the size of a compressed stream.

use crate::compressor::core::shared::constants::{OUTPUT_RESERVE_CONST, OUTPUT_SLACK};
use crate::compressor::{BrotliCompressError, BrotliResult, CompressParams, QualityLevel};

/// Returns an upper bound on the compressed size of `input_size` bytes.
///
/// Both encoders reserve `2 * fragment + 503` bytes per meta-block, exactly
/// like the reference encoder, plus the bit writer's whole-word headroom and
/// two bytes for the stream header. Counting the headroom per fragment is what
/// lets the fast path write straight into a buffer sized by this bound instead
/// of copying through its own scratch space.
///
/// # Errors
///
/// Returns [`BrotliCompressError::BoundOverflow`] when that arithmetic does not
/// fit in a `usize`, rather than wrapping or saturating into a bound that no
/// longer bounds anything.
pub(crate) const fn bound(params: &CompressParams, input_size: usize) -> BrotliResult<usize> {
    let fragment = 1usize << fragment_bits(params);
    let fragments = if input_size == 0 {
        1
    } else {
        (input_size - 1) / fragment + 1
    };

    let Some(overhead) = fragments.checked_mul(OUTPUT_RESERVE_CONST + OUTPUT_SLACK) else {
        return Err(BrotliCompressError::BoundOverflow);
    };
    let Some(payload) = input_size.checked_mul(2) else {
        return Err(BrotliCompressError::BoundOverflow);
    };
    let Some(total) = payload.checked_add(overhead) else {
        return Err(BrotliCompressError::BoundOverflow);
    };
    match total.checked_add(2) {
        Some(total) => Ok(total),
        None => Err(BrotliCompressError::BoundOverflow),
    }
}

/// Returns the base-2 logarithm of the input the encoder consumes per step.
///
/// Both encoder families emit at most one meta-block per step, so this is what
/// bounds how often the per-meta-block overhead is paid. The fast qualities
/// cut the input at the window size; the greedy ones use the block size, which
/// is fourteen bits below quality four and the requested or default sixteen
/// above it.
const fn fragment_bits(params: &CompressParams) -> usize {
    match params.quality {
        QualityLevel::Q3 => 14,
        QualityLevel::Q4 | QualityLevel::Q5 => match params.lgblock {
            Some(lgblock) => lgblock.0,
            None => 16,
        },
        // Quality 0 and 1 cut at the window size; an unimplemented quality
        // never reaches an encoder, but still has to return some bound.
        _ => params.lgwin.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compressor::{ParseWindowBitsError, QualityLevel, WindowBits};

    fn params(lgwin: usize) -> Result<CompressParams, ParseWindowBitsError> {
        Ok(CompressParams::new(
            QualityLevel::Q0,
            WindowBits::try_from(lgwin)?,
        ))
    }

    #[test]
    fn empty_input_still_reserves_one_fragment() -> Result<(), ParseWindowBitsError> {
        assert!(matches!(bound(&params(22)?, 0), Ok(513)));
        Ok(())
    }

    #[test]
    fn bound_grows_with_the_number_of_fragments() -> Result<(), ParseWindowBitsError> {
        let small_window = bound(&params(10)?, 1 << 20).ok();
        let large_window = bound(&params(22)?, 1 << 20).ok();
        assert!(small_window > large_window);
        Ok(())
    }

    #[test]
    fn bound_covers_at_least_the_input() -> Result<(), ParseWindowBitsError> {
        let params = params(22)?;
        for size in [0usize, 1, 1024, 1 << 20] {
            assert!(bound(&params, size).is_ok_and(|value| value >= size));
        }
        Ok(())
    }

    #[test]
    fn bound_reports_an_overflow_instead_of_wrapping() -> Result<(), ParseWindowBitsError> {
        assert!(matches!(
            bound(&params(22)?, usize::MAX),
            Err(BrotliCompressError::BoundOverflow)
        ));
        assert!(matches!(
            bound(&params(10)?, usize::MAX / 2),
            Err(BrotliCompressError::BoundOverflow)
        ));
        Ok(())
    }
}
