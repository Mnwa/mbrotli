use crate::compressor::BrotliCompressParams;
use fearless_simd::Level;
use std::io::Read;

pub struct BrotliCompressorReader<T: Read> {
    pub(crate) reader: T,
    pub(crate) level: Level,
    pub(crate) params: BrotliCompressParams,
}

impl<T: Read> Read for BrotliCompressorReader<T> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        todo!()
    }
}
