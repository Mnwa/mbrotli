//! The exported C functions, called through their ABI exactly as C would.

use google_brotli_ffi as google;
use mbrotli_ffi::{MbrotliResult, mbrotli_compress, mbrotli_compress_bound, mbrotli_decompress};
use std::ffi::c_int;
use std::ptr;

const ALICE: &[u8] = include_bytes!("../../brotli-ffi/vendor/brotli/tests/testdata/alice29.txt");

/// A text, a binary, an incompressible and a tiny input.
fn corpus() -> Vec<Vec<u8>> {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let noise = (0..20_000)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect();
    let binary = (0u32..16_384)
        .flat_map(|value| (value * 7).to_le_bytes())
        .collect();
    vec![
        Vec::new(),
        b"a".to_vec(),
        ALICE[..40_000].to_vec(),
        binary,
        noise,
    ]
}

/// Calls `mbrotli_compress` into a buffer of `capacity` bytes.
fn compress(
    input: &[u8],
    capacity: usize,
    quality: c_int,
    lgwin: c_int,
) -> (MbrotliResult, Vec<u8>) {
    let mut output = vec![0u8; capacity];
    let mut output_len = capacity;
    // SAFETY: both buffers are live and have the stated lengths.
    let status = unsafe {
        mbrotli_compress(
            input.as_ptr(),
            input.len(),
            output.as_mut_ptr(),
            &mut output_len,
            quality,
            lgwin,
        )
    };
    output.truncate(output_len);
    (status, output)
}

/// Calls `mbrotli_decompress` into a buffer of `capacity` bytes.
fn decompress(input: &[u8], capacity: usize) -> (MbrotliResult, usize, Vec<u8>) {
    let mut output = vec![0u8; capacity];
    let mut output_len = capacity;
    // SAFETY: both buffers are live and have the stated lengths.
    let status = unsafe {
        mbrotli_decompress(
            input.as_ptr(),
            input.len(),
            output.as_mut_ptr(),
            &mut output_len,
        )
    };
    (status, output_len, output)
}

/// Google's one-shot encoder with the same parameters.
fn google_compress(input: &[u8], quality: c_int, lgwin: c_int) -> Vec<u8> {
    // SAFETY: a pure function of its argument.
    let mut output = vec![0u8; unsafe { google::BrotliEncoderMaxCompressedSize(input.len()) }];
    let mut output_len = output.len();
    // SAFETY: both buffers are live and have the stated lengths.
    let accepted = unsafe {
        google::BrotliEncoderCompress(
            quality,
            lgwin,
            google::BROTLI_MODE_GENERIC,
            input.len(),
            input.as_ptr(),
            &mut output_len,
            output.as_mut_ptr(),
        )
    };
    assert_eq!(accepted, google::BROTLI_TRUE);
    output.truncate(output_len);
    output
}

/// Google's one-shot decoder, into a buffer of exactly `len` bytes.
fn google_decompress(input: &[u8], len: usize) -> Vec<u8> {
    let mut output = vec![0u8; len];
    let mut output_len = len;
    // SAFETY: both buffers are live and have the stated lengths.
    let result = unsafe {
        google::BrotliDecoderDecompress(
            input.len(),
            input.as_ptr(),
            &mut output_len,
            output.as_mut_ptr(),
        )
    };
    assert_eq!(result, google::BROTLI_DECODER_RESULT_SUCCESS);
    output.truncate(output_len);
    output
}

#[test]
fn compression_is_byte_identical_to_google_at_every_quality() {
    for (index, input) in corpus().into_iter().enumerate() {
        for quality in 0..=11 {
            // Qualities 10 and 11 price candidates in floating point. On
            // aarch64, clang contracts the C oracle's multiply-adds into FMA
            // instructions by default, which moves a few ties on the
            // arithmetic `binary` input; built with `-ffp-contract=off` the C
            // library agrees there too.
            if quality >= 10 && index == 3 && cfg!(target_arch = "aarch64") {
                continue;
            }
            for lgwin in [10, 16, 22, 24] {
                let (status, ours) =
                    compress(&input, mbrotli_compress_bound(input.len()), quality, lgwin);
                assert_eq!(status, MbrotliResult::Ok);
                assert_eq!(
                    ours,
                    google_compress(&input, quality, lgwin),
                    "q{quality} w{lgwin} len {}",
                    input.len()
                );
            }
        }
    }
}

#[test]
fn compress_bound_equals_googles() {
    for len in [
        0,
        1,
        100,
        (1 << 14) - 1,
        1 << 14,
        1 << 20,
        usize::MAX / 2,
        usize::MAX - 10,
        usize::MAX,
    ] {
        // SAFETY: a pure function of its argument.
        assert_eq!(
            mbrotli_compress_bound(len),
            unsafe { google::BrotliEncoderMaxCompressedSize(len) },
            "{len}"
        );
    }
}

#[test]
fn a_destination_between_the_stream_and_the_bound_matches_google() {
    let input = &ALICE[..30_000];
    let bound = mbrotli_compress_bound(input.len());
    for capacity in [bound, bound - 1, 12_000] {
        let (status, ours) = compress(input, capacity, 5, 22);
        let mut theirs = vec![0u8; capacity];
        let mut theirs_len = capacity;
        // SAFETY: both buffers are live and have the stated lengths.
        let accepted = unsafe {
            google::BrotliEncoderCompress(
                5,
                22,
                google::BROTLI_MODE_GENERIC,
                input.len(),
                input.as_ptr(),
                &mut theirs_len,
                theirs.as_mut_ptr(),
            )
        };
        assert_eq!(
            accepted == google::BROTLI_TRUE,
            status == MbrotliResult::Ok,
            "{capacity}"
        );
        if accepted == google::BROTLI_TRUE {
            assert_eq!(ours, theirs[..theirs_len]);
        }
    }
}

#[test]
fn compressed_streams_round_trip_through_both_decoders() {
    for input in corpus() {
        for quality in [0, 1, 5, 9, 11] {
            let (_, compressed) =
                compress(&input, mbrotli_compress_bound(input.len()), quality, 22);
            assert_eq!(google_decompress(&compressed, input.len()), input);
            let (status, len, output) = decompress(&compressed, input.len());
            assert_eq!(status, MbrotliResult::Ok);
            assert_eq!(&output[..len], input.as_slice());
        }
    }
}

#[test]
fn google_streams_decompress() {
    for input in corpus() {
        let compressed = google_compress(&input, 6, 22);
        let (status, len, output) = decompress(&compressed, input.len() + 10);
        assert_eq!(status, MbrotliResult::Ok);
        assert_eq!(&output[..len], input.as_slice());
    }
}

#[test]
fn compress_bound_covers_every_quality_and_window() {
    for input in corpus() {
        let bound = mbrotli_compress_bound(input.len());
        for quality in [0, 1, 2, 11] {
            for lgwin in [10, 24] {
                let (status, output) = compress(&input, bound, quality, lgwin);
                assert_eq!(status, MbrotliResult::Ok);
                assert!(output.len() <= bound);
            }
        }
    }
}

#[test]
fn compress_fits_an_exactly_sized_buffer_and_refuses_one_byte_less() {
    let input = &ALICE[..5_000];
    let (_, expected) = compress(input, mbrotli_compress_bound(input.len()), 7, 22);

    let (status, exact) = compress(input, expected.len(), 7, 22);
    assert_eq!(status, MbrotliResult::Ok);
    assert_eq!(exact, expected);

    let (status, short) = compress(input, expected.len() - 1, 7, 22);
    assert_eq!(status, MbrotliResult::OutputTooSmall);
    assert!(short.is_empty(), "output_len is zeroed on failure");
}

#[test]
fn compress_accepts_a_null_empty_input() {
    let mut output = [0u8; 4];
    let mut output_len = output.len();
    // SAFETY: an empty input is never read; `output` has `output_len` bytes.
    let status =
        unsafe { mbrotli_compress(ptr::null(), 0, output.as_mut_ptr(), &mut output_len, 11, 22) };
    assert_eq!(status, MbrotliResult::Ok);
    assert_eq!(
        &output[..output_len],
        google_compress(&[], 11, 22).as_slice()
    );
}

#[test]
fn compress_into_a_null_empty_output_is_too_small() {
    let mut output_len = 0;
    // SAFETY: an empty output is never written.
    let status =
        unsafe { mbrotli_compress(b"x".as_ptr(), 1, ptr::null_mut(), &mut output_len, 5, 22) };
    assert_eq!(status, MbrotliResult::OutputTooSmall);
    assert_eq!(output_len, 0);
}

#[test]
fn compress_rejects_parameters_out_of_range_and_zeroes_the_length() {
    for (quality, lgwin) in [
        (-1, 22),
        (12, 22),
        (c_int::MAX, 22),
        (5, 9),
        (5, 25),
        (5, -1),
        (5, 30),
    ] {
        let (status, output) = compress(b"payload", 64, quality, lgwin);
        assert_eq!(
            status,
            MbrotliResult::InvalidParameter,
            "q{quality} w{lgwin}"
        );
        assert!(output.is_empty());
    }
}

#[test]
fn a_null_output_length_is_rejected_without_being_written() {
    let mut output = [0u8; 16];
    // SAFETY: a null `output_len` is detected before anything is touched.
    let compressed = unsafe {
        mbrotli_compress(
            b"x".as_ptr(),
            1,
            output.as_mut_ptr(),
            ptr::null_mut(),
            5,
            22,
        )
    };
    // SAFETY: as above.
    let decompressed =
        unsafe { mbrotli_decompress([0x3b].as_ptr(), 1, output.as_mut_ptr(), ptr::null_mut()) };
    assert_eq!(compressed, MbrotliResult::InvalidParameter);
    assert_eq!(decompressed, MbrotliResult::InvalidParameter);
}

#[test]
fn a_misaligned_output_length_is_rejected_without_being_written() {
    let mut storage = [0xa5u8; 2 * size_of::<usize>()];
    let base = storage.as_mut_ptr();
    let offset = base.align_offset(align_of::<usize>()) + 1;
    let misaligned = base.wrapping_add(offset).cast::<usize>();
    let mut output = [0u8; 16];
    // SAFETY: a misaligned `output_len` is detected before it is dereferenced.
    let status =
        unsafe { mbrotli_compress(b"x".as_ptr(), 1, output.as_mut_ptr(), misaligned, 5, 22) };
    assert_eq!(status, MbrotliResult::InvalidParameter);
    assert!(storage.iter().all(|&byte| byte == 0xa5));
}

#[test]
fn null_buffers_with_a_length_are_rejected() {
    let mut output = [0u8; 16];
    let mut output_len = output.len();
    // SAFETY: a null input with a length is detected before it is read.
    let status =
        unsafe { mbrotli_compress(ptr::null(), 4, output.as_mut_ptr(), &mut output_len, 5, 22) };
    assert_eq!(status, MbrotliResult::InvalidParameter);
    assert_eq!(output_len, 0);

    let mut output_len = 16;
    // SAFETY: a null output with a capacity is detected before it is written.
    let status =
        unsafe { mbrotli_decompress([0x3b].as_ptr(), 1, ptr::null_mut(), &mut output_len) };
    assert_eq!(status, MbrotliResult::InvalidParameter);
    assert_eq!(output_len, 0);
}

#[test]
fn lengths_no_allocation_can_have_are_rejected() {
    let input = [0u8; 4];
    let mut output = [0u8; 16];
    let mut output_len = output.len();
    // SAFETY: the impossible length is detected before `input` is read.
    let status = unsafe {
        mbrotli_compress(
            input.as_ptr(),
            usize::MAX,
            output.as_mut_ptr(),
            &mut output_len,
            5,
            22,
        )
    };
    assert_eq!(status, MbrotliResult::InvalidParameter);

    let mut output_len = usize::MAX / 2 + 1;
    // SAFETY: the impossible capacity is detected before `output` is written.
    let status =
        unsafe { mbrotli_decompress([0x3b].as_ptr(), 1, output.as_mut_ptr(), &mut output_len) };
    assert_eq!(status, MbrotliResult::InvalidParameter);
    assert_eq!(output_len, 0);
}

#[test]
fn overlapping_input_and_output_are_rejected() {
    let mut buffer = [0u8; 64];
    let base = buffer.as_mut_ptr();
    let mut output_len = 32;
    // SAFETY: the overlap is detected before either buffer is touched.
    let status =
        unsafe { mbrotli_compress(base, 40, base.wrapping_add(8), &mut output_len, 5, 22) };
    assert_eq!(status, MbrotliResult::InvalidParameter);
    assert_eq!(output_len, 0);

    // Adjacent halves share no byte and are accepted.
    let mut output_len = 32;
    // SAFETY: the halves are disjoint and live.
    let status =
        unsafe { mbrotli_compress(base, 32, base.wrapping_add(32), &mut output_len, 5, 22) };
    assert_eq!(status, MbrotliResult::Ok);
}

#[test]
fn a_buffer_overlapping_the_output_length_is_rejected() {
    let mut words = [0usize; 4];
    words[0] = 3 * size_of::<usize>();
    let length = words.as_mut_ptr();
    let bytes = length.cast::<u8>();
    // SAFETY: the output region covers `words[0]`; rejected before any write.
    let status = unsafe { mbrotli_decompress([0x3b].as_ptr(), 1, bytes, length) };
    assert_eq!(status, MbrotliResult::InvalidParameter);
    assert_eq!(words[0], 0);

    words[0] = 8;
    // SAFETY: the input region covers `words[0]`; rejected before any write.
    let status = unsafe {
        mbrotli_compress(
            bytes,
            size_of::<usize>(),
            bytes.wrapping_add(size_of::<usize>()),
            length,
            5,
            22,
        )
    };
    assert_eq!(status, MbrotliResult::InvalidParameter);
    assert_eq!(words[0], 0);
}

#[test]
fn decompress_reports_a_short_buffer_and_keeps_the_prefix() {
    let input = &ALICE[..2_000];
    let compressed = google_compress(input, 9, 22);
    let (status, len, output) = decompress(&compressed, 100);
    assert_eq!(status, MbrotliResult::OutputTooSmall);
    assert_eq!(len, 0);
    assert_eq!(output.as_slice(), &input[..100]);

    let (status, len, _) = decompress(&compressed, input.len());
    assert_eq!((status, len), (MbrotliResult::Ok, input.len()));
}

#[test]
fn decompress_rejects_corrupt_truncated_and_trailing_input() {
    let compressed = google_compress(&ALICE[..2_000], 9, 22);
    let truncated = &compressed[..compressed.len() - 1];
    let mut trailing = compressed.clone();
    trailing.push(0);
    for input in [&[0xff, 0xff, 0xff][..], &[][..], truncated, &trailing] {
        let (status, len, _) = decompress(input, 4_000);
        assert_eq!(status, MbrotliResult::Error, "{input:?}");
        assert_eq!(len, 0);
    }
}

#[test]
fn decompress_of_an_empty_stream_writes_nothing() {
    let mut output_len = 0;
    // SAFETY: `[0x3b]` is live; the empty output is never written.
    let status =
        unsafe { mbrotli_decompress([0x3b].as_ptr(), 1, ptr::null_mut(), &mut output_len) };
    assert_eq!((status, output_len), (MbrotliResult::Ok, 0));
}

#[test]
fn concurrent_calls_do_not_interfere() {
    let input = &ALICE[..20_000];
    let expected = google_compress(input, 5, 22);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                for _ in 0..4 {
                    let (status, compressed) =
                        compress(input, mbrotli_compress_bound(input.len()), 5, 22);
                    assert_eq!(status, MbrotliResult::Ok);
                    assert_eq!(compressed, expected);
                    let (status, len, output) = decompress(&compressed, input.len());
                    assert_eq!(status, MbrotliResult::Ok);
                    assert_eq!(&output[..len], input);
                }
            });
        }
    });
}

#[test]
fn status_codes_match_the_c_header() {
    let header = include_str!("../include/mbrotli.h");
    for (name, status) in [
        ("MBROTLI_OK", MbrotliResult::Ok),
        ("MBROTLI_INVALID_PARAMETER", MbrotliResult::InvalidParameter),
        ("MBROTLI_OUTPUT_TOO_SMALL", MbrotliResult::OutputTooSmall),
        ("MBROTLI_ERROR", MbrotliResult::Error),
    ] {
        assert!(
            header.contains(&format!("{name} = {}", status as c_int)),
            "{name}"
        );
    }
    assert_eq!(size_of::<MbrotliResult>(), size_of::<c_int>());
}

#[test]
fn corruption_past_a_full_output_reports_the_output_first() {
    // Found by the `c_abi` AFL target: this stream emits output before its
    // corrupt tail, so an empty buffer fills before the corruption is seen.
    let stream = [0x07, 0xb2, 0xfc];
    let (status, len, _) = decompress(&stream, 0);
    assert_eq!((status, len), (MbrotliResult::OutputTooSmall, 0));
    let (status, len, _) = decompress(&stream, 1 << 16);
    assert_eq!((status, len), (MbrotliResult::Error, 0));
}
