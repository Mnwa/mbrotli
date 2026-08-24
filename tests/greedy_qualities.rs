//! Byte-for-byte differential tests for qualities three, four and five.
//!
//! Every parameter these qualities react to changes the emitted bytes, so each
//! one is compared against the pinned C encoder configured identically. The C
//! side goes through the streaming API, which is the only way to set a size
//! hint, a block size or distance parameters explicitly.

mod support;

use google_brotli_ffi as ffi;
use mbrotli::Brotli;
use mbrotli::compressor::{
    BlockBits, CompressMode, CompressParams, DistanceCodes, QualityLevel, WindowBits,
};
use support::{CParams, GREEDY_QUALITIES, c_compress_with, c_decompress, vendor_file};

/// One configuration to compare, expressed once for both encoders.
#[derive(Copy, Clone, Debug)]
struct Case {
    quality: QualityLevel,
    lgwin: usize,
    mode: CompressMode,
    size_hint: Option<usize>,
    lgblock: Option<usize>,
    distance_codes: (u32, u32),
    literal_context_modeling: bool,
}

impl Case {
    /// Returns the defaults for a quality and window size.
    fn new(quality: QualityLevel, lgwin: usize) -> Self {
        Self {
            quality,
            lgwin,
            mode: CompressMode::Generic,
            size_hint: None,
            lgblock: None,
            distance_codes: (0, 0),
            literal_context_modeling: true,
        }
    }

    /// Builds the parameters this crate's encoder takes.
    fn rust(&self) -> CompressParams {
        let lgwin = WindowBits::try_from(self.lgwin).expect("window size out of range");
        let mut params = CompressParams::new(self.quality, lgwin)
            .with_mode(self.mode)
            .with_size_hint(self.size_hint)
            .with_literal_context_modeling(self.literal_context_modeling)
            .with_distance_codes(
                DistanceCodes::try_from(self.distance_codes).expect("distance codes out of range"),
            );
        if let Some(lgblock) = self.lgblock {
            params = params.with_block_bits(Some(
                BlockBits::try_from(lgblock).expect("block size in range"),
            ));
        }
        params
    }

    /// Builds the parameters the C harness takes, for the same input length.
    fn c(&self, input_len: usize) -> CParams {
        let mut params = CParams::new(usize::from(self.quality) as i32, self.lgwin as i32);
        params.mode = match self.mode {
            CompressMode::Generic => ffi::BROTLI_MODE_GENERIC,
            CompressMode::Text => ffi::BROTLI_MODE_TEXT,
            CompressMode::Font => ffi::BROTLI_MODE_FONT,
        };
        // The one-shot entry point substitutes the input length, exactly as
        // `BrotliEncoderCompress` does.
        params.size_hint = Some(self.size_hint.unwrap_or(input_len) as u32);
        params.lgblock = self.lgblock.map(|bits| bits as u32);
        params.npostfix = self.distance_codes.0;
        params.ndirect = self.distance_codes.1;
        params.disable_literal_context_modeling = !self.literal_context_modeling;
        params
    }
}

/// Compresses `data` both ways and asserts the streams are identical.
fn assert_matches_c(name: &str, data: &[u8], case: Case) {
    let compressor = Brotli::default().compressor();
    let actual = compressor
        .compress(case.rust(), data)
        .expect("compression failed");
    let expected = c_compress_with(case.c(data.len()), data);
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
    assert_eq!(
        c_decompress(&actual, data.len()).as_deref(),
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
            for lgwin in 10..=24usize {
                assert_matches_c(name, data, Case::new(quality, lgwin));
            }
        }
    }
}

#[test]
fn the_size_hint_boundary_selects_the_large_match_finder() {
    // Quality four switches from H4 to H54, and quality five from H5 to H6
    // when the window is wide enough, at exactly one mebibyte.
    let data = text();
    for quality in GREEDY_QUALITIES {
        for lgwin in [16usize, 17, 18, 19, 22] {
            for hint in [0usize, (1 << 20) - 1, 1 << 20, (1 << 20) + 1, 8 << 20] {
                let mut case = Case::new(quality, lgwin);
                case.size_hint = Some(hint);
                assert_matches_c("size-hint", &data, case);
            }
        }
    }
}

#[test]
fn the_small_window_match_finder_is_reached_at_quality_five() {
    // A window of sixteen bits or fewer routes quality five to the forgetful
    // chain matcher instead of a bucketed one.
    let data = text();
    for lgwin in [10usize, 14, 16, 17] {
        assert_matches_c("small-window", &data, Case::new(QualityLevel::Q5, lgwin));
    }
}

#[test]
fn every_mode_matches_the_c_encoder() {
    let data = text();
    for quality in GREEDY_QUALITIES {
        for mode in [
            CompressMode::Generic,
            CompressMode::Text,
            CompressMode::Font,
        ] {
            let mut case = Case::new(quality, 22);
            case.mode = mode;
            assert_matches_c("mode", &data, case);
        }
    }
}

#[test]
fn font_mode_uses_the_reference_distance_parameters() {
    // Font mode asks for one postfix bit and twelve direct codes, but only
    // from quality four upwards.
    let data = text();
    let compressor = Brotli::default().compressor();
    for quality in GREEDY_QUALITIES {
        let mut generic = Case::new(quality, 22);
        let mut font = Case::new(quality, 22);
        font.mode = CompressMode::Font;
        assert_matches_c("font", &data, font);

        generic.mode = CompressMode::Generic;
        let with_font = compressor.compress(font.rust(), &data).expect("font");
        let with_generic = compressor.compress(generic.rust(), &data).expect("generic");
        if quality == QualityLevel::Q3 {
            assert_eq!(
                with_font, with_generic,
                "quality three must ignore the font distance parameters"
            );
        } else {
            assert_ne!(
                with_font, with_generic,
                "quality {quality:?} ignored the font distance parameters"
            );
        }
    }
}

#[test]
fn every_valid_distance_layout_matches_the_c_encoder() {
    let data = text();
    for quality in GREEDY_QUALITIES {
        for postfix in 0u32..=3 {
            for groups in [0u32, 1, 4, 15] {
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
    // The reference silently falls back to zero for a layout it cannot
    // express; this crate refuses to build one at all, so that path is
    // unreachable from the public API.
    assert!(DistanceCodes::try_from((4u32, 0u32)).is_err());
    assert!(DistanceCodes::try_from((0u32, 121u32)).is_err());
    assert!(DistanceCodes::try_from((2u32, 6u32)).is_err());
    assert!(DistanceCodes::try_from((3u32, 120u32)).is_ok());
}

#[test]
fn every_block_size_matches_the_c_encoder() {
    let data = text();
    for quality in GREEDY_QUALITIES {
        for lgblock in [16usize, 17, 18, 20, 24] {
            let mut case = Case::new(quality, 22);
            case.lgblock = Some(lgblock);
            assert_matches_c("lgblock", &data, case);
        }
    }
}

#[test]
fn quality_three_ignores_the_requested_block_size() {
    // `ComputeLgBlock` pins qualities below four to fourteen bits.
    let compressor = Brotli::default().compressor();
    let data = text();
    let default = Case::new(QualityLevel::Q3, 22);
    let baseline = compressor.compress(default.rust(), &data).expect("default");
    for lgblock in [16usize, 20, 24] {
        let mut case = default;
        case.lgblock = Some(lgblock);
        let actual = compressor.compress(case.rust(), &data).expect("explicit");
        assert_eq!(actual, baseline, "quality three honoured lgblock {lgblock}");
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

/// Text whose previous byte predicts the next one well enough to earn a
/// context model: alternating one-byte and two-byte UTF-8 sequences.
fn context_friendly() -> Vec<u8> {
    let mut data = Vec::new();
    while data.len() < (1 << 18) {
        data.extend_from_slice("añbñcñdñ eñfñgñhñ".as_bytes());
    }
    data
}

#[test]
fn only_quality_five_reacts_to_literal_context_modeling() {
    let compressor = Brotli::default().compressor();
    let data = context_friendly();
    for quality in GREEDY_QUALITIES {
        let on = Case::new(quality, 22);
        let mut off = on;
        off.literal_context_modeling = false;
        assert_matches_c("context-on", &data, on);
        assert_matches_c("context-off", &data, off);
        let with = compressor.compress(on.rust(), &data).expect("on");
        let without = compressor.compress(off.rust(), &data).expect("off");
        if quality == QualityLevel::Q5 {
            assert_ne!(with, without, "quality five ignored the setting");
        } else {
            assert_eq!(with, without, "quality {quality:?} honoured the setting");
        }
    }
}

#[test]
fn the_complex_context_map_is_reachable_at_quality_five() {
    // The thirteen-context map needs a size hint of at least one mebibyte and
    // data whose contexts predict the next byte well.
    let compressor = Brotli::default().compressor();
    let mut data = Vec::new();
    while data.len() < (2 << 20) {
        data.extend_from_slice(&text());
    }
    data.truncate(2 << 20);

    let mut small = Case::new(QualityLevel::Q5, 22);
    small.size_hint = Some((1 << 20) - 1);
    let mut large = Case::new(QualityLevel::Q5, 22);
    large.size_hint = Some(1 << 20);
    assert_matches_c("complex-map-below", &data, small);
    assert_matches_c("complex-map-at", &data, large);
    assert_ne!(
        compressor.compress(small.rust(), &data).expect("below"),
        compressor.compress(large.rust(), &data).expect("at"),
        "the size hint did not change the context map"
    );
}

#[test]
fn the_delayed_symbol_bound_is_reached_at_quality_three() {
    // Below the block-splitting quality the encoder flushes once it has
    // buffered `0x2FFF` symbols, so a stream that crosses that bound has to
    // agree with the reference on where the meta-blocks end.
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
        assert_matches_c(
            "delayed-literals",
            &literals,
            Case::new(QualityLevel::Q3, 22),
        );

        // Short repeats reach it through commands instead.
        let commands: Vec<u8> = (0..length).map(|index| (index % 3) as u8).collect();
        assert_matches_c(
            "delayed-commands",
            &commands,
            Case::new(QualityLevel::Q3, 22),
        );
    }
}

#[test]
fn input_block_boundaries_match_the_c_encoder() {
    // Quality three works in sixteen kibibyte blocks and the others in
    // sixty-four kibibyte blocks; every boundary is a chance to disagree.
    let source = text();
    for quality in GREEDY_QUALITIES {
        let block = if quality == QualityLevel::Q3 {
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
        for lgwin in [10usize, 16, 22] {
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
        for lgwin in [10usize, 11, 12] {
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
            // The one-shot empty-input shortcut is compared by the
            // `differential_c` suite instead; the streaming C harness does not
            // apply it.
            if length == 0 {
                continue;
            }
            assert_matches_c("short", &source[..length], Case::new(quality, 22));
        }
    }
}
