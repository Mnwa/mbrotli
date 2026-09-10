use super::{Result, WINDOW, c_compress, corpora};
use criterion::{BenchmarkId, Criterion, Throughput};
use google_brotli_ffi as ffi;
use mbrotli::{DecoderConfig, Decompressor};
use std::hint::black_box;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

const DECODERS: [&str; 4] = ["c-brotli", "mbrotli", "rust-brotli", "burli"];

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("C decompression failed")]
    CDecode,
    #[error("unknown decoder: {0}")]
    UnknownDecoder(String),
    #[error("{decoder} decoded incorrect bytes for {corpus} at source quality {quality}")]
    Mismatch {
        decoder: &'static str,
        corpus: &'static str,
        quality: u8,
    },
}

fn decompress(decoder: &str, compressed: &[u8], output_bytes: usize) -> Result<Vec<u8>> {
    match decoder {
        "c-brotli" => {
            // C's native one-shot API requires caller-provided output capacity.
            // Include this allocation and initialization in the cold measurement.
            let mut output = vec![0; output_bytes.max(1)];
            let mut written = output.len();
            // SAFETY: compressed and output are live, disjoint slices; written
            // is the writable capacity, which C respects and updates on return.
            let status = unsafe {
                ffi::BrotliDecoderDecompress(
                    compressed.len(),
                    compressed.as_ptr(),
                    &mut written,
                    output.as_mut_ptr(),
                )
            };
            if status != ffi::BROTLI_DECODER_RESULT_SUCCESS {
                return Err(Error::CDecode.into());
            }
            output.truncate(written);
            Ok(output)
        }
        "mbrotli" => Ok(Decompressor::new(DecoderConfig::default())?.decompress(compressed)?),
        "rust-brotli" => {
            let mut output = Vec::new();
            brotli::BrotliDecompress(&mut &compressed[..], &mut output)?;
            Ok(output)
        }
        "burli" => Ok(burli::decompress(compressed)?),
        _ => Err(Error::UnknownDecoder(decoder.to_owned()).into()),
    }
}

fn validate(
    decoder: &'static str,
    corpus: &'static str,
    quality: u8,
    input: &[u8],
    compressed: &[u8],
) -> Result<()> {
    if decompress(decoder, compressed, input.len())? != input {
        return Err(Error::Mismatch {
            decoder,
            corpus,
            quality,
        }
        .into());
    }
    Ok(())
}

pub(crate) fn run() -> Result<()> {
    let output = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/criterion-decoders");
    std::fs::create_dir_all(&output)?;
    let mut criterion = Criterion::default()
        .output_directory(&output)
        .sample_size(20)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3))
        .configure_from_args();
    let corpora = corpora();
    let mut streams = Vec::new();
    let mut sizes = Vec::new();
    writeln!(
        sizes,
        "corpus,input_bytes,quality,lgwin,implementation,compressed_bytes"
    )?;
    // Retain exactly the validated streams, including in filtered runs. Quality
    // describes their source encoder; Burli's encoder q5 limit does not apply.
    for (name, input) in &corpora {
        for quality in 0..=11 {
            let compressed = c_compress(input, quality)?;
            for decoder in DECODERS {
                validate(decoder, name, quality, input, &compressed)?;
                writeln!(
                    sizes,
                    "{name},{},{quality},{WINDOW},{decoder},{}",
                    input.len(),
                    compressed.len()
                )?;
            }
            streams.push((name, input.len(), quality, compressed));
        }
    }
    std::fs::write(output.join("sizes.csv"), sizes)?;
    for (name, output_bytes, quality, compressed) in &streams {
        let mut group = criterion.benchmark_group(format!("decoders/cold/{name}/q{quality}"));
        // Throughput is restored bytes per second, not compressed input bytes.
        group.throughput(Throughput::Bytes(*output_bytes as u64));
        for decoder in DECODERS {
            let mut failure = None;
            group.bench_function(BenchmarkId::from_parameter(decoder), |b| {
                b.iter(
                    || match decompress(decoder, black_box(compressed), *output_bytes) {
                        Ok(output) => {
                            black_box(output);
                        }
                        Err(error) => {
                            failure = Some(error);
                        }
                    },
                );
            });
            if let Some(error) = failure {
                return Err(error);
            }
        }
        group.finish();
    }
    criterion.final_summary();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_decoder_restores_empty_tiny_and_buffer_boundaries_at_all_source_qualities()
    -> Result<()> {
        for quality in 0..=11 {
            for length in [0, 1, 15, 16, 17, 4095, 4096, 4097] {
                let input: Vec<_> = (0..length).map(|i| (i % 251) as u8).collect();
                let compressed = c_compress(&input, quality)?;
                for decoder in DECODERS {
                    validate(decoder, "boundary", quality, &input, &compressed)?;
                }
            }
        }
        Ok(())
    }

    #[test]
    fn adapters_reject_invalid_streams_and_validation_rejects_wrong_content() -> Result<()> {
        for decoder in DECODERS {
            assert!(decompress(decoder, &[], 1).is_err());
            assert!(validate(decoder, "wrong-content", 5, b"y", &c_compress(b"x", 5)?).is_err());
        }
        assert!(decompress("c-brotli", &c_compress(b"too long", 5)?, 1).is_err());
        assert!(decompress("unknown", &[], 0).is_err());
        Ok(())
    }
}
