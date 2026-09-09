#![cfg(feature = "compression")]
//! Exercises the alloc-backed API with the library's std-only surface removed.
#![cfg(feature = "no_std")]

mod support;

use mbrotli::dictionary::DictionaryBuilder;
use mbrotli::{
    Backend, Compressor, EncodeError, EncoderConfig, EncoderStatus, InputSize, Operation, Quality,
};
use support::{CParams, c_compress, c_compress_with_prefixes, c_decompress_with_prefixes};

#[test]
fn every_quality_preserves_slice_session_and_reference_output() {
    let payload = b"alloc-backed compression with repeated bytes ".repeat(100);
    for quality in 0..=11 {
        let mut encoder = Compressor::new(
            EncoderConfig::default().with_quality(Quality::try_from(quality).expect("quality")),
        )
        .expect("configuration");
        for input in [&[][..], &payload[..1], &payload[..31], payload.as_slice()] {
            let expected = encoder.compress(input).expect("compress");
            assert_eq!(expected, c_compress(quality.into(), 22, input));
            let mut destination = vec![0; Compressor::max_compressed_size(input.len()).unwrap()];
            let written = encoder.compress_to_slice(input, &mut destination).unwrap();
            assert_eq!(expected, destination[..written]);

            let mut session = encoder
                .start(InputSize::Exact(input.len() as u64).into())
                .unwrap();
            let mut streamed = Vec::new();
            for chunk in input.chunks(17) {
                let mut remaining = chunk;
                while !remaining.is_empty() {
                    let mut output = [0; 7];
                    let progress = session
                        .process(remaining, &mut output, Operation::Process)
                        .unwrap();
                    remaining = &remaining[progress.consumed..];
                    streamed.extend_from_slice(&output[..progress.produced]);
                    assert!(progress.consumed > 0 || progress.produced > 0);
                }
            }
            loop {
                let mut output = [0; 7];
                let progress = session
                    .process(&[], &mut output, Operation::Finish)
                    .unwrap();
                streamed.extend_from_slice(&output[..progress.produced]);
                if progress.status == EncoderStatus::Finished {
                    break;
                }
                assert!(progress.produced > 0);
            }
            assert_eq!(expected, streamed);
        }
    }
}

#[test]
fn prepared_prefixes_remain_compatible_with_the_reference() {
    let prefix = b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n";
    let dictionary = DictionaryBuilder::default()
        .add_prefix(&prefix[..])
        .build()
        .unwrap();
    let payload = b"Content-Type: text/html; charset=utf-8\r\n".repeat(30);
    for quality in [Quality::Q5, Quality::Q9, Quality::Q11] {
        let mut encoder = Compressor::new(EncoderConfig::default().with_quality(quality)).unwrap();
        let encoded = encoder
            .compress_with_dictionary(&dictionary, &payload)
            .unwrap();
        let mut params = CParams::new(quality.get().into(), 22);
        params.size_hint = Some(payload.len() as u32);
        assert_eq!(
            encoded,
            c_compress_with_prefixes(params, &[prefix], &payload)
        );
        assert_eq!(
            c_decompress_with_prefixes(&[prefix], &encoded, payload.len()),
            Some(payload.clone())
        );
    }
}

#[test]
fn errors_keep_core_error_traits_and_compressors_recover() {
    let mut encoder = Compressor::new(EncoderConfig::default().with_quality(Quality::Q5)).unwrap();
    let error = encoder.compress_to_slice(b"payload", &mut []).unwrap_err();
    assert!(matches!(error, EncodeError::OutputTooSmall { provided: 0 }));
    let error: &dyn core::error::Error = &error;
    assert!(!error.to_string().is_empty());
    assert!(!encoder.compress(b"payload").unwrap().is_empty());
    assert!(Backend::available().contains(&Backend::default()));
}

#[test]
fn flushing_sessions_match_c_with_small_output_buffers() {
    let first = b"first block of repeated input ".repeat(30);
    let last = b"last block of repeated input ".repeat(20);
    let chunks = [first.as_slice(), last.as_slice()];
    for quality in [Quality::Q0, Quality::Q1, Quality::Q5, Quality::Q11] {
        let mut encoder = Compressor::new(EncoderConfig::default().with_quality(quality)).unwrap();
        let mut encoded = Vec::new();
        {
            let mut session = encoder.start(Default::default()).unwrap();
            for (mut input, operation) in [
                (chunks[0], Operation::Flush),
                (chunks[1], Operation::Finish),
            ] {
                loop {
                    let mut output = [0; 7];
                    let progress = session.process(input, &mut output, operation).unwrap();
                    input = &input[progress.consumed..];
                    encoded.extend_from_slice(&output[..progress.produced]);
                    if matches!(
                        progress.status,
                        EncoderStatus::NeedsInput | EncoderStatus::Finished
                    ) {
                        assert!(input.is_empty());
                        assert_eq!(
                            progress.status == EncoderStatus::Finished,
                            operation == Operation::Finish
                        );
                        break;
                    }
                    assert!(progress.consumed > 0 || progress.produced > 0);
                }
            }
        }
        assert_eq!(
            encoded,
            support::c_compress_flushing(CParams::new(quality.get().into(), 22), &chunks)
        );
        assert_eq!(
            support::c_decompress(&encoded, first.len() + last.len()),
            Some(chunks.concat())
        );
        assert!(
            !encoder
                .compress(b"reused after flushing")
                .unwrap()
                .is_empty()
        );
    }
}
