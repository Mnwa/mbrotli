//! RFC 9841 LZ77 prefix dictionaries, actually consulted by the encoder.
//!
//! The oracle is the reference's own compound dictionary: the same bytes are
//! prepared and attached to Google's C encoder, and the two streams have to be
//! identical. A stream is then handed back to the C decoder with the same
//! dictionaries attached, which is an independent check that the distances the
//! encoder emitted address what it thought they did.

mod support;

use mbrotli::Brotli;
use mbrotli::compressor::shared::{SharedBrotliError, SharedContext};
use mbrotli::compressor::{BrotliCompressError, CompressParams, QualityLevel, WindowBits};
use support::{CParams, c_compress_with_prefixes, c_decompress_with_prefixes, vendor_file};

/// Window every case in this file uses.
const LGWIN: u8 = 22;

/// The qualities whose match finders consult an attached prefix.
const PREFIX_QUALITIES: [QualityLevel; 7] = [
    QualityLevel::Q5,
    QualityLevel::Q6,
    QualityLevel::Q7,
    QualityLevel::Q8,
    QualityLevel::Q9,
    QualityLevel::Q10,
    QualityLevel::Q11,
];

/// The qualities that refuse a non-empty context.
const REFUSING_QUALITIES: [QualityLevel; 5] = [
    QualityLevel::Q0,
    QualityLevel::Q1,
    QualityLevel::Q2,
    QualityLevel::Q3,
    QualityLevel::Q4,
];

/// Cap on the payload the two slow qualities are exercised over.
const HQ_CAP: usize = 1 << 14;

fn params(quality: QualityLevel) -> CompressParams {
    // The size hint is pinned so the C harness and this crate resolve the same
    // match finder whatever the payload length is.
    CompressParams::new(quality, WindowBits::standard(LGWIN).expect("window"))
        .with_size_hint(Some(0))
}

fn c_params(quality: QualityLevel) -> CParams {
    let mut params = CParams::new(usize::from(quality) as std::ffi::c_int, LGWIN.into());
    params.size_hint = Some(0);
    params
}

fn context(prefixes: &[&[u8]], quality: QualityLevel) -> SharedContext {
    let compressor = Brotli::default().compressor();
    let mut builder = compressor.shared_context_builder(quality);
    for prefix in prefixes {
        builder = builder.add_prefix_dictionary(*prefix);
    }
    builder.prepare().expect("prepare")
}

fn cap(quality: QualityLevel, data: &[u8]) -> &[u8] {
    match quality {
        QualityLevel::Q10 | QualityLevel::Q11 => &data[..data.len().min(HQ_CAP)],
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
    let compressor = Brotli::default().compressor();
    for case in cases() {
        // The empty-input shortcut has no one-shot C counterpart to compare
        // against — `BrotliEncoderCompress` cannot take a dictionary, and the
        // streaming API writes the window header instead of the shortcut. It
        // has its own test below.
        let (name, payload) = (case.name, &case.payload);
        if payload.is_empty() {
            continue;
        }
        let borrowed = case.borrowed();
        for quality in PREFIX_QUALITIES {
            let input = cap(quality, payload);
            let mut shared = context(&borrowed, quality);
            let ours = compressor
                .compress_shared(params(quality), &mut shared, input)
                .unwrap_or_else(|error| panic!("{name} q{quality:?}: {error}"));
            let theirs = c_compress_with_prefixes(c_params(quality), &borrowed, input);
            assert_eq!(
                ours.len(),
                theirs.len(),
                "{name} q{quality:?}: {} bytes against {} bytes",
                ours.len(),
                theirs.len()
            );
            assert_eq!(ours, theirs, "{name} q{quality:?}: bytes differ");
        }
    }
}

#[test]
fn an_attached_prefix_round_trips_through_the_c_decoder() {
    let compressor = Brotli::default().compressor();
    for case in cases() {
        let (name, payload) = (case.name, &case.payload);
        let borrowed = case.borrowed();
        for quality in PREFIX_QUALITIES {
            let input = cap(quality, payload);
            let mut shared = context(&borrowed, quality);
            let compressed = compressor
                .compress_shared(params(quality), &mut shared, input)
                .expect("compress");
            let decoded = c_decompress_with_prefixes(&borrowed, &compressed, input.len().max(1))
                .unwrap_or_else(|| panic!("{name} q{quality:?}: the decoder rejected the stream"));
            assert_eq!(decoded, input, "{name} q{quality:?}: round trip differs");
        }
    }
}

#[test]
fn an_attached_prefix_actually_shrinks_a_matching_payload() {
    // Without this the two tests above would still pass on an encoder that
    // silently ignored the dictionary and happened to match a C build that did
    // the same. A payload that *is* the dictionary has to compress far better.
    let compressor = Brotli::default().compressor();
    let prefix = vendor_file("alice29.txt");
    let payload = &prefix[..prefix.len().min(1 << 15)];

    for quality in PREFIX_QUALITIES {
        let input = cap(quality, payload);
        let mut shared = context(&[&prefix], quality);
        let with = compressor
            .compress_shared(params(quality), &mut shared, input)
            .expect("shared");
        let without = compressor.compress(params(quality), input).expect("plain");
        assert!(
            with.len() * 4 < without.len(),
            "q{quality:?}: {} bytes with the dictionary against {} without",
            with.len(),
            without.len()
        );
    }
}

#[test]
fn an_empty_context_is_the_ordinary_stream() {
    let compressor = Brotli::default().compressor();
    let payload = b"an empty context must change nothing at all. ".repeat(50);
    for quality in PREFIX_QUALITIES {
        let mut shared = context(&[], quality);
        assert_eq!(
            compressor
                .compress_shared(params(quality), &mut shared, &payload)
                .expect("shared"),
            compressor
                .compress(params(quality), &payload)
                .expect("plain"),
            "q{quality:?}: an empty context changed the stream"
        );
    }
}

#[test]
fn the_qualities_without_a_prefix_search_still_refuse() {
    let compressor = Brotli::default().compressor();
    let payload = b"payload".repeat(20);
    for quality in REFUSING_QUALITIES {
        let mut shared = context(&[b"a dictionary".as_slice()], QualityLevel::Q11);
        let outcome = compressor.compress_shared(params(quality), &mut shared, &payload);
        let expected = usize::from(quality);
        assert!(
            matches!(
                outcome,
                Err(BrotliCompressError::Shared(
                    SharedBrotliError::UnsupportedSharedContextForQuality { quality: reported }
                )) if reported == expected
            ),
            "q{quality:?} did not refuse a non-empty context"
        );
    }
}

#[test]
fn the_slice_entry_point_agrees_with_the_vector_one() {
    let compressor = Brotli::default().compressor();
    for case in cases() {
        let (name, payload) = (case.name, &case.payload);
        let borrowed = case.borrowed();
        for quality in PREFIX_QUALITIES {
            let input = cap(quality, payload);
            let mut shared = context(&borrowed, quality);
            let expected = compressor
                .compress_shared(params(quality), &mut shared, input)
                .expect("vector");
            let bound = compressor
                .calculate_shared_bound(&params(quality), &shared, input.len())
                .expect("bound");
            let mut buffer = vec![0u8; bound];
            let written = compressor
                .compress_shared_to_slice(params(quality), &mut shared, input, &mut buffer)
                .expect("slice");
            assert_eq!(
                &buffer[..written],
                expected.as_slice(),
                "{name} q{quality:?}"
            );
        }
    }
}

#[test]
fn an_empty_input_keeps_the_one_shot_shortcut() {
    // A stream with no bytes in it cannot reference a dictionary, so the
    // shortcut the ordinary one-shot entry point takes is still the right
    // answer — and still the byte `BrotliEncoderCompress` produces.
    let compressor = Brotli::default().compressor();
    for quality in PREFIX_QUALITIES {
        let mut shared = context(&[b"a dictionary".as_slice()], quality);
        assert_eq!(
            compressor
                .compress_shared(params(quality), &mut shared, b"")
                .expect("shared"),
            vec![6],
            "q{quality:?}"
        );
    }
}

#[test]
fn a_prefix_copy_continues_across_an_input_block_boundary() {
    // `ExtendLastCommand` runs when a block ends mid-copy: the command it left
    // behind is extended into the next block's bytes. With a prefix attached
    // that copy may address the dictionary rather than the window, which is a
    // branch nothing else here reaches — a match is bounded by the block it
    // was found in, so only a payload larger than one input block can produce
    // it.
    //
    // The payload *is* the dictionary, so every block after the first starts
    // inside a dictionary copy. Quality five's input block is 64 KiB at this
    // window, hence the size.
    let compressor = Brotli::default().compressor();
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
    let payload = &prefix[..];

    for quality in [
        QualityLevel::Q5,
        QualityLevel::Q6,
        QualityLevel::Q7,
        QualityLevel::Q8,
        QualityLevel::Q9,
    ] {
        let mut shared = context(&[&prefix], quality);
        let ours = compressor
            .compress_shared(params(quality), &mut shared, payload)
            .unwrap_or_else(|error| panic!("q{quality:?}: {error}"));
        let theirs = c_compress_with_prefixes(c_params(quality), &[&prefix], payload);
        assert_eq!(
            ours.len(),
            theirs.len(),
            "q{quality:?}: {} bytes against {} bytes",
            ours.len(),
            theirs.len()
        );
        assert_eq!(ours, theirs, "q{quality:?}: bytes differ");

        let decoded = c_decompress_with_prefixes(&[&prefix], &ours, payload.len())
            .unwrap_or_else(|| panic!("q{quality:?}: the decoder rejected the stream"));
        assert_eq!(decoded, payload, "q{quality:?}: round trip differs");
    }
}
