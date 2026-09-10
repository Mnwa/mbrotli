//! Decoder grammar and session progress oracles with explicit resource budgets.

use crate::{Context, cap};
use mbrotli::{
    DecodeError, DecodeLimits, DecodeOperation, DecodeStreamConfig, DecoderConfig, DecoderStatus,
    Decompressor,
};

const MAX_OUTPUT: usize = 64 * 1024;

fn config() -> DecoderConfig {
    DecoderConfig::default().with_limits(
        DecodeLimits::default()
            .with_max_output_bytes(Some(MAX_OUTPUT as u64))
            .with_max_workspace_bytes(Some(8 * 1024 * 1024)),
    )
}

/// C-encoded payloads at qualities 0–11 must decode exactly, including streaming.
///
/// Uses the common six-byte parameter header and caps plaintext at 64 KiB so
/// successful decoding fits the same budgets as the arbitrary-byte targets.
/// One input selects one quality; seeds and regression inputs are shared with
/// the encoder's parameterized targets.
///
/// # Panics
///
/// Panics if C compression fails, Rust rejects the valid stream, the plaintext
/// differs, or chunked decoding violates its progress/equivalence oracle.
pub fn decode_roundtrip(ctx: &Context, input: &[u8]) {
    let case = crate::decode_case(input);
    let payload = &case.data[..case.data.len().min(MAX_OUTPUT)];
    let compressed = crate::c_compress_with(&case.config, payload);
    let mut decoder = Decompressor::builder(config())
        .with_backend(ctx.level)
        .build()
        .unwrap();
    let actual = decoder
        .decompress(&compressed)
        .expect("Rust must decode the bounded C-encoded payload");
    assert_eq!(
        actual, payload,
        "C-to-Rust round-trip changed the plaintext"
    );
    decode_streaming(ctx, &compressed);
}

/// Arbitrary compressed bytes reach the grammar directly; C success must not
/// become a Rust format failure. RFC windows beyond C's range are Rust-only.
pub fn decompress(ctx: &Context, data: &[u8]) {
    let data = cap(data);
    let mut decoder = Decompressor::builder(config())
        .with_backend(ctx.level)
        .build()
        .unwrap();
    let actual = decoder.decompress(data);
    if let Ok(expected) = &actual {
        // The bounded decode has proven the output size. Replay successful
        // streams without limits to exercise the complete-stored-member path
        // too, without exposing arbitrary input to unbounded expansion.
        let mut owned = Decompressor::builder(DecoderConfig::default())
            .with_backend(ctx.level)
            .build()
            .unwrap();
        assert_eq!(owned.decompress(data).unwrap(), *expected);
    }
    compare(
        actual,
        crate::decode_oracle::decode(data, MAX_OUTPUT, None),
        data.len(),
    );
}

fn compare(
    actual: Result<Vec<u8>, DecodeError>,
    oracle: crate::decode_oracle::Outcome,
    input_length: usize,
) {
    if let crate::decode_oracle::Outcome::Success { payload, consumed } = oracle {
        match actual {
            Ok(actual) => {
                assert_eq!(consumed, input_length);
                assert_eq!(actual, payload);
            }
            Err(DecodeError::TrailingData { offset }) => assert_eq!(offset, consumed as u64),
            Err(DecodeError::MemoryLimitExceeded { .. } | DecodeError::AllocationFailed) => {}
            Err(error) => panic!("C accepted a stream Rust rejected: {error}"),
        }
    }
}

/// Chunked sessions agree with one-shot decoding and preserve exact progress.
pub fn decode_streaming(ctx: &Context, data: &[u8]) {
    let data = cap(data);
    let mut decoder = Decompressor::builder(config())
        .with_backend(ctx.level)
        .build()
        .unwrap();
    let expected = decoder.decompress(data);
    let chunk = data.first().map_or(1, |value| usize::from(value % 31) + 1);
    let output_size = data.last().map_or(1, |value| usize::from(value % 31) + 1);
    let mut session = decoder.start(DecodeStreamConfig::default()).unwrap();
    let mut cursor = 0;
    let mut actual = Vec::new();
    for _ in 0..data.len() + MAX_OUTPUT + 2 {
        let end = (cursor + chunk).min(data.len());
        let operation = if end == data.len() {
            DecodeOperation::Finish
        } else {
            DecodeOperation::Process
        };
        let mut output = [0xa5; 32];
        match session.process(&data[cursor..end], &mut output[..output_size], operation) {
            Err(failure) => {
                assert!(failure.consumed <= end - cursor && failure.produced <= output_size);
                assert!(output[output_size..].iter().all(|&byte| byte == 0xa5));
                assert!(
                    expected.is_err(),
                    "session rejected successful one-shot stream"
                );
                return;
            }
            Ok(progress) => {
                assert!(progress.consumed <= end - cursor && progress.produced <= output_size);
                cursor += progress.consumed;
                actual.extend_from_slice(&output[..progress.produced]);
                assert_eq!(session.total_in(), cursor as u64);
                assert_eq!(session.total_out(), actual.len() as u64);
                assert!(actual.len() <= MAX_OUTPUT);
                if progress.status == DecoderStatus::Finished {
                    match expected {
                        Ok(expected) => {
                            assert_eq!(cursor, data.len());
                            assert_eq!(actual, expected);
                        }
                        Err(DecodeError::TrailingData { offset }) => {
                            assert_eq!(cursor as u64, offset)
                        }
                        Err(error) => panic!("session finished after one-shot failure: {error}"),
                    }
                    return;
                }
                assert!(
                    progress.consumed != 0 || progress.produced != 0,
                    "active decoder made no progress"
                );
            }
        }
    }
    panic!("decoder exceeded the progress bound");
}

/// Bounded raw prefix construction followed by arbitrary attached decoding.
pub fn decode_dictionary(ctx: &Context, data: &[u8]) {
    use mbrotli::dictionary::{DecodeDictionary, DecodeDictionaryLimits, DictionaryAttachment};
    let data = cap(data);
    let split = data.first().map_or(0, |&v| usize::from(v).min(data.len()));
    let (prefix, input) = data.split_at(split);
    let dictionary = DecodeDictionary::new(
        &[DictionaryAttachment::Raw(prefix)],
        DecodeDictionaryLimits {
            max_source_bytes: Some(128 * 1024),
            max_owned_bytes: Some(128 * 1024),
        },
    )
    .unwrap();
    let mut decoder = Decompressor::builder(config())
        .with_backend(ctx.level)
        .build()
        .unwrap();
    let first = decoder.decompress_with_dictionary(&dictionary, input);
    let second = decoder.decompress_with_dictionary(&dictionary, input);
    compare(
        decoder.decompress_with_dictionary(&dictionary, input),
        crate::decode_oracle::decode(
            input,
            MAX_OUTPUT,
            Some((google_brotli_ffi::BROTLI_SHARED_DICTIONARY_RAW, prefix)),
        ),
        input.len(),
    );
    match (first, second) {
        (Ok(a), Ok(b)) => assert_eq!(a, b),
        (Err(_), Err(_)) => {}
        _ => panic!("dictionary reuse changed success"),
    }
}

/// Serialized/custom representation is absent from stable production builds.
#[cfg(feature = "experimental")]
pub fn decode_serialized(ctx: &Context, data: &[u8]) {
    use mbrotli::dictionary::{DecodeDictionary, DecodeDictionaryLimits, DictionaryAttachment};
    let data = cap(data);
    let Some((&split, bytes)) = data.split_first() else {
        return;
    };
    let (source, input) = bytes.split_at(usize::from(split).min(bytes.len()));
    let dictionary = DecodeDictionary::new(
        &[DictionaryAttachment::Serialized(source)],
        DecodeDictionaryLimits {
            max_source_bytes: Some(128 * 1024),
            max_owned_bytes: Some(8 * 1024 * 1024),
        },
    );
    if let Ok(dictionary) = dictionary {
        let mut decoder = Decompressor::builder(config())
            .with_backend(ctx.level)
            .build()
            .unwrap();
        let actual = decoder.decompress_with_dictionary(&dictionary, input);
        compare(
            actual,
            crate::decode_oracle::decode(
                input,
                MAX_OUTPUT,
                Some((
                    google_brotli_ffi::BROTLI_SHARED_DICTIONARY_SERIALIZED,
                    source,
                )),
            ),
            input.len(),
        );
    }
}

/// Abandoned/error states cannot contaminate later operations or hide budgets.
pub fn decode_lifecycle(ctx: &Context, data: &[u8]) {
    let mut decoder = Decompressor::builder(config())
        .with_backend(ctx.level)
        .build()
        .unwrap();
    for &operation in cap(data).iter().take(256) {
        match operation % 5 {
            0 => {
                let mut session = decoder.start(DecodeStreamConfig::default()).unwrap();
                let _ = session.process(&[operation], &mut [0; 3], DecodeOperation::Process);
            }
            1 => {
                let session = decoder.start(DecodeStreamConfig::default()).unwrap();
                std::mem::forget(session);
                assert!(matches!(
                    decoder.decompress(&[0x3b]),
                    Err(DecodeError::AbandonedSession)
                ));
                decoder.recover();
            }
            2 => {
                let _ = decoder.decompress(&[operation]);
            }
            3 => decoder.trim(mbrotli::RetentionPolicy::ReleaseAll),
            _ => decoder.reconfigure(config()).unwrap(),
        }
        assert!(decoder.decompress(&[0x3b]).unwrap().is_empty());
    }
}

/// A single retryable sink failure never duplicates accepted input or payload.
pub fn decode_io_limits(ctx: &Context, data: &[u8]) {
    use std::io::{self, Write};
    struct Sink {
        bytes: Vec<u8>,
        fault: Option<usize>,
    }
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.fault.is_some_and(|offset| self.bytes.len() >= offset) {
                self.fault = None;
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let count = bytes.len().min(7).min(
                self.fault
                    .map_or(usize::MAX, |offset| offset - self.bytes.len()),
            );
            self.bytes.extend_from_slice(&bytes[..count]);
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let data = cap(data);
    let limit = data.first().map_or(0, |&value| u64::from(value) * 257);
    let config = config().with_limits(config().limits().with_max_output_bytes(Some(limit)));
    let mut decoder = Decompressor::builder(config)
        .with_backend(ctx.level)
        .build()
        .unwrap();
    let expected = decoder.decompress(data);
    let sink = Sink {
        bytes: Vec::new(),
        fault: Some(data.last().map_or(0, |&value| usize::from(value))),
    };
    let mut writer = decoder.writer(sink, DecodeStreamConfig::default()).unwrap();
    let mut cursor = 0;
    for _ in 0..data.len() + MAX_OUTPUT + 10 {
        let result = if cursor == data.len() {
            writer.try_finish().map(|()| 0)
        } else {
            writer.write(&data[cursor..])
        };
        match result {
            Ok(count) => {
                cursor += count;
                if writer.is_finished() {
                    assert_eq!(writer.get_ref().bytes, expected.unwrap());
                    return;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(_) => {
                assert!(expected.is_err());
                return;
            }
        }
    }
    panic!("writer exceeded its progress bound");
}
