//! Brotli compression, in safe Rust.
//!
//! `mbrotli` implements every Brotli quality as a port of Google's reference
//! encoder, and emits bytes that are identical to it. There is no `unsafe` in
//! this crate, and the SIMD instruction set is resolved once per compressor
//! rather than inside any loop.
//!
//! There is no decoder: round-trip verification uses Google's C decoder, and
//! this crate compresses only.
//!
//! # The shape of the API
//!
//! ```text
//! EncoderConfig       what every stream is encoded with
//! Compressor          a reusable encoder, and the workspace it owns
//! StreamConfig        what one stream knows about itself
//! EncoderSession      one stream, driven a chunk at a time
//! PreparedDictionary  immutable knowledge many compressors can share
//! EncoderReader       adapters over a session
//! EncoderWriter
//! ```
//!
//! A [`Compressor`] is stateful, and every encoding method takes `&mut self`.
//! That is the whole design: the encoder's hash tables, sliding window and
//! histograms are expensive to build and cheap to reuse, so a compressor keeps
//! them and hands them to the next call. Reuse is what ordinary code gets,
//! rather than something to opt into.
//!
//! One compressor belongs to one worker. For parallel compression build one
//! per worker with [`Compressor::fork_empty`]; a lock around a single
//! compressor would serialise the compression itself, not merely the access.
//!
//! # Choosing a quality
//!
//! | Quality | What it does | Typical use |
//! | --- | --- | --- |
//! | 0 | One pass, static entropy codes | Fastest, largest output |
//! | 1 | Two passes, per-block entropy codes | Fast |
//! | 2 | Greedy matching with the format's fixed codes | Fast |
//! | 3 | Greedy matching, one prefix code per stream | Balanced |
//! | 4 | Adds block splitting and histogram optimisation | Balanced, denser |
//! | 5 | Adds an extensive search and literal context modelling | Densest of these |
//! | 6 to 9 | Wider match search, more cached distances, richer context models | Denser, slower |
//! | 10, 11 | Binary-tree matching and a Zopfli dynamic program | Densest, slowest |
//!
//! [`EncoderConfig::default`] is quality 11, which mirrors the reference
//! encoder's default and is far slower than most callers want. For online
//! compression, say so:
//!
//! ```
//! use mbrotli::{Compressor, EncoderConfig, Quality};
//!
//! let mut encoder = Compressor::new(EncoderConfig::default().with_quality(Quality::Q5))?;
//! let payload = "the quick brown fox ".repeat(500);
//!
//! let compressed = encoder.compress(payload.as_bytes())?;
//!
//! assert!(compressed.len() < payload.len() / 100);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Reusing a compressor and its destination
//!
//! [`Compressor::compress_into`] is the entry point to reach for when there is
//! more than one thing to compress. It appends to a destination the caller
//! owns, so both the encoder's workspace and the output buffer are reused, and
//! a warm compressor writing into a destination that is already big enough
//! allocates nothing at all.
//!
//! ```
//! use mbrotli::{Compressor, EncoderConfig, Quality};
//!
//! let mut encoder = Compressor::new(EncoderConfig::default().with_quality(Quality::Q5))?;
//! let mut output = Vec::new();
//!
//! for payload in [&b"first"[..], b"second", b"third"] {
//!     output.clear();
//!     encoder.compress_into(payload, &mut output)?;
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Streaming
//!
//! [`Compressor::writer`] compresses everything written to it. `Write` has no
//! closing hook and a meta-block boundary need not land on a byte boundary, so
//! the stream is terminated explicitly:
//!
//! ```
//! use mbrotli::io::FinishError;
//! use mbrotli::{Compressor, EncoderConfig, InputSize, Quality};
//! use std::io::Write;
//!
//! let mut encoder = Compressor::new(EncoderConfig::default().with_quality(Quality::Q5))?;
//! let payload = b"streamed in chunks".repeat(50);
//!
//! let streamed = {
//!     let stream = InputSize::Exact(payload.len() as u64).into();
//!     let mut sink = encoder.writer(Vec::new(), stream)?;
//!     for chunk in payload.chunks(64) {
//!         sink.write_all(chunk)?;
//!     }
//!     sink.finish().map_err(FinishError::into_error)?
//! };
//!
//! assert_eq!(streamed, encoder.compress(&payload)?);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Declaring [`InputSize::Exact`] is what makes that last assertion hold:
//! qualities four and five choose their match finder from how much input is
//! coming, so a stream that does not say produces different — equally valid —
//! bytes. [`Compressor::reader`] is the pull-shaped counterpart, and
//! [`Compressor::start`] is the state machine both are built on.
//!
//! # Large Window Brotli
//!
//! [RFC 9841] widens the sliding window past what RFC 7932 can express. Which
//! header a stream carries is part of the window itself: build one with
//! [`Window::standard`] or [`Window::large`], never by widening a number.
//!
//! ```
//! use mbrotli::{Compressor, EncoderConfig, Quality, Window};
//!
//! let config = EncoderConfig::default()
//!     .with_quality(Quality::Q5)
//!     .with_window(Window::large(30)?);
//! let mut encoder = Compressor::new(config)?;
//!
//! let compressed = encoder.compress("large window ".repeat(1000).as_bytes())?;
//!
//! // The stream carries the RFC 9841 header, so it needs a decoder expecting one.
//! assert_eq!(compressed[0], 0b0001_0001);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Qualities 0, 1 and 2 write distances through a model built for the RFC 7932
//! alphabet, so [`Compressor::new`] refuses a Large Window there rather than
//! quietly dropping the request.
//!
//! # Shared dictionaries
//!
//! RFC 9841 also lets a caller attach up to fifteen LZ77 prefix dictionaries in
//! front of a stream. A [`PreparedDictionary`](dictionary::PreparedDictionary)
//! is immutable and holds no per-stream state, so any number of compressors may
//! borrow one at once without a lock.
//!
//! ```
//! use mbrotli::dictionary::DictionaryBuilder;
//! use mbrotli::{Compressor, EncoderConfig, Quality};
//!
//! let dictionary = DictionaryBuilder::new()
//!     .add_prefix(&b"HTTP/1.1 200 OK\r\nContent-Type: "[..])
//!     .build()?;
//! let mut encoder = Compressor::new(EncoderConfig::default().with_quality(Quality::Q5))?;
//!
//! let payload = b"Content-Type: text/html; charset=utf-8";
//! assert!(
//!     encoder.compress_with_dictionary(&dictionary, payload)?.len()
//!         < encoder.compress(payload)?.len()
//! );
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Below quality five no match finder can consult a dictionary, and one handed
//! to such a compressor is refused with
//! [`EncodeError::DictionaryUnsupportedForQuality`] rather than ignored: a
//! stream compressed without the dictionary it was given decodes perfectly
//! well, which is what would make the mistake invisible.
//!
//! The `experimental` feature adds serialized shared dictionaries, custom word
//! and transform indexes, headerless stream continuations, and the separate
//! Shared Brotli framing writer. These extensions do not carry the standard
//! encoder's blanket C byte-identity guarantee.
//!
//! [RFC 9841]: https://www.rfc-editor.org/rfc/rfc9841.html

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
#![deny(missing_docs)]
#![deny(missing_debug_implementations)]
#![deny(rustdoc::broken_intra_doc_links)]

mod compressor;

#[cfg(feature = "experimental")]
pub use compressor::framing;
pub use compressor::{
    BlockBits, BlockSize, CompressionMode, Compressor, CompressorBuilder, ConfigError,
    DistanceParams, EncodeError, EncoderConfig, EncoderSession, EncoderStatus, InputSize,
    LiteralContextMode, Operation, Progress, Quality, RetentionPolicy, SizeOverflow, StreamConfig,
    Window, WindowEncoding,
};
pub use compressor::{dictionary, io};
