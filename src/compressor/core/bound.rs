//! Upper bound on the size of a compressed stream.

use crate::compressor::core::fast::constants::{OUTPUT_RESERVE_CONST, OUTPUT_SLACK};
use crate::compressor::{BrotliCompressError, BrotliCompressParams, BrotliResult};

/// Returns an upper bound on the compressed size of `input_size` bytes.
///
/// The fast path cuts the input into `1 << lgwin` fragments and reserves
/// `2 * fragment + 503` bytes for each of them, exactly like the reference
/// encoder, plus the bit writer's whole-word headroom and two bytes for the
/// stream header. Counting the headroom per fragment is what lets the encoder
/// write straight into a buffer sized by this bound instead of copying through
/// its own scratch space.
///
/// # Errors
///
/// Returns [`BrotliCompressError::BoundOverflow`] when that arithmetic does not
/// fit in a `usize`, rather than wrapping or saturating into a bound that no
/// longer bounds anything.
pub(crate) const fn bound(params: &BrotliCompressParams, input_size: usize) -> BrotliResult<usize> {
    let fragment = 1usize << params.lgwin.0;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compressor::{BrotliQualityLevel, BrotliWindowBits, ParseWindowBitsError};

    fn params(lgwin: usize) -> Result<BrotliCompressParams, ParseWindowBitsError> {
        Ok(BrotliCompressParams::new(
            BrotliQualityLevel::Q0,
            BrotliWindowBits::try_from(lgwin)?,
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
