use crate::compressor::BrotliCompressParams;
use fearless_simd::Level;
use std::io::Write;

/// Write to
pub struct BrotliCompressorWriter<T: Write> {
    pub(crate) writer: T,
    pub(crate) level: Level,
    pub(crate) params: BrotliCompressParams,
}

impl<T: Write> Write for BrotliCompressorWriter<T> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        todo!()
    }

    fn flush(&mut self) -> std::io::Result<()> {
        todo!()
    }
}
