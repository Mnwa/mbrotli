//! Repeatable native decoder CPU/allocation profile; accepts an optional .br path.
use mbrotli::{Compressor, DecoderConfig, Decompressor, EncoderConfig};
use std::hint::black_box;

#[cfg_attr(all(feature = "hotpath", not(feature = "no_std")), hotpath::main)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = if let Some(path) = std::env::args_os().nth(1) {
        std::fs::read(path)?
    } else {
        Compressor::new(EncoderConfig::default())?
            .compress(&b"incremental decoding with shared dictionary transforms\n".repeat(20000))?
    };
    let mut decoder = Decompressor::new(DecoderConfig::default())?;
    let mut output = decoder.decompress(&input)?;
    for _ in 0..100 {
        decoder.decompress_to_slice(black_box(&input), black_box(&mut output))?;
    }
    eprintln!(
        "compressed={} decoded={} retained={}",
        input.len(),
        output.len(),
        decoder.retained_bytes()
    );
    Ok(())
}
