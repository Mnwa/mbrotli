//! Coverage of the public API surface: constructors, conversions, accessors
//! and the error model.

mod support;

use mbrotli::Brotli;
use mbrotli::compressor::shared::SharedBrotliError;
use mbrotli::compressor::{
    BlockBits, BrotliCompressError, CompressMode, CompressParams, Compressor, DistanceCodes,
    ParseBlockBitsError, ParseDistanceCodesError, ParseQualityLevelError, ParseWindowBitsError,
    QualityLevel, WindowBits,
};
use std::io::{Read, Write};
use support::{IMPLEMENTED_QUALITIES, c_decompress, params};

#[test]
fn a_compressor_can_be_built_from_a_level_or_from_brotli() {
    let level = fearless_simd::Level::new();
    let from_level = Compressor::from(level);
    let from_brotli = Compressor::from(Brotli::from(level));
    let from_entry = Brotli::from(level).compressor();

    let parameters = params(QualityLevel::Q0, 22);
    let expected = from_level
        .compress(parameters, b"identical output please")
        .expect("compression failed");
    for compressor in [from_brotli, from_entry] {
        let actual = compressor
            .compress(parameters, b"identical output please")
            .expect("compression failed");
        assert_eq!(actual, expected);
    }
}

#[test]
fn parameters_report_what_they_were_built_with() {
    let parameters = CompressParams::new(QualityLevel::Q1, WindowBits::MIN);
    assert_eq!(usize::from(parameters.quality()), 1);
    assert_eq!(parameters.lgwin(), WindowBits::MIN);
    assert_eq!(WindowBits::default(), WindowBits::DEFAULT);
}

#[test]
fn quality_levels_round_trip_through_their_numeric_value() {
    for value in 0usize..=11 {
        let quality = QualityLevel::try_from(value).expect("valid quality");
        assert_eq!(usize::from(quality), value);
    }
    assert!(matches!(
        QualityLevel::try_from(12),
        Err(ParseQualityLevelError::UpperBound)
    ));
}

#[test]
fn error_messages_describe_what_went_wrong() {
    assert!(
        ParseQualityLevelError::LowerBound
            .to_string()
            .contains("positive")
    );
    assert!(
        ParseQualityLevelError::UpperBound
            .to_string()
            .contains("11")
    );
    assert!(ParseWindowBitsError::LowerBound.to_string().contains("10"));
    assert!(ParseWindowBitsError::UpperBound.to_string().contains("24"));
    assert!(
        BrotliCompressError::UnsupportedQuality(7)
            .to_string()
            .contains('7')
    );
    assert!(
        BrotliCompressError::OutputTooSmall
            .to_string()
            .contains("too small")
    );
    assert!(
        BrotliCompressError::BufferOverflow
            .to_string()
            .contains("overflow")
    );
    assert!(
        BrotliCompressError::BoundOverflow
            .to_string()
            .contains("overflow")
    );
}

#[test]
fn encoder_errors_travel_through_the_io_error_conversion() {
    let io_error = std::io::Error::from(BrotliCompressError::UnsupportedQuality(5));
    assert_eq!(io_error.kind(), std::io::ErrorKind::Other);

    let inner = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "gone");
    let wrapped = std::io::Error::from(BrotliCompressError::IOError(inner));
    assert_eq!(wrapped.kind(), std::io::ErrorKind::BrokenPipe);
}

#[test]
fn the_bound_rejects_arithmetic_that_cannot_fit() {
    let compressor = Brotli::default().compressor();
    let parameters = params(QualityLevel::Q0, 10);
    assert!(compressor.calculate_bound(&parameters, 4096).is_ok());
    assert!(matches!(
        compressor.calculate_bound(&parameters, usize::MAX),
        Err(BrotliCompressError::BoundOverflow)
    ));
}

#[test]
fn the_slice_entry_point_reports_a_buffer_that_is_one_byte_short() {
    let compressor = Brotli::default().compressor();
    for quality in IMPLEMENTED_QUALITIES {
        let parameters = params(quality, 22);
        let input = b"a payload long enough that compressing it actually shrinks it a lot lot lot";
        let expected = compressor
            .compress(parameters, input)
            .expect("compression failed");

        let mut exact = vec![0u8; expected.len()];
        let written = compressor
            .compress_to_slice(parameters, input, &mut exact)
            .expect("an exactly sized buffer must be accepted");
        assert_eq!(written, expected.len());
        assert_eq!(exact, expected);

        let mut short = vec![0u8; expected.len() - 1];
        assert!(matches!(
            compressor.compress_to_slice(parameters, input, &mut short),
            Err(BrotliCompressError::OutputTooSmall)
        ));

        let mut empty: [u8; 0] = [];
        assert!(matches!(
            compressor.compress_to_slice(parameters, b"", &mut empty),
            Err(BrotliCompressError::OutputTooSmall)
        ));
        let mut one = [0u8; 1];
        assert_eq!(
            compressor
                .compress_to_slice(parameters, b"", &mut one)
                .expect("one byte is enough for an empty input"),
            1
        );
    }
}

#[test]
fn every_quality_the_format_defines_is_reachable_from_every_entry_point() {
    let compressor = Brotli::default().compressor();
    let mut buffer = [0u8; 256];

    for quality in IMPLEMENTED_QUALITIES {
        let parameters = params(quality, 22);
        let expected = compressor
            .compress(parameters, b"data data data")
            .unwrap_or_else(|error| panic!("q{quality:?}: {error}"));
        assert!(!expected.is_empty(), "q{quality:?} produced nothing");

        let written = compressor
            .compress_to_slice(parameters, b"data data data", &mut buffer)
            .unwrap_or_else(|error| panic!("q{quality:?} slice: {error}"));
        assert_eq!(&buffer[..written], expected.as_slice(), "q{quality:?}");

        let mut sink = compressor.compress_writer(parameters, Vec::new());
        sink.write_all(b"data data data")
            .unwrap_or_else(|error| panic!("q{quality:?} writer: {error}"));
        sink.finish()
            .unwrap_or_else(|error| panic!("q{quality:?} finish: {error}"));

        let mut source = compressor.compress_reader(parameters, &b"data data data"[..]);
        let mut out = Vec::new();
        source
            .read_to_end(&mut out)
            .unwrap_or_else(|error| panic!("q{quality:?} reader: {error}"));
        assert!(!out.is_empty(), "q{quality:?} reader produced nothing");
    }
}

#[test]
fn the_streaming_adapters_expose_their_inner_stream() {
    let compressor = Brotli::default().compressor();
    let parameters = params(QualityLevel::Q0, 22);

    let mut sink = compressor.compress_writer(parameters, Vec::new());
    assert!(sink.get_ref().is_empty());
    sink.write_all(b"payload").expect("write failed");
    sink.flush().expect("flush failed");
    let compressed = sink.finish().expect("finish failed");
    assert_eq!(
        c_decompress(&compressed, 7).as_deref(),
        Some(&b"payload"[..])
    );

    let source = compressor.compress_reader(parameters, &b"payload"[..]);
    assert_eq!(source.get_ref().len(), 7);
}

#[test]
fn a_reader_yields_nothing_for_a_zero_length_buffer() {
    let compressor = Brotli::default().compressor();
    let parameters = params(QualityLevel::Q1, 22);
    let mut source = compressor.compress_reader(parameters, &b"payload"[..]);
    let mut empty: [u8; 0] = [];
    assert_eq!(source.read(&mut empty).expect("read failed"), 0);
}

#[test]
fn the_default_entry_point_uses_a_detected_level() {
    let brotli = Brotli::default();
    let parameters = params(QualityLevel::Q0, 22);
    let compressed = brotli
        .compressor()
        .compress(parameters, b"detected level output")
        .expect("compression failed");
    assert_eq!(
        c_decompress(&compressed, 21).as_deref(),
        Some(&b"detected level output"[..])
    );
    // `Brotli` is `Copy` and `Debug`, which the public API documents.
    let copied = brotli;
    assert!(!format!("{copied:?}").is_empty());
}

#[test]
fn the_new_parameters_default_to_the_encoders_own_choice() {
    let parameters = params(QualityLevel::Q5, 22);
    assert_eq!(parameters.mode(), CompressMode::default());
    assert_eq!(parameters.mode(), CompressMode::Generic);
    assert_eq!(parameters.distance_codes(), DistanceCodes::default());
    assert_eq!(DistanceCodes::default(), DistanceCodes::DEFAULT);
    assert_eq!(DistanceCodes::default().postfix_bits(), 0);
    assert_eq!(DistanceCodes::default().direct_codes(), 0);
    assert!(parameters.lgblock().is_none());
    assert!(parameters.size_hint().is_none());
    assert!(parameters.literal_context_modeling());
}

#[test]
fn every_parameter_survives_being_set() {
    let codes = DistanceCodes::try_from((2u32, 8u32)).expect("a valid layout");
    let parameters = params(QualityLevel::Q5, 22)
        .with_mode(CompressMode::Font)
        .with_block_bits(Some(BlockBits::MAX))
        .with_size_hint(Some(4 << 20))
        .with_distance_codes(codes)
        .with_literal_context_modeling(false);

    assert_eq!(parameters.quality(), QualityLevel::Q5);
    assert_eq!(parameters.mode(), CompressMode::Font);
    assert_eq!(parameters.lgblock(), Some(BlockBits::MAX));
    assert_eq!(parameters.size_hint(), Some(4 << 20));
    assert_eq!(parameters.distance_codes(), codes);
    assert!(!parameters.literal_context_modeling());

    // Setting a parameter back restores the encoder's own choice.
    let restored = parameters.with_block_bits(None).with_size_hint(None);
    assert!(restored.lgblock().is_none());
    assert!(restored.size_hint().is_none());
}

#[test]
fn block_bits_reject_everything_outside_the_encoders_range() {
    assert_eq!(BlockBits::try_from(16).ok(), Some(BlockBits::MIN));
    assert_eq!(BlockBits::try_from(24).ok(), Some(BlockBits::MAX));
    assert_eq!(usize::from(BlockBits::MIN), 16);
    assert_eq!(usize::from(BlockBits::MAX), 24);
    for value in [0usize, 1, 15] {
        assert!(matches!(
            BlockBits::try_from(value),
            Err(ParseBlockBitsError::LowerBound)
        ));
    }
    for value in [25usize, 64, usize::MAX] {
        assert!(matches!(
            BlockBits::try_from(value),
            Err(ParseBlockBitsError::UpperBound)
        ));
    }
    assert!(BlockBits::MIN < BlockBits::MAX);
}

#[test]
fn distance_codes_reject_layouts_the_format_cannot_express() {
    for postfix in 0u32..=3 {
        for groups in 0u32..16 {
            let direct = groups << postfix;
            if direct > 120 {
                continue;
            }
            let codes = DistanceCodes::try_from((postfix, direct))
                .unwrap_or_else(|error| panic!("({postfix}, {direct}) rejected: {error}"));
            assert_eq!(codes.postfix_bits(), postfix);
            assert_eq!(codes.direct_codes(), direct);
        }
    }
    assert!(matches!(
        DistanceCodes::try_from((4u32, 0u32)),
        Err(ParseDistanceCodesError::PostfixBits)
    ));
    assert!(matches!(
        DistanceCodes::try_from((0u32, 121u32)),
        Err(ParseDistanceCodesError::DirectCodes)
    ));
    assert!(matches!(
        DistanceCodes::try_from((2u32, 6u32)),
        Err(ParseDistanceCodesError::Misaligned)
    ));
    // Sixteen groups is one too many for the four-bit field.
    assert!(matches!(
        DistanceCodes::try_from((0u32, 16u32)),
        Err(ParseDistanceCodesError::Misaligned)
    ));
}

#[test]
fn the_new_error_messages_describe_what_went_wrong() {
    assert_eq!(
        BlockBits::try_from(0).unwrap_err().to_string(),
        "Block bits should be greater than or equal to 16"
    );
    assert_eq!(
        BlockBits::try_from(99).unwrap_err().to_string(),
        "Block bits should be less than or equal to 24"
    );
    assert_eq!(
        DistanceCodes::try_from((7u32, 0u32))
            .unwrap_err()
            .to_string(),
        "Distance postfix bits should be less than or equal to 3"
    );
    assert_eq!(
        DistanceCodes::try_from((0u32, 200u32))
            .unwrap_err()
            .to_string(),
        "Direct distance codes should be less than or equal to 120"
    );
    assert_eq!(
        DistanceCodes::try_from((3u32, 4u32))
            .unwrap_err()
            .to_string(),
        "Direct distance codes should be a whole number of postfix groups"
    );
}

#[test]
fn quality_levels_order_by_effort() {
    assert!(QualityLevel::Q0 < QualityLevel::Q1);
    assert!(QualityLevel::Q3 < QualityLevel::Q5);
    assert!(QualityLevel::Q9 < QualityLevel::Q11);
    assert_eq!(QualityLevel::Q5, QualityLevel::Q5);

    let mut sorted = [QualityLevel::Q5, QualityLevel::Q0, QualityLevel::Q3];
    sorted.sort();
    assert_eq!(
        sorted,
        [QualityLevel::Q0, QualityLevel::Q3, QualityLevel::Q5]
    );
}

#[test]
fn every_implemented_quality_compresses_and_round_trips() {
    let compressor = Brotli::default().compressor();
    let payload: Vec<u8> = (0..100_000u32).map(|index| (index % 251) as u8).collect();
    for quality in IMPLEMENTED_QUALITIES {
        let compressed = compressor
            .compress(params(quality, 22), &payload)
            .unwrap_or_else(|error| panic!("quality {quality:?}: {error}"));
        assert!(compressed.len() < payload.len(), "quality {quality:?}");
        assert_eq!(
            c_decompress(&compressed, payload.len()).as_deref(),
            Some(payload.as_slice()),
            "quality {quality:?}"
        );
    }
}

#[test]
fn the_size_hint_is_what_makes_streaming_match_one_shot() {
    use std::io::Write;

    let compressor = Brotli::default().compressor();
    let payload: Vec<u8> = (0..(2 << 20u32)).map(|index| (index % 251) as u8).collect();

    // Without a hint the one-shot path substitutes the input length and the
    // streaming path does not, which quality five reacts to.
    let unpinned = params(QualityLevel::Q5, 22);
    let one_shot = compressor
        .compress(unpinned, &payload)
        .expect("compression failed");
    let mut sink = compressor.compress_writer(unpinned, Vec::new());
    sink.write_all(&payload).expect("write failed");
    let streamed = sink.finish().expect("finish failed");
    assert_ne!(streamed, one_shot);

    // Pinning it makes them agree.
    let pinned = unpinned.with_size_hint(Some(payload.len()));
    let one_shot = compressor
        .compress(pinned, &payload)
        .expect("compression failed");
    let mut sink = compressor.compress_writer(pinned, Vec::new());
    sink.write_all(&payload).expect("write failed");
    let streamed = sink.finish().expect("finish failed");
    assert_eq!(streamed, one_shot);
}

#[test]
fn a_large_window_is_a_separate_constructor_not_a_wider_range() -> Result<(), ParseWindowBitsError>
{
    // Every size the ordinary header allows is also a legal large window, and
    // the two are never the same value.
    for bits in 10u8..=24 {
        let ordinary = WindowBits::standard(bits)?;
        let large = WindowBits::large(bits)?;
        assert_eq!(ordinary.bits(), large.bits());
        assert_ne!(ordinary, large);
        assert!(!ordinary.is_large());
        assert!(large.is_large());
    }
    // And the large header reaches sizes the ordinary one cannot express.
    for bits in 25u8..=62 {
        assert!(WindowBits::large(bits)?.is_large());
        assert!(WindowBits::standard(bits).is_err());
    }
    Ok(())
}

#[test]
fn window_bits_reject_what_their_header_cannot_express() {
    assert!(matches!(
        WindowBits::standard(0),
        Err(ParseWindowBitsError::LowerBound)
    ));
    assert!(matches!(
        WindowBits::large(9),
        Err(ParseWindowBitsError::LowerBound)
    ));
    assert!(matches!(
        WindowBits::standard(25),
        Err(ParseWindowBitsError::UpperBound)
    ));
    assert!(matches!(
        WindowBits::large(63),
        Err(ParseWindowBitsError::LargeUpperBound)
    ));
}

#[test]
fn the_shared_error_travels_transparently() {
    let inner = SharedBrotliError::UnsupportedLargeWindow { quality: 0 };
    assert_eq!(
        inner.to_string(),
        "Quality level 0 does not implement large window Brotli"
    );

    let outer = BrotliCompressError::from(inner);
    // `#[error(transparent)]`: the wrapper adds no text of its own.
    assert_eq!(outer.to_string(), inner.to_string());
    assert!(matches!(
        outer,
        BrotliCompressError::Shared(SharedBrotliError::UnsupportedLargeWindow { quality: 0 })
    ));

    // And it survives the trip through `std::io::Error` the adapters take.
    let converted = std::io::Error::from(BrotliCompressError::from(inner));
    assert!(converted.to_string().contains("large window"));
}

#[test]
fn a_finished_empty_stream_still_declares_its_large_window()
-> Result<(), Box<dyn std::error::Error>> {
    let compressor = Brotli::default().compressor();
    let params = CompressParams::new(QualityLevel::Q5, WindowBits::large(30)?);

    // The one-shot shortcut answers an empty input with an ordinary one-byte
    // stream, matching the reference; a streaming session has no such shortcut
    // and emits the header that was asked for.
    assert_eq!(compressor.compress(params, b"")?, vec![6]);
    let streamed = compressor.compress_writer(params, Vec::new()).finish()?;
    assert_eq!(streamed[0], 0x11, "the large window marker");
    assert_eq!(streamed[1] & 0x3F, 30, "the declared window");
    Ok(())
}
