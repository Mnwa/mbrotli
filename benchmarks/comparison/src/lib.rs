//! Standalone end-to-end Brotli implementation comparison.

mod core;

/// Run the isolated Criterion suite with command-line filtering and sampling.
///
/// # Errors
/// Returns an error if encoding, round-trip validation, or size reporting fails.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    core::run()
}

/// Run the isolated cold decoder comparison on identical C-encoded streams.
///
/// # Errors
/// Returns an error if stream preparation, decoding, validation, or reporting fails.
pub fn run_decoders() -> Result<(), Box<dyn std::error::Error>> {
    core::decoder::run()
}
