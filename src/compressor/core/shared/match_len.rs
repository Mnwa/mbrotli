//! Exact match-length scan, with a portable SIMD tail.
//!
//! The reference encoder compares eight bytes at a time with an XOR and a
//! trailing-zero count (`c/enc/find_match_length.h`). That is already very fast
//! for the short matches the fast qualities mostly find, so this port keeps a
//! scalar prefix and only widens to native SIMD vectors once a match has
//! already run past [`SCALAR_PREFIX_BYTES`]. The result is the exact same
//! length in every case, so the emitted bitstream never depends on the level.

use fearless_simd::{Simd, SimdBase, SimdMask, u8x16, u8x32, u8x64};

/// Bytes compared with scalar 64-bit loads before the SIMD loop is entered.
///
/// Short matches dominate quality 0 and 1, and entering a vector loop for them
/// costs more than it saves.
pub(crate) const SCALAR_PREFIX_BYTES: usize = 16;

/// Reads eight little-endian bytes at `offset`, or zero past the end.
///
/// Borrowing a fixed-size chunk instead of copying into a scratch array keeps
/// this to a single unaligned load in the generated code, while staying inside
/// safe Rust: the bounds test is the only thing that survives.
#[inline(always)]
pub(crate) fn load_u64_le(data: &[u8], offset: usize) -> u64 {
    match data.get(offset..).and_then(|tail| tail.first_chunk::<8>()) {
        Some(chunk) => u64::from_le_bytes(*chunk),
        None => 0,
    }
}

/// Compares two equal-length windows eight bytes at a time.
///
/// Returns the number of leading equal bytes, or the length of the whole-word
/// prefix when every word matched. Iterating over fixed-size chunks keeps the
/// loop free of bounds checks without any unsafe code.
#[inline(always)]
fn match_len_words(left: &[u8], right: &[u8]) -> usize {
    let (left_words, _) = left.as_chunks::<8>();
    let (right_words, _) = right.as_chunks::<8>();
    let mut matched = 0usize;
    for (left_word, right_word) in left_words.iter().zip(right_words) {
        let difference = u64::from_le_bytes(*left_word) ^ u64::from_le_bytes(*right_word);
        if difference != 0 {
            return matched + (difference.trailing_zeros() as usize >> 3);
        }
        matched += 8;
    }
    matched
}

/// Defines a vector comparison loop for one concrete vector width.
///
/// `as_chunks` hands the loop `&[u8; LANES]` references, which are exactly the
/// array type the vector loads from, so `load_array_ref` takes them by
/// reference with neither a copy, a bounds check, nor a length assertion.
macro_rules! vector_scan {
    ($name:ident, $vector:ident, $lanes:literal) => {
        #[doc = concat!(
                            "Compares two windows ", stringify!($lanes), " bytes at a time.\n\n",
                            "Returns the number of leading equal bytes, or the length of the \
             whole-vector prefix when every vector matched."
                        )]
        #[inline(always)]
        fn $name<S: Simd>(simd: S, left: &[u8], right: &[u8]) -> usize {
            let (left_vectors, _) = left.as_chunks::<$lanes>();
            let (right_vectors, _) = right.as_chunks::<$lanes>();
            let mut matched = 0usize;
            for (left_lanes, right_lanes) in left_vectors.iter().zip(right_vectors) {
                let equal = $vector::<S>::load_array_ref(simd, left_lanes)
                    .simd_eq($vector::<S>::load_array_ref(simd, right_lanes));
                if equal.any_false() {
                    return matched + equal.to_bitmask().trailing_ones() as usize;
                }
                matched += $lanes;
            }
            matched
        }
    };
}

vector_scan!(match_len_vectors_16, u8x16, 16);
vector_scan!(match_len_vectors_32, u8x32, 32);
vector_scan!(match_len_vectors_64, u8x64, 64);

/// Bytes [`match_len_native_vectors`] advances per step on this backend.
///
/// The caller uses this to work out how many bytes a fully matching scan would
/// have reported, so it has to be the stride the scan really takes rather than
/// the backend's lane count: a width with no vector loop degrades to a
/// byte-at-a-time scan, whose stride is one.
#[inline(always)]
const fn native_vector_stride<S: Simd>() -> usize {
    match <S::u8s as SimdBase<S>>::N {
        16 => 16,
        32 => 32,
        64 => 64,
        _ => 1,
    }
}

/// Runs the vector scan with the backend's native lane count baked in.
///
/// The match resolves at monomorphisation time, because `S::u8s::N` is a
/// constant for every backend; no branch survives into the generated code.
#[inline(always)]
fn match_len_native_vectors<S: Simd>(simd: S, left: &[u8], right: &[u8]) -> usize {
    match <S::u8s as SimdBase<S>>::N {
        16 => match_len_vectors_16(simd, left, right),
        32 => match_len_vectors_32(simd, left, right),
        64 => match_len_vectors_64(simd, left, right),
        // No supported backend has another width. Falling back to single bytes
        // rather than to whole words is what keeps the caller's "did every step
        // match?" test exact: it pairs with the stride of one that
        // `native_vector_stride` reports, whereas a word scan would round the
        // window down to a multiple of eight and under-report a shorter match.
        _ => match_len_bytes(left, right),
    }
}

/// Compares two windows byte by byte, stopping at the shorter one.
#[inline(always)]
fn match_len_bytes(left: &[u8], right: &[u8]) -> usize {
    left.iter()
        .zip(right)
        .take_while(|(left_byte, right_byte)| left_byte == right_byte)
        .count()
}

/// Returns how many bytes of `data[left..]` and `data[right..]` agree.
///
/// The comparison stops after `limit` bytes; neither side is read beyond it.
/// A window that does not fit inside `data` yields zero rather than panicking.
#[inline(always)]
pub(crate) fn find_match_length<S: Simd>(
    simd: S,
    data: &[u8],
    left: usize,
    right: usize,
    limit: usize,
) -> usize {
    let (Some(left_window), Some(right_window)) =
        (data.get(left..left + limit), data.get(right..right + limit))
    else {
        return 0;
    };

    // Cheap scalar prefix. Most matches the fast qualities find are short, and
    // entering a vector loop for them costs more than it saves.
    let prefix = limit.min(SCALAR_PREFIX_BYTES) & !7;
    let mut matched = match_len_words(&left_window[..prefix], &right_window[..prefix]);
    if matched < prefix {
        return matched;
    }

    // Native-width vectors over the bulk of a long match.
    let stride = native_vector_stride::<S>();
    let whole_vectors = (limit - matched) - (limit - matched) % stride;
    let vectored =
        match_len_native_vectors(simd, &left_window[matched..], &right_window[matched..]);
    matched += vectored;
    if vectored < whole_vectors {
        return matched;
    }

    // Whole-word and single-byte tails.
    let whole_words = (limit - matched) & !7;
    let tail = match_len_words(
        &left_window[matched..matched + whole_words],
        &right_window[matched..matched + whole_words],
    );
    matched += tail;
    if tail < whole_words {
        return matched;
    }

    matched + match_len_bytes(&left_window[matched..], &right_window[matched..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use fearless_simd::{Level, dispatch};

    /// Straight-line reference implementation used as the oracle.
    fn baseline(data: &[u8], left: usize, right: usize, limit: usize) -> usize {
        (0..limit)
            .take_while(|&i| data[left + i] == data[right + i])
            .count()
    }

    fn measure(data: &[u8], left: usize, right: usize, limit: usize) -> usize {
        let level = Level::new();
        dispatch!(level, simd => find_match_length(simd, data, left, right, limit))
    }

    fn measure_fallback(data: &[u8], left: usize, right: usize, limit: usize) -> usize {
        let level = Level::fallback();
        dispatch!(level, simd => find_match_length(simd, data, left, right, limit))
    }

    #[test]
    fn loads_read_little_endian_words_at_every_offset() {
        let data: Vec<u8> = (1..=32u8).collect();
        for offset in 0..=(data.len() - 8) {
            let mut expected = [0u8; 8];
            expected.copy_from_slice(&data[offset..offset + 8]);
            assert_eq!(load_u64_le(&data, offset), u64::from_le_bytes(expected));
        }
    }

    #[test]
    fn loads_work_at_the_exact_end_of_the_slice() {
        let data = [1u8, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(load_u64_le(&data, 0), 0x0807_0605_0403_0201);
        assert_eq!(load_u64_le(&data, 1), 0);
    }

    #[test]
    fn loads_past_the_end_read_as_zero() {
        let data = [1u8, 2, 3];
        assert_eq!(load_u64_le(&data, 0), 0);
        assert_eq!(load_u64_le(&data, 3), 0);
        assert_eq!(load_u64_le(&data, 99), 0);
    }

    #[test]
    fn reports_every_mismatch_position() {
        let length = 300usize;
        for mismatch in 0..length {
            let mut data = vec![0u8; 2 * length];
            data[length + mismatch] = 1;
            let limit = length;
            assert_eq!(measure(&data, 0, length, limit), mismatch);
            assert_eq!(measure_fallback(&data, 0, length, limit), mismatch);
            assert_eq!(baseline(&data, 0, length, limit), mismatch);
        }
    }

    #[test]
    fn respects_every_limit() {
        let data = vec![7u8; 512];
        for limit in 0..=256 {
            assert_eq!(measure(&data, 0, 256, limit), limit);
            assert_eq!(measure_fallback(&data, 0, 256, limit), limit);
        }
    }

    #[test]
    fn matches_the_baseline_on_pseudo_random_data() {
        let mut data = vec![0u8; 4096];
        let mut state = 0x1234_5678u32;
        for byte in data.iter_mut() {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            *byte = (state >> 16) as u8 & 0x03;
        }
        for left in (0..1024).step_by(7) {
            for right in (1024..2048).step_by(13) {
                let limit = 512;
                let expected = baseline(&data, left, right, limit);
                assert_eq!(measure(&data, left, right, limit), expected);
                assert_eq!(measure_fallback(&data, left, right, limit), expected);
            }
        }
    }

    #[test]
    fn a_window_that_does_not_fit_the_input_reports_no_match() {
        let data = vec![0u8; 32];
        for (left, right, limit) in [(0usize, 16usize, 17usize), (24, 0, 9), (33, 0, 1)] {
            assert_eq!(measure(&data, left, right, limit), 0);
            assert_eq!(measure_fallback(&data, left, right, limit), 0);
        }
    }

    /// The stride the caller reasons with has to be the one the scan takes.
    ///
    /// `find_match_length` decides "did every step match?" by comparing the
    /// scan's result against the window rounded down to a whole number of
    /// strides. A stride wider than the scan's own step would round the window
    /// up past what the scan can report and truncate a match that ran to the
    /// limit, so the two must agree for every backend.
    #[test]
    fn the_reported_stride_is_the_one_the_vector_scan_takes() {
        fn check<S: Simd>(simd: S) {
            let stride = native_vector_stride::<S>();
            assert!(matches!(stride, 1 | 16 | 32 | 64), "stride {stride}");
            let data = vec![0xCDu8; 4 * stride];
            let left = vec![0xCDu8; 4 * stride];
            assert_eq!(
                match_len_native_vectors(simd, &left, &data),
                4 * stride,
                "a fully matching window has to report every stride"
            );
            // One byte short of two strides: a scan stepping by `stride` sees
            // one whole step, and the caller's guard has to agree.
            let window = 2 * stride - 1;
            assert_eq!(
                match_len_native_vectors(simd, &left[..window], &data[..window]),
                window - window % stride
            );
        }
        dispatch!(Level::new(), simd => check(simd));
        dispatch!(Level::fallback(), simd => check(simd));
    }

    #[test]
    fn overlapping_ranges_extend_like_the_reference() {
        let data = vec![0xABu8; 64];
        assert_eq!(measure(&data, 0, 1, 63), 63);
        assert_eq!(measure_fallback(&data, 0, 1, 63), 63);
    }
}
