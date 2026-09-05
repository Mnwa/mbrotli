//! Coverage of the public API surface, from outside the crate.
//!
//! Constructors, conversions, accessors, the error model, and the promise that
//! every entry point reaches the same bytes.

mod support;

use mbrotli::dictionary::{DictionaryBuilder, DictionaryError, DictionaryLimits};
use mbrotli::io::FinishError;
use mbrotli::{
    BlockBits, BlockSize, CompressionMode, Compressor, ConfigError, DistanceParams, EncodeError,
    EncoderConfig, EncoderStatus, InputSize, LiteralContextMode, Operation, Progress, Quality,
    RetentionPolicy, SizeOverflow, StreamConfig, Window, WindowEncoding,
};
use std::io::{Read, Write};
use support::{IMPLEMENTED_QUALITIES, c_decompress, config, encoder};

#[test]
fn quality_round_trips_through_its_numeric_value() {
    for value in 0u8..=11 {
        let quality = Quality::try_from(value).expect("a legal quality");
        assert_eq!(quality.get(), value);
        assert_eq!(u8::from(quality), value);
    }
    assert_eq!(
        Quality::try_from(12u8),
        Err(ConfigError::Quality { requested: 12 })
    );
    assert_eq!(Quality::MIN, Quality::Q0);
    assert_eq!(Quality::MAX, Quality::Q11);
    assert_eq!(Quality::default(), Quality::Q11);
    assert!(Quality::Q1 < Quality::Q5);
}

#[test]
fn the_default_configuration_is_the_reference_encoders() {
    let config = EncoderConfig::default();
    assert_eq!(config.quality(), Quality::Q11);
    assert_eq!(config.window(), Window::DEFAULT);
    assert_eq!(config.window().bits(), 22);
    assert_eq!(config.window().encoding(), WindowEncoding::Standard);
    assert_eq!(config.block_size(), BlockSize::Auto);
    assert_eq!(config.mode(), CompressionMode::Generic);
    assert_eq!(config.distance(), DistanceParams::Auto);
    assert_eq!(config.literal_context(), LiteralContextMode::Auto);
}

#[test]
fn every_configuration_setter_survives_a_round_trip() {
    let codes = DistanceParams::explicit(2, 8).expect("a legal layout");
    let config = EncoderConfig::default()
        .with_quality(Quality::Q5)
        .with_window(Window::large(40).expect("a legal window"))
        .with_block_size(BlockSize::Bits(BlockBits::MAX))
        .with_mode(CompressionMode::Font)
        .with_distance(codes)
        .with_literal_context(LiteralContextMode::Disabled);

    assert_eq!(config.quality(), Quality::Q5);
    assert_eq!(config.window().bits(), 40);
    assert_eq!(config.block_size(), BlockSize::Bits(BlockBits::MAX));
    assert_eq!(config.mode(), CompressionMode::Font);
    assert_eq!(config.distance(), codes);
    assert_eq!(config.literal_context(), LiteralContextMode::Disabled);

    // Setting a value back restores the encoder's own choice.
    let restored = config
        .with_block_size(BlockSize::Auto)
        .with_distance(DistanceParams::Auto);
    assert_eq!(restored.block_size(), BlockSize::Auto);
    assert_eq!(restored.distance(), DistanceParams::Auto);
}

#[test]
fn block_bits_accept_exactly_the_encoders_range() {
    for bits in 16u8..=24 {
        let block = BlockBits::try_from(bits).expect("a legal block size");
        assert_eq!(block.get(), bits);
        assert_eq!(usize::from(block), usize::from(bits));
        assert_eq!(BlockSize::from(block), BlockSize::Bits(block));
    }
    for bits in [0u8, 15, 25, u8::MAX] {
        assert_eq!(
            BlockBits::try_from(bits),
            Err(ConfigError::BlockBits { requested: bits })
        );
    }
    assert_eq!(BlockBits::MIN.get(), 16);
    assert_eq!(BlockBits::MAX.get(), 24);
    assert_eq!(BlockSize::default(), BlockSize::Auto);
}

#[test]
fn distance_layouts_the_format_cannot_express_are_refused() {
    for postfix in 0u8..=3 {
        for groups in 0u16..16 {
            let direct = groups << postfix;
            if direct > DistanceParams::MAX_DIRECT_CODES {
                continue;
            }
            assert_eq!(
                DistanceParams::explicit(postfix, direct),
                Ok(DistanceParams::Explicit {
                    postfix_bits: postfix,
                    direct_codes: direct
                })
            );
        }
    }
    assert_eq!(
        DistanceParams::explicit(4, 0),
        Err(ConfigError::DistancePostfixBits { requested: 4 })
    );
    assert_eq!(
        DistanceParams::explicit(0, 121),
        Err(ConfigError::DirectDistanceCodes { requested: 121 })
    );
    assert_eq!(
        DistanceParams::explicit(2, 6),
        Err(ConfigError::MisalignedDistanceCodes {
            postfix_bits: 2,
            direct_codes: 6
        })
    );
    assert_eq!(DistanceParams::MAX_POSTFIX_BITS, 3);
    assert_eq!(DistanceParams::MAX_DIRECT_CODES, 120);
}

#[test]
fn a_compressor_reports_what_it_was_built_with() {
    let config = config(Quality::Q5, 18);
    let mut encoder = Compressor::new(config).expect("a legal configuration");

    assert_eq!(*encoder.config(), config);
    assert_eq!(encoder.retention(), RetentionPolicy::Aggressive);
    assert_eq!(encoder.retained_bytes(), 0);

    encoder
        .compress(b"payload payload")
        .expect("compression failed");
    assert!(encoder.retained_bytes() > 0);
}

#[test]
fn the_bound_covers_every_stream_and_reports_an_overflow() {
    for size in [0usize, 1, 1024, 1 << 16] {
        let bound = Compressor::max_compressed_size(size).expect("a representable bound");
        assert!(bound >= size);
    }
    assert_eq!(
        Compressor::max_compressed_size(usize::MAX),
        Err(SizeOverflow)
    );
    assert!(
        SizeOverflow
            .to_string()
            .contains("overflows the address space")
    );
}

#[test]
fn every_quality_is_reachable_from_every_entry_point() {
    let payload = b"data data data data data";
    for quality in IMPLEMENTED_QUALITIES {
        let mut encoder = encoder(quality, 22);
        let expected = encoder
            .compress(payload)
            .unwrap_or_else(|error| panic!("q{}: {error}", quality.get()));
        assert!(!expected.is_empty());

        let mut appended = Vec::new();
        let range = encoder
            .compress_into(payload, &mut appended)
            .expect("appending failed");
        assert_eq!(&appended[range], expected.as_slice());

        let mut buffer = vec![0u8; Compressor::max_compressed_size(payload.len()).expect("bound")];
        let written = encoder
            .compress_to_slice(payload, &mut buffer)
            .expect("the slice entry point failed");
        assert_eq!(&buffer[..written], expected.as_slice());

        // The streaming shapes need the size declared to reach the same bytes.
        let stream = StreamConfig::from(InputSize::Exact(payload.len() as u64));
        let mut sink = encoder.writer(Vec::new(), stream).expect("a legal stream");
        sink.write_all(payload).expect("write failed");
        let streamed = sink
            .finish()
            .map_err(FinishError::into_error)
            .expect("finish failed");
        assert_eq!(streamed, expected, "q{}: writer", quality.get());

        let mut source = encoder
            .reader(&payload[..], stream)
            .expect("a legal stream");
        let mut pulled = Vec::new();
        source.read_to_end(&mut pulled).expect("read failed");
        assert_eq!(pulled, expected, "q{}: reader", quality.get());
    }
}

#[test]
fn the_slice_entry_point_reports_a_destination_one_byte_short() {
    for quality in IMPLEMENTED_QUALITIES {
        let mut encoder = encoder(quality, 22);
        let input = b"a payload long enough that compressing it actually shrinks it a lot lot lot";
        let expected = encoder.compress(input).expect("compression failed");

        let mut exact = vec![0u8; expected.len()];
        let written = encoder
            .compress_to_slice(input, &mut exact)
            .expect("an exactly sized destination must be accepted");
        assert_eq!(written, expected.len());
        assert_eq!(exact, expected);

        let mut short = vec![0u8; expected.len() - 1];
        assert!(matches!(
            encoder.compress_to_slice(input, &mut short),
            Err(EncodeError::OutputTooSmall { .. })
        ));

        let mut empty: [u8; 0] = [];
        assert!(matches!(
            encoder.compress_to_slice(b"", &mut empty),
            Err(EncodeError::OutputTooSmall { provided: 0 })
        ));
        let mut one = [0u8; 1];
        assert_eq!(
            encoder
                .compress_to_slice(b"", &mut one)
                .expect("one byte is enough for an empty input"),
            1
        );
    }
}

#[test]
fn the_streaming_adapters_expose_their_inner_stream() {
    let mut encoder = encoder(Quality::Q0, 22);

    let mut sink = encoder
        .writer(Vec::new(), StreamConfig::default())
        .expect("a legal stream");
    assert!(sink.get_ref().is_empty());
    sink.get_mut().reserve(64);
    sink.write_all(b"payload").expect("write failed");
    sink.flush().expect("flush failed");
    assert!(!sink.is_finished());
    let compressed = sink
        .finish()
        .map_err(FinishError::into_error)
        .expect("finish failed");
    assert_eq!(
        c_decompress(&compressed, 7).as_deref(),
        Some(&b"payload"[..])
    );

    let mut source = encoder
        .reader(&b"payload"[..], StreamConfig::default())
        .expect("a legal stream");
    assert_eq!(source.get_ref().len(), 7);
    assert_eq!(source.get_mut().len(), 7);
    assert!(!source.is_finished());
}

#[test]
fn a_stream_configuration_carries_only_what_one_stream_knows() {
    assert_eq!(StreamConfig::default().input_size(), InputSize::Unknown);
    assert_eq!(StreamConfig::default().stream_offset(), 0);
    assert_eq!(InputSize::default(), InputSize::Unknown);
    assert_eq!(InputSize::from(4096u64), InputSize::Exact(4096));

    let stream = StreamConfig::from(InputSize::Exact(10)).with_stream_offset(64);
    assert_eq!(stream.input_size(), InputSize::Exact(10));
    assert_eq!(stream.stream_offset(), 64);
    assert_eq!(
        StreamConfig::default().with_input_size(InputSize::Exact(10)),
        StreamConfig::from(InputSize::Exact(10))
    );
}

#[test]
fn a_non_zero_stream_offset_is_refused_rather_than_ignored() {
    let mut encoder = encoder(Quality::Q1, 22);
    let stream = StreamConfig::default().with_stream_offset(64);

    assert!(matches!(
        encoder.start(stream),
        Err(EncodeError::UnsupportedStreamOffset { offset: 64 })
    ));
    assert!(matches!(
        encoder.writer(Vec::new(), stream),
        Err(EncodeError::UnsupportedStreamOffset { offset: 64 })
    ));
    assert!(matches!(
        encoder.reader(&b"payload"[..], stream),
        Err(EncodeError::UnsupportedStreamOffset { offset: 64 })
    ));
    // And the compressor is still perfectly usable.
    assert!(
        !encoder
            .compress(b"payload")
            .expect("compression failed")
            .is_empty()
    );
}

#[test]
fn a_session_reports_exactly_what_it_moved() {
    let mut encoder = encoder(Quality::Q1, 22);
    let mut session = encoder
        .start(StreamConfig::from(InputSize::Exact(7)))
        .expect("a legal stream");
    let mut output = [0u8; 512];

    let progress = session
        .process(b"payload", &mut output, Operation::Finish)
        .expect("the session failed");
    assert_eq!(progress.consumed, 7);
    assert!(progress.produced > 0);
    assert_eq!(progress.status, EncoderStatus::Finished);
    assert!(session.is_finished());

    // `Progress` is a plain value a caller can build and compare.
    let sample = Progress {
        consumed: 1,
        produced: 2,
        status: EncoderStatus::NeedsInput,
    };
    assert_eq!(sample.consumed, 1);
    assert_eq!(sample.produced, 2);
    assert_ne!(sample.status, EncoderStatus::Finished);
    assert_eq!(Operation::default(), Operation::Process);
}

#[test]
fn a_configuration_error_says_what_was_refused() {
    assert!(
        ConfigError::Quality { requested: 12 }
            .to_string()
            .contains("12")
    );
    assert!(
        ConfigError::BlockBits { requested: 15 }
            .to_string()
            .contains("16..=24")
    );
    let refused = Compressor::new(
        EncoderConfig::default()
            .with_quality(Quality::Q0)
            .with_window(Window::large(30).expect("a legal window")),
    )
    .expect_err("quality zero cannot carry a large window");
    assert_eq!(
        refused,
        ConfigError::LargeWindowUnsupportedForQuality {
            quality: Quality::Q0
        }
    );
    assert!(refused.to_string().contains("large window"));
}

#[test]
fn an_encoding_error_travels_through_the_io_conversion() {
    let io = std::io::Error::from(EncodeError::OutputTooSmall { provided: 4 });
    assert_eq!(io.kind(), std::io::ErrorKind::WriteZero);
    assert!(io.to_string().contains('4'));

    let io = std::io::Error::from(EncodeError::AbandonedSession);
    assert_eq!(io.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn a_dictionary_is_immutable_shareable_and_refused_where_it_cannot_be_read() {
    let dictionary = DictionaryBuilder::new()
        .add_prefix(&b"a common prefix worth sharing"[..])
        .build()
        .expect("prepared");
    assert_eq!(dictionary.attachment_count(), 1);
    assert_eq!(dictionary.source_bytes(), 29);
    assert!(dictionary.retained_bytes() > dictionary.source_bytes());

    for quality in IMPLEMENTED_QUALITIES {
        let mut encoder = encoder(quality, 22);
        let outcome = encoder.compress_with_dictionary(&dictionary, b"a common prefix");
        if quality >= Quality::Q5 {
            assert!(outcome.is_ok(), "q{} refused a dictionary", quality.get());
        } else {
            assert!(
                matches!(
                    outcome,
                    Err(EncodeError::DictionaryUnsupportedForQuality { quality: reported })
                        if reported == quality
                ),
                "q{} did not refuse a dictionary",
                quality.get()
            );
        }
    }
}

#[test]
fn a_dictionary_with_nothing_in_it_is_refused() {
    assert_eq!(
        DictionaryBuilder::new().build().unwrap_err(),
        DictionaryError::Empty
    );
    assert_eq!(
        DictionaryBuilder::new()
            .add_prefix(&b""[..])
            .build()
            .unwrap_err(),
        DictionaryError::Empty
    );
    assert!(DictionaryError::Empty.to_string().contains("no bytes"));
}

#[test]
fn the_dictionary_limits_expose_their_documented_defaults() {
    let limits = DictionaryLimits::default();
    assert_eq!(limits.max_source_bytes(), 64 << 20);
    assert_eq!(limits.max_prefix_bytes(), 64 << 20);
    assert_eq!(limits.max_retained_bytes(), 1 << 30);

    let tightened = limits
        .with_max_source_bytes(1)
        .with_max_prefix_bytes(2)
        .with_max_retained_bytes(3);
    assert_eq!(tightened.max_source_bytes(), 1);
    assert_eq!(tightened.max_prefix_bytes(), 2);
    assert_eq!(tightened.max_retained_bytes(), 3);
    assert_ne!(tightened, limits);
}

#[test]
fn the_public_types_are_send_and_the_dictionary_is_sync() {
    const fn assert_send<T: Send>() {}
    const fn assert_sync<T: Sync>() {}
    assert_send::<Compressor>();
    assert_send::<mbrotli::CompressorBuilder>();
    assert_send::<mbrotli::dictionary::PreparedDictionary>();
    assert_sync::<mbrotli::dictionary::PreparedDictionary>();
    assert_send::<EncoderConfig>();
    assert_sync::<EncoderConfig>();
}

#[test]
fn a_compressor_moves_between_threads() {
    let mut encoder = encoder(Quality::Q5, 22);
    let expected = encoder
        .compress(b"payload payload")
        .expect("compression failed");

    let moved = std::thread::spawn(move || {
        encoder
            .compress(b"payload payload")
            .expect("compression failed")
    })
    .join()
    .expect("the worker finished");

    assert_eq!(moved, expected);
}

#[test]
fn every_implemented_quality_compresses_and_round_trips() {
    let payload: Vec<u8> = (0..100_000u32).map(|index| (index % 251) as u8).collect();
    for quality in IMPLEMENTED_QUALITIES {
        let compressed = encoder(quality, 22)
            .compress(&payload)
            .unwrap_or_else(|error| panic!("quality {}: {error}", quality.get()));
        assert!(
            compressed.len() < payload.len(),
            "quality {}",
            quality.get()
        );
        assert_eq!(
            c_decompress(&compressed, payload.len()).as_deref(),
            Some(payload.as_slice()),
            "quality {}",
            quality.get()
        );
    }
}

#[test]
fn every_public_type_reports_something_useful_when_debugged() {
    // `missing_debug_implementations` is denied crate-wide, so every one of
    // these exists; this is what checks they say something rather than nothing.
    let dictionary = DictionaryBuilder::new()
        .add_prefix(&b"a prefix"[..])
        .build()
        .expect("prepared");
    let builder = Compressor::builder(config(Quality::Q5, 22));
    assert!(format!("{builder:?}").contains("CompressorBuilder"));

    let mut encoder = builder.build().expect("a legal configuration");
    assert!(format!("{encoder:?}").contains("Compressor"));
    assert!(format!("{:?}", DictionaryBuilder::new()).contains("DictionaryBuilder"));
    assert!(format!("{dictionary:?}").contains("PreparedDictionary"));
    assert!(format!("{:?}", EncoderConfig::default()).contains("EncoderConfig"));
    assert!(format!("{:?}", StreamConfig::default()).contains("StreamConfig"));
    assert!(format!("{:?}", RetentionPolicy::default()).contains("Aggressive"));

    {
        let session = encoder
            .start(StreamConfig::default())
            .expect("a legal stream");
        assert!(format!("{session:?}").contains("EncoderSession"));
    }
    {
        let sink = encoder
            .writer(Vec::new(), StreamConfig::default())
            .expect("a legal stream");
        let rendered = format!("{sink:?}");
        assert!(rendered.contains("EncoderWriter"));
        assert!(rendered.contains("undelivered"));
    }
    {
        let source = encoder
            .reader(&b"payload"[..], StreamConfig::default())
            .expect("a legal stream");
        let rendered = format!("{source:?}");
        assert!(rendered.contains("EncoderReader"));
        assert!(rendered.contains("buffered"));

        let parts = source.into_parts();
        assert!(format!("{parts:?}").contains("EncoderReaderParts"));
    }
}

#[test]
fn a_finish_failure_carries_both_halves_and_prints_them() {
    use std::error::Error as _;
    use std::io::ErrorKind;

    /// A sink that refuses everything, so finishing always fails.
    #[derive(Debug)]
    struct Refusing;
    impl Write for Refusing {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(ErrorKind::WouldBlock, "never"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut encoder = encoder(Quality::Q1, 22);
    {
        let mut sink = encoder
            .writer(Refusing, StreamConfig::default())
            .expect("a legal stream");
        // Buffered, so the refusal only surfaces when the stream is finished.
        sink.write_all(b"payload payload").unwrap_or_default();

        let failure = sink.finish().expect_err("the sink refuses everything");
        assert_eq!(failure.error().kind(), ErrorKind::WouldBlock);
        assert!(format!("{failure:?}").contains("FinishError"));
        assert!(failure.to_string().contains("could not be finished"));
        assert!(failure.source().is_some());

        // Both halves come back rather than the adapter being destroyed.
        let (error, writer) = failure.into_parts();
        assert_eq!(error.kind(), ErrorKind::WouldBlock);
        assert!(!writer.is_finished());
    }

    // And the failure converts into a plain I/O error when the caller does not
    // want the adapter back.
    let sink = encoder
        .writer(Refusing, StreamConfig::default())
        .expect("a legal stream");
    let converted = sink
        .finish()
        .expect_err("the sink refuses everything")
        .into_error();
    assert_eq!(converted.kind(), ErrorKind::WouldBlock);
    let sink = encoder
        .writer(Refusing, StreamConfig::default())
        .expect("stream");
    let converted = std::io::Error::from(sink.finish().expect_err("refused"));
    assert_eq!(converted.kind(), ErrorKind::WouldBlock);
}
