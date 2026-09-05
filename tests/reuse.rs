//! A reused compressor must produce exactly what a fresh one produces.
//!
//! Reuse exists only to keep allocations alive; the moment it changes a byte it
//! is a bug. Every test here compares a compressor that has been used before
//! against one that has not, over the same input.

mod support;

use mbrotli::io::FinishError;
use mbrotli::{
    Compressor, EncodeError, EncoderConfig, InputSize, Operation, Quality, RetentionPolicy,
    StreamConfig, Window,
};
use std::io::Write;
use support::{IMPLEMENTED_QUALITIES, c_decompress, config, encoder, structural_corpora};

/// Window every case in this file uses.
const LGWIN: u8 = 22;

/// Payloads chosen to move the encoder between its internal thresholds.
fn payloads() -> Vec<Vec<u8>> {
    vec![
        Vec::new(),
        b"a".to_vec(),
        b"hello hello hello hello hello".to_vec(),
        b"the quick brown fox jumps over the lazy dog. ".repeat(50),
        b"the quick brown fox jumps over the lazy dog. ".repeat(3000),
        (0u8..=255).cycle().take(70_000).collect(),
        vec![0u8; 100_000],
        (0u32..40_000)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
            .collect(),
    ]
}

/// Compresses `payload` with a compressor that has never been used.
fn fresh(quality: Quality, payload: &[u8]) -> Vec<u8> {
    encoder(quality, LGWIN)
        .compress(payload)
        .expect("a fresh compressor failed")
}

#[test]
fn a_reused_compressor_matches_a_fresh_one() {
    for quality in IMPLEMENTED_QUALITIES {
        let mut warm = encoder(quality, LGWIN);
        for payload in payloads() {
            let reused = warm.compress(&payload).expect("a reused compressor failed");
            assert_eq!(
                reused,
                fresh(quality, &payload),
                "q{}: reuse changed the output for {} bytes",
                quality.get(),
                payload.len()
            );
        }
    }
}

#[test]
fn appending_reuses_the_destination_and_preserves_its_prefix() {
    for quality in IMPLEMENTED_QUALITIES {
        let mut warm = encoder(quality, LGWIN);
        let mut output = b"a prefix the caller already had".to_vec();
        let prefix = output.clone();

        for payload in payloads() {
            let start = output.len();
            let range = warm
                .compress_into(&payload, &mut output)
                .expect("appending failed");
            assert_eq!(range.start, start);
            assert_eq!(range.end, output.len());
            assert_eq!(
                &output[range.clone()],
                fresh(quality, &payload).as_slice(),
                "q{}: appending changed the output",
                quality.get()
            );
            assert_eq!(&output[..prefix.len()], prefix.as_slice());
            output.truncate(start);
        }
    }
}

#[test]
fn a_reused_compressor_matches_over_the_structural_corpora() {
    for quality in IMPLEMENTED_QUALITIES {
        let mut warm = encoder(quality, LGWIN);
        for corpus in structural_corpora() {
            let reused = warm
                .compress(&corpus.data)
                .expect("a reused compressor failed");
            assert_eq!(
                reused,
                fresh(quality, &corpus.data),
                "q{}: reuse changed the output for {}",
                quality.get(),
                corpus.name
            );
        }
    }
}

#[test]
fn the_slice_entry_point_reuses_the_same_way() {
    for quality in IMPLEMENTED_QUALITIES {
        let mut warm = encoder(quality, LGWIN);
        for payload in payloads() {
            let expected = fresh(quality, &payload);
            let mut buffer =
                vec![0u8; Compressor::max_compressed_size(payload.len()).expect("bound")];
            let written = warm
                .compress_to_slice(&payload, &mut buffer)
                .expect("a reused compressor failed");
            assert_eq!(
                &buffer[..written],
                expected.as_slice(),
                "q{}: slice reuse changed the output for {} bytes",
                quality.get(),
                payload.len()
            );
        }
    }
}

#[test]
fn reused_output_still_round_trips() {
    for quality in IMPLEMENTED_QUALITIES {
        let mut warm = encoder(quality, LGWIN);
        for payload in payloads() {
            let compressed = warm.compress(&payload).expect("a reused compressor failed");
            let decoded = c_decompress(&compressed, payload.len().max(1))
                .expect("the decoder rejected a reused stream");
            assert_eq!(
                decoded,
                payload,
                "q{}: reuse broke the round trip",
                quality.get()
            );
        }
    }
}

#[test]
fn the_whole_lifecycle_leaves_the_output_unchanged() {
    // The sequence the specification asks for: two payloads, an empty one, a
    // deliberate failure, a trim, two reconfigurations and an abandoned
    // session. Every comparable output has to match a fresh compressor.
    let first = b"the quick brown fox jumps over the lazy dog. ".repeat(400);
    let second: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
    let expected_first = fresh(Quality::Q5, &first);
    let mut encoder = encoder(Quality::Q5, LGWIN);

    assert_eq!(encoder.compress(&first).expect("A"), expected_first);
    assert_eq!(
        encoder.compress(&second).expect("B"),
        fresh(Quality::Q5, &second)
    );
    assert_eq!(
        encoder.compress(b"").expect("empty"),
        fresh(Quality::Q5, b"")
    );

    // A destination too small to hold anything abandons a stream part-written.
    let mut cramped = [0u8; 4];
    assert!(matches!(
        encoder.compress_to_slice(&first, &mut cramped),
        Err(EncodeError::OutputTooSmall { provided: 4 })
    ));
    assert_eq!(
        encoder.compress(&first).expect("A after a failure"),
        expected_first
    );

    encoder.trim(RetentionPolicy::ReleaseAll);
    assert_eq!(encoder.retained_bytes(), 0);
    assert_eq!(
        encoder.compress(&first).expect("A after a trim"),
        expected_first
    );

    encoder.reconfigure(config(Quality::Q1, LGWIN)).expect("q1");
    assert_eq!(
        encoder.compress(&first).expect("A at q1"),
        fresh(Quality::Q1, &first)
    );
    encoder.reconfigure(config(Quality::Q9, LGWIN)).expect("q9");
    assert_eq!(
        encoder.compress(&first).expect("A at q9"),
        fresh(Quality::Q9, &first)
    );
    encoder.reconfigure(config(Quality::Q5, LGWIN)).expect("q5");

    // A session dropped before it finished leaves half a stream behind.
    {
        let mut session = encoder
            .start(StreamConfig::default())
            .expect("a legal stream");
        let mut buffer = [0u8; 64];
        session
            .process(&first, &mut buffer, Operation::Process)
            .expect("the session failed");
    }
    assert_eq!(
        encoder
            .compress(&first)
            .expect("A after an abandoned session"),
        expected_first
    );
}

#[test]
fn a_failed_call_does_not_poison_the_compressor() {
    let payload = b"the quick brown fox jumps over the lazy dog. ".repeat(400);
    for quality in IMPLEMENTED_QUALITIES {
        let mut encoder = encoder(quality, LGWIN);
        let mut cramped = [0u8; 4];
        assert!(
            matches!(
                encoder.compress_to_slice(&payload, &mut cramped),
                Err(EncodeError::OutputTooSmall { provided: 4 })
            ),
            "q{}: a four-byte destination was accepted",
            quality.get()
        );
        assert_eq!(
            encoder
                .compress(&payload)
                .expect("the compressor did not recover"),
            fresh(quality, &payload),
            "q{}: a failed call changed the next one",
            quality.get()
        );
    }
}

#[test]
fn a_failed_append_leaves_the_destination_as_it_found_it() {
    // There is no way to make the append path fail without exhausting memory,
    // so this pins the contract that holds when it succeeds: the prefix is
    // untouched and the range covers exactly what was added.
    let mut encoder = encoder(Quality::Q5, LGWIN);
    let mut output = b"untouched".to_vec();
    let range = encoder
        .compress_into(b"payload payload", &mut output)
        .expect("appending failed");
    assert_eq!(&output[..9], b"untouched");
    assert_eq!(range, 9..output.len());
}

#[test]
fn an_abandoned_session_is_detected_rather_than_trusted() {
    let mut encoder = encoder(Quality::Q5, LGWIN);
    let expected = fresh(Quality::Q5, b"payload payload");

    std::mem::forget(
        encoder
            .start(StreamConfig::default())
            .expect("a legal stream"),
    );
    assert!(matches!(
        encoder.compress(b"payload payload"),
        Err(EncodeError::AbandonedSession)
    ));
    assert!(matches!(
        encoder.start(StreamConfig::default()),
        Err(EncodeError::AbandonedSession)
    ));

    encoder.recover();
    assert_eq!(
        encoder.compress(b"payload payload").expect("recovered"),
        expected
    );
}

#[test]
fn every_retention_policy_keeps_the_output_the_same() {
    let payload = b"the quick brown fox jumps over the lazy dog. ".repeat(200);
    for quality in IMPLEMENTED_QUALITIES {
        let expected = fresh(quality, &payload);
        for retention in [
            RetentionPolicy::Aggressive,
            RetentionPolicy::CurrentConfig,
            RetentionPolicy::Bounded { max_bytes: 0 },
            RetentionPolicy::Bounded {
                max_bytes: usize::MAX,
            },
            RetentionPolicy::ReleaseAll,
        ] {
            let mut encoder = Compressor::builder(config(quality, LGWIN))
                .with_retention(retention)
                .build()
                .expect("a legal configuration");
            for _ in 0..3 {
                assert_eq!(
                    encoder.compress(&payload).expect("compression failed"),
                    expected,
                    "q{} under {retention:?}",
                    quality.get()
                );
            }
        }
    }
}

#[test]
fn a_policy_that_releases_everything_retains_nothing() {
    let payload = b"payload payload payload".repeat(100);
    let mut encoder = Compressor::builder(config(Quality::Q9, LGWIN))
        .with_retention(RetentionPolicy::ReleaseAll)
        .build()
        .expect("a legal configuration");
    encoder.compress(&payload).expect("compression failed");
    assert_eq!(encoder.retained_bytes(), 0);

    let mut warm = encoder.fork_empty();
    assert_eq!(warm.retained_bytes(), 0);
    warm.compress(&payload).expect("compression failed");
    assert_eq!(warm.retained_bytes(), 0, "the policy was not forked");
}

#[test]
fn a_bounded_policy_keeps_the_compressor_under_its_ceiling() {
    let payload = b"payload payload payload".repeat(400);
    let mut encoder = Compressor::builder(config(Quality::Q5, LGWIN))
        .with_retention(RetentionPolicy::Bounded { max_bytes: 1 << 12 })
        .build()
        .expect("a legal configuration");
    for _ in 0..4 {
        encoder.compress(&payload).expect("compression failed");
        assert!(
            encoder.retained_bytes() <= 1 << 12,
            "{} bytes retained past the ceiling",
            encoder.retained_bytes()
        );
    }
}

#[test]
fn a_forked_compressor_encodes_the_same_stream() {
    let payload = b"the quick brown fox jumps over the lazy dog. ".repeat(100);
    for quality in IMPLEMENTED_QUALITIES {
        let mut warm = encoder(quality, LGWIN);
        let expected = warm.compress(&payload).expect("compression failed");

        let mut forked = warm.fork_empty();
        assert_eq!(forked.config(), warm.config());
        assert_eq!(forked.retained_bytes(), 0);
        assert_eq!(
            forked.compress(&payload).expect("compression failed"),
            expected,
            "q{}: a fork left the original",
            quality.get()
        );
    }
}

#[test]
fn a_rejected_reconfiguration_changes_nothing() {
    let payload = b"payload payload payload";
    let mut encoder = encoder(Quality::Q5, LGWIN);
    let expected = encoder.compress(payload).expect("compression failed");

    let refused = EncoderConfig::default()
        .with_quality(Quality::Q0)
        .with_window(Window::large(30).expect("a legal window"));
    assert!(encoder.reconfigure(refused).is_err());

    assert_eq!(encoder.config().quality(), Quality::Q5);
    assert_eq!(
        encoder.compress(payload).expect("compression failed"),
        expected
    );
}

#[test]
fn reuse_survives_a_streaming_session_in_the_middle() {
    let payload = b"the quick brown fox jumps over the lazy dog. ".repeat(100);
    for quality in IMPLEMENTED_QUALITIES {
        let mut encoder = encoder(quality, LGWIN);
        let expected = encoder.compress(&payload).expect("compression failed");

        let streamed = {
            let stream = StreamConfig::from(InputSize::Exact(payload.len() as u64));
            let mut sink = encoder.writer(Vec::new(), stream).expect("a legal stream");
            sink.write_all(&payload).expect("write failed");
            sink.finish()
                .map_err(FinishError::into_error)
                .expect("finish failed")
        };
        assert_eq!(streamed, expected, "q{}", quality.get());
        assert_eq!(
            encoder.compress(&payload).expect("compression failed"),
            expected,
            "q{}: a session changed the next one-shot call",
            quality.get()
        );
    }
}
