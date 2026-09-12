//! Reproducible framing CPU/allocation workload; no files are extracted.
#[cfg(not(feature = "no_std"))]
use mbrotli::framing::*;

#[cfg(not(feature = "no_std"))]
#[cfg_attr(feature = "hotpath", hotpath::main)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::{hint::black_box, io::Write};
    let mut framed_encoder = FramedCompressor::new(Default::default())?;
    let mut writer = framed_encoder.framed_writer(Vec::new(), Default::default())?;
    for _ in 0..256 {
        writer.metadata(
            MetadataKind::Resource,
            &[
                MetadataField {
                    code: *b"id",
                    value: b"resource",
                },
                MetadataField {
                    code: *b"AA",
                    value: b"custom metadata",
                },
            ],
        )?;
        let mut resource = writer.uncompressed_resource(Default::default())?;
        resource.write_all(&[b'x'; 256])?;
        resource.try_finish()?;
    }
    let bytes = writer.finish().map_err(|e| e.error)?;
    let mut decoder = FramedDecompressor::new(
        FramedDecodeConfig::default().with_internal_dictionaries(InternalDictionaryPolicy::Reject),
    )?;
    for _ in 0..128 {
        let output = decoder.decompress(black_box(&bytes))?;
        assert_eq!(output.resources.len(), 256);
        assert!(output.resources.iter().all(|r| r.data == [b'x'; 256]));
        black_box(output);
    }
    eprintln!(
        "resources=256 payload=65536 wire={} retained={}",
        bytes.len(),
        decoder.retained_bytes()
    );
    Ok(())
}
#[cfg(feature = "no_std")]
fn main() {}
