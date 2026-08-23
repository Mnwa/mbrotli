//! Brotli compression and decompression library.

use crate::compressor::BrotliCompressor;
use fearless_simd::Level;

pub mod compressor;

#[derive(Copy, Clone, Debug)]
pub struct Brotli {
    level: Level,
}

impl Default for Brotli {
    fn default() -> Self {
        Self::from(Level::try_detect().unwrap_or_else(|| Level::baseline()))
    }
}

impl Brotli {
    pub fn compressor(&self) -> BrotliCompressor {
        BrotliCompressor::from(*self)
    }
}

impl From<Level> for Brotli {
    fn from(value: Level) -> Self {
        Self { level: value }
    }
}
