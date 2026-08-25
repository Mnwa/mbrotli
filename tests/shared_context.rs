//! The RFC 9841 shared context: what it holds, what it refuses, and what a
//! compression call does with one.

mod support;

use mbrotli::Brotli;
use mbrotli::compressor::shared::{SharedBrotliError, SharedContext, SharedContextLimits};
use mbrotli::compressor::{
    BrotliCompressError, CompressParams, Compressor, QualityLevel, WindowBits,
};
use std::sync::{Arc, Mutex};
use support::{IMPLEMENTED_QUALITIES, c_decompress, params, structural_corpora};

/// The dictionary the coverage cases attach.
const DICTIONARY: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n";

fn compressor() -> Compressor {
    Brotli::default().compressor()
}

fn empty_context(compressor: &Compressor, max_quality: QualityLevel) -> SharedContext {
    compressor
        .shared_context_builder(max_quality)
        .prepare()
        .expect("an empty context always prepares")
}

fn filled_context(compressor: &Compressor, max_quality: QualityLevel) -> SharedContext {
    compressor
        .shared_context_builder(max_quality)
        .add_prefix_dictionary(DICTIONARY.to_vec())
        .prepare()
        .expect("one small dictionary always prepares")
}

#[test]
fn a_prepared_context_reports_what_it_was_given() {
    let compressor = compressor();
    let context = compressor
        .shared_context_builder(QualityLevel::Q9)
        .add_prefix_dictionary(b"oldest attachment".to_vec())
        .add_prefix_dictionary(b"newest attachment".to_vec())
        .prepare()
        .expect("prepared");

    assert_eq!(context.max_quality(), QualityLevel::Q9);
    assert_eq!(context.attachment_count(), 2);
    assert_eq!(context.prefix_dictionary_count(), 2);
    assert_eq!(context.source_size(), 34);
    assert!(!context.has_custom_static_dictionary());
    assert!(context.allocated_size() > context.source_size());
}

#[test]
fn an_empty_context_reports_nothing_attached() {
    let compressor = compressor();
    let context = empty_context(&compressor, QualityLevel::Q5);

    assert_eq!(context.attachment_count(), 0);
    assert_eq!(context.prefix_dictionary_count(), 0);
    assert_eq!(context.source_size(), 0);
    assert!(!context.has_custom_static_dictionary());
}

#[test]
fn attaching_more_than_fifteen_dictionaries_is_refused() {
    let compressor = compressor();
    let mut builder = compressor.shared_context_builder(QualityLevel::Q5);
    for _ in 0..15 {
        builder = builder.add_prefix_dictionary(b"payload".to_vec());
    }
    assert_eq!(
        builder
            .prepare()
            .expect("fifteen is the limit, not one past it")
            .prefix_dictionary_count(),
        15
    );

    let mut builder = compressor.shared_context_builder(QualityLevel::Q5);
    for _ in 0..16 {
        builder = builder.add_prefix_dictionary(b"payload".to_vec());
    }
    assert!(matches!(
        builder.prepare(),
        Err(BrotliCompressError::Shared(
            SharedBrotliError::TooManyPrefixDictionaries {
                attached: 16,
                limit: 15
            }
        ))
    ));
}

#[test]
fn a_dictionary_past_its_limit_is_refused_and_nothing_is_retained() {
    let compressor = compressor();
    let outcome = compressor
        .shared_context_builder(QualityLevel::Q5)
        .add_prefix_dictionary(DICTIONARY.to_vec())
        .with_limits(SharedContextLimits::default().with_max_prefix_bytes(16))
        .prepare();

    assert!(matches!(
        outcome,
        Err(BrotliCompressError::Shared(
            SharedBrotliError::DictionaryTooLarge { limit: 16, .. }
        ))
    ));

    // The same builder shape without the limit still prepares, so nothing
    // global was poisoned by the failure.
    assert!(
        compressor
            .shared_context_builder(QualityLevel::Q5)
            .add_prefix_dictionary(DICTIONARY.to_vec())
            .prepare()
            .is_ok()
    );
}

#[test]
fn an_allocation_past_its_limit_is_refused_before_it_is_made() {
    let compressor = compressor();
    let outcome = compressor
        .shared_context_builder(QualityLevel::Q5)
        .add_prefix_dictionary(DICTIONARY.to_vec())
        .with_limits(SharedContextLimits::default().with_max_allocated_bytes(4096))
        .prepare();

    assert!(matches!(
        outcome,
        Err(BrotliCompressError::Shared(
            SharedBrotliError::SharedContextTooLarge { limit: 4096, .. }
        ))
    ));
}

#[test]
fn a_context_prepared_for_a_lower_quality_is_refused_at_a_higher_one() {
    let compressor = compressor();
    let mut context = empty_context(&compressor, QualityLevel::Q5);
    let low = params(QualityLevel::Q5, 22);
    let high = params(QualityLevel::Q9, 22);

    assert!(
        compressor
            .compress_shared(low, &mut context, b"payload")
            .is_ok()
    );
    assert!(matches!(
        compressor.compress_shared(high, &mut context, b"payload"),
        Err(BrotliCompressError::Shared(
            SharedBrotliError::SharedContextQualityMismatch {
                requested: 9,
                prepared: 5
            }
        ))
    ));
    assert!(matches!(
        compressor.calculate_shared_bound(&high, &context, 4096),
        Err(BrotliCompressError::Shared(
            SharedBrotliError::SharedContextQualityMismatch { .. }
        ))
    ));
    let mut buffer = [0u8; 64];
    assert!(matches!(
        compressor.compress_shared_to_slice(high, &mut context, b"payload", &mut buffer),
        Err(BrotliCompressError::Shared(
            SharedBrotliError::SharedContextQualityMismatch { .. }
        ))
    ));
}

#[test]
fn preparation_does_not_depend_on_the_quality_it_was_prepared_for() {
    let compressor = compressor();
    let low = filled_context(&compressor, QualityLevel::Q0);
    let high = filled_context(&compressor, QualityLevel::Q11);

    assert_eq!(low.max_quality(), QualityLevel::Q0);
    assert_eq!(high.max_quality(), QualityLevel::Q11);
    assert_eq!(low.allocated_size(), high.allocated_size());
    assert_eq!(low.source_size(), high.source_size());
    assert_eq!(
        compressor.longest_prefix_match(&low, b"Content-Type: text/plain"),
        compressor.longest_prefix_match(&high, b"Content-Type: text/plain")
    );
}

#[test]
fn an_attached_dictionary_is_refused_rather_than_ignored() {
    // Qualities below five have no match finder that could carry a prefix
    // match, so a dictionary handed to one must be refused, never dropped.
    // The qualities that can consult one are covered by
    // `tests/shared_dictionary.rs`, which checks the bytes against the C
    // encoder configured the same way.
    let compressor = compressor();
    for quality in IMPLEMENTED_QUALITIES {
        let numeric = usize::from(quality);
        if numeric >= 5 {
            continue;
        }
        let mut context = filled_context(&compressor, quality);
        let params = params(quality, 22);
        assert!(
            matches!(
                compressor.compress_shared(params, &mut context, b"payload payload"),
                Err(BrotliCompressError::Shared(
                    SharedBrotliError::UnsupportedSharedContextForQuality { quality }
                )) if quality == numeric
            ),
            "quality {numeric} silently ignored its dictionary"
        );
    }
}

#[test]
fn a_large_window_is_reported_before_the_context_is() {
    // Quality two cannot carry a large window, and that has to be the error a
    // caller sees even when the context would also have been refused.
    let compressor = compressor();
    let mut context = filled_context(&compressor, QualityLevel::Q11);
    let params = CompressParams::new(
        QualityLevel::Q2,
        WindowBits::large(30).expect("a legal large window"),
    );

    assert!(matches!(
        compressor.compress_shared(params, &mut context, b"payload"),
        Err(BrotliCompressError::Shared(
            SharedBrotliError::UnsupportedLargeWindow { quality: 2 }
        ))
    ));
}

#[test]
fn an_empty_context_compresses_exactly_as_the_ordinary_call_does() {
    let compressor = compressor();
    for quality in IMPLEMENTED_QUALITIES {
        let mut context = empty_context(&compressor, quality);
        for corpus in structural_corpora() {
            let params = params(quality, 22);
            let expected = compressor.compress(params, &corpus.data).expect("ordinary");
            let actual = compressor
                .compress_shared(params, &mut context, &corpus.data)
                .expect("shared");
            assert_eq!(
                actual, expected,
                "quality {quality:?} corpus {}",
                corpus.name
            );
            assert_eq!(
                c_decompress(&actual, corpus.data.len()).as_deref(),
                Some(corpus.data.as_slice()),
                "quality {quality:?} corpus {} did not round trip",
                corpus.name
            );
        }
    }
}

#[test]
fn an_empty_context_compresses_a_large_window_stream_unchanged() {
    let compressor = compressor();
    let lgwin = WindowBits::large(30).expect("thirty is a legal large window");
    for quality in [QualityLevel::Q5, QualityLevel::Q9, QualityLevel::Q11] {
        let mut context = empty_context(&compressor, quality);
        let params = CompressParams::new(quality, lgwin);
        let input = b"the quick brown fox jumps over the lazy dog, repeatedly and at length";
        assert_eq!(
            compressor
                .compress_shared(params, &mut context, input)
                .expect("shared"),
            compressor.compress(params, input).expect("ordinary")
        );
    }
}

#[test]
fn a_large_window_at_a_fast_quality_is_refused_through_the_shared_path_too() {
    let compressor = compressor();
    let lgwin = WindowBits::large(30).expect("thirty is a legal large window");
    for quality in [QualityLevel::Q0, QualityLevel::Q1] {
        let mut context = empty_context(&compressor, quality);
        let params = CompressParams::new(quality, lgwin);
        assert!(matches!(
            compressor.compress_shared(params, &mut context, b"payload"),
            Err(BrotliCompressError::Shared(
                SharedBrotliError::UnsupportedLargeWindow { .. }
            ))
        ));
    }
}

#[test]
fn the_shared_slice_and_vector_entry_points_agree() {
    let compressor = compressor();
    for quality in IMPLEMENTED_QUALITIES {
        let mut context = empty_context(&compressor, quality);
        let params = params(quality, 22);
        let input: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();

        let expected = compressor
            .compress_shared(params, &mut context, &input)
            .expect("vector");
        let bound = compressor
            .calculate_shared_bound(&params, &context, input.len())
            .expect("bound");
        assert!(bound >= expected.len());

        let mut buffer = vec![0u8; bound];
        let written = compressor
            .compress_shared_to_slice(params, &mut context, &input, &mut buffer)
            .expect("slice");
        assert_eq!(
            &buffer[..written],
            expected.as_slice(),
            "quality {quality:?}"
        );

        let mut tight = vec![0u8; 1];
        assert!(matches!(
            compressor.compress_shared_to_slice(params, &mut context, &input, &mut tight),
            Err(BrotliCompressError::OutputTooSmall)
        ));
    }
}

#[test]
fn reusing_one_context_is_deterministic_across_failures() {
    let compressor = compressor();
    let mut context = empty_context(&compressor, QualityLevel::Q9);
    let nine = params(QualityLevel::Q9, 22);
    let eleven = params(QualityLevel::Q11, 22);
    let first = b"the quick brown fox jumps over the lazy dog";
    let second: Vec<u8> = (0..20_000u32).map(|i| (i % 97) as u8).collect();

    let before = compressor
        .compress_shared(nine, &mut context, first)
        .expect("first");
    compressor
        .compress_shared(nine, &mut context, &second)
        .expect("second");

    // A deliberate failure between the two runs of the same input.
    let mut tiny = [0u8; 1];
    assert!(
        compressor
            .compress_shared_to_slice(nine, &mut context, &second, &mut tiny)
            .is_err()
    );
    assert!(matches!(
        compressor.compress_shared(eleven, &mut context, first),
        Err(BrotliCompressError::Shared(
            SharedBrotliError::SharedContextQualityMismatch { .. }
        ))
    ));

    let after = compressor
        .compress_shared(nine, &mut context, first)
        .expect("first again");
    assert_eq!(after, before);
}

#[test]
fn a_context_moves_between_threads_and_serialises_behind_a_caller_lock() {
    let compressor = compressor();
    let context = filled_context(&compressor, QualityLevel::Q5);
    let dictionary_size = context.source_size();

    // `Send`: the context moves into the worker.
    let moved = std::thread::spawn(move || context.source_size())
        .join()
        .expect("the worker finished");
    assert_eq!(moved, dictionary_size);

    // The synchronisation policy is entirely the caller's.
    let shared = Arc::new(Mutex::new(filled_context(&compressor, QualityLevel::Q5)));
    let guard = shared.lock().expect("not poisoned");
    assert_eq!(guard.source_size(), dictionary_size);
}

#[test]
fn a_prefix_match_is_found_where_the_dictionary_really_covers_the_input() {
    let compressor = compressor();
    let context = filled_context(&compressor, QualityLevel::Q5);

    let found = compressor
        .longest_prefix_match(&context, b"Content-Type: text/plain")
        .expect("the header prefix is in the dictionary");
    assert_eq!(found.length(), 19);
    assert_eq!(
        &DICTIONARY[found.dictionary_offset() as usize..][..found.length()],
        b"Content-Type: text/"
    );

    assert!(
        compressor
            .longest_prefix_match(&context, b"nothing in common here")
            .is_none()
    );
    // Fewer than the eight bytes the index is keyed on.
    assert!(compressor.longest_prefix_match(&context, b"HTTP").is_none());
    // An empty context can never match.
    let empty = empty_context(&compressor, QualityLevel::Q5);
    assert!(
        compressor
            .longest_prefix_match(&empty, b"Content-Type: text/plain")
            .is_none()
    );
}

#[test]
fn a_prefix_match_crosses_from_one_attachment_into_the_next() {
    let compressor = compressor();
    let context = compressor
        .shared_context_builder(QualityLevel::Q5)
        .add_prefix_dictionary(b"the quick brown fox jum".to_vec())
        .add_prefix_dictionary(b"ps over the lazy dog".to_vec())
        .prepare()
        .expect("prepared");

    let found = compressor
        .longest_prefix_match(&context, b"brown fox jumps over the")
        .expect("the two attachments are one byte sequence");
    assert_eq!(found.dictionary_offset(), 10);
    assert_eq!(found.length(), 24);
}

#[test]
fn a_prefix_offset_maps_to_the_distance_that_addresses_it() {
    let compressor = compressor();
    let context = compressor
        .shared_context_builder(QualityLevel::Q5)
        .add_prefix_dictionary(b"oldest".to_vec())
        .add_prefix_dictionary(b"newest".to_vec())
        .prepare()
        .expect("prepared");
    let max_backward = 1u64 << 20;

    for offset in 0..12u64 {
        let distance = context
            .backward_distance(offset, max_backward)
            .expect("inside the prefix");
        assert!(distance > max_backward);
        assert_eq!(
            context.dictionary_offset(distance, max_backward),
            Some(offset)
        );
    }

    // The newest prefix byte sits immediately past the sliding window.
    assert_eq!(
        context.backward_distance(11, max_backward),
        Some(max_backward + 1)
    );
    assert_eq!(
        context.backward_distance(0, max_backward),
        Some(max_backward + 12)
    );

    // Off both ends, and inside the window.
    assert_eq!(context.backward_distance(12, max_backward), None);
    assert_eq!(context.dictionary_offset(max_backward, max_backward), None);
    assert_eq!(
        context.dictionary_offset(max_backward + 13, max_backward),
        None
    );
    assert_eq!(context.dictionary_offset(u64::MAX, u64::MAX), None);
}

#[test]
fn an_empty_context_addresses_no_distance_at_all() {
    let compressor = compressor();
    let context = empty_context(&compressor, QualityLevel::Q5);

    assert_eq!(context.backward_distance(0, 1024), None);
    assert_eq!(context.dictionary_offset(1025, 1024), None);
}

#[test]
fn the_limits_expose_their_documented_defaults() {
    let limits = SharedContextLimits::default();
    assert_eq!(limits.max_total_source_bytes(), 64 << 20);
    assert_eq!(limits.max_prefix_bytes(), 64 << 20);
    assert_eq!(limits.max_allocated_bytes(), 1 << 30);

    let tightened = limits
        .with_max_total_source_bytes(1)
        .with_max_prefix_bytes(2)
        .with_max_allocated_bytes(3);
    assert_eq!(tightened.max_total_source_bytes(), 1);
    assert_eq!(tightened.max_prefix_bytes(), 2);
    assert_eq!(tightened.max_allocated_bytes(), 3);
    assert_ne!(tightened, limits);
}

#[test]
fn every_shared_error_prints_what_went_wrong() {
    let messages = [
        SharedBrotliError::TooManyPrefixDictionaries {
            attached: 16,
            limit: 15,
        }
        .to_string(),
        SharedBrotliError::DictionaryTooLarge {
            bytes: 99,
            limit: 8,
        }
        .to_string(),
        SharedBrotliError::SharedContextTooLarge {
            bytes: 99,
            limit: 8,
        }
        .to_string(),
        SharedBrotliError::SharedContextQualityMismatch {
            requested: 11,
            prepared: 5,
        }
        .to_string(),
        SharedBrotliError::UnsupportedSharedContextForQuality { quality: 7 }.to_string(),
    ];
    for message in &messages {
        assert!(!message.is_empty());
    }
    assert!(messages[0].contains("15"));
    assert!(messages[3].contains("11"));
    assert!(messages[4].contains('7'));

    // Every variant travels through the public error as a transparent source.
    let wrapped =
        BrotliCompressError::from(SharedBrotliError::UnsupportedSharedContextForQuality {
            quality: 7,
        });
    assert_eq!(wrapped.to_string(), messages[4]);
}
