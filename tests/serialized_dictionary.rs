//! The RFC 9841 serialized shared dictionary format, end to end.
//!
//! Behind the `experimental` feature, which also builds the vendored C library
//! with `BROTLI_EXPERIMENTAL` so its own parser — the only other implementation
//! of this format — is available to compare against.
//!
//! What is checked here:
//!
//! - round trips between a built dictionary, its bytes, and the parse of those
//!   bytes, including that serializing twice is stable;
//! - the canonical encoding of a fixture, byte for byte;
//! - every truncation, every single-byte mutation, and a set of hand-written
//!   malformed streams, all against the C parser's own accept or reject;
//! - the structure the C parser recovered, field for field;
//! - every transform operation, against `BrotliTransformDictionaryWord`;
//! - each configurable resource limit.

#![cfg(feature = "experimental")]
mod support;

use google_brotli_ffi::{
    MAX_TRANSFORMED_WORD_BYTES, MbrotliSharedDictInfo, mbrotli_shim_parse_shared_dictionary,
    mbrotli_shim_transform_dictionary_word,
};
use mbrotli::dictionary::{
    ContextMap, DictionaryBuilder, DictionaryCombination, DictionaryError, DictionaryLimits,
    ListSelector, OmitLength, SerializedDictionary, SerializedDictionaryError, TransformList,
    TransformOperation, WordList,
};

/// Runs the reference parser over `bytes` and reports what it made of them.
fn c_parse(bytes: &[u8]) -> MbrotliSharedDictInfo {
    let mut info = MbrotliSharedDictInfo::default();
    // SAFETY: `bytes` is a live slice readable for its own length, and `info`
    // is a live, correctly typed, correctly aligned local the shim fully
    // writes. The shim owns and frees the dictionary it builds internally.
    unsafe {
        mbrotli_shim_parse_shared_dictionary(bytes.as_ptr(), bytes.len(), &raw mut info);
    }
    info
}

/// Returns what the reference makes of one transform of one word.
fn c_transform(
    dictionary: &[u8],
    combination: u32,
    length: u32,
    word_index: u32,
    transform: u32,
) -> Option<Vec<u8>> {
    let mut out = vec![0u8; MAX_TRANSFORMED_WORD_BYTES];
    // SAFETY: `dictionary` is readable for its own length and `out` is a live
    // buffer of exactly the size the shim documents.
    let written = unsafe {
        mbrotli_shim_transform_dictionary_word(
            dictionary.as_ptr(),
            dictionary.len(),
            combination,
            length,
            word_index,
            transform,
            out.as_mut_ptr(),
        )
    };
    let written = usize::try_from(written).ok()?;
    out.truncate(written);
    Some(out)
}

/// Asserts that this crate and the reference agree on whether `bytes` parse.
fn assert_agrees_with_c(name: &str, bytes: &[u8]) {
    // The reference stops once the structure is complete and ignores whatever
    // follows; this crate refuses a tail. Retrying without it is what makes the
    // two comparable on everything else, and the one case where they
    // deliberately differ has its own test.
    let ours = match SerializedDictionary::try_from(bytes) {
        Err(SerializedDictionaryError::TrailingBytes { extra }) => {
            let head = bytes.len().saturating_sub(extra);
            SerializedDictionary::try_from(&bytes[..head])
        }
        other => other,
    };
    let theirs = c_parse(bytes);
    assert_eq!(
        ours.is_ok(),
        theirs.ok == 1,
        "case {name}: this crate {} but the reference {}: {bytes:02X?}",
        if ours.is_ok() { "accepted" } else { "rejected" },
        if theirs.ok == 1 {
            "accepted"
        } else {
            "rejected"
        },
    );
    let Ok(ours) = ours else {
        return;
    };
    assert_eq!(
        theirs.num_prefix,
        u32::from(!ours.prefix().is_empty()),
        "case {name}: prefix count"
    );
    if !ours.prefix().is_empty() {
        assert_eq!(
            theirs.prefix_size[0] as usize,
            ours.prefix().len(),
            "case {name}: prefix length"
        );
    }
    assert_eq!(
        usize::from(theirs.num_word_lists),
        ours.word_list_count(),
        "case {name}: word list count"
    );
    assert_eq!(
        usize::from(theirs.num_transform_lists),
        ours.transform_list_count(),
        "case {name}: transform list count"
    );
    let combinations = ours.combination_count().max(1);
    assert_eq!(
        usize::from(theirs.num_dictionaries),
        combinations,
        "case {name}: combination count"
    );
    assert_eq!(
        theirs.context_based == 1,
        ours.context_map().is_some(),
        "case {name}: context flag"
    );
    if let Some(map) = ours.context_map() {
        assert_eq!(
            theirs.context_map,
            <[u8; 64]>::from(map),
            "case {name}: map"
        );
    }
}

/// A dictionary carrying a prefix, custom words, custom transforms and a map.
fn rich_dictionary() -> SerializedDictionary {
    let mut map = ContextMap::uniform(0);
    map.set(3, 1);
    SerializedDictionary::builder()
        .with_prefix(&b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n"[..])
        .add_word_list(
            WordList::builder()
                .add_word(b"charset")
                .add_word(b"encoding")
                .add_word(b"content")
                .build()
                .expect("well formed"),
        )
        .add_transform_list(
            TransformList::builder()
                .add_transform(b"", TransformOperation::Identity, b"")
                .add_transform(b"<", TransformOperation::FermentFirst, b">")
                .add_transform(
                    b"",
                    TransformOperation::OmitLast(OmitLength::try_from(2).expect("in range")),
                    b"...",
                )
                .add_transform(b"", TransformOperation::ShiftAll(3), b"")
                .build()
                .expect("well formed"),
        )
        .add_combination(DictionaryCombination::new(
            ListSelector::Custom(0),
            ListSelector::Custom(0),
        ))
        .add_combination(DictionaryCombination::new(
            ListSelector::Builtin,
            ListSelector::Builtin,
        ))
        .with_context_map(map)
        .build()
        .expect("the parts are consistent")
}

#[test]
fn a_prefix_only_dictionary_round_trips() {
    let dictionary = SerializedDictionary::builder()
        .with_prefix(&b"a shared prefix"[..])
        .build()
        .expect("valid");
    let bytes = dictionary.to_bytes();

    let parsed = SerializedDictionary::try_from(&bytes[..]).expect("what was written parses");

    assert_eq!(parsed.prefix(), b"a shared prefix");
    assert_eq!(parsed.to_bytes(), bytes);
    assert_eq!(bytes.len(), dictionary.serialized_len());
}

#[test]
fn the_canonical_encoding_of_a_prefix_is_fixed() {
    let dictionary = SerializedDictionary::builder()
        .with_prefix(&b"abc"[..])
        .build()
        .expect("valid");

    // Magic, a one-byte varint length of three, the prefix, and the two zero
    // list counts. No combination block, because there is no custom list.
    assert_eq!(
        dictionary.to_bytes(),
        vec![0x91, 0x00, 0x03, b'a', b'b', b'c', 0x00, 0x00]
    );
}

#[test]
fn the_empty_dictionary_is_five_bytes() {
    let dictionary = SerializedDictionary::builder().build().expect("valid");

    assert_eq!(dictionary.to_bytes(), vec![0x91, 0x00, 0x00, 0x00, 0x00]);
    assert!(!dictionary.is_custom_static());
}

#[test]
fn a_rich_dictionary_round_trips() {
    let dictionary = rich_dictionary();
    let bytes = dictionary.to_bytes();
    let parsed = SerializedDictionary::try_from(&bytes[..]).expect("what was written parses");

    assert_eq!(parsed.prefix(), dictionary.prefix());
    assert_eq!(parsed.word_list_count(), 1);
    assert_eq!(parsed.transform_list_count(), 1);
    assert_eq!(parsed.combination_count(), 2);
    assert_eq!(
        parsed.combinations().collect::<Vec<_>>(),
        dictionary.combinations().collect::<Vec<_>>()
    );
    assert_eq!(parsed.context_map(), dictionary.context_map());
    assert_eq!(parsed.to_bytes(), bytes);
}

#[test]
fn a_parsed_word_list_holds_the_words_that_went_in() {
    let dictionary = rich_dictionary();
    let words = dictionary.word_list(0).expect("one list was added");

    // "charset" and "content" are both seven bytes, so that group holds two;
    // "encoding" is alone at eight and is padded to two by repetition.
    assert_eq!(words.word_count(7), 2);
    assert_eq!(words.word(7, 0), b"charset");
    assert_eq!(words.word(7, 1), b"content");
    assert_eq!(words.word_count(8), 2);
    assert_eq!(words.word(8, 0), b"encoding");
    assert_eq!(words.word(8, 1), b"encoding");
}

#[test]
fn a_parsed_transform_list_holds_the_transforms_that_went_in() {
    let dictionary = rich_dictionary();
    let transforms = dictionary.transform_list(0).expect("one list was added");

    assert_eq!(transforms.len(), 4);
    assert_eq!(
        transforms.operation(0),
        Some(TransformOperation::Identity),
        "identity"
    );
    assert_eq!(transforms.prefix(1), b"<");
    assert_eq!(transforms.suffix(1), b">");
    assert_eq!(
        transforms.operation(3),
        Some(TransformOperation::ShiftAll(3))
    );
    assert_eq!(transforms.operation(4), None);
}

#[test]
fn the_reference_accepts_what_this_crate_writes() {
    for (name, dictionary) in [
        ("empty", SerializedDictionary::builder().build()),
        (
            "prefix",
            SerializedDictionary::builder()
                .with_prefix(&b"prefix bytes"[..])
                .build(),
        ),
        ("rich", Ok(rich_dictionary())),
    ] {
        let bytes = dictionary.expect("valid").to_bytes();
        assert_agrees_with_c(name, &bytes);
        assert_eq!(c_parse(&bytes).ok, 1, "case {name} was rejected by C");
    }
}

#[test]
fn every_truncation_agrees_with_the_reference() {
    let bytes = rich_dictionary().to_bytes();

    for cut in 0..bytes.len() {
        assert_agrees_with_c(&format!("truncated to {cut}"), &bytes[..cut]);
    }
}

#[test]
fn every_single_byte_mutation_agrees_with_the_reference() {
    let bytes = rich_dictionary().to_bytes();

    for position in 0..bytes.len() {
        for delta in [1u8, 0x0F, 0x40, 0x80, 0xFF] {
            let mut mutated = bytes.clone();
            mutated[position] = mutated[position].wrapping_add(delta);
            assert_agrees_with_c(&format!("byte {position} plus {delta}"), &mutated);
        }
    }
}

#[test]
fn hand_written_malformed_streams_agree_with_the_reference() {
    let cases: [(&str, Vec<u8>); 10] = [
        ("empty", Vec::new()),
        ("magic only", vec![0x91, 0x00]),
        ("wrong magic", vec![0x91, 0x01, 0, 0, 0]),
        ("reversed magic", vec![0x00, 0x91, 0, 0, 0]),
        ("unterminated varint", vec![0x91, 0x00, 0xFF]),
        (
            "prefix longer than the stream",
            vec![0x91, 0x00, 0x40, 0, 0],
        ),
        ("65 word lists", vec![0x91, 0x00, 0x00, 65]),
        ("65 transform lists", vec![0x91, 0x00, 0x00, 0, 65]),
        ("one word list, no combinations", {
            let mut bytes = vec![0x91, 0x00, 0x00, 1];
            bytes.extend_from_slice(&[0u8; 28]);
            bytes.extend_from_slice(&[0, 0]);
            bytes
        }),
        (
            "a transform list with no terminator",
            vec![0x91, 0x00, 0x00, 0, 1, 2, 0, 1, b'a', 0],
        ),
    ];

    for (name, bytes) in cases {
        assert_agrees_with_c(name, &bytes);
    }
}

#[test]
fn a_stream_the_reference_leaves_a_tail_on_is_refused_here() {
    let mut bytes = SerializedDictionary::builder()
        .with_prefix(&b"prefix"[..])
        .build()
        .expect("valid")
        .to_bytes();
    bytes.extend_from_slice(b"trailing");

    // The reference stops once the structure is complete and ignores the rest;
    // this crate refuses it, so a dictionary's bytes and its meaning are one to
    // one. This is the one place the two deliberately disagree.
    assert_eq!(c_parse(&bytes).ok, 1);
    assert!(matches!(
        SerializedDictionary::try_from(&bytes[..]),
        Err(SerializedDictionaryError::TrailingBytes { extra: 8 })
    ));
}

#[test]
fn every_transform_operation_matches_the_reference() {
    let operations = [
        TransformOperation::Identity,
        TransformOperation::FermentFirst,
        TransformOperation::FermentAll,
        TransformOperation::ShiftFirst(1),
        TransformOperation::ShiftAll(1),
        TransformOperation::ShiftFirst(0xFFFF),
        TransformOperation::ShiftAll(0x8000),
    ];
    let omits = (1..=9u8).map(|n| OmitLength::try_from(n).expect("in range"));
    let all: Vec<TransformOperation> = operations
        .into_iter()
        .chain(omits.clone().map(TransformOperation::OmitLast))
        .chain(omits.map(TransformOperation::OmitFirst))
        .collect();

    let mut builder = TransformList::builder();
    for operation in &all {
        builder = builder.add_transform(b"[", *operation, b"]");
    }
    let list = builder.build().expect("well formed");

    // Words that exercise ASCII, a two-byte rune, a three-byte rune and a
    // sequence that ends mid-rune.
    let words: [&[u8]; 4] = [
        b"lowercase!!",
        &[0xC3, 0xA9, b'a', b'b', b'c', b'd'],
        &[0xE2, 0x82, 0xAC, b'x', b'y', b'z'],
        &[b'a', b'b', b'c', b'd', 0xF0, 0x9F],
    ];
    let mut words_builder = WordList::builder();
    for word in words {
        words_builder = words_builder.add_word(word);
    }
    let dictionary = SerializedDictionary::builder()
        .add_word_list(words_builder.build().expect("well formed"))
        .add_transform_list(list)
        .add_combination(DictionaryCombination::new(
            ListSelector::Custom(0),
            ListSelector::Custom(0),
        ))
        .build()
        .expect("valid");
    let bytes = dictionary.to_bytes();
    assert_eq!(c_parse(&bytes).ok, 1, "the reference rejected the fixture");

    let words = dictionary.word_list(0).expect("one list");
    let transforms = dictionary.transform_list(0).expect("one list");
    let mut compared = 0usize;
    for length in 4..=31usize {
        for index in 0..words.word_count(length) {
            for transform in 0..transforms.len() {
                let expected = c_transform(
                    &bytes,
                    0,
                    u32::try_from(length).expect("in range"),
                    u32::try_from(index).expect("in range"),
                    u32::try_from(transform).expect("in range"),
                )
                .expect("the reference transformed the word");
                let actual = transforms.apply(transform, words.word(length, index));

                assert_eq!(
                    actual, expected,
                    "length {length}, word {index}, transform {transform}"
                );
                compared += 1;
            }
        }
    }
    assert!(compared >= all.len(), "only {compared} comparisons ran");
}

#[test]
fn every_builtin_transform_matches_the_reference() {
    // The built-in list reached through a combination that names it, so the
    // reference applies exactly the transforms RFC 7932 fixes.
    let dictionary = SerializedDictionary::builder()
        .add_word_list(
            WordList::builder()
                .add_word(b"alpha")
                .add_word(b"bravo")
                .add_word([0xC3, 0xA9, b'x', b'y'])
                .add_word([0xE2, 0x82, 0xAC, b'z'])
                .build()
                .expect("well formed"),
        )
        .add_combination(DictionaryCombination::new(
            ListSelector::Custom(0),
            ListSelector::Builtin,
        ))
        .build()
        .expect("valid");
    let bytes = dictionary.to_bytes();
    assert_eq!(c_parse(&bytes).ok, 1);

    let words = dictionary.word_list(0).expect("one list");
    let builtin = TransformList::builtin();
    for length in [4usize, 5] {
        for index in 0..words.word_count(length) {
            for transform in 0..builtin.len() {
                let expected = c_transform(
                    &bytes,
                    0,
                    u32::try_from(length).expect("in range"),
                    u32::try_from(index).expect("in range"),
                    u32::try_from(transform).expect("in range"),
                )
                .expect("the reference transformed the word");

                assert_eq!(
                    builtin.apply(transform, words.word(length, index)),
                    expected,
                    "length {length}, word {index}, transform {transform}"
                );
            }
        }
    }
}

#[test]
fn a_serialized_prefix_prepares_the_same_dictionary_a_raw_one_does() {
    let described = SerializedDictionary::builder()
        .with_prefix(&b"a shared prefix worth indexing"[..])
        .build()
        .expect("valid");

    let from_serialized = DictionaryBuilder::new()
        .add_serialized(&described)
        .build()
        .expect("valid");
    let from_raw = DictionaryBuilder::new()
        .add_prefix(&b"a shared prefix worth indexing"[..])
        .build()
        .expect("valid");

    assert_eq!(
        from_serialized.attachment_count(),
        from_raw.attachment_count()
    );
    assert_eq!(from_serialized.source_bytes(), from_raw.source_bytes());
    assert_eq!(from_serialized.retained_bytes(), from_raw.retained_bytes());
}

#[test]
fn a_custom_static_dictionary_prepares_without_a_prefix() {
    let described = SerializedDictionary::builder()
        .add_word_list(
            WordList::builder()
                .add_word(b"payload")
                .build()
                .expect("valid"),
        )
        .build()
        .expect("valid");

    let prepared = DictionaryBuilder::new()
        .add_serialized(&described)
        .build()
        .expect("custom index");
    assert_eq!(prepared.attachment_count(), 0);
    assert!(prepared.retained_bytes() > 0);
}

#[test]
fn custom_index_limits_are_enforced_before_expansion() {
    let described = SerializedDictionary::builder()
        .add_word_list(WordList::builder().add_word(b"word").build().expect("word"))
        .build()
        .expect("dictionary");
    for limits in [
        DictionaryLimits::default().with_max_transformed_word_bytes(1),
        DictionaryLimits::default().with_max_static_entries(1),
        DictionaryLimits::default().with_max_source_bytes(1),
        DictionaryLimits::default().with_max_retained_bytes(1),
    ] {
        assert!(
            DictionaryBuilder::default()
                .add_serialized(&described)
                .with_limits(limits)
                .build()
                .is_err()
        );
    }
    assert_eq!(
        DictionaryLimits::default()
            .with_max_transformed_word_bytes(12)
            .max_transformed_word_bytes(),
        12
    );
    assert_eq!(
        DictionaryLimits::default()
            .with_max_static_entries(34)
            .max_static_entries(),
        34
    );
}

fn decode_custom(dictionary: &[u8], compressed: &[u8], expected: &[u8]) {
    use google_brotli_ffi as ffi;
    let mut output = vec![0; expected.len().max(1)];
    // SAFETY: the dictionary and input outlive the decoder; output is writable
    // for its advertised capacity. The instance is destroyed before returning.
    unsafe {
        let state = ffi::BrotliDecoderCreateInstance(None, None, std::ptr::null_mut());
        assert!(!state.is_null());
        assert_eq!(
            ffi::BrotliDecoderAttachDictionary(
                state,
                ffi::BROTLI_SHARED_DICTIONARY_SERIALIZED,
                dictionary.len(),
                dictionary.as_ptr()
            ),
            ffi::BROTLI_TRUE
        );
        let mut available_in = compressed.len();
        let mut next_in = compressed.as_ptr();
        let mut available_out = output.len();
        let mut next_out = output.as_mut_ptr();
        let mut total = 0;
        let result = ffi::BrotliDecoderDecompressStream(
            state,
            &raw mut available_in,
            &raw mut next_in,
            &raw mut available_out,
            &raw mut next_out,
            &raw mut total,
        );
        ffi::BrotliDecoderDestroyInstance(state);
        assert_eq!(result, ffi::BROTLI_DECODER_RESULT_SUCCESS);
        assert_eq!(available_in, 0);
        output.truncate(total);
    }
    assert_eq!(output, expected);
}

#[test]
fn custom_words_and_transforms_interoperate_at_every_dictionary_quality() {
    use mbrotli::{Compressor, EncoderConfig, Quality};
    use std::io::Write;
    let described = rich_dictionary();
    let bytes = described.to_bytes();
    let prepared = DictionaryBuilder::default()
        .add_serialized(&described)
        .build()
        .expect("prepare");
    let payload =
        b"charset <Content> encoding frqwhqw conte... charset <Charset> HELLO WORLD ".repeat(7);
    for quality in [
        Quality::Q5,
        Quality::Q6,
        Quality::Q7,
        Quality::Q8,
        Quality::Q9,
        Quality::Q10,
        Quality::Q11,
    ] {
        let mut compressor =
            Compressor::new(EncoderConfig::default().with_quality(quality)).expect("config");
        let encoded = compressor
            .compress_with_dictionary(&prepared, &payload)
            .expect("encode");
        decode_custom(&bytes, &encoded, &payload);
        for chunk in [1, 7, 64] {
            let mut writer = compressor
                .writer_with_dictionary(
                    &prepared,
                    Vec::new(),
                    mbrotli::InputSize::Exact(payload.len() as u64).into(),
                )
                .expect("writer");
            for input in payload.chunks(chunk) {
                writer.write_all(input).expect("write");
            }
            let streamed = writer
                .finish()
                .map_err(mbrotli::io::FinishError::into_error)
                .expect("finish");
            assert_eq!(encoded, streamed, "{quality:?}, chunk {chunk}");
        }
    }
}

#[test]
fn preparation_budget_includes_the_description_alive_during_indexing() {
    let mut words = WordList::builder();
    for _ in 0..16384 {
        words = words.add_word(b"abcd");
    }
    let described = SerializedDictionary::builder()
        .add_word_list(words.build().expect("words"))
        .add_transform_list(
            TransformList::builder()
                .add_transform(b"", TransformOperation::Identity, b"")
                .build()
                .expect("transforms"),
        )
        .build()
        .expect("description");
    let retained = DictionaryBuilder::default()
        .add_serialized(&described)
        .build()
        .expect("prepare")
        .retained_bytes();
    let result = DictionaryBuilder::default()
        .add_serialized(&described)
        .with_limits(DictionaryLimits::default().with_max_retained_bytes(retained as u64))
        .build();
    assert!(matches!(
        result,
        Err(DictionaryError::PreparationTooLarge { .. })
    ));
}

#[test]
fn greedy_uses_a_dictionary_containing_only_an_extended_transform() {
    use mbrotli::{Compressor, EncoderConfig, Quality};
    let described = SerializedDictionary::builder()
        .add_word_list(
            WordList::builder()
                .add_word(b"abcdefghijklmnopqrstuvwxyzABCDE")
                .build()
                .expect("words"),
        )
        .add_transform_list(
            TransformList::builder()
                .add_transform(b"<<", TransformOperation::Identity, b">>")
                .build()
                .expect("transforms"),
        )
        .build()
        .expect("description");
    let prepared = DictionaryBuilder::default()
        .add_serialized(&described)
        .build()
        .expect("prepare");
    let input = b"<<abcdefghijklmnopqrstuvwxyzABCDE>> trailing text for dictionary probing";
    for quality in [
        Quality::Q5,
        Quality::Q6,
        Quality::Q7,
        Quality::Q8,
        Quality::Q9,
    ] {
        let mut compressor =
            Compressor::new(EncoderConfig::default().with_quality(quality)).expect("config");
        let output = compressor
            .compress_with_dictionary(&prepared, input)
            .expect("encode");
        decode_custom(&described.to_bytes(), &output, input);
        assert!(
            output.len() < 55,
            "{quality:?}: dictionary was not used ({} bytes)",
            output.len()
        );
    }
}

#[test]
fn every_custom_transform_is_searched_on_every_host_backend_at_greedy_qualities() {
    use mbrotli::Quality;
    let operations = [
        TransformOperation::Identity,
        TransformOperation::FermentFirst,
        TransformOperation::FermentAll,
        TransformOperation::ShiftFirst(1),
        TransformOperation::ShiftAll(3),
    ];
    let omits = (1..=9u8).map(|n| OmitLength::try_from(n).expect("omit"));
    for operation in operations
        .into_iter()
        .chain(omits.clone().map(TransformOperation::OmitFirst))
        .chain(omits.map(TransformOperation::OmitLast))
    {
        let word = b"abcdefghijklmnopqrstuvwxyzABCDE";
        let transforms = TransformList::builder()
            .add_transform(b"<", operation, b">")
            .build()
            .expect("transforms");
        let mut input = transforms.apply(0, word);
        input.extend_from_slice(b" trailing literals");
        let described = SerializedDictionary::builder()
            .add_word_list(WordList::builder().add_word(word).build().expect("words"))
            .add_transform_list(transforms)
            .build()
            .expect("description");
        let prepared = DictionaryBuilder::default()
            .add_serialized(&described)
            .build()
            .expect("prepare");
        for quality in [
            Quality::Q5,
            Quality::Q6,
            Quality::Q7,
            Quality::Q8,
            Quality::Q9,
        ] {
            let mut expected = None;
            for (name, level) in support::host_levels() {
                let mut compressor = support::encoder_on(level, quality, 22);
                let output = compressor
                    .compress_with_dictionary(&prepared, &input)
                    .expect("encode");
                decode_custom(&described.to_bytes(), &output, &input);
                assert!(
                    output.len() + 8 < input.len(),
                    "{operation:?} {quality:?} {name}: dictionary was not used"
                );
                if let Some(expected) = &expected {
                    assert_eq!(&output, expected);
                } else {
                    expected = Some(output);
                }
            }
        }
    }
}

#[test]
fn long_transformed_words_keep_their_base_length_in_hq_commands() {
    use mbrotli::{Compressor, EncoderConfig, Quality};
    let prefix = vec![b'X'; 200];
    let suffix = vec![b'Y'; 200];
    let described = SerializedDictionary::builder()
        .add_word_list(
            WordList::builder()
                .add_word(b"word")
                .build()
                .expect("words"),
        )
        .add_transform_list(
            TransformList::builder()
                .add_transform(&prefix, TransformOperation::Identity, &suffix)
                .build()
                .expect("transforms"),
        )
        .build()
        .expect("dictionary");
    let prepared = DictionaryBuilder::default()
        .add_serialized(&described)
        .build()
        .expect("prepare");
    let mut input = prefix;
    input.extend_from_slice(b"word");
    input.extend_from_slice(&suffix);
    input.extend_from_slice(b" a trailing literal");
    for quality in [Quality::Q5, Quality::Q9, Quality::Q10, Quality::Q11] {
        let mut compressor =
            Compressor::new(EncoderConfig::default().with_quality(quality)).expect("config");
        let encoded = compressor
            .compress_with_dictionary(&prepared, &input)
            .expect("compress");
        decode_custom(&described.to_bytes(), &encoded, &input);
    }
}

fn c_encode_custom(dictionary: &[u8], input: &[u8], quality: mbrotli::Quality) -> Vec<u8> {
    use google_brotli_ffi as ffi;
    let mut output = vec![0; input.len() * 2 + 4096];
    // SAFETY: input/dictionary live through encoder destruction; output has
    // the advertised writable length. Both C instances are destroyed below.
    unsafe {
        let state = ffi::BrotliEncoderCreateInstance(None, None, std::ptr::null_mut());
        assert!(!state.is_null());
        assert_eq!(
            ffi::BrotliEncoderSetParameter(
                state,
                ffi::BROTLI_PARAM_QUALITY,
                u32::from(quality.get())
            ),
            ffi::BROTLI_TRUE
        );
        let prepared = ffi::BrotliEncoderPrepareDictionary(
            ffi::BROTLI_SHARED_DICTIONARY_SERIALIZED,
            dictionary.len(),
            dictionary.as_ptr(),
            i32::from(quality.get()),
            None,
            None,
            std::ptr::null_mut(),
        );
        assert!(!prepared.is_null());
        assert_eq!(
            ffi::BrotliEncoderAttachPreparedDictionary(state, prepared),
            ffi::BROTLI_TRUE
        );
        let mut available_in = input.len();
        let mut next_in = input.as_ptr();
        let mut available_out = output.len();
        let mut next_out = output.as_mut_ptr();
        let mut total = 0;
        assert_eq!(
            ffi::BrotliEncoderCompressStream(
                state,
                ffi::BROTLI_OPERATION_FINISH,
                &raw mut available_in,
                &raw mut next_in,
                &raw mut available_out,
                &raw mut next_out,
                &raw mut total
            ),
            ffi::BROTLI_TRUE
        );
        assert_eq!(ffi::BrotliEncoderIsFinished(state), ffi::BROTLI_TRUE);
        ffi::BrotliEncoderDestroyInstance(state);
        ffi::BrotliEncoderDestroyPreparedDictionary(prepared);
        output.truncate(total);
    }
    output
}

#[test]
fn a_custom_identity_dictionary_matches_c_and_every_host_backend() {
    use mbrotli::Quality;
    use std::io::Write;
    let described = SerializedDictionary::builder()
        .add_word_list(
            WordList::builder()
                .add_word(b"unusualword")
                .add_word(b"otherword")
                .build()
                .expect("words"),
        )
        .add_transform_list(
            TransformList::builder()
                .add_transform(b"", TransformOperation::Identity, b"")
                .build()
                .expect("transforms"),
        )
        .build()
        .expect("dictionary");
    let prepared = DictionaryBuilder::default()
        .add_serialized(&described)
        .build()
        .expect("prepare");
    let payload = b"unusualword otherword unusualword another string otherword ".repeat(13);
    for quality in [Quality::Q5, Quality::Q9, Quality::Q10, Quality::Q11] {
        let expected = c_encode_custom(&described.to_bytes(), &payload, quality);
        decode_custom(&described.to_bytes(), &expected, &payload);
        for (name, level) in support::host_levels() {
            let mut compressor = support::encoder_on(level, quality, 22);
            let mut writer = compressor
                .writer_with_dictionary(&prepared, Vec::new(), Default::default())
                .expect("writer");
            writer.write_all(&payload).expect("write");
            let encoded = writer
                .finish()
                .map_err(mbrotli::io::FinishError::into_error)
                .expect("finish");
            assert_eq!(encoded, expected, "{quality:?}, {name}");
        }
    }
}

#[test]
fn each_resource_limit_refuses_what_it_bounds() {
    let bytes = rich_dictionary().to_bytes();
    let cases: [(&str, DictionaryLimits); 6] = [
        (
            "total size",
            DictionaryLimits::default().with_max_serialized_bytes(8),
        ),
        (
            "LZ77 prefix",
            DictionaryLimits::default().with_max_prefix_bytes(4),
        ),
        (
            "word lists",
            DictionaryLimits::default().with_max_word_lists(0),
        ),
        (
            "word data",
            DictionaryLimits::default().with_max_word_bytes(4),
        ),
        (
            "transform lists",
            DictionaryLimits::default().with_max_transform_lists(0),
        ),
        (
            "transform data",
            DictionaryLimits::default().with_max_transform_bytes(4),
        ),
    ];

    for (what, limits) in cases {
        let outcome = SerializedDictionary::parse(&bytes, limits);
        assert!(
            matches!(
                &outcome,
                Err(SerializedDictionaryError::LimitExceeded { what: hit, .. }) if *hit == what
            ),
            "limit on {what} gave {outcome:?}"
        );
    }
}

#[test]
fn a_combination_limit_refuses_a_dictionary_with_too_many() {
    let bytes = rich_dictionary().to_bytes();
    let limits = DictionaryLimits::default().with_max_combinations(1);

    assert!(matches!(
        SerializedDictionary::parse(&bytes, limits),
        Err(SerializedDictionaryError::LimitExceeded {
            what: "combinations",
            found: 2,
            limit: 1,
        })
    ));
}

#[test]
fn the_limits_are_carried_into_the_builder() {
    let outcome = SerializedDictionary::builder()
        .with_prefix(&b"far too long for this limit"[..])
        .with_limits(DictionaryLimits::default().with_max_prefix_bytes(4))
        .build();

    assert!(matches!(
        outcome,
        Err(SerializedDictionaryError::LimitExceeded {
            what: "LZ77 prefix",
            limit: 4,
            ..
        })
    ));
}

#[test]
fn preparation_rechecks_description_limits_without_reparsing() {
    let described = rich_dictionary();
    for limits in [
        DictionaryLimits::default().with_max_serialized_bytes(0),
        DictionaryLimits::default().with_max_word_bytes(0),
        DictionaryLimits::default().with_max_word_lists(0),
        DictionaryLimits::default().with_max_transform_bytes(0),
        DictionaryLimits::default().with_max_transform_lists(0),
        DictionaryLimits::default().with_max_combinations(0),
    ] {
        let error = DictionaryBuilder::default()
            .add_serialized(&described)
            .with_limits(limits)
            .build()
            .expect_err("preparation ceiling");
        assert!(matches!(error, DictionaryError::LimitExceeded { .. }));
        assert!(error.to_string().contains("exceeds the limit of 0"));
    }
}

#[test]
fn rfc_noncanonical_prefix_varints_may_exceed_the_c_helpers_five_bytes() {
    // Minimized AFL oracle disagreement: RFC 9841 section 4 permits a
    // redundant six-byte zero; C's ReadVarint32 stops after five bytes.
    let bytes = [0x91, 0, 0x80, 0x80, 0x80, 0x80, 0x80, 0, 0, 0];
    let parsed = SerializedDictionary::try_from(bytes.as_slice()).expect("RFC varint");
    assert_eq!(c_parse(&bytes).ok, 0);
    assert_eq!(parsed.to_bytes(), [0x91, 0, 0, 0, 0]);
    assert_eq!(c_parse(&parsed.to_bytes()).ok, 1);
}

#[test]
fn a_context_map_naming_a_missing_combination_is_refused() {
    let outcome = SerializedDictionary::builder()
        .add_word_list(
            WordList::builder()
                .add_word(b"payload")
                .build()
                .expect("valid"),
        )
        .add_combination(DictionaryCombination::new(
            ListSelector::Custom(0),
            ListSelector::Builtin,
        ))
        .with_context_map(ContextMap::uniform(1))
        .build();

    assert!(matches!(
        outcome,
        Err(SerializedDictionaryError::UndefinedReference { .. })
    ));
}

#[test]
fn a_combination_naming_a_missing_list_is_refused() {
    let outcome = SerializedDictionary::builder()
        .add_word_list(
            WordList::builder()
                .add_word(b"payload")
                .build()
                .expect("valid"),
        )
        .add_combination(DictionaryCombination::new(
            ListSelector::Custom(4),
            ListSelector::Builtin,
        ))
        .build();

    assert!(matches!(
        outcome,
        Err(SerializedDictionaryError::UndefinedReference { .. })
    ));
}
