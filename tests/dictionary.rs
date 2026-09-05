//! RFC 9841 LZ77 prefix dictionaries, from the outside.
//!
//! The oracle is the reference's own compound dictionary: the same bytes are
//! prepared and attached to Google's C encoder, and the two streams have to be
//! identical. A stream is then handed back to the C decoder with the same
//! dictionaries attached, which is an independent check that the distances the
//! encoder emitted address what it thought they did.

mod support;

use mbrotli::dictionary::{
    DictionaryBuilder, DictionaryError, DictionaryLimits, PreparedDictionary,
};
use mbrotli::io::FinishError;
use mbrotli::{Compressor, EncodeError, InputSize, Quality, StreamConfig};
use std::io::{Read, Write};
use std::sync::Arc;
use support::{
    CParams, c_compress_with_prefixes, c_decompress_with_prefixes, encoder, structural_corpora,
    vendor_file,
};

/// Window every case in this file uses.
const LGWIN: u8 = 22;

/// Dictionaries one prepared dictionary may hold, as RFC 9841 fixes it.
const MAX_ATTACHMENTS: usize = 15;

/// The qualities whose match finders consult an attached prefix.
const PREFIX_QUALITIES: [Quality; 7] = [
    Quality::Q5,
    Quality::Q6,
    Quality::Q7,
    Quality::Q8,
    Quality::Q9,
    Quality::Q10,
    Quality::Q11,
];

/// The qualities that refuse a dictionary rather than ignoring it.
const REFUSING_QUALITIES: [Quality; 5] = [
    Quality::Q0,
    Quality::Q1,
    Quality::Q2,
    Quality::Q3,
    Quality::Q4,
];

/// Cap on the payload the two slow qualities are exercised over.
const HQ_CAP: usize = 1 << 14;

/// The C parameters matching the Rust configuration, so the two are comparable.
///
/// The size hint is pinned to zero on both sides, which is what a streaming
/// session with an undeclared size resolves to, so the match finder is the same
/// whatever the payload length is.
fn c_params(quality: Quality) -> CParams {
    let mut params = CParams::new(std::ffi::c_int::from(quality.get()), LGWIN.into());
    params.size_hint = Some(0);
    params
}

/// Prepares a dictionary from `prefixes`, in attachment order.
fn dictionary(prefixes: &[&[u8]]) -> PreparedDictionary {
    let mut builder = DictionaryBuilder::new();
    for prefix in prefixes {
        builder = builder.add_prefix(*prefix);
    }
    builder.build().expect("prepared")
}

/// Compresses `src` against `prefixes` through a session with no declared size.
///
/// The one-shot path declares the true input length, which the C harness cannot
/// be told to do for a dictionary stream without also changing its match
/// finder, so the comparison runs through the streaming shape both sides share.
fn compress_with_dictionary(
    quality: Quality,
    prefixes: &[&[u8]],
    src: &[u8],
) -> (Vec<u8>, PreparedDictionary) {
    let prepared = dictionary(prefixes);
    let mut encoder = encoder(quality, LGWIN);
    let streamed = {
        let mut sink = encoder
            .writer_with_dictionary(&prepared, Vec::new(), StreamConfig::default())
            .expect("a legal stream");
        sink.write_all(src).expect("write failed");
        sink.finish()
            .map_err(FinishError::into_error)
            .expect("finish failed")
    };
    (streamed, prepared)
}

fn cap(quality: Quality, data: &[u8]) -> &[u8] {
    match quality {
        Quality::Q10 | Quality::Q11 => &data[..data.len().min(HQ_CAP)],
        _ => data,
    }
}

/// One differential case: a label, the attachments, and the payload.
struct Case {
    name: &'static str,
    prefixes: Vec<Vec<u8>>,
    payload: Vec<u8>,
}

impl Case {
    fn new(name: &'static str, prefixes: Vec<Vec<u8>>, payload: Vec<u8>) -> Self {
        Self {
            name,
            prefixes,
            payload,
        }
    }

    /// The attachments in the borrowed form both harnesses take.
    fn borrowed(&self) -> Vec<&[u8]> {
        self.prefixes.iter().map(Vec::as_slice).collect()
    }
}

/// Dictionary and payload pairs with real overlap between them.
fn cases() -> Vec<Case> {
    let alice = vendor_file("alice29.txt");
    let (head, tail) = alice.split_at(alice.len() / 2);
    vec![
        Case::new(
            "payload-is-the-dictionary",
            vec![b"the quick brown fox jumps over the lazy dog. ".repeat(40)],
            b"the quick brown fox jumps over the lazy dog. ".repeat(40),
        ),
        Case::new(
            "half-of-alice-as-the-dictionary",
            vec![head.to_vec()],
            tail.to_vec(),
        ),
        Case::new(
            "dictionary-shares-nothing",
            vec![(0u8..=255).cycle().take(20_000).collect()],
            b"entirely unrelated text that shares nothing at all. ".repeat(80),
        ),
        Case::new(
            "three-attachments",
            vec![
                b"first attachment first attachment first attachment ".repeat(20),
                b"second attachment second attachment second attachment ".repeat(20),
                b"third attachment third attachment third attachment ".repeat(20),
            ],
            b"third attachment first attachment second attachment ".repeat(30),
        ),
        Case::new(
            "attachment-shorter-than-a-hash-key",
            vec![b"tiny".to_vec()],
            b"tiny payload that mentions tiny more than once. tiny. ".repeat(40),
        ),
        Case::new(
            "empty-payload",
            vec![b"a dictionary".repeat(50)],
            Vec::new(),
        ),
    ]
}

#[test]
fn an_attached_prefix_matches_the_reference_byte_for_byte() {
    for case in cases() {
        let (name, payload) = (case.name, &case.payload);
        let borrowed = case.borrowed();
        for quality in PREFIX_QUALITIES {
            let input = cap(quality, payload);
            let (ours, _) = compress_with_dictionary(quality, &borrowed, input);
            let theirs = c_compress_with_prefixes(c_params(quality), &borrowed, input);
            assert_eq!(
                ours.len(),
                theirs.len(),
                "{name} q{}: {} bytes against {} bytes",
                quality.get(),
                ours.len(),
                theirs.len()
            );
            assert_eq!(ours, theirs, "{name} q{}: bytes differ", quality.get());
        }
    }
}

#[test]
fn an_attached_prefix_round_trips_through_the_c_decoder() {
    for case in cases() {
        let (name, payload) = (case.name, &case.payload);
        let borrowed = case.borrowed();
        for quality in PREFIX_QUALITIES {
            let input = cap(quality, payload);
            let (compressed, _) = compress_with_dictionary(quality, &borrowed, input);
            let decoded = c_decompress_with_prefixes(&borrowed, &compressed, input.len().max(1))
                .unwrap_or_else(|| {
                    panic!("{name} q{}: the decoder rejected the stream", quality.get())
                });
            assert_eq!(
                decoded,
                input,
                "{name} q{}: round trip differs",
                quality.get()
            );
        }
    }
}

#[test]
fn an_attached_prefix_actually_shrinks_a_matching_payload() {
    // Without this the two tests above would still pass on an encoder that
    // silently ignored the dictionary and happened to match a C build doing the
    // same. A payload that *is* the dictionary has to compress far better.
    let prefix = vendor_file("alice29.txt");
    let payload = &prefix[..prefix.len().min(1 << 15)];

    for quality in PREFIX_QUALITIES {
        let input = cap(quality, payload);
        let prepared = dictionary(&[&prefix]);
        let mut encoder = encoder(quality, LGWIN);
        let with = encoder
            .compress_with_dictionary(&prepared, input)
            .expect("compression failed");
        let without = encoder.compress(input).expect("compression failed");
        assert!(
            with.len() * 4 < without.len(),
            "q{}: {} bytes with the dictionary against {} without",
            quality.get(),
            with.len(),
            without.len()
        );
    }
}

#[test]
fn every_dictionary_entry_point_reaches_the_same_bytes() {
    for case in cases() {
        let (name, payload) = (case.name, &case.payload);
        let borrowed = case.borrowed();
        for quality in PREFIX_QUALITIES {
            let input = cap(quality, payload);
            let prepared = dictionary(&borrowed);
            let mut encoder = encoder(quality, LGWIN);

            let expected = encoder
                .compress_with_dictionary(&prepared, input)
                .expect("the vector entry point failed");

            let mut appended = Vec::new();
            let range = encoder
                .compress_with_dictionary_into(&prepared, input, &mut appended)
                .expect("the appending entry point failed");
            assert_eq!(&appended[range], expected.as_slice(), "{name}: appending");

            let mut buffer =
                vec![0u8; Compressor::max_compressed_size(input.len()).expect("bound")];
            let written = encoder
                .compress_with_dictionary_to_slice(&prepared, input, &mut buffer)
                .expect("the slice entry point failed");
            assert_eq!(&buffer[..written], expected.as_slice(), "{name}: slice");

            // A session that declares the same size reaches the same bytes,
            // except where the one-shot special cases apply.
            if !input.is_empty() {
                let stream = StreamConfig::from(InputSize::Exact(input.len() as u64));
                let mut sink = encoder
                    .writer_with_dictionary(&prepared, Vec::new(), stream)
                    .expect("a legal stream");
                sink.write_all(input).expect("write failed");
                let streamed = sink
                    .finish()
                    .map_err(FinishError::into_error)
                    .expect("finish failed");
                assert_eq!(streamed, expected, "{name}: writer");

                let mut source = encoder
                    .reader_with_dictionary(&prepared, input, stream)
                    .expect("a legal stream");
                let mut pulled = Vec::new();
                source.read_to_end(&mut pulled).expect("read failed");
                assert_eq!(pulled, expected, "{name}: reader");
            }
        }
    }
}

#[test]
fn the_qualities_without_a_prefix_search_refuse_rather_than_ignore() {
    let prepared = dictionary(&[b"a dictionary".as_slice()]);
    let payload = b"payload".repeat(20);
    for quality in REFUSING_QUALITIES {
        let mut encoder = encoder(quality, LGWIN);
        let mut buffer = [0u8; 256];
        let mut appended = Vec::new();

        for outcome in [
            encoder.compress_with_dictionary(&prepared, &payload).err(),
            encoder
                .compress_with_dictionary_into(&prepared, &payload, &mut appended)
                .err(),
            encoder
                .compress_with_dictionary_to_slice(&prepared, &payload, &mut buffer)
                .err(),
            encoder
                .start_with_dictionary(&prepared, StreamConfig::default())
                .err(),
        ] {
            assert!(
                matches!(
                    outcome,
                    Some(EncodeError::DictionaryUnsupportedForQuality { quality: reported })
                        if reported == quality
                ),
                "q{} did not refuse a dictionary",
                quality.get()
            );
        }
        // The refusal costs the caller nothing: the compressor still works.
        assert!(
            !encoder
                .compress(&payload)
                .expect("compression failed")
                .is_empty()
        );
        assert!(appended.is_empty(), "a refused call appended bytes");
    }
}

#[test]
fn a_dictionary_backs_many_compressors_at_once() {
    let prefix = vendor_file("alice29.txt");
    let payload = prefix[..prefix.len().min(1 << 14)].to_vec();
    let prepared = Arc::new(dictionary(&[&prefix]));

    let expected = encoder(Quality::Q5, LGWIN)
        .compress_with_dictionary(&prepared, &payload)
        .expect("compression failed");

    let workers: Vec<_> = (0..4)
        .map(|_| {
            let prepared = Arc::clone(&prepared);
            let payload = payload.clone();
            std::thread::spawn(move || {
                encoder(Quality::Q5, LGWIN)
                    .compress_with_dictionary(&prepared, &payload)
                    .expect("compression failed")
            })
        })
        .collect();

    for worker in workers {
        assert_eq!(worker.join().expect("the worker finished"), expected);
    }
}

#[test]
fn an_empty_input_keeps_the_one_shot_shortcut() {
    // A stream with no bytes in it cannot reference a dictionary, so the
    // shortcut the ordinary one-shot entry point takes is still the right
    // answer — and still the byte `BrotliEncoderCompress` produces.
    let prepared = dictionary(&[b"a dictionary".as_slice()]);
    for quality in PREFIX_QUALITIES {
        assert_eq!(
            encoder(quality, LGWIN)
                .compress_with_dictionary(&prepared, b"")
                .expect("compression failed"),
            vec![6],
            "q{}",
            quality.get()
        );
    }
}

#[test]
fn a_prefix_copy_continues_across_an_input_block_boundary() {
    // `ExtendLastCommand` runs when a block ends mid-copy: the command it left
    // behind is extended into the next block's bytes. With a prefix attached
    // that copy may address the dictionary rather than the window, which is a
    // branch nothing else here reaches — a match is bounded by the block it was
    // found in, so only a payload larger than one input block can produce it.
    let prefix: Vec<u8> = {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        (0..300_000)
            .map(|index| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                // Long runs of repeated text between random bytes, so the
                // matches are long enough to straddle a block boundary.
                if index % 64 < 56 {
                    b"the quick brown fox jumps over the lazy dog. "[index % 44]
                } else {
                    (state >> 33) as u8
                }
            })
            .collect()
    };

    for quality in [
        Quality::Q5,
        Quality::Q6,
        Quality::Q7,
        Quality::Q8,
        Quality::Q9,
    ] {
        let (ours, _) = compress_with_dictionary(quality, &[&prefix], &prefix);
        let theirs = c_compress_with_prefixes(c_params(quality), &[&prefix], &prefix);
        assert_eq!(
            ours.len(),
            theirs.len(),
            "q{}: {} bytes against {} bytes",
            quality.get(),
            ours.len(),
            theirs.len()
        );
        assert_eq!(ours, theirs, "q{}: bytes differ", quality.get());

        let decoded = c_decompress_with_prefixes(&[&prefix], &ours, prefix.len())
            .unwrap_or_else(|| panic!("q{}: the decoder rejected the stream", quality.get()));
        assert_eq!(decoded, prefix, "q{}: round trip differs", quality.get());
    }
}

#[test]
fn a_dictionary_stream_flushes_the_way_an_ordinary_one_does() {
    // A flush has to reach the match finder with the dictionary still attached,
    // or the bytes after it would be compressed against nothing.
    let prefix = b"a common prefix worth attaching to every stream".repeat(20);
    let prepared = dictionary(&[&prefix]);
    for quality in [Quality::Q5, Quality::Q9, Quality::Q11] {
        let mut encoder = encoder(quality, LGWIN);
        let mut sink = encoder
            .writer_with_dictionary(&prepared, Vec::new(), StreamConfig::default())
            .expect("a legal stream");
        sink.write_all(b"a common prefix worth attaching")
            .expect("write failed");
        sink.flush().expect("flush failed");
        sink.write_all(b" to every stream").expect("write failed");
        let compressed = sink
            .finish()
            .map_err(FinishError::into_error)
            .expect("finish failed");

        let decoded = c_decompress_with_prefixes(&[&prefix], &compressed, 64)
            .unwrap_or_else(|| panic!("q{}: the decoder rejected the stream", quality.get()));
        assert_eq!(decoded, b"a common prefix worth attaching to every stream");
    }
}

#[test]
fn preparation_reports_what_it_refused_and_retains_nothing() {
    let mut builder = DictionaryBuilder::new();
    for _ in 0..MAX_ATTACHMENTS {
        builder = builder.add_prefix(&b"payload"[..]);
    }
    assert_eq!(
        builder
            .build()
            .expect("fifteen is legal")
            .attachment_count(),
        MAX_ATTACHMENTS
    );

    let mut builder = DictionaryBuilder::new();
    for _ in 0..=MAX_ATTACHMENTS {
        builder = builder.add_prefix(&b"payload"[..]);
    }
    assert_eq!(
        builder.build().unwrap_err(),
        DictionaryError::TooManyAttachments {
            attached: 16,
            limit: 15
        }
    );

    let refused = DictionaryBuilder::new()
        .add_prefix(&b"nine byte"[..])
        .with_limits(DictionaryLimits::default().with_max_prefix_bytes(8))
        .build();
    assert_eq!(
        refused.unwrap_err(),
        DictionaryError::TooLarge { bytes: 9, limit: 8 }
    );

    let too_much_index = DictionaryBuilder::new()
        .add_prefix(&b"eight!!!"[..])
        .with_limits(DictionaryLimits::default().with_max_retained_bytes(1024))
        .build();
    assert!(matches!(
        too_much_index.unwrap_err(),
        DictionaryError::PreparationTooLarge { limit: 1024, .. }
    ));

    // Nothing global was disturbed by any of those refusals.
    assert!(
        DictionaryBuilder::new()
            .add_prefix(&b"nine byte"[..])
            .build()
            .is_ok()
    );
}

#[test]
fn attachment_order_is_call_order_and_reaches_the_reference() {
    // The order the attachments are given in is part of the format: a decoder
    // has to attach the same bytes in the same order. Swapping two changes the
    // stream, and both orders have to match the reference given the same order.
    let first: &[u8] = b"first attachment first attachment ";
    let second: &[u8] = b"second attachment second attachment ";
    let payload = b"second attachment first attachment ".repeat(40);

    let forwards = compress_with_dictionary(Quality::Q5, &[first, second], &payload).0;
    let backwards = compress_with_dictionary(Quality::Q5, &[second, first], &payload).0;
    assert_ne!(forwards, backwards, "attachment order did not matter");

    assert_eq!(
        forwards,
        c_compress_with_prefixes(c_params(Quality::Q5), &[first, second], &payload)
    );
    assert_eq!(
        backwards,
        c_compress_with_prefixes(c_params(Quality::Q5), &[second, first], &payload)
    );
}

#[test]
fn a_dictionary_reports_its_own_shape() {
    let prepared = dictionary(&[b"oldest".as_slice(), b"newest".as_slice()]);
    assert_eq!(prepared.attachment_count(), 2);
    assert_eq!(prepared.source_bytes(), 12);
    assert!(prepared.retained_bytes() > prepared.source_bytes());

    // The addressing and its inverse agree over the whole prefix.
    let max_backward = 1u64 << 20;
    for offset in 0..12u64 {
        let distance = prepared
            .backward_distance(offset, max_backward)
            .expect("inside the prefix");
        assert!(distance > max_backward);
        assert_eq!(prepared.prefix_offset(distance, max_backward), Some(offset));
    }
    assert_eq!(prepared.backward_distance(12, max_backward), None);
    assert_eq!(prepared.prefix_offset(max_backward, max_backward), None);
}

#[test]
fn reusing_one_compressor_with_a_dictionary_is_deterministic() {
    let prepared = dictionary(&[b"a common prefix worth attaching".as_slice()]);
    let mut encoder = encoder(Quality::Q9, LGWIN);
    let payload = b"a common prefix worth attaching to a stream".repeat(20);

    let expected = encoder
        .compress_with_dictionary(&prepared, &payload)
        .expect("compression failed");

    // A deliberate failure, an ordinary call and a session in between.
    let mut tiny = [0u8; 1];
    assert!(
        encoder
            .compress_with_dictionary_to_slice(&prepared, &payload, &mut tiny)
            .is_err()
    );
    encoder.compress(&payload).expect("compression failed");
    drop(
        encoder
            .start_with_dictionary(&prepared, StreamConfig::default())
            .expect("a legal stream"),
    );

    assert_eq!(
        encoder
            .compress_with_dictionary(&prepared, &payload)
            .expect("compression failed"),
        expected
    );
}

#[test]
fn a_dictionary_never_changes_an_ordinary_stream() {
    // Every corpus compressed without a dictionary has to be exactly what it
    // was before dictionaries existed, whatever else the compressor has done.
    for quality in PREFIX_QUALITIES {
        let prepared = dictionary(&[b"an attached prefix".as_slice()]);
        let mut encoder = encoder(quality, LGWIN);
        for corpus in structural_corpora() {
            let data = cap(quality, &corpus.data);
            let plain = encoder.compress(data).expect("compression failed");
            encoder
                .compress_with_dictionary(&prepared, data)
                .expect("compression failed");
            assert_eq!(
                encoder.compress(data).expect("compression failed"),
                plain,
                "q{}: a dictionary call changed the next ordinary one for {}",
                quality.get(),
                corpus.name
            );
        }
    }
}
