mod core;
pub mod reader;
pub mod writer;

use crate::Brotli;
use crate::compressor::reader::BrotliCompressorReader;
use crate::compressor::writer::BrotliCompressorWriter;
use fearless_simd::Level;
use std::io::{Read, Write};
use thiserror::Error;

#[derive(Copy, Clone, Debug)]
pub struct BrotliCompressor {
    level: Level,
}

impl BrotliCompressor {
    pub const fn calculate_bound(&self, params: &BrotliCompressParams, input_size: usize) -> usize {
        core::bound::bound(params, input_size)
    }
    pub fn compress(&self, params: BrotliCompressParams, src: &[u8]) -> BrotliResult<Vec<u8>> {
        let mut output = Vec::with_capacity(self.calculate_bound(&params, src.len()));
        self.compress_to_slice(params, src, &mut output)?;
        Ok(output)
    }

    pub fn compress_to_slice(
        &self,
        params: BrotliCompressParams,
        src: &[u8],
        dst: &mut [u8],
    ) -> BrotliResult<()> {
        self.compress_reader(params, src)
            .read_exact(dst)
            .map_err(From::from)
    }

    pub fn compress_writer<T: Write>(
        &self,
        params: BrotliCompressParams,
        writer: T,
    ) -> BrotliCompressorWriter<T> {
        BrotliCompressorWriter {
            writer,
            level: self.level,
            params,
        }
    }

    pub fn compress_reader<T: Read>(
        &self,
        params: BrotliCompressParams,
        reader: T,
    ) -> BrotliCompressorReader<T> {
        BrotliCompressorReader {
            reader,
            level: self.level,
            params,
        }
    }
}

impl From<Level> for BrotliCompressor {
    fn from(value: Level) -> Self {
        Self { level: value }
    }
}

impl From<Brotli> for BrotliCompressor {
    fn from(value: Brotli) -> Self {
        Self::from(value.level)
    }
}

#[derive(Copy, Clone, Debug)]
pub struct BrotliCompressParams {
    quality: BrotliQualityLevel,
    lgwin: usize,
}

#[derive(Copy, Clone, Debug)]
pub enum BrotliQualityLevel {
    Q0,
    Q1,
    Q2,
    Q3,
    Q4,
    Q5,
    Q6,
    Q7,
    Q8,
    Q9,
    Q11,
}

impl TryFrom<usize> for BrotliQualityLevel {
    type Error = ParseQualityLevelError;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        todo!()
    }
}

#[derive(Error, Debug)]
#[non_exhaustive]
pub enum ParseQualityLevelError {
    #[error("Quality level should be positive")]
    LowerBound,
    #[error("Quality level should be less than or equal to 11")]
    UpperBound,
}

#[derive(Error, Debug)]
#[non_exhaustive]
pub enum BrotliCompressError {
    #[error("IO error: {0}")]
    IOError(#[from] std::io::Error),
}

pub type BrotliResult<T> = Result<T, BrotliCompressError>;
