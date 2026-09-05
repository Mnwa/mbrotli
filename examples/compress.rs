//! Compresses a payload with every entry point the public API offers.
//!
//! This is the code the README shows, kept here so it is compiled and run by
//! the ordinary check commands rather than drifting.
//!
//! ```sh
//! cargo run --example compress
//! ```

use mbrotli::io::FinishError;
use mbrotli::{Compressor, EncoderConfig, InputSize, Quality, StreamConfig};
use std::io::{Read, Write};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = EncoderConfig::default().with_quality(Quality::Q5);
    let mut encoder = Compressor::new(config)?;
    let payload = "brotli ".repeat(1000);

    // One shot, into a freshly allocated buffer.
    let compressed = encoder.compress(payload.as_bytes())?;
    println!("one-shot:  {} -> {} bytes", payload.len(), compressed.len());

    // One shot, appending to a buffer the caller owns and reuses. This is the
    // entry point to reach for when there is more than one thing to compress:
    // both the encoder's workspace and the destination's capacity are reused.
    let mut output = Vec::new();
    for _ in 0..3 {
        output.clear();
        let range = encoder.compress_into(payload.as_bytes(), &mut output)?;
        assert_eq!(&output[range], compressed.as_slice());
    }
    println!("appended:  {} bytes, into a reused buffer", output.len());

    // One shot, into a caller-owned slice sized by the compressed-size bound.
    let bound = Compressor::max_compressed_size(payload.len())?;
    let mut buffer = vec![0u8; bound];
    let written = encoder.compress_to_slice(payload.as_bytes(), &mut buffer)?;
    assert_eq!(&buffer[..written], compressed.as_slice());
    println!("to slice:  {written} bytes into a {bound} byte buffer");

    // Streaming into a writer. Declaring the size is what makes a streamed
    // stream reach the same bytes as the same input compressed in one shot.
    let stream = StreamConfig::from(InputSize::Exact(payload.len() as u64));
    let mut sink = encoder.writer(Vec::new(), stream)?;
    for chunk in payload.as_bytes().chunks(512) {
        sink.write_all(chunk)?;
    }
    let streamed = sink.finish().map_err(FinishError::into_error)?;
    assert_eq!(streamed, compressed);
    println!("writer:    {} bytes", streamed.len());

    // Streaming out of a reader. The adapter borrows the compressor for as
    // long as it lives, so it is scoped to give the compressor back.
    let pulled = {
        let mut source = encoder.reader(payload.as_bytes(), stream)?;
        let mut pulled = Vec::new();
        source.read_to_end(&mut pulled)?;
        pulled
    };
    assert_eq!(pulled, compressed);
    println!("reader:    {} bytes", pulled.len());

    println!(
        "retained:  {} bytes of reusable workspace",
        encoder.retained_bytes()
    );
    Ok(())
}
