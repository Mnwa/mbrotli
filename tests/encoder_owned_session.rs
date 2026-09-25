#![cfg(feature = "compression")]
//! `EncoderSessionOwned` against the borrowed `EncoderSession` it mirrors.
//!
//! Every scenario is driven through both session shapes by the same schedule;
//! the owned one must reproduce the borrowed one's bytes and per-call progress
//! exactly, since the two share one state machine and differ only in who owns
//! the compressor.

mod support;

use mbrotli::dictionary::{DictionaryBuilder, PreparedDictionary};
use mbrotli::{
    Compressor, EncodeError, EncoderSession, EncoderSessionOwned, EncoderStatus, InputSize,
    Operation, Progress, Quality, StreamConfig,
};
use std::sync::Arc;
use support::{Rng, c_decompress, c_decompress_partial, config, encoder, prefix_for};

/// The one call both session shapes answer, so a schedule can drive either.
trait Session {
    fn step(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        operation: Operation,
    ) -> Result<Progress, EncodeError>;
    fn finished(&self) -> bool;
    fn flush_now(&mut self, output: &mut [u8]) -> Result<Progress, EncodeError>;
    fn finish_now(&mut self, output: &mut [u8]) -> Result<Progress, EncodeError>;
}

impl Session for EncoderSession<'_, '_> {
    fn step(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        operation: Operation,
    ) -> Result<Progress, EncodeError> {
        self.process(input, output, operation)
    }
    fn finished(&self) -> bool {
        self.is_finished()
    }
    fn flush_now(&mut self, output: &mut [u8]) -> Result<Progress, EncodeError> {
        self.flush(output)
    }
    fn finish_now(&mut self, output: &mut [u8]) -> Result<Progress, EncodeError> {
        self.finish(output)
    }
}

impl<D: AsRef<PreparedDictionary> + 'static> Session for EncoderSessionOwned<D> {
    fn step(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        operation: Operation,
    ) -> Result<Progress, EncodeError> {
        self.process(input, output, operation)
    }
    fn finished(&self) -> bool {
        self.is_finished()
    }
    fn flush_now(&mut self, output: &mut [u8]) -> Result<Progress, EncodeError> {
        self.flush(output)
    }
    fn finish_now(&mut self, output: &mut [u8]) -> Result<Progress, EncodeError> {
        self.finish(output)
    }
}

/// How a stream is fed: input chunk size, output buffer size and flush cadence.
#[derive(Clone, Copy, Debug)]
struct Schedule {
    chunk: usize,
    output: usize,
    flush_every: Option<usize>,
}

/// What a schedule produced: the bytes, every call's progress, and the output
/// length at each completed flush.
#[derive(Debug, Default, PartialEq)]
struct Trace {
    bytes: Vec<u8>,
    calls: Vec<Progress>,
    flushes: Vec<(usize, usize)>,
}

fn call(
    session: &mut impl Session,
    trace: &mut Trace,
    input: &[u8],
    output: usize,
    operation: Operation,
) -> Progress {
    let mut buffer = vec![0; output];
    let progress = session
        .step(input, &mut buffer, operation)
        .expect("the stream encodes");
    trace.bytes.extend_from_slice(&buffer[..progress.produced]);
    trace.calls.push(progress);
    progress
}

fn drive(session: &mut impl Session, data: &[u8], schedule: Schedule) -> Trace {
    let mut trace = Trace::default();
    let mut accepted = 0;
    for (index, chunk) in data.chunks(schedule.chunk).enumerate() {
        let mut remaining = chunk;
        loop {
            let progress = call(
                session,
                &mut trace,
                remaining,
                schedule.output,
                Operation::Process,
            );
            assert!(progress.consumed != 0 || progress.produced != 0 || remaining.is_empty());
            remaining = &remaining[progress.consumed..];
            accepted += progress.consumed;
            if remaining.is_empty() && progress.status != EncoderStatus::NeedsOutput {
                break;
            }
        }
        if schedule
            .flush_every
            .is_some_and(|every| (index + 1) % every == 0)
        {
            while call(session, &mut trace, &[], schedule.output, Operation::Flush).status
                == EncoderStatus::NeedsOutput
            {}
            trace.flushes.push((accepted, trace.bytes.len()));
        }
    }
    while call(session, &mut trace, &[], schedule.output, Operation::Finish).status
        != EncoderStatus::Finished
    {}
    assert!(session.finished());
    trace
}

fn borrowed(quality: Quality, stream: StreamConfig, data: &[u8], schedule: Schedule) -> Trace {
    let mut compressor = encoder(quality, 22);
    let mut session = compressor.start(stream).expect("borrowed start");
    drive(&mut session, data, schedule)
}

fn owned(quality: Quality, stream: StreamConfig, data: &[u8], schedule: Schedule) -> Trace {
    let mut session = encoder(quality, 22)
        .into_session(stream)
        .expect("owned start");
    drive(&mut session, data, schedule)
}

fn payloads() -> Vec<(&'static str, Vec<u8>)> {
    let text = b"The owned session keeps the same bytes as the borrowed one. ".repeat(100);
    let mut rng = Rng::new(0x5eed);
    let mut large = Vec::with_capacity((1 << 20) + 17);
    while large.len() < (1 << 20) + 17 {
        large.extend_from_slice(&text[..usize::from(rng.next_u8()) + 1]);
        large.extend_from_slice(&rng.bytes(64, 16));
    }
    large.truncate((1 << 20) + 17);
    vec![
        ("empty", Vec::new()),
        ("tiny", b"x".to_vec()),
        ("several KiB", text),
        ("large", large),
    ]
}

const PARITY_QUALITIES: [Quality; 3] = [Quality::Q0, Quality::Q5, Quality::Q11];

const SCHEDULES: [Schedule; 4] = [
    Schedule {
        chunk: 7,
        output: 1,
        flush_every: None,
    },
    Schedule {
        chunk: 4093,
        output: 13,
        flush_every: None,
    },
    Schedule {
        chunk: 1000,
        output: 64,
        flush_every: Some(3),
    },
    Schedule {
        chunk: 65539,
        output: 4096,
        flush_every: Some(1),
    },
];

#[test]
fn owned_and_borrowed_sessions_emit_identical_bytes_and_progress() {
    for (name, payload) in payloads() {
        for quality in PARITY_QUALITIES {
            let data = prefix_for(quality, &payload);
            for stream in [
                StreamConfig::default(),
                InputSize::Exact(data.len() as u64).into(),
            ] {
                for schedule in SCHEDULES {
                    // Byte-at-a-time schedules cover the small payloads; the
                    // large ones are too slow to feed that way in debug builds.
                    if schedule.chunk < 64 && data.len() > 1 << 14 {
                        continue;
                    }
                    let context = format!("{name}, q{}, {stream:?}, {schedule:?}", quality.get());
                    let expected = borrowed(quality, stream, data, schedule);
                    let actual = owned(quality, stream, data, schedule);
                    assert_eq!(actual, expected, "{context}");
                    assert_eq!(
                        c_decompress(&actual.bytes, data.len()).as_deref(),
                        Some(data),
                        "{context}"
                    );
                }
            }
        }
    }
}

#[test]
fn an_owned_exact_size_stream_matches_one_shot_compression() {
    for (name, payload) in payloads() {
        for quality in PARITY_QUALITIES {
            let data = prefix_for(quality, &payload);
            let one_shot = encoder(quality, 22).compress(data).expect("one shot");
            let streamed = owned(
                quality,
                InputSize::Exact(data.len() as u64).into(),
                data,
                SCHEDULES[1],
            );
            assert_eq!(streamed.bytes, one_shot, "{name}, q{}", quality.get());
        }
    }
}

#[test]
fn an_owned_session_encodes_chunked_input_that_decodes() {
    let data = b"chunk after chunk after chunk ".repeat(300);
    let mut session = Compressor::new(config(Quality::Q5, 22))
        .expect("config")
        .into_session(StreamConfig::default())
        .expect("start");
    let trace = drive(
        &mut session,
        &data,
        Schedule {
            chunk: 257,
            output: 100,
            flush_every: None,
        },
    );
    assert_eq!(
        c_decompress(&trace.bytes, data.len()).as_deref(),
        Some(&data[..])
    );
}

/// Finishes `data` through a zero-byte buffer and then tiny ones.
fn finish_through_tiny_buffers(session: &mut impl Session, data: &[u8]) -> Trace {
    let mut trace = Trace::default();
    let first = call(session, &mut trace, data, 0, Operation::Finish);
    assert_eq!(first.produced, 0);
    assert_eq!(first.status, EncoderStatus::NeedsOutput);
    let mut remaining = &data[first.consumed..];
    for size in [0, 1, 2, 3].into_iter().cycle() {
        let progress = call(session, &mut trace, remaining, size, Operation::Finish);
        remaining = &remaining[progress.consumed..];
        if progress.status == EncoderStatus::Finished {
            break;
        }
        assert_eq!(progress.status, EncoderStatus::NeedsOutput);
        assert!(!session.finished());
    }
    assert!(remaining.is_empty());
    assert!(session.finished());
    let again = call(session, &mut trace, b"ignored", 16, Operation::Finish);
    assert_eq!(
        again,
        Progress {
            consumed: 0,
            produced: 0,
            status: EncoderStatus::Finished,
        }
    );
    trace
}

#[test]
fn a_repeated_finish_through_tiny_buffers_matches_the_borrowed_session() {
    let data = b"finish me one byte at a time ".repeat(150);
    for quality in PARITY_QUALITIES {
        let mut compressor = encoder(quality, 22);
        let expected = finish_through_tiny_buffers(
            &mut compressor.start(StreamConfig::default()).expect("start"),
            &data,
        );
        let actual = finish_through_tiny_buffers(
            &mut encoder(quality, 22)
                .into_session(StreamConfig::default())
                .expect("start"),
            &data,
        );
        assert_eq!(actual, expected, "q{}", quality.get());
        assert_eq!(
            c_decompress(&actual.bytes, data.len()).as_deref(),
            Some(&data[..])
        );
    }
}

#[test]
fn an_owned_flush_makes_everything_accepted_so_far_decodable() {
    let data = b"flushed prefixes decode on their own. ".repeat(200);
    let schedule = Schedule {
        chunk: 999,
        output: 37,
        flush_every: Some(2),
    };
    for quality in PARITY_QUALITIES {
        let trace = owned(quality, StreamConfig::default(), &data, schedule);
        assert_eq!(
            trace,
            borrowed(quality, StreamConfig::default(), &data, schedule)
        );
        assert!(!trace.flushes.is_empty());
        for &(accepted, written) in &trace.flushes {
            assert_eq!(
                c_decompress_partial(&trace.bytes[..written], data.len()).as_deref(),
                Some(&data[..accepted]),
                "q{}",
                quality.get()
            );
        }
    }
}

/// Encodes `data` in one owned stream and returns the compressor for reuse.
fn encode_reusing(compressor: Compressor, data: &[u8]) -> (Vec<u8>, Compressor) {
    let mut session = compressor
        .into_session(InputSize::Exact(data.len() as u64).into())
        .expect("a reused compressor starts");
    let trace = drive(&mut session, data, SCHEDULES[1]);
    (trace.bytes, session.into_compressor())
}

#[test]
fn a_returned_compressor_starts_the_next_stream_cleanly() {
    let data = b"reuse the compressor after every kind of ending ".repeat(80);
    for quality in PARITY_QUALITIES {
        let expected = encoder(quality, 22).compress(&data).expect("one shot");

        // Finished.
        let (first, compressor) = encode_reusing(encoder(quality, 22), &data);
        assert_eq!(first, expected);
        let (second, compressor) = encode_reusing(compressor, &data);
        assert_eq!(second, expected);

        // Unfinished: input staged, nothing terminated.
        let mut session = compressor
            .into_session(StreamConfig::default())
            .expect("start");
        session
            .process(&data, &mut [0; 5], Operation::Process)
            .expect("process");
        assert!(!session.is_finished());
        let mut compressor = session.into_compressor();
        assert_eq!(compressor.compress(&data).expect("one shot"), expected);

        // Flushed, then abandoned.
        let mut session = compressor
            .into_session(StreamConfig::default())
            .expect("start");
        let mut output = vec![0; 1 << 16];
        let progress = session
            .process(&data, &mut output, Operation::Flush)
            .expect("flush");
        assert_eq!(progress.status, EncoderStatus::NeedsInput);
        let mut compressor = session.into_compressor();
        let mut borrowed = compressor.start(StreamConfig::default()).expect("start");
        let progress = borrowed
            .process(&data, &mut output, Operation::Finish)
            .expect("finish");
        assert_eq!(progress.status, EncoderStatus::Finished);
        drop(borrowed);

        let (last, _) = encode_reusing(compressor, &data);
        assert_eq!(last, expected, "q{}", quality.get());
    }
}

#[test]
fn a_returned_compressor_applies_its_retention_policy() {
    let data = b"retention after an owned session ".repeat(200);
    let compressor = Compressor::builder(config(Quality::Q5, 22))
        .with_retention(mbrotli::RetentionPolicy::ReleaseAll)
        .build()
        .expect("config");
    let (_, compressor) = encode_reusing(compressor, &data);
    assert_eq!(compressor.retained_bytes(), 0);

    let (_, compressor) = encode_reusing(encoder(Quality::Q5, 22), &data);
    assert!(compressor.retained_bytes() > 0);
}

#[test]
fn owned_starts_reject_what_borrowed_starts_reject() {
    // A leaked borrowed session blocks the owned start too.
    let mut compressor = encoder(Quality::Q5, 22);
    std::mem::forget(compressor.start(StreamConfig::default()).expect("start"));
    assert!(matches!(
        compressor.into_session(StreamConfig::default()),
        Err(EncodeError::AbandonedSession)
    ));

    let dictionary = DictionaryBuilder::new()
        .add_prefix(&b"prefix"[..])
        .build()
        .expect("dictionary");
    assert!(matches!(
        encoder(Quality::Q1, 22).into_session_with_dictionary(dictionary, StreamConfig::default()),
        Err(EncodeError::DictionaryUnsupportedForQuality { .. })
    ));

    let offset = StreamConfig::default().with_stream_offset(64);
    assert!(matches!(
        encoder(Quality::Q1, 22).into_session(offset),
        Err(EncodeError::UnsupportedStreamOffset { offset: 64 })
    ));
}

/// Drives an owned dictionary session of any dictionary owner, returning the
/// compressor for reuse.
fn owned_with<D: AsRef<PreparedDictionary> + 'static>(
    quality: Quality,
    dictionary: D,
    data: &[u8],
) -> (Trace, Compressor) {
    let mut session = encoder(quality, 22)
        .into_session_with_dictionary(dictionary, StreamConfig::default())
        .expect("owned");
    let trace = drive(&mut session, data, SCHEDULES[1]);
    (trace, session.into_compressor())
}

#[test]
fn an_owned_dictionary_session_matches_the_borrowed_one_and_round_trips() {
    let prefix = b"a shared prefix both sides know about. ".repeat(20);
    let data = [&prefix[..100], b" and a tail that is new"].concat();
    let build = || {
        DictionaryBuilder::new()
            .add_prefix(&prefix[..])
            .build()
            .expect("dictionary")
    };
    let shared = Arc::new(build());
    let leaked: &'static PreparedDictionary = Box::leak(Box::new(build()));
    for quality in [Quality::Q5, Quality::Q11] {
        let mut compressor = encoder(quality, 22);
        let expected = drive(
            &mut compressor
                .start_with_dictionary(&shared, StreamConfig::default())
                .expect("borrowed"),
            &data,
            SCHEDULES[1],
        );
        let (by_arc, mut compressor) = owned_with(quality, Arc::clone(&shared), &data);
        let (by_value, _) = owned_with(quality, build(), &data);
        let (by_static, _) = owned_with(quality, leaked, &data);
        for actual in [&by_arc, &by_value, &by_static] {
            assert_eq!(actual, &expected, "q{}", quality.get());
        }
        assert_eq!(
            support::c_decompress_with_prefixes(&[&prefix], &by_arc.bytes, data.len()).as_deref(),
            Some(&data[..])
        );
        assert!(compressor.compress(&data).is_ok());
    }
    // The sessions released their clones of the shared dictionary.
    assert_eq!(Arc::strong_count(&shared), 1);
}

#[cfg(feature = "experimental")]
#[test]
fn a_returned_compressor_recovers_from_a_failed_process() {
    let stream = StreamConfig::default().with_stream_offset((1 << 63) - 1);
    let mut compressor = encoder(Quality::Q5, 22);
    let expected = {
        let mut session = compressor.start(stream).expect("borrowed");
        format!(
            "{:?}",
            session.process(b"x", &mut [0; 10], Operation::Finish)
        )
    };
    let mut session = compressor.into_session(stream).expect("owned");
    let failure = session.process(b"x", &mut [0; 10], Operation::Finish);
    assert!(matches!(
        failure,
        Err(EncodeError::StreamPositionOverflow { .. })
    ));
    assert_eq!(format!("{failure:?}"), expected);

    let mut compressor = session.into_compressor();
    let data = b"after the failure";
    assert_eq!(
        c_decompress(&compressor.compress(data).expect("one shot"), data.len()).as_deref(),
        Some(&data[..])
    );
}

#[test]
fn an_owned_session_moves_between_threads() {
    const fn assert_send<T: Send>() {}
    assert_send::<EncoderSessionOwned>();
    assert_send::<EncoderSessionOwned<Arc<PreparedDictionary>>>();
    assert_send::<EncoderSessionOwned<&'static PreparedDictionary>>();

    let data = b"encoded on another thread ".repeat(40);
    let mut session = encoder(Quality::Q5, 22)
        .into_session(InputSize::Exact(data.len() as u64).into())
        .expect("start");
    let mut output = vec![0; 64];
    let first = session
        .process(&data[..100], &mut output, Operation::Process)
        .expect("process");
    let rest = data[first.consumed..].to_vec();
    let (bytes, compressor) = std::thread::spawn(move || {
        let mut trace = Trace::default();
        trace.bytes.extend_from_slice(&output[..first.produced]);
        let mut remaining = rest.as_slice();
        loop {
            let progress = call(&mut session, &mut trace, remaining, 64, Operation::Finish);
            remaining = &remaining[progress.consumed..];
            if progress.status == EncoderStatus::Finished {
                break;
            }
        }
        (trace.bytes, session.into_compressor())
    })
    .join()
    .expect("the worker finished");
    let mut compressor = compressor;
    assert_eq!(bytes, compressor.compress(&data).expect("one shot"));
}

/// Leaves `session` in one of the states `reinit` must recover from.
fn end_first_operation(session: &mut EncoderSessionOwned, ending: &str, data: &[u8]) {
    let mut output = [0; 4];
    match ending {
        "finished" => {
            drive(&mut *session, data, SCHEDULES[1]);
        }
        "needs input" => {
            let p = session
                .process(&data[..10], &mut output, Operation::Process)
                .expect("process");
            assert_eq!(p.status, EncoderStatus::NeedsInput);
        }
        "needs output" => {
            let p = session
                .process(data, &mut output, Operation::Finish)
                .expect("finish");
            assert_eq!(p.status, EncoderStatus::NeedsOutput);
        }
        "flushed" => {
            let p = session
                .process(data, &mut vec![0; 1 << 16], Operation::Flush)
                .expect("flush");
            assert_eq!(p.status, EncoderStatus::NeedsInput);
        }
        _ => unreachable!(),
    }
}

#[test]
fn reinit_starts_an_operation_identical_to_a_fresh_borrowed_one() {
    let first = b"the first operation, ended in different ways ".repeat(40);
    let second = b"the second operation must not remember the first ".repeat(30);
    for quality in PARITY_QUALITIES {
        for ending in ["finished", "needs input", "needs output", "flushed"] {
            for stream in [
                StreamConfig::default(),
                InputSize::Exact(second.len() as u64).into(),
            ] {
                let mut session = encoder(quality, 22)
                    .into_session(StreamConfig::default())
                    .expect("start");
                end_first_operation(&mut session, ending, &first);
                session.reinit(stream).expect("reinit");
                assert!(!session.is_finished());
                for schedule in [SCHEDULES[0], SCHEDULES[2]] {
                    let expected = borrowed(quality, stream, &second, schedule);
                    let actual = drive(&mut session, &second, schedule);
                    assert_eq!(actual, expected, "q{}, {ending}", quality.get());
                    session.reinit(stream).expect("reinit again");
                }
            }
        }
    }
}

#[test]
fn a_rejected_reinit_leaves_a_failed_session_that_a_later_reinit_recovers() {
    let data = b"after a rejected reinit ".repeat(20);
    let mut session = encoder(Quality::Q1, 22)
        .into_session(StreamConfig::default())
        .expect("start");
    session
        .process(&data, &mut [0; 8], Operation::Process)
        .expect("process");
    let offset = StreamConfig::default().with_stream_offset(64);
    assert!(matches!(
        session.reinit(offset),
        Err(EncodeError::UnsupportedStreamOffset { offset: 64 })
    ));
    assert!(!session.is_finished());
    assert!(matches!(
        session.process(&data, &mut [0; 64], Operation::Finish),
        Err(EncodeError::InvalidState { .. })
    ));

    session.reinit(StreamConfig::default()).expect("reinit");
    let actual = drive(&mut session, &data, SCHEDULES[2]);
    assert_eq!(
        actual,
        borrowed(Quality::Q1, StreamConfig::default(), &data, SCHEDULES[2])
    );
    // And the owner handed back after a rejected reinit is reusable.
    session.reinit(offset).expect_err("rejected");
    let mut compressor = session.into_compressor();
    assert_eq!(
        c_decompress(&compressor.compress(&data).expect("compress"), data.len()).as_deref(),
        Some(&data[..])
    );
}

#[test]
fn reinit_keeps_the_dictionary_of_the_session() {
    let prefix = b"a shared prefix both sides know about. ".repeat(20);
    let data = [&prefix[..100], b" and a tail that is new"].concat();
    let dictionary = Arc::new(
        DictionaryBuilder::new()
            .add_prefix(&prefix[..])
            .build()
            .expect("dictionary"),
    );
    let mut compressor = encoder(Quality::Q5, 22);
    let expected = drive(
        &mut compressor
            .start_with_dictionary(&dictionary, StreamConfig::default())
            .expect("borrowed"),
        &data,
        SCHEDULES[1],
    );
    let mut session = encoder(Quality::Q5, 22)
        .into_session_with_dictionary(Arc::clone(&dictionary), StreamConfig::default())
        .expect("owned");
    session
        .process(b"interrupted", &mut [0; 4], Operation::Finish)
        .expect("process");
    session.reinit(StreamConfig::default()).expect("reinit");
    assert_eq!(drive(&mut session, &data, SCHEDULES[1]), expected);
}

#[cfg(feature = "experimental")]
#[test]
fn reinit_recovers_from_a_failed_process() {
    let stream = StreamConfig::default().with_stream_offset((1 << 63) - 1);
    let mut session = encoder(Quality::Q5, 22)
        .into_session(stream)
        .expect("owned");
    assert!(
        session
            .process(b"x", &mut [0; 10], Operation::Finish)
            .is_err()
    );
    session.reinit(StreamConfig::default()).expect("reinit");
    let data = b"valid after failure".repeat(10);
    assert_eq!(
        drive(&mut session, &data, SCHEDULES[2]),
        borrowed(Quality::Q5, StreamConfig::default(), &data, SCHEDULES[2])
    );
}

/// Feeds `data`, flushes after the first chunk and finishes, either through
/// the input-free shorthands or through `process` with empty input.
fn with_shorthands(session: &mut impl Session, data: &[u8], shorthand: bool) -> Trace {
    fn empty(
        session: &mut impl Session,
        trace: &mut Trace,
        shorthand: bool,
        operation: Operation,
        width: usize,
    ) {
        loop {
            let mut output = vec![0; width];
            let result = match (shorthand, operation) {
                (true, Operation::Flush) => session.flush_now(&mut output),
                (true, _) => session.finish_now(&mut output),
                (false, operation) => session.step(&[], &mut output, operation),
            };
            let progress = result.expect("the stream encodes");
            trace.bytes.extend_from_slice(&output[..progress.produced]);
            trace.calls.push(progress);
            if progress.status != EncoderStatus::NeedsOutput {
                break;
            }
        }
    }
    let mut trace = Trace::default();
    for (index, chunk) in data.chunks(333).enumerate() {
        let mut remaining = chunk;
        while !remaining.is_empty() {
            let progress = call(session, &mut trace, remaining, 7, Operation::Process);
            remaining = &remaining[progress.consumed..];
        }
        if index == 0 {
            empty(session, &mut trace, shorthand, Operation::Flush, 3);
        }
    }
    empty(session, &mut trace, shorthand, Operation::Finish, 5);
    assert!(session.finished());
    trace
}

#[test]
fn flush_and_finish_are_process_without_input_in_both_session_shapes() {
    let data = b"flush and finish take no input ".repeat(60);
    for quality in PARITY_QUALITIES {
        let mut compressor = encoder(quality, 22);
        let expected = with_shorthands(
            &mut compressor.start(StreamConfig::default()).expect("start"),
            &data,
            false,
        );
        let borrowed = with_shorthands(
            &mut compressor.start(StreamConfig::default()).expect("start"),
            &data,
            true,
        );
        let mut session = encoder(quality, 22)
            .into_session(StreamConfig::default())
            .expect("start");
        let owned = with_shorthands(&mut session, &data, true);
        assert_eq!(borrowed, expected, "q{}", quality.get());
        assert_eq!(owned, expected, "q{}", quality.get());
        assert!(session.is_finished());
        assert_eq!(
            c_decompress(&owned.bytes, data.len()).as_deref(),
            Some(&data[..])
        );
        // A finished stream stays finished through the shorthands too.
        let idle = Progress {
            consumed: 0,
            produced: 0,
            status: EncoderStatus::Finished,
        };
        assert_eq!(session.finish(&mut [0; 4]).expect("finished"), idle);
        assert_eq!(session.flush(&mut [0; 4]).expect("finished"), idle);
    }
}
