//! Streaming adapters: chunk boundaries must not change the stream.

mod support;

use mbrotli::io::FinishError;
use mbrotli::{Compressor, EncoderStatus, InputSize, Operation, StreamConfig};
use std::io::{Read, Write};
use support::{IMPLEMENTED_QUALITIES, c_decompress, encoder, structural_corpora};

/// Compresses `data` through the writer adapter using fixed-size chunks.
fn compress_with_writer(
    encoder: &mut Compressor,
    data: &[u8],
    chunk: usize,
    stream: StreamConfig,
) -> Vec<u8> {
    let mut sink = encoder.writer(Vec::new(), stream).expect("a legal stream");
    for piece in data.chunks(chunk.max(1)) {
        sink.write_all(piece).expect("write failed");
    }
    sink.finish()
        .map_err(FinishError::into_error)
        .expect("finish failed")
}

/// Compresses `data` through the reader adapter using fixed-size reads.
fn compress_with_reader(
    encoder: &mut Compressor,
    data: &[u8],
    chunk: usize,
    stream: StreamConfig,
) -> Vec<u8> {
    let mut source = encoder.reader(data, stream).expect("a legal stream");
    let mut output = Vec::new();
    let mut buffer = vec![0u8; chunk.max(1)];
    loop {
        let count = source.read(&mut buffer).expect("read failed");
        if count == 0 {
            return output;
        }
        output.extend_from_slice(&buffer[..count]);
    }
}

/// Compresses `data` through the low-level session in fixed-size steps.
fn compress_with_session(
    encoder: &mut Compressor,
    data: &[u8],
    chunk: usize,
    stream: StreamConfig,
) -> Vec<u8> {
    let mut compressed = Vec::new();
    let mut buffer = vec![0u8; chunk.max(1)];
    let mut session = encoder.start(stream).expect("a legal stream");
    let mut offset = 0usize;
    loop {
        let take = (data.len() - offset).min(chunk.max(1));
        // Only the call that carries the last of the input may finish; asking
        // to finish early would terminate the stream on a prefix.
        let operation = if offset + take == data.len() {
            Operation::Finish
        } else {
            Operation::Process
        };
        let progress = session
            .process(&data[offset..offset + take], &mut buffer, operation)
            .expect("the session failed");
        offset += progress.consumed;
        compressed.extend_from_slice(&buffer[..progress.produced]);
        if progress.status == EncoderStatus::Finished {
            assert!(session.is_finished());
            return compressed;
        }
    }
}

#[test]
fn writer_output_is_independent_of_the_chunk_size() {
    let data: Vec<u8> = (0..200_000u32).map(|i| (i % 61) as u8).collect();
    for quality in IMPLEMENTED_QUALITIES {
        let mut encoder = encoder(quality, 16);
        let stream = StreamConfig::default();
        let reference = compress_with_writer(&mut encoder, &data, data.len(), stream);
        for chunk in [1usize, 3, 1024, 65_536, 65_537, 131_072] {
            let actual = compress_with_writer(&mut encoder, &data, chunk, stream);
            assert_eq!(
                actual,
                reference,
                "chunk {chunk}, quality {}",
                quality.get()
            );
        }
    }
}

#[test]
fn reader_output_is_independent_of_the_read_size() {
    let data: Vec<u8> = (0..200_000u32).map(|i| (i % 61) as u8).collect();
    for quality in IMPLEMENTED_QUALITIES {
        let mut encoder = encoder(quality, 16);
        let stream = StreamConfig::default();
        let reference = compress_with_reader(&mut encoder, &data, 1 << 20, stream);
        for chunk in [1usize, 7, 4096, 65_536] {
            let actual = compress_with_reader(&mut encoder, &data, chunk, stream);
            assert_eq!(
                actual,
                reference,
                "chunk {chunk}, quality {}",
                quality.get()
            );
        }
    }
}

#[test]
fn every_streaming_shape_agrees_with_the_others() {
    for corpus in structural_corpora() {
        for quality in IMPLEMENTED_QUALITIES {
            let mut encoder = encoder(quality, 18);
            let stream = StreamConfig::default();
            let written = compress_with_writer(&mut encoder, &corpus.data, 4096, stream);
            let read = compress_with_reader(&mut encoder, &corpus.data, 4096, stream);
            let session = compress_with_session(&mut encoder, &corpus.data, 4096, stream);
            assert_eq!(written, read, "case {}: writer and reader", corpus.name);
            assert_eq!(written, session, "case {}: writer and session", corpus.name);
        }
    }
}

#[test]
fn a_one_byte_schedule_reaches_the_same_stream() {
    // One byte in and one byte out at every step: the state machine has to make
    // progress without ever having room for a whole meta-block.
    let data = b"the quick brown fox jumps over the lazy dog. ".repeat(40);
    for quality in IMPLEMENTED_QUALITIES {
        let mut encoder = encoder(quality, 16);
        let stream = StreamConfig::default();
        let reference = compress_with_session(&mut encoder, &data, 1 << 20, stream);
        let crawled = compress_with_session(&mut encoder, &data, 1, stream);
        assert_eq!(crawled, reference, "quality {}", quality.get());
    }
}

#[test]
fn streaming_matches_one_shot_when_the_size_is_declared() {
    for corpus in structural_corpora() {
        for quality in IMPLEMENTED_QUALITIES {
            let mut encoder = encoder(quality, 18);
            let one_shot = encoder.compress(&corpus.data).expect("compression failed");
            // Declaring the size is what makes the two agree: qualities four and
            // five choose their match finder from it.
            let stream = StreamConfig::from(InputSize::Exact(corpus.data.len() as u64));
            for streamed in [
                compress_with_writer(&mut encoder, &corpus.data, 4096, stream),
                compress_with_reader(&mut encoder, &corpus.data, 4096, stream),
                compress_with_session(&mut encoder, &corpus.data, 4096, stream),
            ] {
                assert_eq!(
                    streamed,
                    one_shot,
                    "case {}, quality {}",
                    corpus.name,
                    quality.get()
                );
            }
        }
    }
}

#[test]
fn universal_empty_output_intentionally_differs_from_native_c_one_shot() {
    for quality in IMPLEMENTED_QUALITIES {
        let mut compressor = encoder(quality, 22);
        let canonical = compressor.compress(&[]).expect("canonical empty stream");
        let native = support::c_compress_native_one_shot(quality.get().into(), 22, &[]);
        assert_ne!(
            canonical,
            native,
            "q{} must not use C's shortcut",
            quality.get()
        );
        assert_eq!(c_decompress(&canonical, 0), Some(Vec::new()));
        assert_eq!(c_decompress(&native, 0), Some(Vec::new()));
    }
}

#[test]
fn incompressible_small_window_streams_are_identical_across_all_api_shapes() {
    let mut state = 0x1234_5678u32;
    let data: Vec<u8> = (0..8193)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect();
    for quality in IMPLEMENTED_QUALITIES {
        let mut compressor = encoder(quality, 10);
        for data in [&[][..], &data[..1], &data[..1024], data.as_slice()] {
            let stream = InputSize::Exact(data.len() as u64).into();
            let expected = compress_with_session(&mut compressor, data, 97, stream);
            let one_shot = compressor.compress(data).expect("one shot");
            assert_eq!(one_shot, expected, "q{} len{}", quality.get(), data.len());
            let mut appended = b"prefix".to_vec();
            let range = compressor
                .compress_into(data, &mut appended)
                .expect("append");
            assert_eq!(&appended[..range.start], b"prefix");
            assert_eq!(&appended[range], expected);
            let mut exact = vec![0; expected.len()];
            let written = compressor
                .compress_to_slice(data, &mut exact)
                .expect("exact slice");
            assert_eq!(written, expected.len());
            assert_eq!(exact, expected);
            let mut short = vec![0; expected.len() - 1];
            assert!(matches!(
                compressor.compress_to_slice(data, &mut short),
                Err(mbrotli::EncodeError::OutputTooSmall { .. })
            ));
            for chunk in [1, 1024, 8193] {
                assert_eq!(
                    compress_with_writer(&mut compressor, data, chunk, stream),
                    expected
                );
                assert_eq!(
                    compress_with_reader(&mut compressor, data, chunk, stream),
                    expected
                );
                assert_eq!(
                    compress_with_session(&mut compressor, data, chunk, stream),
                    expected
                );
            }
        }
    }
}

#[test]
fn an_undeclared_size_still_round_trips() {
    for corpus in structural_corpora() {
        for quality in IMPLEMENTED_QUALITIES {
            let mut encoder = encoder(quality, 16);
            for chunk in [1usize, 4096] {
                if chunk == 1 && corpus.data.len() > 4096 {
                    continue;
                }
                let compressed = compress_with_writer(
                    &mut encoder,
                    &corpus.data,
                    chunk,
                    StreamConfig::default(),
                );
                let decoded = c_decompress(&compressed, corpus.data.len()).unwrap_or_else(|| {
                    panic!("case {}: the decoder rejected the stream", corpus.name)
                });
                assert_eq!(decoded, corpus.data, "case {}", corpus.name);
            }
        }
    }
}

#[test]
fn empty_streams_are_valid() {
    for quality in IMPLEMENTED_QUALITIES {
        let mut encoder = encoder(quality, 22);
        let stream = StreamConfig::default();
        let written = compress_with_writer(&mut encoder, &[], 1, stream);
        assert!(!written.is_empty());
        assert_eq!(c_decompress(&written, 1), Some(Vec::new()));

        let read = compress_with_reader(&mut encoder, &[], 1, stream);
        assert_eq!(read, written);

        let session = compress_with_session(&mut encoder, &[], 1, stream);
        assert_eq!(session, written);
    }
}

#[test]
fn a_finished_session_stays_finished() {
    let mut encoder = encoder(IMPLEMENTED_QUALITIES[1], 22);
    let mut buffer = [0u8; 512];
    let mut session = encoder
        .start(StreamConfig::default())
        .expect("a legal stream");

    let first = session
        .process(b"payload payload", &mut buffer, Operation::Finish)
        .expect("the session failed");
    assert_eq!(first.status, EncoderStatus::Finished);
    assert!(first.produced > 0);

    for _ in 0..3 {
        let again = session
            .process(b"ignored", &mut buffer, Operation::Finish)
            .expect("a finished session must stay usable");
        assert_eq!(again.consumed, 0);
        assert_eq!(again.produced, 0);
        assert_eq!(again.status, EncoderStatus::Finished);
    }
}

#[test]
fn a_session_never_reports_progress_it_did_not_make() {
    // Zero consumed and zero produced is only allowed alongside a status that
    // explains why, which is what stops a caller from spinning.
    let mut encoder = encoder(IMPLEMENTED_QUALITIES[5], 22);
    let mut session = encoder
        .start(StreamConfig::default())
        .expect("a legal stream");
    let mut nothing: [u8; 0] = [];

    let idle = session
        .process(&[], &mut nothing, Operation::Process)
        .expect("the session failed");
    assert_eq!((idle.consumed, idle.produced), (0, 0));
    assert_eq!(idle.status, EncoderStatus::NeedsInput);

    // With no room at all, finishing has to ask for output rather than claim it.
    let cramped = session
        .process(b"payload", &mut nothing, Operation::Finish)
        .expect("the session failed");
    assert_eq!(cramped.produced, 0);
    assert_eq!(cramped.status, EncoderStatus::NeedsOutput);
}

#[test]
fn a_reader_yields_nothing_for_a_zero_length_buffer() {
    let mut encoder = encoder(IMPLEMENTED_QUALITIES[1], 22);
    let mut source = encoder
        .reader(&b"payload"[..], StreamConfig::default())
        .expect("a legal stream");
    let mut empty: [u8; 0] = [];
    assert_eq!(source.read(&mut empty).expect("read failed"), 0);
}

#[test]
fn a_reader_hands_back_what_it_read_ahead() {
    let payload = b"a payload long enough to be read ahead of".repeat(50);
    let mut encoder = encoder(IMPLEMENTED_QUALITIES[5], 22);
    let mut source = encoder
        .reader(payload.as_slice(), StreamConfig::default())
        .expect("a legal stream");

    let mut head = [0u8; 8];
    let taken = source.read(&mut head).expect("read failed");
    assert!(taken <= head.len());
    let parts = source.into_parts();

    // Whatever the source produced but the encoder had not accepted comes back
    // rather than being dropped on the floor.
    assert!(parts.buffered_input.len() <= payload.len());
    if !parts.buffered_input.is_empty() {
        assert!(
            payload
                .windows(parts.buffered_input.len())
                .any(|window| window == parts.buffered_input.as_slice())
        );
    }
}
