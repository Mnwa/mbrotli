//! Compresses a payload with every entry point the public API offers.
//!
//! This is the code the README shows, kept here so it is compiled and run by
//! the ordinary check commands rather than drifting.
//!
//! ```sh
//! cargo run --example compress
//! ```

use mbrotli::Brotli;
use mbrotli::compressor::{BrotliCompressParams, BrotliQualityLevel, BrotliWindowBits};
use std::io::{Read, Write};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let compressor = Brotli::default().compressor();
    let params = BrotliCompressParams::new(BrotliQualityLevel::Q1, BrotliWindowBits::DEFAULT);
    let payload = "brotli ".repeat(1000);

    // One shot, into a freshly allocated buffer.
    let compressed = compressor.compress(params, payload.as_bytes())?;
    println!("one-shot:  {} -> {} bytes", payload.len(), compressed.len());

    // One shot, into a caller-owned buffer sized by the compressed-size bound.
    let bound = compressor.calculate_bound(&params, payload.len())?;
    let mut buffer = vec![0u8; bound];
    let written = compressor.compress_to_slice(params, payload.as_bytes(), &mut buffer)?;
    assert_eq!(&buffer[..written], compressed.as_slice());
    println!("to slice:  {written} bytes into a {bound} byte buffer");

    // Streaming into a writer. The stream is terminated by `finish`.
    let mut sink = compressor.compress_writer(params, Vec::new());
    for chunk in payload.as_bytes().chunks(512) {
        sink.write_all(chunk)?;
    }
    let streamed = sink.finish()?;
    assert_eq!(streamed, compressed);
    println!("writer:    {} bytes", streamed.len());

    // Streaming out of a reader.
    let mut source = compressor.compress_reader(params, payload.as_bytes());
    let mut pulled = Vec::new();
    source.read_to_end(&mut pulled)?;
    assert_eq!(pulled, compressed);
    println!("reader:    {} bytes", pulled.len());

    Ok(())
}
