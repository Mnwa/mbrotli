//! RFC 9841 Large Window Brotli, over the public API.
//!
//! Every stream produced here is handed back to the pinned Google Brotli C
//! decoder with `BROTLI_DECODER_PARAM_LARGE_WINDOW` set, which is an
//! independent implementation of the header this crate writes.

mod support;

use mbrotli::io::FinishError;
use mbrotli::{
    Compressor, ConfigError, EncoderConfig, InputSize, Quality, StreamConfig, Window,
    WindowEncoding,
};
use std::io::{Read, Write};
use support::{
    Corpus, boundary_corpora, c_decompress, c_decompress_large_window, host_levels,
    structural_corpora,
};

/// Every quality that implements the large window.
const LARGE_WINDOW_QUALITIES: [Quality; 9] = [
    Quality::Q3,
    Quality::Q4,
    Quality::Q5,
    Quality::Q6,
    Quality::Q7,
    Quality::Q8,
    Quality::Q9,
    Quality::Q10,
    Quality::Q11,
];

/// Widest window the pinned C decoder accepts (`BROTLI_LARGE_MAX_WBITS`).
///
/// RFC 9841 allows 62, but the pinned reference is built for 32-bit arithmetic
/// and rejects a declared window above 30. Streams wider than this are checked
/// against the RFC directly instead; see
/// `a_window_wider_than_the_c_decoder_only_changes_the_header`.
const C_DECODER_MAX_WINDOW_BITS: u8 = 30;

/// Qualities whose distance model cannot carry a large window.
const REFUSING_QUALITIES: [Quality; 3] = [Quality::Q0, Quality::Q1, Quality::Q2];

/// Builds a configuration asking for a large window of `bits` bits.
fn large(quality: Quality, bits: u8) -> Result<EncoderConfig, ConfigError> {
    Ok(EncoderConfig::default()
        .with_quality(quality)
        .with_window(Window::large(bits)?))
}

/// Builds a compressor asking for a large window of `bits` bits.
fn encoder(quality: Quality, bits: u8) -> Result<Compressor, ConfigError> {
    Compressor::new(large(quality, bits)?)
}

/// Overwrites the six window bits of a large-window stream header.
///
/// The header is fourteen bits: an eight-bit marker in the first byte, then the
/// window in the low six bits of the second. Everything above that belongs to
/// the first meta-block.
fn repoint_window(stream: &[u8], bits: u8) -> Vec<u8> {
    let mut patched = stream.to_vec();
    patched[1] = (patched[1] & 0xC0) | (bits & 0x3F);
    patched
}

#[test]
fn the_default_configuration_asks_for_no_large_window() {
    assert_eq!(
        EncoderConfig::default().window().encoding(),
        WindowEncoding::Standard
    );
}

#[test]
fn the_window_carries_both_the_size_and_the_syntax() -> Result<(), ConfigError> {
    let window = Window::large(40)?;
    let config = EncoderConfig::default()
        .with_quality(Quality::Q5)
        .with_window(window);

    assert_eq!(config.window(), window);
    assert_eq!(config.window().bits(), 40);
    assert_eq!(config.window().encoding(), WindowEncoding::Large);
    Ok(())
}

#[test]
fn the_window_range_is_the_one_rfc_9841_allows() {
    assert_eq!(
        Window::large(9),
        Err(ConfigError::LargeWindow { requested: 9 })
    );
    assert_eq!(
        Window::large(63),
        Err(ConfigError::LargeWindow { requested: 63 })
    );
    for bits in 10u8..=62 {
        assert!(Window::large(bits).is_ok(), "{bits} bits");
    }
}

#[test]
fn the_stream_header_is_the_marker_and_six_window_bits() -> Result<(), Box<dyn std::error::Error>> {
    // A payload short enough that the first meta-block starts inside the second
    // byte, so the header bits are the only thing under test.
    let payload = b"large window payload large window payload";

    for bits in 10u8..=62 {
        let encoded = encoder(Quality::Q5, bits)?.compress(payload)?;
        let header = u16::from(encoded[0]) | (u16::from(encoded[1]) << 8);
        assert_eq!(header & 0xFF, 0x11, "{bits} bits: marker");
        assert_eq!((header >> 8) & 0x3F, u16::from(bits), "{bits} bits: window");
    }
    Ok(())
}

#[test]
fn every_window_round_trips_through_the_c_decoder() -> Result<(), Box<dyn std::error::Error>> {
    let payload: Vec<u8> = "the quick brown fox jumps over the lazy dog "
        .repeat(400)
        .into_bytes();

    for bits in 10u8..=C_DECODER_MAX_WINDOW_BITS {
        for quality in LARGE_WINDOW_QUALITIES {
            let encoded = encoder(quality, bits)?.compress(&payload)?;
            assert_eq!(
                c_decompress_large_window(&encoded, payload.len()).as_deref(),
                Some(payload.as_slice()),
                "{bits} bits at quality {}",
                quality.get()
            );
        }
    }
    Ok(())
}

#[test]
fn a_window_wider_than_the_c_decoder_only_changes_the_header()
-> Result<(), Box<dyn std::error::Error>> {
    let payload: Vec<u8> = "the quick brown fox jumps over the lazy dog "
        .repeat(400)
        .into_bytes();

    // The encoder keeps at most thirty bits of history whatever the header
    // declares, so every wider stream is the thirty-bit stream with a different
    // window written into it. That is what makes the range above the C decoder's
    // limit checkable without a 64-bit decoder: the payload is one the C decoder
    // has already accepted.
    for quality in LARGE_WINDOW_QUALITIES {
        let baseline = encoder(quality, C_DECODER_MAX_WINDOW_BITS)?.compress(&payload)?;
        assert_eq!(
            c_decompress_large_window(&baseline, payload.len()).as_deref(),
            Some(payload.as_slice()),
            "quality {}",
            quality.get()
        );
        for bits in (C_DECODER_MAX_WINDOW_BITS + 1)..=62 {
            assert_eq!(
                encoder(quality, bits)?.compress(&payload)?,
                repoint_window(&baseline, bits),
                "{bits} bits at quality {}",
                quality.get()
            );
        }
    }
    Ok(())
}

#[test]
fn a_large_window_stream_needs_a_large_window_decoder() -> Result<(), Box<dyn std::error::Error>> {
    let payload = b"a large window header is not an ordinary one".repeat(20);

    // Above 24 bits the header is not expressible in RFC 7932 at all, so an
    // ordinary decoder has to reject it.
    let encoded = encoder(Quality::Q5, 30)?.compress(&payload)?;
    assert_eq!(c_decompress(&encoded, payload.len()), None);
    assert_eq!(
        c_decompress_large_window(&encoded, payload.len()).as_deref(),
        Some(payload.as_slice())
    );
    Ok(())
}

#[test]
fn a_large_window_is_never_the_same_stream_as_an_ordinary_one()
-> Result<(), Box<dyn std::error::Error>> {
    let payload = b"selection is explicit, never inferred".repeat(30);

    for quality in LARGE_WINDOW_QUALITIES {
        let ordinary = EncoderConfig::default()
            .with_quality(quality)
            .with_window(Window::standard(22)?);
        // The same numeric window still switches syntax when asked for.
        let requested = large(quality, 22)?;
        assert_ne!(
            Compressor::new(ordinary)?.compress(&payload)?,
            Compressor::new(requested)?.compress(&payload)?,
            "quality {}",
            quality.get()
        );
    }
    Ok(())
}

#[test]
fn the_qualities_that_cannot_carry_one_refuse_when_the_compressor_is_built()
-> Result<(), Box<dyn std::error::Error>> {
    // `SanitizeParams` forces `large_window` off at or below quality two; this
    // crate refuses instead of silently dropping the request, and does it before
    // any input has been touched.
    for quality in REFUSING_QUALITIES {
        assert_eq!(
            Compressor::new(large(quality, 30)?).err(),
            Some(ConfigError::LargeWindowUnsupportedForQuality { quality }),
            "quality {} accepted a large window",
            quality.get()
        );
        // An ordinary window at the same quality is fine.
        assert!(
            Compressor::new(
                EncoderConfig::default()
                    .with_quality(quality)
                    .with_window(Window::standard(22)?)
            )
            .is_ok()
        );
    }
    Ok(())
}

#[test]
fn an_empty_input_preserves_the_window_across_api_shapes() -> Result<(), Box<dyn std::error::Error>>
{
    for quality in LARGE_WINDOW_QUALITIES {
        for bits in [10u8, 24, 30, 62] {
            let mut compressor = encoder(quality, bits)?;
            let encoded = compressor.compress(b"")?;
            let streamed = compressor
                .writer(Vec::new(), StreamConfig::default())?
                .finish()
                .map_err(FinishError::into_error)?;
            assert_eq!(
                encoded,
                streamed,
                "{bits} bits at quality {}",
                quality.get()
            );
            assert_eq!(encoded[0], 0x11);
            assert_eq!(encoded[1] & 0x3f, bits);
            if bits <= C_DECODER_MAX_WINDOW_BITS {
                assert_eq!(
                    c_decompress_large_window(&encoded, 1).as_deref(),
                    Some(b"".as_slice())
                );
            }
        }
    }
    Ok(())
}

#[test]
fn a_finished_empty_stream_still_declares_its_large_window()
-> Result<(), Box<dyn std::error::Error>> {
    // Empty input keeps the declared header, just like every other API shape.
    let mut compressor = encoder(Quality::Q5, 30)?;
    let streamed = compressor
        .writer(Vec::new(), StreamConfig::default())?
        .finish()
        .map_err(FinishError::into_error)?;

    assert_eq!(streamed[0], 0x11, "the large window marker");
    assert_eq!(streamed[1] & 0x3F, 30, "the declared window");
    Ok(())
}

#[test]
fn a_tiny_input_under_the_widest_window_still_decodes() -> Result<(), Box<dyn std::error::Error>> {
    for length in [1usize, 2, 3, 7, 16, 64, 256, 1024, 4096] {
        let payload: Vec<u8> = (0..length).map(|index| (index % 251) as u8).collect();
        for quality in LARGE_WINDOW_QUALITIES {
            let encoded = encoder(quality, C_DECODER_MAX_WINDOW_BITS)?.compress(&payload)?;
            assert_eq!(
                c_decompress_large_window(&encoded, payload.len()).as_deref(),
                Some(payload.as_slice()),
                "{length} bytes at quality {}",
                quality.get()
            );
            // The widest legal declaration reaches the same bytes.
            assert_eq!(
                encoder(quality, 62)?.compress(&payload)?,
                repoint_window(&encoded, 62),
                "{length} bytes at quality {}",
                quality.get()
            );
        }
    }
    Ok(())
}

#[test]
fn declaring_a_wider_window_than_the_input_costs_nothing() -> Result<(), Box<dyn std::error::Error>>
{
    let payload: Vec<u8> = "history repeats ".repeat(4000).into_bytes();

    // Everything from the widest legal declaration down to the point where the
    // encoder's own history is the binding constraint compresses identically:
    // the header is all that changed.
    let widest = encoder(Quality::Q5, 62)?.compress(&payload)?;
    for bits in [30u8, 40, 50, 61] {
        assert_eq!(
            encoder(Quality::Q5, bits)?.compress(&payload)?.len(),
            widest.len(),
            "{bits} bits"
        );
    }
    Ok(())
}

#[test]
fn the_bound_covers_every_large_window_stream() -> Result<(), Box<dyn std::error::Error>> {
    let corpora: Vec<Corpus> = structural_corpora()
        .into_iter()
        .chain(boundary_corpora())
        .collect();

    for corpus in corpora {
        for quality in [Quality::Q3, Quality::Q5, Quality::Q9] {
            for bits in [10u8, 24, 30, 62] {
                let mut compressor = encoder(quality, bits)?;
                let bound = Compressor::max_compressed_size(corpus.data.len())?;
                let encoded = compressor.compress(&corpus.data)?;
                assert!(
                    encoded.len() <= bound,
                    "{}: {bits} bits at quality {}: {} > {bound}",
                    corpus.name,
                    quality.get(),
                    encoded.len()
                );

                let mut buffer = vec![0u8; bound];
                let written = compressor.compress_to_slice(&corpus.data, &mut buffer)?;
                assert_eq!(&buffer[..written], encoded.as_slice(), "{}", corpus.name);
            }
        }
    }
    Ok(())
}

#[test]
fn streaming_and_one_shot_agree() -> Result<(), Box<dyn std::error::Error>> {
    let payload: Vec<u8> = "streamed large window payload ".repeat(500).into_bytes();

    for quality in LARGE_WINDOW_QUALITIES {
        let mut compressor = encoder(quality, 30)?;
        let expected = compressor.compress(&payload)?;
        let stream = StreamConfig::from(InputSize::Exact(payload.len() as u64));

        for chunk in [
            1usize, 2, 3, 7, 8, 15, 16, 31, 32, 63, 64, 127, 128, 255, 256, 4096,
        ] {
            let mut sink = compressor.writer(Vec::new(), stream)?;
            for block in payload.chunks(chunk) {
                sink.write_all(block)?;
            }
            assert_eq!(
                sink.finish().map_err(FinishError::into_error)?,
                expected,
                "quality {}, {chunk} byte chunks",
                quality.get()
            );
        }

        let mut source = compressor.reader(payload.as_slice(), stream)?;
        let mut streamed = Vec::new();
        source.read_to_end(&mut streamed)?;
        assert_eq!(streamed, expected, "quality {}: reader", quality.get());

        assert_eq!(
            c_decompress_large_window(&expected, payload.len()).as_deref(),
            Some(payload.as_slice()),
            "quality {}",
            quality.get()
        );
    }
    Ok(())
}

#[test]
fn every_backend_produces_the_same_large_window_stream() -> Result<(), Box<dyn std::error::Error>> {
    let payload: Vec<u8> = "backends must agree byte for byte "
        .repeat(300)
        .into_bytes();

    for quality in LARGE_WINDOW_QUALITIES {
        for bits in [10u8, 24, 30, 62] {
            let config = large(quality, bits)?;
            let mut expected: Option<Vec<u8>> = None;
            for (name, level) in host_levels() {
                let encoded = Compressor::builder(config)
                    .with_backend(level)
                    .build()?
                    .compress(&payload)?;
                match &expected {
                    None => expected = Some(encoded),
                    Some(first) => assert_eq!(
                        &encoded,
                        first,
                        "{name} disagrees at quality {}, {bits} bits",
                        quality.get()
                    ),
                }
            }
        }
    }
    Ok(())
}

#[test]
fn a_corpus_round_trips_at_every_quality() -> Result<(), Box<dyn std::error::Error>> {
    for corpus in structural_corpora() {
        for quality in LARGE_WINDOW_QUALITIES {
            for bits in [10u8, 25, 30] {
                let encoded = encoder(quality, bits)?.compress(&corpus.data)?;
                assert_eq!(
                    c_decompress_large_window(&encoded, corpus.data.len()).as_deref(),
                    Some(corpus.data.as_slice()),
                    "{}: {bits} bits at quality {}",
                    corpus.name,
                    quality.get()
                );
            }
        }
    }
    Ok(())
}
