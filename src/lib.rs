//! Safe Rust Brotli codecs, independently selected with Cargo features.
//!
//! `compression` and `decompression` are enabled by default. Disable default
//! features and select either codec alongside `std` or `no_std`. For example:
//!
//! ```toml
//! mbrotli = { version = "0.2", default-features = false, features = ["std", "decompression"] }
//! ```
//!
//! A disabled codec has no module, API re-exports, dictionaries or I/O adapters.
//! Shared `Backend` and `RetentionPolicy` types exist when either codec is enabled.
//! With neither codec enabled, the crate exposes no codec API.
#![cfg_attr(feature = "compression", doc = include_str!("compressor.md"))]
// The port is safe Rust by construction: the bit writer, the match scans and
// the SIMD kernels all shed their bounds checks through `as_chunks`,
// `first_chunk` and const-generic widths rather than through raw pointers.
// `forbid` rather than `deny`, so no module can opt back in.
//
// The differential unit tests inside `core::hq` and `core::rfc9841` call
// Google's C encoder through `google-brotli-ffi` to compare a stage against
// its reference, which is unavoidably `unsafe`. Those live behind `cfg(test)`
// and reach nothing that ships, so the ban is on everything but the test
// build rather than weakened to a `deny` the shipped code could opt out of.
#![cfg_attr(not(test), forbid(unsafe_code))]
#![cfg_attr(feature = "no_std", no_std)]
#![cfg_attr(
    feature = "no_std",
    doc = "
Std-only modules are unavailable in this mode:

```compile_fail
use mbrotli::io::EncoderWriter;
```

```compile_fail
use mbrotli::compressor::parallel::ParallelCompressor;
```

```compile_fail
use mbrotli::framing::FramingConfig;
```"
)]
#![deny(missing_docs)]
#![deny(missing_debug_implementations)]
#![deny(rustdoc::broken_intra_doc_links)]

#[cfg(any(feature = "compression", feature = "decompression"))]
#[cfg_attr(any(feature = "compression", test), macro_use)]
extern crate alloc;

#[cfg(test)]
extern crate std;

#[cfg(any(feature = "compression", feature = "decompression"))]
mod backend;
#[cfg(any(feature = "compression", feature = "decompression"))]
mod retention;
#[cfg(any(feature = "compression", feature = "decompression"))]
mod shared;
#[cfg(any(feature = "compression", feature = "decompression"))]
mod window;
#[cfg(any(feature = "compression", feature = "decompression"))]
pub use backend::Backend;
#[cfg(any(feature = "compression", feature = "decompression"))]
pub use retention::RetentionPolicy;
#[cfg(any(feature = "compression", feature = "decompression"))]
pub use window::{ConfigError, Window, WindowEncoding};

#[cfg(feature = "compression")]
pub mod compressor;
#[cfg(feature = "decompression")]
pub mod decompressor;
#[cfg(feature = "decompression")]
pub use decompressor::{
    DecodeFailure, DecodeOperation, DecodeProgress, DecoderSession, DecoderStatus, Decompressor,
    DecompressorBuilder,
};

#[cfg(feature = "decompression")]
pub use decompressor::{
    DecodeConfigError, DecodeError, DecodeLimits, DecodeStreamConfig, DecoderConfig,
    InvalidDataKind, MemberMode, OutputSize, WindowLimit,
};

#[cfg(any(feature = "compression", feature = "decompression"))]
pub mod dictionary;
#[cfg(all(
    feature = "compression",
    feature = "experimental",
    not(feature = "no_std")
))]
pub use compressor::framing;
#[cfg(all(
    any(feature = "compression", feature = "decompression"),
    not(feature = "no_std")
))]
mod finish_error;
#[cfg(all(
    any(feature = "compression", feature = "decompression"),
    not(feature = "no_std")
))]
pub mod io;
#[cfg(feature = "compression")]
pub use compressor::{
    BlockBits, BlockSize, CompressionMode, Compressor, CompressorBuilder, DistanceParams,
    EncodeError, EncoderConfig, EncoderSession, EncoderStatus, InputSize, LiteralContextMode,
    Operation, Progress, Quality, SizeOverflow, StreamConfig,
};
