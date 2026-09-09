//! Benchmark inputs shared by the compressor and the decompressor benchmarks.
//!
//! Every corpus is generated deterministically at startup or read from Google
//! Brotli's own test data in `brotli-ffi/vendor/brotli/tests/testdata`, so the
//! two benchmarks measure identical bytes: the decompressor decodes exactly
//! the payloads the compressor encodes, at the same qualities and window.
//!
//! The file lives under `benches/support/` rather than `benches/` because
//! Cargo registers every top-level `benches/*.rs` file as its own benchmark
//! target; a subdirectory is reachable only through `#[path]`.

use std::path::Path;

/// A named benchmark input.
pub struct Corpus {
    pub name: String,
    pub data: Vec<u8>,
}

impl Corpus {
    pub fn new(name: impl Into<String>, data: Vec<u8>) -> Self {
        Self {
            name: name.into(),
            data,
        }
    }
}

/// Builds the deterministic corpora: text, binary, compressible,
/// incompressible, small, and large inputs, followed by the vendored files.
pub fn corpora() -> Vec<Corpus> {
    let mut corpora = vec![
        Corpus::new("text-1KiB", text(1 << 10)),
        Corpus::new("text-1MiB", text(1 << 20)),
        Corpus::new("binary-256KiB", binary(1 << 18)),
        Corpus::new("compressible-256KiB", compressible(1 << 18)),
        Corpus::new("incompressible-256KiB", incompressible(1 << 18)),
    ];
    corpora.extend(vendor_corpora());
    corpora
}

/// Payload sizes used to measure the fixed per-call cost.
pub const TINY_SIZES: [usize; 4] = [16, 64, 256, 1024];

/// Files of Google Brotli's own corpus used as real-world inputs.
const VENDOR_FILES: [&str; 6] = [
    "alice29.txt",
    "lcet10.txt",
    "plrabn12.txt",
    "mapsdatazrh",
    "random_org_10k.bin",
    "quickfox_repeated",
];

/// Reads the vendored reference corpus, skipping files that are not present.
pub fn vendor_corpora() -> Vec<Corpus> {
    let directory =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("brotli-ffi/vendor/brotli/tests/testdata");
    VENDOR_FILES
        .iter()
        .filter_map(|name| {
            let data = std::fs::read(directory.join(name)).ok()?;
            Some(Corpus::new(format!("vendor-{name}"), data))
        })
        .collect()
}

/// Generates `len` bytes of English-like text.
pub fn text(len: usize) -> Vec<u8> {
    const PARAGRAPH: &str = concat!(
        "Brotli is a generic-purpose lossless compression algorithm that ",
        "compresses data using a combination of a modern variant of the LZ77 ",
        "algorithm, Huffman coding and second order context modeling. ",
    );

    let mut out = String::with_capacity(len + PARAGRAPH.len());
    let mut line = 0_usize;
    while out.len() < len {
        out.push_str(&format!("{line}. {PARAGRAPH}\n"));
        line += 1;
    }

    let mut bytes = out.into_bytes();
    bytes.truncate(len);
    bytes
}

/// Generates `len` bytes of structured binary data: fixed-size records holding
/// a counter, a derived tag, and bytes drawn from a small pool.
fn binary(len: usize) -> Vec<u8> {
    const POOL: [u8; 8] = [0x00, 0xff, 0x7f, 0x80, 0x01, 0xfe, 0x10, 0xef];

    let mut bytes = Vec::with_capacity(len + 16);
    let mut record = 0_u32;
    while bytes.len() < len {
        bytes.extend_from_slice(&record.to_le_bytes());
        bytes.extend_from_slice(&record.wrapping_mul(2_654_435_761).to_le_bytes());
        for offset in 0..8 {
            bytes.push(POOL[(record as usize + offset) % POOL.len()]);
        }
        record += 1;
    }

    bytes.truncate(len);
    bytes
}

/// Generates `len` bytes of highly compressible data: long runs broken up by a
/// short repeating marker.
fn compressible(len: usize) -> Vec<u8> {
    let mut bytes = vec![0; len];
    for (index, byte) in bytes.iter_mut().enumerate() {
        if index % 1024 == 0 {
            *byte = b'#';
        }
    }
    bytes
}

/// Generates `len` bytes of incompressible data from a deterministic PRNG.
pub fn incompressible(len: usize) -> Vec<u8> {
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as u8
        })
        .collect()
}
