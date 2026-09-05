//! Shared dictionary stream creation versus the pinned C implementation.
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use google_brotli_ffi as ffi;
use mbrotli::dictionary::{
    DictionaryBuilder, SerializedDictionary, TransformList, TransformOperation, WordList,
};
use mbrotli::framing::{DictionaryId, DictionaryReference, FramingConfig};
use mbrotli::{Compressor, EncoderConfig, Quality};
use std::hint::black_box;
use std::io::Write;
use std::marker::PhantomData;

struct Reference<'a>(
    *mut ffi::BrotliEncoderPreparedDictionary,
    PhantomData<&'a [u8]>,
);
impl Drop for Reference<'_> {
    fn drop(&mut self) {
        // SAFETY: exclusively owns the instance returned by PrepareDictionary.
        unsafe {
            ffi::BrotliEncoderDestroyPreparedDictionary(self.0);
        }
    }
}
impl<'a> Reference<'a> {
    fn new(bytes: &'a [u8], quality: Quality) -> Self {
        // SAFETY: the caller retains `bytes` for this reference's lifetime.
        let pointer = unsafe {
            ffi::BrotliEncoderPrepareDictionary(
                ffi::BROTLI_SHARED_DICTIONARY_SERIALIZED,
                bytes.len(),
                bytes.as_ptr(),
                i32::from(quality.get()),
                None,
                None,
                std::ptr::null_mut(),
            )
        };
        assert!(!pointer.is_null());
        Self(pointer, PhantomData)
    }
    fn compress(&self, input: &[u8], quality: Quality, output: &mut [u8]) -> usize {
        // SAFETY: all buffers have the declared lengths, the dictionary remains
        // live, and the temporary encoder is destroyed before returning.
        unsafe {
            let state = ffi::BrotliEncoderCreateInstance(None, None, std::ptr::null_mut());
            assert!(!state.is_null());
            assert_eq!(
                ffi::BrotliEncoderSetParameter(
                    state,
                    ffi::BROTLI_PARAM_QUALITY,
                    u32::from(quality.get())
                ),
                ffi::BROTLI_TRUE
            );
            assert_eq!(
                ffi::BrotliEncoderAttachPreparedDictionary(state, self.0),
                ffi::BROTLI_TRUE
            );
            let mut available_in = input.len();
            let mut next_in = input.as_ptr();
            let mut available_out = output.len();
            let mut next_out = output.as_mut_ptr();
            let mut total = 0;
            assert_eq!(
                ffi::BrotliEncoderCompressStream(
                    state,
                    ffi::BROTLI_OPERATION_FINISH,
                    &raw mut available_in,
                    &raw mut next_in,
                    &raw mut available_out,
                    &raw mut next_out,
                    &raw mut total
                ),
                ffi::BROTLI_TRUE
            );
            assert_eq!(ffi::BrotliEncoderIsFinished(state), ffi::BROTLI_TRUE);
            ffi::BrotliEncoderDestroyInstance(state);
            total
        }
    }
}

fn number(mut value: usize, output: &mut Vec<u8>) {
    while value >= 128 {
        output.push(value as u8 | 128);
        value >>= 7;
    }
    output.push(value as u8);
}

// C has no container API. This literal RFC single-resource envelope gives its
// raw encoder the same wire overhead, and is checked against FramedWriter.
fn frame_reference(encoded: &[u8], input_size: usize, output: &mut Vec<u8>) {
    let mut header = vec![2, 3];
    number(input_size, &mut header);
    header.extend_from_slice(&[1, 6, 3]);
    header.extend_from_slice(&[0; 32]);
    header.push(0);
    output.clear();
    output.extend_from_slice(&[0x91, 10, 66, 82, 0]);
    number(header.len() + encoded.len(), output);
    output.extend_from_slice(&header);
    output.extend_from_slice(encoded);
}

fn dictionaries(c: &mut Criterion) {
    let description = SerializedDictionary::builder()
        .add_word_list(
            WordList::builder()
                .add_word(b"unusualword")
                .add_word(b"otherword")
                .build()
                .expect("words"),
        )
        .add_transform_list(
            TransformList::builder()
                .add_transform(b"", TransformOperation::Identity, b"")
                .build()
                .expect("transforms"),
        )
        .build()
        .expect("dictionary");
    let bytes = description.to_bytes();
    let prepared = DictionaryBuilder::default()
        .add_serialized(&description)
        .build()
        .expect("prepare");
    let payload = b"unusualword otherword unusualword another string otherword ".repeat(72);
    for quality in [Quality::Q5, Quality::Q9, Quality::Q11] {
        let reference = Reference::new(&bytes, quality);
        let mut c_output = vec![0; payload.len() * 2 + 8192];
        let expected_len = reference.compress(&payload, quality, &mut c_output);
        let mut compressor =
            Compressor::new(EncoderConfig::default().with_quality(quality)).expect("config");
        let mut output = Vec::with_capacity(c_output.len());
        {
            let mut writer = compressor
                .writer_with_dictionary(&prepared, &mut output, Default::default())
                .expect("writer");
            writer.write_all(&payload).expect("write");
            writer.try_finish().expect("finish");
        }
        assert_eq!(&output, &c_output[..expected_len]);
        eprintln!(
            "q{} input={} compressed={} retained_dictionary={}",
            quality.get(),
            payload.len(),
            expected_len,
            prepared.retained_bytes()
        );
        let mut group = c.benchmark_group(format!("track-b/custom/q{}", quality.get()));
        group.throughput(Throughput::Bytes(payload.len() as u64));
        group.bench_function("c-brotli", |b| {
            b.iter(|| reference.compress(black_box(&payload), quality, &mut c_output))
        });
        group.bench_function("mbrotli", |b| {
            b.iter(|| {
                output.clear();
                let mut writer = compressor
                    .writer_with_dictionary(&prepared, &mut output, Default::default())
                    .expect("writer");
                writer.write_all(black_box(&payload)).expect("write");
                writer.try_finish().expect("finish");
            })
        });
        group.finish();

        let mut c_framed = Vec::with_capacity(c_output.len());
        frame_reference(&c_output[..expected_len], payload.len(), &mut c_framed);
        let framing_config = FramingConfig {
            container: false,
            central_directory: false,
            chunk_bytes: payload.len() + 1,
            ..Default::default()
        };
        let mut framed = |output: &mut Vec<u8>| {
            output.clear();
            let mut container = compressor
                .framed_writer(output, framing_config)
                .expect("container");
            {
                let mut resource = container
                    .resource_with_dictionary(
                        Default::default(),
                        Default::default(),
                        &prepared,
                        &[DictionaryReference::SerializedId(DictionaryId([0; 32]))],
                    )
                    .expect("resource");
                resource.write_all(black_box(&payload)).expect("write");
                resource.try_finish().expect("finish resource");
            }
            container.try_finish().expect("finish container");
        };
        framed(&mut output);
        assert_eq!(output, c_framed);
        let mut group = c.benchmark_group(format!("track-b/framing/q{}", quality.get()));
        group.throughput(Throughput::Bytes(payload.len() as u64));
        // Both timings are end-to-end stream/resource creation and finalization;
        // prepared dictionary construction stays outside the timed regions.
        group.bench_function("mbrotli", |b| b.iter(|| framed(&mut output)));
        let mut reference_output = Vec::with_capacity(c_framed.len());
        group.bench_function("c-brotli-with-rfc-envelope", |b| {
            b.iter(|| {
                let size = reference.compress(black_box(&payload), quality, &mut c_output);
                frame_reference(&c_output[..size], payload.len(), &mut reference_output);
            })
        });
        group.finish();
    }
}
fn metadata_envelope(encoded: &[u8], plain_size: usize, output: &mut Vec<u8>) {
    output.clear();
    output.extend_from_slice(&[0x91, 10, 66, 82, 4]);
    let mut header = vec![7, 2];
    number(plain_size, &mut header);
    number(header.len() + encoded.len(), output);
    output.extend_from_slice(&header);
    output.extend_from_slice(encoded);
    let mut footer = Vec::with_capacity(10);
    let mut size = output.len();
    loop {
        footer.clear();
        number(size, &mut footer);
        footer.reverse();
        footer.push(0); // no central directory
        let next = output.len() + footer.len() + 2;
        if next == size {
            break;
        }
        size = next;
    }
    output.push((footer.len() + 1) as u8);
    output.push(10);
    output.extend_from_slice(&footer);
}

fn metadata(c: &mut Criterion) {
    use mbrotli::framing::{MetadataEncoding, MetadataField, MetadataKind, MetadataOptions};
    let mut random = 7u64;
    let binary: Vec<u8> = (0..16384)
        .map(|_| {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            random as u8
        })
        .collect();
    for (name, value) in [
        ("small", b"a short description".to_vec()),
        (
            "text",
            b"application metadata with repeating words ".repeat(400),
        ),
        ("binary", binary),
    ] {
        let mut plain = b"AB".to_vec();
        number(value.len(), &mut plain);
        plain.extend_from_slice(&value);
        for quality in [Quality::Q5, Quality::Q9, Quality::Q11] {
            let mut c_output =
                vec![0; Compressor::max_compressed_size(plain.len()).expect("bound")];
            let c_encode = |output: &mut [u8]| {
                let mut size = output.len();
                // SAFETY: plain/output remain live with exactly these lengths;
                // the one-shot C API does not retain either pointer.
                let ok = unsafe {
                    ffi::BrotliEncoderCompress(
                        i32::from(quality.get()),
                        22,
                        ffi::BROTLI_DEFAULT_MODE,
                        plain.len(),
                        plain.as_ptr(),
                        &raw mut size,
                        output.as_mut_ptr(),
                    )
                };
                assert_eq!(ok, ffi::BROTLI_TRUE);
                size
            };
            let size = c_encode(&mut c_output);
            let mut compressor =
                Compressor::new(EncoderConfig::default().with_quality(quality)).expect("config");
            let mut output = Vec::with_capacity(c_output.len() + 128);
            let mut rust_encode = |output: &mut Vec<u8>| {
                output.clear();
                let mut writer = compressor
                    .framed_writer(
                        output,
                        FramingConfig {
                            central_directory: false,
                            ..Default::default()
                        },
                    )
                    .expect("writer");
                writer
                    .metadata_with_options(
                        MetadataKind::Global,
                        &[MetadataField {
                            code: *b"AB",
                            value: black_box(&value),
                        }],
                        MetadataOptions {
                            encoding: MetadataEncoding::Brotli,
                            ..Default::default()
                        },
                    )
                    .expect("metadata");
                writer.try_finish().expect("finish");
            };
            rust_encode(&mut output);
            let mut reference = Vec::with_capacity(output.capacity());
            metadata_envelope(&c_output[..size], plain.len(), &mut reference);
            assert_eq!(output, reference);
            let mut decoded = vec![0; plain.len()];
            let mut decoded_size = decoded.len();
            // SAFETY: both slices have the declared lengths and out-size is writable.
            let result = unsafe {
                ffi::BrotliDecoderDecompress(
                    size,
                    c_output.as_ptr(),
                    &raw mut decoded_size,
                    decoded.as_mut_ptr(),
                )
            };
            assert_eq!(result, ffi::BROTLI_DECODER_RESULT_SUCCESS);
            assert_eq!(decoded, plain);
            eprintln!(
                "metadata {name} q{} plain={} compressed={} container={}",
                quality.get(),
                plain.len(),
                size,
                output.len()
            );
            let mut group =
                c.benchmark_group(format!("track-b/metadata/{name}/q{}", quality.get()));
            group.throughput(Throughput::Bytes(value.len() as u64));
            group.bench_function("mbrotli", |b| b.iter(|| rust_encode(&mut output)));
            group.bench_function("c-brotli-with-rfc-envelope", |b| {
                b.iter(|| {
                    let size = c_encode(&mut c_output);
                    metadata_envelope(&c_output[..size], plain.len(), &mut reference);
                })
            });
            group.finish();
        }
    }
}

criterion_group!(benches, dictionaries, metadata);
criterion_main!(benches);
