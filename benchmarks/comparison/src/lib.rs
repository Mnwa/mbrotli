//! Standalone end-to-end Brotli implementation comparison.

mod core;

/// Run the isolated Criterion suite with command-line filtering and sampling.
///
/// # Errors
/// Returns an error if encoding, round-trip validation, or size reporting fails.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    core::run()
}
