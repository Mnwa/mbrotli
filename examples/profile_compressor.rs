//! Repeatable CPU/allocation profile, enabled only with the hotpath features.

use mbrotli::{Compressor, EncoderConfig, Quality};
use std::hint::black_box;

#[cfg_attr(feature = "hotpath", hotpath::main)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let text = b"the quick brown fox jumps over the lazy dog 0123456789\n".repeat(1300);
    for number in 0..=11 {
        let mut compressor =
            Compressor::new(EncoderConfig::default().with_quality(Quality::try_from(number)?))?;
        let mut output = Vec::with_capacity(Compressor::max_compressed_size(text.len())?);
        for _ in 0..10 {
            output.clear();
            compressor.compress_into(black_box(&text), &mut output)?;
            black_box(&output);
        }
    }
    for _ in 0..100 {
        let mut compressor = Compressor::new(EncoderConfig::default().with_quality(Quality::Q9))?;
        black_box(compressor.compress(black_box(b"0. Brotli is a ge"))?);
    }
    // An optional corpus path keeps the example usable from a published crate,
    // whose package deliberately does not include the vendored test fixtures.
    let source = if let Some(path) = std::env::args_os().nth(1) {
        std::fs::read(path)?
    } else {
        text
    };
    for number in [5, 7, 9] {
        let mut compressor =
            Compressor::new(EncoderConfig::default().with_quality(Quality::try_from(number)?))?;
        for _ in 0..10 {
            black_box(compressor.compress(black_box(&source))?);
        }
    }
    Ok(())
}
