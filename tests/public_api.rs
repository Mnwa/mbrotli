//! Coverage of the public API surface: constructors, conversions, accessors
//! and the error model.

mod support;

use mbrotli::Brotli;
use mbrotli::compressor::{
    BrotliCompressError, BrotliCompressParams, BrotliCompressor, BrotliQualityLevel,
    BrotliWindowBits, ParseQualityLevelError, ParseWindowBitsError,
};
use std::io::{Read, Write};
use support::{FAST_QUALITIES, c_decompress, params};

#[test]
fn a_compressor_can_be_built_from_a_level_or_from_brotli() {
    let level = fearless_simd::Level::new();
    let from_level = BrotliCompressor::from(level);
    let from_brotli = BrotliCompressor::from(Brotli::from(level));
    let from_entry = Brotli::from(level).compressor();

    let parameters = params(BrotliQualityLevel::Q0, 22);
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
    let parameters = BrotliCompressParams::new(BrotliQualityLevel::Q1, BrotliWindowBits::MIN);
    assert_eq!(usize::from(parameters.quality()), 1);
    assert_eq!(parameters.lgwin(), BrotliWindowBits::MIN);
    assert_eq!(BrotliWindowBits::default(), BrotliWindowBits::DEFAULT);
}

#[test]
fn quality_levels_round_trip_through_their_numeric_value() {
    for value in [0usize, 1, 2, 3, 4, 5, 6, 7, 8, 9, 11] {
        let quality = BrotliQualityLevel::try_from(value).expect("valid quality");
        assert_eq!(usize::from(quality), value);
    }
    assert!(matches!(
        BrotliQualityLevel::try_from(10),
        Err(ParseQualityLevelError::Unrepresentable)
    ));
    assert!(matches!(
        BrotliQualityLevel::try_from(12),
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
    assert!(
        ParseQualityLevelError::Unrepresentable
            .to_string()
            .contains("10")
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
    let parameters = params(BrotliQualityLevel::Q0, 10);
    assert!(compressor.calculate_bound(&parameters, 4096).is_ok());
    assert!(matches!(
        compressor.calculate_bound(&parameters, usize::MAX),
        Err(BrotliCompressError::BoundOverflow)
    ));
}

#[test]
fn the_slice_entry_point_reports_a_buffer_that_is_one_byte_short() {
    let compressor = Brotli::default().compressor();
    for quality in FAST_QUALITIES {
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
fn unsupported_qualities_are_reported_by_every_entry_point() {
    let compressor = Brotli::default().compressor();
    let parameters = params(BrotliQualityLevel::Q9, 22);
    let mut buffer = [0u8; 64];

    assert!(matches!(
        compressor.compress(parameters, b"data"),
        Err(BrotliCompressError::UnsupportedQuality(9))
    ));
    assert!(matches!(
        compressor.compress_to_slice(parameters, b"data", &mut buffer),
        Err(BrotliCompressError::UnsupportedQuality(9))
    ));

    let mut sink = compressor.compress_writer(parameters, Vec::new());
    assert!(sink.write_all(b"data").is_err());

    let mut source = compressor.compress_reader(parameters, &b"data"[..]);
    let mut out = Vec::new();
    assert!(source.read_to_end(&mut out).is_err());
}

#[test]
fn the_streaming_adapters_expose_their_inner_stream() {
    let compressor = Brotli::default().compressor();
    let parameters = params(BrotliQualityLevel::Q0, 22);

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
    let parameters = params(BrotliQualityLevel::Q1, 22);
    let mut source = compressor.compress_reader(parameters, &b"payload"[..]);
    let mut empty: [u8; 0] = [];
    assert_eq!(source.read(&mut empty).expect("read failed"), 0);
}

#[test]
fn the_default_entry_point_uses_a_detected_level() {
    let brotli = Brotli::default();
    let parameters = params(BrotliQualityLevel::Q0, 22);
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
