//! Byte-for-byte differential tests for qualities two to nine.
//!
//! Every parameter these qualities react to changes the emitted bytes, so each
//! one is compared against the pinned C encoder configured identically. The C
//! side goes through the streaming API, which is the only way to set a size
//! hint, a block size or distance parameters explicitly.
//!
//! How much input is coming is no longer an encoder setting here: it belongs to
//! the stream. A case that declares the true length is compared through the
//! one-shot path as well, because that is what the one-shot path declares for
//! itself; a case that declares something else can only be expressed by a
//! session, and is compared through one.

mod support;

use google_brotli_ffi as ffi;
use mbrotli::io::FinishError;
use mbrotli::{
    BlockBits, BlockSize, CompressionMode, Compressor, DistanceParams, EncoderConfig, InputSize,
    LiteralContextMode, Quality, StreamConfig, Window,
};
use std::io::Write;
use support::{CParams, GREEDY_QUALITIES, c_compress_with, c_decompress, vendor_file};

/// One configuration to compare, expressed once for both encoders.
#[derive(Copy, Clone, Debug)]
struct Case {
    quality: Quality,
    lgwin: u8,
    mode: CompressionMode,
    size_hint: Option<usize>,
    lgblock: Option<u8>,
    distance_codes: (u8, u16),
    literal_context_modeling: bool,
}

impl Case {
    /// Returns the defaults for a quality and window size.
    fn new(quality: Quality, lgwin: u8) -> Self {
        Self {
            quality,
            lgwin,
            mode: CompressionMode::Generic,
            size_hint: None,
            lgblock: None,
            distance_codes: (0, 0),
            literal_context_modeling: true,
        }
    }

    /// Builds the configuration this crate's encoder takes.
    fn rust(&self) -> EncoderConfig {
        let mut config = EncoderConfig::default()
            .with_quality(self.quality)
            .with_window(Window::standard(self.lgwin).expect("window size out of range"))
            .with_mode(self.mode)
            .with_literal_context(if self.literal_context_modeling {
                LiteralContextMode::Auto
            } else {
                LiteralContextMode::Disabled
            })
            .with_distance(
                DistanceParams::explicit(self.distance_codes.0, self.distance_codes.1)
                    .expect("distance codes out of range"),
            );
        if let Some(lgblock) = self.lgblock {
            config = config.with_block_size(BlockSize::Bits(
                BlockBits::try_from(lgblock).expect("block size in range"),
            ));
        }
        config
    }

    /// Builds the stream configuration, for an input of `input_len` bytes.
    fn stream(&self, input_len: usize) -> StreamConfig {
        StreamConfig::from(InputSize::Exact(self.size_hint.unwrap_or(input_len) as u64))
    }

    /// Builds the parameters the C harness takes, for the same input length.
    fn c(&self, input_len: usize) -> CParams {
        let mut params = CParams::new(
            std::ffi::c_int::from(self.quality.get()),
            i32::from(self.lgwin),
        );
        params.mode = match self.mode {
            CompressionMode::Generic => ffi::BROTLI_MODE_GENERIC,
            CompressionMode::Text => ffi::BROTLI_MODE_TEXT,
            CompressionMode::Font => ffi::BROTLI_MODE_FONT,
        };
        // The one-shot entry point substitutes the input length, exactly as
        // `BrotliEncoderCompress` does.
        params.size_hint = Some(self.size_hint.unwrap_or(input_len) as u32);
        params.lgblock = self.lgblock.map(u32::from);
        params.npostfix = u32::from(self.distance_codes.0);
        params.ndirect = u32::from(self.distance_codes.1);
        params.disable_literal_context_modeling = !self.literal_context_modeling;
        params
    }
}

/// Compresses `data` through a session declaring the case's size.
fn compress(case: Case, data: &[u8]) -> Vec<u8> {
    let mut encoder = Compressor::new(case.rust()).expect("a legal configuration");
    let mut sink = encoder
        .writer(Vec::new(), case.stream(data.len()))
        .expect("a legal stream");
    sink.write_all(data).expect("write failed");
    sink.finish()
        .map_err(FinishError::into_error)
        .expect("finish failed")
}

/// Compresses `data` in one shot, which declares its true length.
fn compress_one_shot(case: Case, data: &[u8]) -> Vec<u8> {
    Compressor::new(case.rust())
        .expect("a legal configuration")
        .compress(data)
        .expect("compression failed")
}

/// Compresses `data` both ways and asserts the streams are identical.
fn assert_matches_c(name: &str, data: &[u8], case: Case) {
    let expected = c_compress_with(case.c(data.len()), data);
    let actual = compress(case, data);
    if actual != expected {
        let prefix = actual
            .iter()
            .zip(&expected)
            .take_while(|(left, right)| left == right)
            .count();
        panic!(
            "case {name} {case:?}: {} bytes against {} bytes, first difference at byte {prefix}",
            actual.len(),
            expected.len()
        );
    }

    // A case that declares the true length is also what the one-shot path
    // declares for itself, so the two have to agree, including empty input.
    if case.size_hint.is_none() {
        let one_shot = compress_one_shot(case, data);
        assert_eq!(
            one_shot, expected,
            "case {name} {case:?}: API shapes differ"
        );
    }

    assert_eq!(
        c_decompress(&actual, data.len().max(1)).as_deref(),
        Some(data),
        "case {name}: the stream does not decode back"
    );
}

/// A text corpus long enough to exercise splitting and context modelling.
fn text() -> Vec<u8> {
    vendor_file("alice29.txt")
}

/// A binary corpus with a very different symbol distribution.
fn binary() -> Vec<u8> {
    let mut data = vendor_file("bb.binast");
    data.truncate(2 << 20);
    data
}

#[test]
fn every_window_size_matches_the_c_encoder() {
    let corpora = [("text", text()), ("binary", binary())];
    for (name, data) in &corpora {
        for quality in GREEDY_QUALITIES {
            for lgwin in 10..=24u8 {
                assert_matches_c(name, data, Case::new(quality, lgwin));
            }
        }
    }
}

#[test]
fn the_size_hint_boundary_selects_the_large_match_finder() {
    // Quality four switches from H4 to H54, and quality five from H5 to H6 when
    // the window is wide enough, at exactly one mebibyte. Declaring a size other
    // than the true one is a stream's business, so this runs through a session.
    let data = text();
    for quality in GREEDY_QUALITIES {
        for lgwin in [16u8, 17, 18, 19, 22] {
            for hint in [0usize, (1 << 20) - 1, 1 << 20, (1 << 20) + 1, 8 << 20] {
                let mut case = Case::new(quality, lgwin);
                case.size_hint = Some(hint);
                assert_matches_c("size-hint", &data, case);
            }
        }
    }
}

#[test]
fn the_small_window_match_finders_are_reached_from_quality_five() {
    // A window of sixteen bits or fewer routes qualities five and up to a
    // forgetful chain matcher instead of a bucketed one: H40 for five and six,
    // H41 for seven and eight, H42 for nine.
    let data = text();
    for quality in [
        Quality::Q5,
        Quality::Q6,
        Quality::Q7,
        Quality::Q8,
        Quality::Q9,
    ] {
        for lgwin in [10u8, 14, 16, 17] {
            assert_matches_c("small-window", &data, Case::new(quality, lgwin));
        }
    }
}

#[test]
fn every_mode_matches_the_c_encoder() {
    let data = text();
    for quality in GREEDY_QUALITIES {
        for mode in [
            CompressionMode::Generic,
            CompressionMode::Text,
            CompressionMode::Font,
        ] {
            let mut case = Case::new(quality, 22);
            case.mode = mode;
            assert_matches_c("mode", &data, case);
        }
    }
}

#[test]
fn font_mode_uses_the_reference_distance_parameters() {
    // Font mode asks for one postfix bit and twelve direct codes, but only from
    // quality four upwards.
    let data = text();
    for quality in GREEDY_QUALITIES {
        let mut generic = Case::new(quality, 22);
        let mut font = Case::new(quality, 22);
        font.mode = CompressionMode::Font;
        assert_matches_c("font", &data, font);

        generic.mode = CompressionMode::Generic;
        let with_font = compress(font, &data);
        let with_generic = compress(generic, &data);
        if quality <= Quality::Q3 {
            // `MIN_QUALITY_FOR_NONZERO_DISTANCE_PARAMS` is four, so qualities
            // two and three encode distances with the default alphabet however
            // the mode is set.
            assert_eq!(
                with_font,
                with_generic,
                "quality {} must ignore the font distance parameters",
                quality.get()
            );
        } else {
            assert_ne!(
                with_font,
                with_generic,
                "quality {} ignored the font distance parameters",
                quality.get()
            );
        }
    }
}

#[test]
fn every_valid_distance_layout_matches_the_c_encoder() {
    let data = text();
    for quality in GREEDY_QUALITIES {
        for postfix in 0u8..=3 {
            for groups in [0u16, 1, 4, 15] {
                let direct = groups << postfix;
                if direct > 120 {
                    continue;
                }
                let mut case = Case::new(quality, 22);
                case.distance_codes = (postfix, direct);
                assert_matches_c("distance-codes", &data, case);
            }
        }
    }
}

#[test]
fn an_unrepresentable_distance_layout_is_refused_by_the_type() {
    // The reference silently falls back to zero for a layout it cannot express;
    // this crate refuses to build one at all, so that path is unreachable from
    // the public API.
    assert!(DistanceParams::explicit(4, 0).is_err());
    assert!(DistanceParams::explicit(0, 121).is_err());
    assert!(DistanceParams::explicit(2, 6).is_err());
    assert!(DistanceParams::explicit(3, 120).is_ok());
}

#[test]
fn every_block_size_matches_the_c_encoder() {
    let data = text();
    for quality in GREEDY_QUALITIES {
        for lgblock in [16u8, 17, 18, 20, 24] {
            let mut case = Case::new(quality, 22);
            case.lgblock = Some(lgblock);
            assert_matches_c("lgblock", &data, case);
        }
    }
}

#[test]
fn quality_three_ignores_the_requested_block_size() {
    // `ComputeLgBlock` pins qualities below four to fourteen bits.
    let data = text();
    let default = Case::new(Quality::Q3, 22);
    let baseline = compress(default, &data);
    for lgblock in [16u8, 20, 24] {
        let mut case = default;
        case.lgblock = Some(lgblock);
        assert_eq!(
            compress(case, &data),
            baseline,
            "quality three honoured lgblock {lgblock}"
        );
    }
}

#[test]
fn disabling_literal_context_modeling_matches_the_c_encoder() {
    let data = text();
    for quality in GREEDY_QUALITIES {
        let mut case = Case::new(quality, 22);
        case.literal_context_modeling = false;
        assert_matches_c("no-context", &data, case);
    }
}

/// Text whose previous byte predicts the next one well enough to earn a context
/// model: alternating one-byte and two-byte UTF-8 sequences.
fn context_friendly() -> Vec<u8> {
    let mut data = Vec::new();
    while data.len() < (1 << 18) {
        data.extend_from_slice("añbñcñdñ eñfñgñhñ".as_bytes());
    }
    data
}

#[test]
fn only_quality_five_and_above_react_to_literal_context_modeling() {
    let data = context_friendly();
    for quality in GREEDY_QUALITIES {
        let on = Case::new(quality, 22);
        let mut off = on;
        off.literal_context_modeling = false;
        assert_matches_c("context-on", &data, on);
        assert_matches_c("context-off", &data, off);
        let with = compress(on, &data);
        let without = compress(off, &data);
        if quality >= Quality::Q5 {
            assert_ne!(
                with,
                without,
                "quality {} ignored the setting",
                quality.get()
            );
        } else {
            assert_eq!(
                with,
                without,
                "quality {} honoured the setting",
                quality.get()
            );
        }
    }
}

#[test]
fn the_complex_context_map_is_reachable_from_quality_five() {
    // The thirteen-context map needs a declared size of at least one mebibyte
    // and data whose contexts predict the next byte well.
    let mut data = Vec::new();
    while data.len() < (2 << 20) {
        data.extend_from_slice(&text());
    }
    data.truncate(2 << 20);

    for quality in [Quality::Q5, Quality::Q7, Quality::Q9] {
        let mut small = Case::new(quality, 22);
        small.size_hint = Some((1 << 20) - 1);
        let mut large = Case::new(quality, 22);
        large.size_hint = Some(1 << 20);
        assert_matches_c("complex-map-below", &data, small);
        assert_matches_c("complex-map-at", &data, large);
        assert_ne!(
            compress(small, &data),
            compress(large, &data),
            "the declared size did not change the context map at quality {}",
            quality.get()
        );
    }
}

#[test]
fn the_three_context_model_is_reachable_from_quality_seven() {
    // `MIN_QUALITY_FOR_HQ_CONTEXT_MODELING` is seven: below it the reference
    // prices the continuation map out of reach, so the same data has to be
    // modelled with at most two contexts.
    let data = context_friendly();
    for quality in [
        Quality::Q5,
        Quality::Q6,
        Quality::Q7,
        Quality::Q8,
        Quality::Q9,
    ] {
        assert_matches_c("hq-contexts", &data, Case::new(quality, 22));
    }
}

#[test]
fn quality_nine_defaults_to_a_larger_input_block() {
    // `ComputeLgBlock` raises the default block to `min(18, lgwin)` at quality
    // nine, so the meta-block boundaries move with the window.
    let source = text();
    let mut data = Vec::with_capacity(1 << 20);
    while data.len() < (1 << 20) {
        data.extend_from_slice(&source);
    }
    data.truncate(1 << 20);
    for lgwin in [16u8, 17, 18, 22] {
        assert_matches_c("q9-lgblock", &data, Case::new(Quality::Q9, lgwin));
        assert_matches_c("q8-lgblock", &data, Case::new(Quality::Q8, lgwin));
    }
}

#[test]
fn the_sparse_search_threshold_matches_the_c_encoder_at_quality_nine() {
    // Quality nine waits five hundred and twelve literals before it starts
    // striding, every other quality sixty-four. Incompressible data reaches both
    // thresholds, and the stride decides which positions are stored.
    let mut rng = 0x0FF1_CE01u64;
    let mut data: Vec<u8> = (0..200_000u32)
        .map(|_| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng >> 24) as u8
        })
        .collect();
    // A compressible tail gives the stored positions something to match.
    data.extend_from_slice(&text());
    data.extend_from_slice(&text());
    for quality in [Quality::Q8, Quality::Q9] {
        assert_matches_c("sparse-threshold", &data, Case::new(quality, 22));
    }
}

#[test]
fn the_delayed_symbol_bound_is_reached_at_quality_three() {
    // Below the block-splitting quality the encoder flushes once it has buffered
    // `0x2FFF` symbols, so a stream that crosses that bound has to agree with
    // the reference on where the meta-blocks end.
    let bound = 0x2FFF;
    for length in [bound - 1, bound, bound + 1, 2 * bound, 4 * bound + 7] {
        // Literal-only data reaches the bound one symbol per byte.
        let mut rng = 0x51ED_0001u64;
        let literals: Vec<u8> = (0..length)
            .map(|_| {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                (rng >> 24) as u8
            })
            .collect();
        assert_matches_c("delayed-literals", &literals, Case::new(Quality::Q3, 22));

        // Short repeats reach it through commands instead.
        let commands: Vec<u8> = (0..length).map(|index| (index % 3) as u8).collect();
        assert_matches_c("delayed-commands", &commands, Case::new(Quality::Q3, 22));
    }
}

#[test]
fn input_block_boundaries_match_the_c_encoder() {
    // Quality three works in sixteen kibibyte blocks and the others in
    // sixty-four kibibyte blocks; every boundary is a chance to disagree.
    let source = text();
    for quality in GREEDY_QUALITIES {
        let block = if quality == Quality::Q3 {
            1 << 14
        } else {
            1 << 16
        };
        for length in [block - 1, block, block + 1, 2 * block, 3 * block + 5] {
            let mut data = Vec::with_capacity(length);
            while data.len() < length {
                data.extend_from_slice(&source);
            }
            data.truncate(length);
            assert_matches_c("block-boundary", &data, Case::new(quality, 22));
        }
    }
}

#[test]
fn the_static_dictionary_is_used_where_the_reference_uses_it() {
    // Dictionary words with no match inside the input can only come from the
    // static dictionary, and only the match finders that probe it will find
    // them.
    let mut data = Vec::new();
    for word in [
        "time",
        "download",
        "government",
        "information",
        "description",
        "background",
    ] {
        data.extend_from_slice(word.as_bytes());
        data.push(b' ');
    }
    for quality in GREEDY_QUALITIES {
        for lgwin in [10u8, 16, 22] {
            assert_matches_c("dictionary", &data, Case::new(quality, lgwin));
        }
    }
}

#[test]
fn ring_buffer_wrapping_matches_the_c_encoder() {
    // A small window forces the ring buffer to lap several times, so matches
    // have to be found across the wrap.
    let source = binary();
    for quality in GREEDY_QUALITIES {
        for lgwin in [10u8, 11, 12] {
            assert_matches_c("wrap", &source, Case::new(quality, lgwin));
        }
    }
}

#[test]
fn incompressible_input_takes_the_uncompressed_path() {
    let mut rng = 0x0BAD_C0DE_1234_5678u64;
    let data: Vec<u8> = (0..(1 << 18))
        .map(|_| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng >> 24) as u8
        })
        .collect();
    for quality in GREEDY_QUALITIES {
        assert_matches_c("incompressible", &data, Case::new(quality, 22));
    }
}

#[test]
fn short_inputs_match_the_c_encoder_at_every_length() {
    let source = text();
    for quality in GREEDY_QUALITIES {
        for length in 0..300usize {
            assert_matches_c("short", &source[..length], Case::new(quality, 22));
        }
    }
}
