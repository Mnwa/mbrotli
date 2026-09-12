//! Reusable framed encoder workloads. C is an oracle only for raw Brotli.
#[cfg(not(feature = "no_std"))]
mod implementation {
    use criterion::{Criterion, Throughput, criterion_group, criterion_main};
    use mbrotli::dictionary::{DictionaryBuilder, PreparedDictionary};
    use mbrotli::framing::*;
    use mbrotli::{Compressor, EncoderConfig, InputSize, Quality};
    use std::{
        hint::black_box,
        io::{self, Write},
    };

    #[derive(Debug)]
    struct Sink<'a> {
        bytes: &'a mut Vec<u8>,
        width: usize,
    }
    impl Write for Sink<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let n = bytes.len().min(self.width);
            self.bytes.extend_from_slice(&bytes[..n]);
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    fn encode(
        owner: &mut FramedCompressor,
        data: &[u8],
        name: &str,
        dictionary: &PreparedDictionary,
        output: &mut Vec<u8>,
    ) {
        output.clear();
        let mut writer = owner
            .framed_writer(
                Sink {
                    bytes: output,
                    width: if name == "tiny-io" { 1 } else { usize::MAX },
                },
                Default::default(),
            )
            .unwrap();
        let resources = if name == "many-small" || name == "metadata" {
            256
        } else {
            1
        };
        for _ in 0..resources {
            if name == "metadata" {
                writer
                    .metadata_with_options(
                        MetadataKind::Resource,
                        &[
                            MetadataField {
                                code: *b"id",
                                value: b"resource",
                            },
                            MetadataField {
                                code: *b"AB",
                                value: b"application metadata",
                            },
                        ],
                        MetadataOptions {
                            encoding: MetadataEncoding::Brotli,
                            repeated_encoding: MetadataEncoding::Brotli,
                        },
                    )
                    .unwrap();
            }
            let mut r = if name == "uncompressed" {
                writer.uncompressed_resource(Default::default()).unwrap()
            } else if name == "dictionary" {
                writer
                    .resource_with_dictionary(
                        Default::default(),
                        InputSize::Exact(data.len() as u64).into(),
                        dictionary,
                        &[DictionaryReference::PrefixId(DictionaryId([7; 32]))],
                    )
                    .unwrap()
            } else {
                writer
                    .resource(
                        Default::default(),
                        InputSize::Exact(data.len() as u64).into(),
                    )
                    .unwrap()
            };
            r.write_all(data).unwrap();
            r.try_finish().unwrap();
        }
        writer.try_finish().unwrap();
    }
    fn benchmarks(c: &mut Criterion) {
        let dictionary = DictionaryBuilder::new()
            .add_prefix(&b"reusable framed encoder resource payload dictionary"[..])
            .build()
            .unwrap();
        let raw_config = EncoderConfig::default().with_quality(Quality::Q5);
        let config = FramedEncodeConfig::default()
            .with_encoder_config(raw_config)
            .with_framing_config(FramingConfig {
                repeat_metadata: true,
                ..Default::default()
            });
        let mut owner = FramedCompressor::new(config).unwrap();
        let mut group = c.benchmark_group("framed-compress");
        for name in [
            "large",
            "many-small",
            "metadata",
            "uncompressed",
            "dictionary",
            "tiny-io",
            "binary",
        ] {
            let mut data = b"reusable framed encoder resource payload dictionary ".repeat(
                if name == "large" || name == "uncompressed" {
                    20_000
                } else if name == "many-small" || name == "metadata" {
                    5
                } else {
                    128
                },
            );
            if name == "binary" {
                let mut state = 0xa076_1d64_78bd_642fu64;
                data = (0..262_144)
                    .map(|_| {
                        state ^= state << 13;
                        state ^= state >> 7;
                        state ^= state << 17;
                        state as u8
                    })
                    .collect();
            }
            let total = data.len()
                * if name == "many-small" || name == "metadata" {
                    256
                } else {
                    1
                };
            let mut output = Vec::with_capacity(total + 65536);
            encode(&mut owner, &data, name, &dictionary, &mut output);
            if let Ok(directory) = std::env::var("MBROTLI_FRAMED_BASELINE_WRITE") {
                std::fs::create_dir_all(&directory).unwrap();
                std::fs::write(format!("{directory}/{name}.bin"), &output).unwrap();
            }
            if let Ok(directory) = std::env::var("MBROTLI_FRAMED_BASELINE_READ") {
                assert_eq!(
                    output,
                    std::fs::read(format!("{directory}/{name}.bin")).unwrap()
                );
            }
            eprintln!(
                "{name}: payload={total} wire={} retained={}",
                output.len(),
                owner.retained_bytes()
            );
            group.throughput(Throughput::Bytes(total as u64));
            group.bench_function(name, |b| {
                b.iter(|| {
                    encode(
                        &mut owner,
                        black_box(&data),
                        name,
                        &dictionary,
                        black_box(&mut output),
                    )
                })
            });
        }
        group.finish();
        let data = b"raw encoder regression reference bytes ".repeat(16384);
        let mut raw = Compressor::new(raw_config).unwrap();
        let mut output = vec![0; Compressor::max_compressed_size(data.len()).unwrap()];
        let written = raw.compress_to_slice(&data, &mut output).unwrap();
        let mut reference = vec![0; output.len()];
        let c_encode = |output: &mut [u8]| {
            let mut size = output.len();
            // SAFETY: both slices remain live with the supplied lengths; the one-shot
            // C encoder does not retain their pointers, and size is writable.
            let ok = unsafe {
                google_brotli_ffi::BrotliEncoderCompress(
                    5,
                    22,
                    google_brotli_ffi::BROTLI_DEFAULT_MODE,
                    data.len(),
                    data.as_ptr(),
                    &raw mut size,
                    output.as_mut_ptr(),
                )
            };
            assert_eq!(ok, google_brotli_ffi::BROTLI_TRUE);
            size
        };
        let size = c_encode(&mut reference);
        assert_eq!(&output[..written], &reference[..size]);
        let mut group = c.benchmark_group("framed-raw-encode-regression");
        group.throughput(Throughput::Bytes(data.len() as u64));
        eprintln!("raw: payload={} wire={written}", data.len());
        group.bench_function("rust", |b| {
            b.iter(|| {
                raw.compress_to_slice(black_box(&data), black_box(&mut output))
                    .unwrap()
            })
        });
        let mut raw_session = |output: &mut [u8]| {
            let mut session = raw
                .start(InputSize::Exact(data.len() as u64).into())
                .unwrap();
            let mut consumed = 0;
            let mut produced = 0;
            loop {
                let p = session
                    .process(
                        black_box(&data[consumed..]),
                        black_box(&mut output[produced..]),
                        mbrotli::Operation::Finish,
                    )
                    .unwrap();
                consumed += p.consumed;
                produced += p.produced;
                if p.status == mbrotli::EncoderStatus::Finished {
                    return produced;
                }
            }
        };
        let native_written = raw_session(&mut output);
        assert_eq!(&output[..native_written], &reference[..size]);
        group.bench_function("rust-session", |b| {
            b.iter(|| raw_session(black_box(&mut output)))
        });
        group.bench_function("c", |b| b.iter(|| c_encode(black_box(&mut reference))));
        group.finish();
    }
    criterion_group!(benches, benchmarks);
    criterion_main!(benches);
    pub(super) fn run() {
        main();
    }
}
#[cfg(not(feature = "no_std"))]
fn main() {
    implementation::run();
}
#[cfg(feature = "no_std")]
fn main() {}
