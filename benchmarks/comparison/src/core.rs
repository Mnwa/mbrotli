use criterion::{BenchmarkId, Criterion, Throughput};
use google_brotli_ffi as ffi;
use mbrotli::{Compressor, EncoderConfig, Quality, Window};
use std::hint::black_box;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const WINDOW: u8 = 22;
const ENCODERS: [&str; 5] = ["c-brotli", "mbrotli", "rust-brotli", "simd-brotli", "burli"];

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("C compression failed")]
    CEncode,
    #[error("compressed output did not round-trip through the C decoder")]
    RoundTrip,
    #[error("unknown encoder: {0}")]
    UnknownEncoder(String),
}

fn compress(encoder: &str, input: &[u8], quality: u8) -> Result<Vec<u8>> {
    match encoder {
        "c-brotli" => c_compress(input, quality),
        "mbrotli" => Ok(Compressor::new(
            EncoderConfig::default()
                .with_quality(Quality::try_from(quality)?)
                .with_window(Window::standard(WINDOW)?),
        )?
        .compress(input)?),
        "rust-brotli" => {
            let params = brotli::enc::BrotliEncoderParams {
                quality: i32::from(quality),
                lgwin: i32::from(WINDOW),
                size_hint: input.len(),
                ..Default::default()
            };
            let mut output = Vec::new();
            brotli::BrotliCompress(&mut &input[..], &mut output, &params)?;
            Ok(output)
        }
        "simd-brotli" => {
            let params = simd_brotli::enc::BrotliEncoderParams {
                quality: i32::from(quality),
                lgwin: i32::from(WINDOW),
                size_hint: input.len(),
                ..Default::default()
            };
            let mut output = Vec::new();
            simd_brotli::BrotliCompress(&mut &input[..], &mut output, &params)?;
            Ok(output)
        }
        "burli" => Ok(burli::compress_with_options(
            input,
            &burli::Options::default()
                .with_quality(quality)?
                .with_window_bits(WINDOW)?
                .with_size_hint(Some(input.len())),
        )?),
        _ => Err(Error::UnknownEncoder(encoder.to_owned()).into()),
    }
}

fn c_compress(input: &[u8], quality: u8) -> Result<Vec<u8>> {
    // SAFETY: this pure size-bound query accepts every usize input length.
    let bound = unsafe { ffi::BrotliEncoderMaxCompressedSize(input.len()) };
    let mut output = vec![0; bound];
    let mut written = output.len();
    // SAFETY: input and output are disjoint live slices; written describes the
    // writable allocation. C writes at most that capacity and updates written.
    let status = unsafe {
        ffi::BrotliEncoderCompress(
            i32::from(quality),
            i32::from(WINDOW),
            ffi::BROTLI_MODE_GENERIC,
            input.len(),
            input.as_ptr(),
            &mut written,
            output.as_mut_ptr(),
        )
    };
    if status != ffi::BROTLI_TRUE {
        return Err(Error::CEncode.into());
    }
    output.truncate(written);
    Ok(output)
}

fn validate(input: &[u8], compressed: &[u8]) -> Result<()> {
    // Allocate one byte for empty input so the C decoder always has real space.
    let mut decoded = vec![0; input.len().max(1)];
    let mut written = decoded.len();
    // SAFETY: both slices are live and disjoint; written is decoded's writable
    // capacity. The decoder bounds its writes and reports the initialized size.
    let status = unsafe {
        ffi::BrotliDecoderDecompress(
            compressed.len(),
            compressed.as_ptr(),
            &mut written,
            decoded.as_mut_ptr(),
        )
    };
    if status != ffi::BROTLI_DECODER_RESULT_SUCCESS || &decoded[..written] != input {
        return Err(Error::RoundTrip.into());
    }
    Ok(())
}

fn corpora() -> Vec<(&'static str, Vec<u8>)> {
    let text = include_bytes!("../../../brotli-ffi/vendor/brotli/tests/testdata/alice29.txt");
    let mut state = 0x243f_6a88_85a3_08d3_u64;
    let random: Vec<u8> = (0..1_048_576)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect();
    vec![
        ("empty", Vec::new()),
        (
            "tiny-text",
            b"The quick brown fox jumps over the lazy dog.".to_vec(),
        ),
        ("alice29", text.to_vec()),
        (
            "text-1m",
            text.iter().copied().cycle().take(1_048_576).collect(),
        ),
        (
            "binary-64k",
            (0..65_536_u32).map(|i| ((i / 16) ^ i) as u8).collect(),
        ),
        ("random-64k", random[..65_536].to_vec()),
        ("random-1m", random),
        ("repeated-1m", vec![b'a'; 1_048_576]),
    ]
}

pub(super) fn run() -> Result<()> {
    let output = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/criterion");
    std::fs::create_dir_all(&output)?;
    let mut criterion = Criterion::default()
        .output_directory(&output)
        .sample_size(20)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3))
        .configure_from_args();
    let corpora = corpora();
    // Preflight the entire matrix before timing any implementation, including
    // filtered runs. A failed oracle never yields a partial successful sweep.
    let mut sizes = Vec::new();
    writeln!(
        sizes,
        "corpus,input_bytes,quality,lgwin,implementation,compressed_bytes"
    )?;
    for (name, input) in &corpora {
        for quality in 0..=11 {
            for encoder in ENCODERS {
                if encoder == "burli" && quality > 5 {
                    continue;
                }
                let compressed = compress(encoder, input, quality)?;
                validate(input, &compressed)?;
                writeln!(
                    sizes,
                    "{name},{},{quality},{WINDOW},{encoder},{}",
                    input.len(),
                    compressed.len()
                )?;
            }
        }
    }
    std::fs::write(output.join("sizes.csv"), sizes)?;
    for (name, input) in &corpora {
        for quality in 0..=11 {
            let mut group =
                criterion.benchmark_group(format!("implementations/cold/{name}/q{quality}"));
            group.throughput(Throughput::Bytes(input.len() as u64));
            for encoder in ENCODERS {
                if encoder == "burli" && quality > 5 {
                    continue;
                }
                let mut failure = None;
                group.bench_function(BenchmarkId::from_parameter(encoder), |b| {
                    b.iter(|| match compress(encoder, black_box(input), quality) {
                        Ok(output) => {
                            black_box(output);
                        }
                        Err(error) => {
                            failure = Some(error);
                        }
                    });
                });
                if let Some(error) = failure {
                    return Err(error);
                }
            }
            group.finish();
        }
    }
    criterion.final_summary();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_encoders_round_trip_empty_tiny_and_buffer_boundaries() -> Result<()> {
        for encoder in ENCODERS {
            for quality in [0, 1, 5] {
                for length in [0, 1, 15, 16, 17, 4095, 4096, 4097] {
                    let input: Vec<u8> = (0..length).map(|i| (i % 251) as u8).collect();
                    validate(&input, &compress(encoder, &input, quality)?)?;
                }
            }
        }
        Ok(())
    }

    #[test]
    fn oracle_rejects_corruption_and_wrong_expected_content() -> Result<()> {
        assert!(validate(b"x", &[]).is_err());
        assert!(validate(b"y", &c_compress(b"x", 5)?).is_err());
        assert!(validate(b"", &c_compress(b"x", 5)?).is_err());
        assert!(compress("unknown", b"", 0).is_err());
        assert!(compress("burli", b"", 6).is_err());
        Ok(())
    }

    #[test]
    fn corpus_is_deterministic_and_has_all_size_classes() {
        let first = corpora();
        assert_eq!(first, corpora());
        assert_eq!(first.len(), 8);
        assert!(first.iter().any(|(_, input)| input.is_empty()));
        assert!(first.iter().any(|(_, input)| input.len() == 1_048_576));
    }
}
