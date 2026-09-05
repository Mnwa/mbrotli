//! Headerless continuation streams, compared against the pinned C encoder.
#![cfg(feature = "experimental")]
mod support;
use mbrotli::{Compressor, EncoderConfig, EncoderStatus, Operation, Quality, StreamConfig};

fn c_stream(input: &[u8], quality: Quality, offset: u32, finish: bool) -> Vec<u8> {
    use google_brotli_ffi as ffi;
    let mut output = vec![0; input.len() * 2 + 8192];
    // SAFETY: all buffers have their advertised lengths and outlive the C
    // encoder. The state is freed after the output has been copied into them.
    unsafe {
        let state = ffi::BrotliEncoderCreateInstance(None, None, std::ptr::null_mut());
        assert!(!state.is_null());
        for (parameter, value) in [
            (ffi::BROTLI_PARAM_QUALITY, u32::from(quality.get())),
            (ffi::BROTLI_PARAM_STREAM_OFFSET, offset),
        ] {
            assert_eq!(
                ffi::BrotliEncoderSetParameter(state, parameter, value),
                ffi::BROTLI_TRUE
            );
        }
        let mut available_in = input.len();
        let mut next_in = input.as_ptr();
        let mut available_out = output.len();
        let mut next_out = output.as_mut_ptr();
        let mut total = 0;
        let operation = if finish {
            ffi::BROTLI_OPERATION_FINISH
        } else {
            ffi::BROTLI_OPERATION_FLUSH
        };
        assert_eq!(
            ffi::BrotliEncoderCompressStream(
                state,
                operation,
                &raw mut available_in,
                &raw mut next_in,
                &raw mut available_out,
                &raw mut next_out,
                &raw mut total
            ),
            ffi::BROTLI_TRUE
        );
        assert_eq!(available_in, 0);
        ffi::BrotliEncoderDestroyInstance(state);
        output.truncate(total);
    }
    output
}

#[test]
fn nonzero_offsets_match_c_without_fabricating_history() {
    let prefix = b"the preceding resource has real bytes".repeat(3);
    let input = b"the continuation has dictionary words and repeated repeated repeated content. "
        .repeat(100);
    for quality in [
        Quality::Q2,
        Quality::Q3,
        Quality::Q5,
        Quality::Q9,
        Quality::Q10,
        Quality::Q11,
    ] {
        let expected = c_stream(&input, quality, prefix.len() as u32, true);
        let mut compressor =
            Compressor::new(EncoderConfig::default().with_quality(quality)).expect("config");
        for output_size in [1, 31, 8192] {
            let mut session = compressor
                .start(StreamConfig::default().with_stream_offset(prefix.len() as u64))
                .expect("continuation");
            let mut encoded = Vec::new();
            let mut consumed = 0;
            let mut output = vec![0; output_size];
            loop {
                let progress = session
                    .process(&input[consumed..], &mut output, Operation::Finish)
                    .expect("encode");
                consumed += progress.consumed;
                encoded.extend_from_slice(&output[..progress.produced]);
                if progress.status == EncoderStatus::Finished {
                    break;
                }
                assert!(progress.consumed != 0 || progress.produced != 0);
            }
            assert_eq!(encoded, expected, "{quality:?}, output {output_size}");
            let mut joined = c_stream(&prefix, quality, 0, false);
            joined.extend_from_slice(&encoded);
            let mut plain = prefix.clone();
            plain.extend_from_slice(&input);
            assert_eq!(support::c_decompress(&joined, plain.len()), Some(plain));
        }
        assert!(compressor.compress(b"a fresh independent stream").is_ok());
    }
}

#[test]
fn logical_position_overflow_is_rejected_before_consuming_input() {
    let mut compressor =
        Compressor::new(EncoderConfig::default().with_quality(Quality::Q5)).expect("config");
    let stream = StreamConfig::default().with_stream_offset((1 << 63) - 1);
    assert!(matches!(
        compressor.start(stream.with_input_size(mbrotli::InputSize::Exact(1))),
        Err(mbrotli::EncodeError::StreamPositionOverflow { .. })
    ));
    let mut session = compressor.start(stream).expect("maximum position");
    assert!(matches!(
        session.process(b"x", &mut [0; 10], Operation::Finish),
        Err(mbrotli::EncodeError::StreamPositionOverflow { .. })
    ));
}

#[test]
fn a_finished_continuation_ignores_input_even_at_the_position_limit() {
    let mut compressor =
        Compressor::new(EncoderConfig::default().with_quality(Quality::Q5)).expect("config");
    let mut session = compressor
        .start(StreamConfig::default().with_stream_offset((1 << 63) - 1))
        .expect("position");
    let progress = session
        .process(&[], &mut [0; 16], Operation::Finish)
        .expect("finish");
    assert_eq!(progress.status, mbrotli::EncoderStatus::Finished);
    let again = session
        .process(b"ignored", &mut [0; 16], Operation::Finish)
        .expect("idempotent finish");
    assert_eq!(again.consumed, 0);
    assert_eq!(again.produced, 0);
    assert_eq!(again.status, mbrotli::EncoderStatus::Finished);
}

#[test]
fn continuation_flint_handles_empty_tiny_and_split_input_on_every_backend() {
    use std::io::Write;
    for quality in [Quality::Q2, Quality::Q5, Quality::Q10, Quality::Q11] {
        for input in [
            b"".as_slice(),
            b"a",
            b"ab",
            b"abc",
            b"repeated dictionary words",
        ] {
            let expected = c_stream(input, quality, 31, true);
            for (name, level) in support::host_levels() {
                let mut compressor = support::encoder_on(level, quality, 22);
                let mut output = Vec::new();
                {
                    let mut writer = compressor
                        .writer(&mut output, StreamConfig::default().with_stream_offset(31))
                        .expect("writer");
                    for byte in input.chunks(1) {
                        writer.write_all(byte).expect("byte");
                    }
                    writer.try_finish().expect("finish");
                }
                assert_eq!(output, expected, "{quality:?}, {name}, {input:?}");
            }
        }
    }
}
