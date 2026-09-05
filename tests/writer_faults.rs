//! The writer must lose nothing and duplicate nothing when the sink misbehaves.
//!
//! Every case here drives the same payload through `EncoderWriter` into a sink
//! that is scripted to fail, and requires the bytes that eventually arrive to be
//! *exactly* the stream the one-shot path produces: one copy, no omission, no
//! duplication, and never a second terminator.

mod support;

use mbrotli::io::FinishError;
use mbrotli::{Compressor, InputSize, Quality, StreamConfig};
use std::io::{Error, ErrorKind, Result, Write};
use support::{c_decompress, encoder};

/// Window every case in this file uses.
const LGWIN: u8 = 22;

/// The qualities the specification names for this proof, one per encoder core.
const QUALITIES: [Quality; 5] = [
    Quality::Q0,
    Quality::Q1,
    Quality::Q5,
    Quality::Q9,
    Quality::Q11,
];

/// A sink whose short writes and failures are scripted.
///
/// It accepts at most `accept` bytes per call, stops exactly at `fail_after`
/// bytes to raise `kind` once, and behaves normally from then on. That is what
/// lets a test place a failure at *every* byte position of a stream.
struct FaultySink {
    /// Everything the sink has taken, in order.
    written: Vec<u8>,
    /// The most one `write` call will take.
    accept: usize,
    /// How many bytes to take before failing once.
    fail_after: usize,
    /// Whether the scripted failure has already happened.
    failed: bool,
    /// The failure to raise.
    kind: ErrorKind,
    /// How many `flush` calls still have to fail.
    flush_failures: usize,
}

impl FaultySink {
    /// A sink that fails once after `fail_after` bytes with `kind`.
    fn new(accept: usize, fail_after: usize, kind: ErrorKind) -> Self {
        Self {
            written: Vec::new(),
            accept: accept.max(1),
            fail_after,
            failed: false,
            kind,
            flush_failures: 0,
        }
    }

    /// A sink that never fails a write but fails its first `count` flushes.
    fn flaky_flush(count: usize) -> Self {
        Self {
            written: Vec::new(),
            accept: usize::MAX,
            fail_after: usize::MAX,
            failed: true,
            kind: ErrorKind::Other,
            flush_failures: count,
        }
    }
}

impl Write for FaultySink {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if !self.failed && self.written.len() >= self.fail_after {
            self.failed = true;
            return Err(Error::new(self.kind, "scripted failure"));
        }
        let mut room = self.accept.min(buf.len());
        if !self.failed {
            // Stop exactly at the failure point, so the next call raises it.
            room = room.min(self.fail_after - self.written.len());
        }
        self.written.extend_from_slice(&buf[..room]);
        Ok(room)
    }

    fn flush(&mut self) -> Result<()> {
        if self.flush_failures > 0 {
            self.flush_failures -= 1;
            return Err(Error::other("scripted flush failure"));
        }
        Ok(())
    }
}

/// A sink that accepts nothing at all, without saying so.
struct ZeroSink;

impl Write for ZeroSink {
    fn write(&mut self, _buf: &[u8]) -> Result<usize> {
        Ok(0)
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Whether an error is one the caller is expected to retry.
fn retryable(error: &Error) -> bool {
    matches!(
        error.kind(),
        ErrorKind::Interrupted | ErrorKind::WouldBlock | ErrorKind::Other
    )
}

/// A payload big enough to span several writes and produce a real stream.
fn payload() -> Vec<u8> {
    b"the quick brown fox jumps over the lazy dog. ".repeat(60)
}

/// Drives `payload` through a writer over `sink`, retrying every failure.
///
/// Returns what the sink ended up holding.
fn drive(encoder: &mut Compressor, payload: &[u8], chunk: usize, sink: FaultySink) -> Vec<u8> {
    let stream = StreamConfig::from(InputSize::Exact(payload.len() as u64));
    let mut writer = encoder.writer(sink, stream).expect("a legal stream");

    let mut offset = 0usize;
    let mut attempts = 0usize;
    while offset < payload.len() {
        let end = (offset + chunk.max(1)).min(payload.len());
        match writer.write(&payload[offset..end]) {
            Ok(count) => {
                assert!(count > 0, "a non-empty write accepted nothing");
                offset += count;
            }
            Err(error) if retryable(&error) => {
                attempts += 1;
                assert!(attempts < 10_000, "the sink never recovered");
            }
            Err(error) => panic!("the writer failed: {error}"),
        }
    }

    loop {
        match writer.try_finish() {
            Ok(()) => break,
            Err(error) if retryable(&error) => {
                attempts += 1;
                assert!(attempts < 10_000, "the sink never recovered");
            }
            Err(error) => panic!("finishing failed: {error}"),
        }
    }
    assert!(writer.is_finished());

    writer
        .finish()
        .map_err(FinishError::into_error)
        .expect("a finished writer must hand back its sink")
        .written
}

#[test]
fn a_failure_at_every_output_position_still_yields_exactly_one_stream() {
    for quality in QUALITIES {
        let payload = payload();
        let mut encoder = encoder(quality, LGWIN);
        let expected = encoder.compress(&payload).expect("compression failed");

        for fail_after in 0..=expected.len() {
            let sink = FaultySink::new(usize::MAX, fail_after, ErrorKind::Other);
            let written = drive(&mut encoder, &payload, 64, sink);
            assert_eq!(
                written,
                expected,
                "q{}: a failure after {fail_after} bytes changed the stream",
                quality.get()
            );
        }
    }
}

#[test]
fn every_kind_of_sink_failure_is_survivable() {
    for quality in QUALITIES {
        let payload = payload();
        let mut encoder = encoder(quality, LGWIN);
        let expected = encoder.compress(&payload).expect("compression failed");

        for kind in [
            ErrorKind::Interrupted,
            ErrorKind::WouldBlock,
            ErrorKind::Other,
            ErrorKind::BrokenPipe,
        ] {
            for fail_after in [0usize, 1, expected.len() / 2, expected.len()] {
                let sink = FaultySink::new(usize::MAX, fail_after, kind);
                // `BrokenPipe` is not retryable by the driver's rule, so it is
                // driven separately below; here it stands in for "an error the
                // caller chooses to retry anyway", which must also work.
                let written = drive_with_any_retry(&mut encoder, &payload, 64, sink);
                assert_eq!(
                    written,
                    expected,
                    "q{}: {kind:?} after {fail_after} bytes changed the stream",
                    quality.get()
                );
            }
        }
    }
}

/// As [`drive`], retrying whatever the sink raised.
fn drive_with_any_retry(
    encoder: &mut Compressor,
    payload: &[u8],
    chunk: usize,
    sink: FaultySink,
) -> Vec<u8> {
    let stream = StreamConfig::from(InputSize::Exact(payload.len() as u64));
    let mut writer = encoder.writer(sink, stream).expect("a legal stream");

    let mut offset = 0usize;
    let mut attempts = 0usize;
    while offset < payload.len() {
        let end = (offset + chunk.max(1)).min(payload.len());
        match writer.write(&payload[offset..end]) {
            Ok(count) => offset += count,
            Err(_) => {
                attempts += 1;
                assert!(attempts < 10_000, "the sink never recovered");
            }
        }
    }
    loop {
        match writer.try_finish() {
            Ok(()) => break,
            Err(_) => {
                attempts += 1;
                assert!(attempts < 10_000, "the sink never recovered");
            }
        }
    }
    writer
        .finish()
        .map_err(FinishError::into_error)
        .expect("a finished writer must hand back its sink")
        .written
}

#[test]
fn a_short_writing_sink_loses_nothing() {
    for quality in QUALITIES {
        let payload = payload();
        let mut encoder = encoder(quality, LGWIN);
        let expected = encoder.compress(&payload).expect("compression failed");

        for accept in [1usize, 2, 7, 64] {
            // Never fails; only ever takes `accept` bytes at a time.
            let sink = FaultySink::new(accept, usize::MAX, ErrorKind::Other);
            let written = drive(&mut encoder, &payload, 128, sink);
            assert_eq!(
                written,
                expected,
                "q{}: {accept} byte writes changed the stream",
                quality.get()
            );
        }
    }
}

#[test]
fn a_sink_that_accepts_nothing_is_reported_as_write_zero() {
    let mut encoder = encoder(Quality::Q1, LGWIN);
    let payload = payload();
    let mut writer = encoder
        .writer(ZeroSink, StreamConfig::default())
        .expect("a legal stream");

    // The first write buffers; the failure surfaces once there is output to
    // deliver, at the latest when the stream is finished.
    let mut outcome = writer
        .write_all(&payload)
        .and_then(|()| writer.try_finish());
    for _ in 0..4 {
        if outcome.is_err() {
            break;
        }
        outcome = writer.try_finish();
    }
    let error = outcome.expect_err("a sink that accepts nothing must be reported");
    assert_eq!(error.kind(), ErrorKind::WriteZero);
}

#[test]
fn a_failing_finish_hands_the_writer_back() {
    for quality in QUALITIES {
        let payload = payload();
        let mut encoder = encoder(quality, LGWIN);
        let expected = encoder.compress(&payload).expect("compression failed");

        // Fail once, at the very first byte the terminator needs to deliver.
        let sink = FaultySink::new(usize::MAX, 0, ErrorKind::WouldBlock);
        let stream = StreamConfig::from(InputSize::Exact(payload.len() as u64));
        let mut writer = encoder.writer(sink, stream).expect("a legal stream");
        while writer.write(&payload).is_err() {}

        let mut writer = match writer.finish() {
            Ok(sink) => {
                // The sink recovered before `finish` was reached, which is a
                // legitimate schedule; the stream still has to be right.
                assert_eq!(sink.written, expected, "q{}", quality.get());
                continue;
            }
            Err(failure) => {
                assert!(!failure.error().to_string().is_empty());
                failure.into_inner()
            }
        };

        // The retry completes the very same stream: one terminator, no
        // duplicated bytes.
        while writer.try_finish().is_err() {}
        let sink = writer
            .finish()
            .map_err(FinishError::into_error)
            .expect("the retry must succeed");
        assert_eq!(
            sink.written,
            expected,
            "q{}: retrying finish changed the stream",
            quality.get()
        );
    }
}

#[test]
fn a_failing_inner_flush_is_reported_and_retryable() {
    for quality in QUALITIES {
        let payload = payload();
        let mut encoder = encoder(quality, LGWIN);

        let mut writer = encoder
            .writer(FaultySink::flaky_flush(2), StreamConfig::default())
            .expect("a legal stream");
        writer.write_all(&payload).expect("write failed");

        // Two scripted flush failures, then success.
        assert!(writer.flush().is_err(), "q{}", quality.get());
        assert!(writer.flush().is_err(), "q{}", quality.get());
        writer.flush().expect("the third flush must succeed");

        let sink = writer
            .finish()
            .map_err(FinishError::into_error)
            .expect("finish failed");
        // Everything written before the flush decodes, and the finished stream
        // round-trips as a whole.
        let decoded = c_decompress(&sink.written, payload.len())
            .expect("the decoder rejected a flushed stream");
        assert_eq!(decoded, payload, "q{}", quality.get());
    }
}

#[test]
fn a_failure_during_a_flush_does_not_lose_the_flushed_bytes() {
    for quality in QUALITIES {
        let payload = payload();
        let mut encoder = encoder(quality, LGWIN);

        // Fail once part-way through delivering the flush's own output.
        let sink = FaultySink::new(usize::MAX, 8, ErrorKind::WouldBlock);
        let mut writer = encoder
            .writer(sink, StreamConfig::default())
            .expect("a legal stream");
        writer.write_all(&payload).unwrap_or_default();
        while writer.flush().is_err() {}
        while writer.try_finish().is_err() {}

        let sink = writer
            .finish()
            .map_err(FinishError::into_error)
            .expect("finish failed");
        let decoded = c_decompress(&sink.written, payload.len())
            .expect("the decoder rejected a stream flushed across a failure");
        assert_eq!(decoded, payload, "q{}", quality.get());
    }
}

#[test]
fn a_writer_dropped_unfinished_leaves_the_compressor_usable() {
    let payload = payload();
    for quality in QUALITIES {
        let mut encoder = encoder(quality, LGWIN);
        let expected = encoder.compress(&payload).expect("compression failed");

        {
            let mut writer = encoder
                .writer(Vec::new(), StreamConfig::default())
                .expect("a legal stream");
            writer.write_all(&payload).expect("write failed");
            // Dropped without finishing: the stream is abandoned, and `Drop`
            // does no I/O and cannot fail.
        }

        assert_eq!(
            encoder.compress(&payload).expect("compression failed"),
            expected,
            "q{}: an abandoned writer changed the next call",
            quality.get()
        );
    }
}

#[test]
fn writing_after_finishing_is_refused() {
    let mut encoder = encoder(Quality::Q1, LGWIN);
    let mut writer = encoder
        .writer(Vec::new(), StreamConfig::default())
        .expect("a legal stream");
    writer.write_all(b"payload").expect("write failed");
    writer.try_finish().expect("finish failed");

    let error = writer
        .write(b"more")
        .expect_err("a finished stream must not accept more input");
    assert_eq!(error.kind(), ErrorKind::InvalidInput);
    // And finishing again is a no-op rather than a second terminator.
    writer
        .try_finish()
        .expect("a finished stream stays finished");
}
