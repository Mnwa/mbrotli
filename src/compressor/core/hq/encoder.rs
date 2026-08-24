//! The streaming quality 10 and 11 encoder.
//!
//! Ports the quality-ten and quality-eleven arms of `EncodeData` and
//! `WriteMetaBlockInternal` from `c/enc/encode.c` of the pinned reference
//! (`google/brotli` v1.2.0, commit `028fb5a`).
//!
//! This is the only place in the high-quality path that branches on the
//! instruction set: one [`dispatch!`] per public call hands a SIMD token to the
//! match search, and everything below it is monomorphised on that token. Which
//! quality runs, how deep it searches, how many times the splitter refines —
//! all of it was resolved before the encoder was built, so nothing about the
//! machine can reach a decision that shows up in the output.

use fearless_simd::{Level, dispatch};

use super::h10::BinaryTreeMatcher;
use super::metablock::MetaBlockBuilder;
use super::params::{HqParams, HqQuality};
use super::zopfli::{
    ZopfliState, ZopfliWorkspace, create_hq_zopfli_backward_references,
    create_zopfli_backward_references,
};
use crate::compressor::core::shared::bits::BitWriter;
use crate::compressor::core::shared::bitstream::{MetaBlockWriter, store_uncompressed_meta_block};
use crate::compressor::core::shared::command::{Command, extend_last_command};
use crate::compressor::core::shared::constants::{OUTPUT_RESERVE_CONST, OUTPUT_SLACK};
use crate::compressor::core::shared::distance::DistanceParams;
use crate::compressor::core::shared::format::ContextMode;
use crate::compressor::core::shared::histogram::{HistogramLiteral, bits_entropy};
use crate::compressor::core::shared::metablock::{MetaBlockSplit, optimize_histograms};
use crate::compressor::core::shared::ringbuffer::{BlockSpan, RingBuffer, wrap_position};
use crate::compressor::{BrotliCompressError, BrotliResult, CompressParams};

/// Stride the compressibility check samples literals at (`kSampleRate`).
const SAMPLE_RATE: u32 = 13;

/// Bits per byte above which sampled data is stored uncompressed.
const MIN_ENTROPY: f64 = 7.92;

/// Fraction of a block that has to be literals before it is even sampled.
const LITERAL_FRACTION: f64 = 0.99;

/// Streaming encoder for qualities ten and eleven.
pub(crate) struct HqEncoder {
    level: Level,
    params: HqParams,
    ringbuffer: RingBuffer,
    matcher: BinaryTreeMatcher,
    is_prepared: bool,
    input_pos: u64,
    last_processed_pos: u64,
    last_flush_pos: u64,
    commands: Vec<Command>,
    references: ZopfliState,
    workspace: ZopfliWorkspace,
    builder: MetaBlockBuilder,
    saved_dist_cache: [i32; 4],
    prev_byte: u8,
    prev_byte2: u8,
    last_bytes: u16,
    last_bytes_bits: u32,
    is_last_block_emitted: bool,
    finished: bool,
    storage: Vec<u8>,
    writer: MetaBlockWriter,
    output_len: usize,
}

impl HqEncoder {
    /// Creates an encoder for `params`.
    ///
    /// Unlike the lower qualities, no size hint is taken: both qualities use
    /// the same matcher whatever the input size.
    ///
    /// # Errors
    ///
    /// Returns [`BrotliCompressError::UnsupportedQuality`] when the quality is
    /// outside the range this encoder implements.
    pub(crate) fn new(level: Level, params: &CompressParams) -> BrotliResult<Self> {
        let resolved = HqParams::new(params)?;
        let (last_bytes, last_bytes_bits) = resolved.window.header();
        let references = ZopfliState::default();
        Ok(Self {
            level,
            params: resolved,
            ringbuffer: RingBuffer::new(resolved.rb_bits(), resolved.lgblock),
            matcher: BinaryTreeMatcher::new(resolved.lgwin),
            is_prepared: false,
            input_pos: 0,
            last_processed_pos: 0,
            last_flush_pos: 0,
            commands: Vec::new(),
            workspace: ZopfliWorkspace::new(
                resolved.input_block_size(),
                resolved.dist.alphabet_size_limit as usize,
            ),
            builder: MetaBlockBuilder::default(),
            saved_dist_cache: references.dist_cache,
            references,
            prev_byte: 0,
            prev_byte2: 0,
            last_bytes,
            last_bytes_bits,
            is_last_block_emitted: false,
            finished: false,
            storage: Vec::new(),
            writer: MetaBlockWriter::default(),
            output_len: 0,
        })
    }

    /// Returns the largest input this encoder accepts in one call.
    pub(crate) const fn block_size_limit(&self) -> usize {
        self.params.input_block_size()
    }

    /// Returns whether the final meta-block has already been written.
    pub(crate) const fn is_finished(&self) -> bool {
        self.finished
    }

    /// Compresses one input block and returns the bytes it completed.
    ///
    /// # Errors
    ///
    /// Returns [`BrotliCompressError::BufferOverflow`] when the scratch buffer
    /// proved too small, which would indicate a bug in the size bound.
    #[hotpath::measure]
    pub(crate) fn encode_block(&mut self, input: &[u8], is_last: bool) -> BrotliResult<&[u8]> {
        debug_assert!(!self.finished);
        debug_assert!(input.len() <= self.block_size_limit());

        self.copy_input_to_ring_buffer(input);
        self.encode_data(is_last)?;
        match self.storage.get(..self.output_len) {
            Some(output) => Ok(output),
            None => Err(BrotliCompressError::BufferOverflow),
        }
    }

    /// Appends `input` to the window (`CopyInputToRingBuffer`).
    fn copy_input_to_ring_buffer(&mut self, input: &[u8]) {
        if input.is_empty() {
            return;
        }
        self.ringbuffer.write(input);
        self.input_pos += input.len() as u64;
        self.ringbuffer.clear_margin();
    }

    /// Marks the input as processed, reporting whether positions wrapped.
    fn update_last_processed_pos(&mut self) -> bool {
        let wrapped_last = wrap_position(self.last_processed_pos);
        let wrapped_input = wrap_position(self.input_pos);
        self.last_processed_pos = self.input_pos;
        wrapped_input < wrapped_last
    }

    /// Makes sure the scratch buffer can hold a meta-block of `size` bytes.
    fn reserve_storage(&mut self, size: usize) -> BrotliResult<()> {
        let Some(reserve) = size
            .checked_mul(2)
            .and_then(|doubled| doubled.checked_add(OUTPUT_RESERVE_CONST))
            .and_then(|reserve| reserve.checked_add(OUTPUT_SLACK))
        else {
            return Err(BrotliCompressError::BufferOverflow);
        };
        if self.storage.len() < reserve {
            self.storage = vec![0u8; reserve];
        }
        Ok(())
    }

    /// Seeds the scratch buffer with the bits carried from the last meta-block.
    fn seed_storage_with_the_partial_byte(&mut self) {
        if let Some(head) = self.storage.get_mut(..2) {
            head[0] = self.last_bytes as u8;
            head[1] = (self.last_bytes >> 8) as u8;
        }
    }

    /// Emits the two bits that close a stream that never had any input.
    fn emit_empty_stream(&mut self) -> BrotliResult<()> {
        self.reserve_storage(0)?;
        self.last_bytes |= 3u16 << self.last_bytes_bits;
        self.last_bytes_bits += 2;
        self.seed_storage_with_the_partial_byte();
        self.output_len = ((self.last_bytes_bits + 7) >> 3) as usize;
        self.finished = true;
        Ok(())
    }

    /// Runs the Zopfli search over the unprocessed input.
    ///
    /// This is the encoder's only SIMD dispatch: the token is resolved here and
    /// passed by value into the monomorphised search.
    fn create_references(&mut self, span: BlockSpan) {
        let position = span.position as usize;
        let num_bytes = span.bytes as usize;
        let Self {
            level,
            params,
            ringbuffer,
            matcher,
            workspace,
            references,
            commands,
            ..
        } = self;
        let data = ringbuffer.buffer();
        let mask = ringbuffer.mask();
        match params.quality {
            HqQuality::Q10 => dispatch!(*level, simd => create_zopfli_backward_references(
                simd, num_bytes, position, data, mask, params, matcher, workspace,
                references, commands,
            )),
            HqQuality::Q11 => dispatch!(*level, simd => create_hq_zopfli_backward_references(
                simd, num_bytes, position, data, mask, params, matcher, workspace,
                references, commands,
            )),
        }
    }

    /// Processes the accumulated input, emitting a meta-block if one is due.
    ///
    /// Mirrors `EncodeData` for qualities ten and eleven.
    fn encode_data(&mut self, is_last: bool) -> BrotliResult<()> {
        self.output_len = 0;
        let delta = self.input_pos - self.last_processed_pos;
        let mut span = BlockSpan {
            position: wrap_position(self.last_processed_pos),
            bytes: delta as u32,
        };

        if delta == 0 {
            if !self.ringbuffer.is_allocated() {
                if is_last {
                    return self.emit_empty_stream();
                }
                return Ok(());
            }
            if !is_last {
                return Ok(());
            }
        }
        if self.is_last_block_emitted {
            return Err(BrotliCompressError::BufferOverflow);
        }
        if is_last {
            self.is_last_block_emitted = true;
        }

        // Theoretical maximum of one command per two bytes, plus room to merge
        // the next block in without reallocating.
        let needed = self.commands.len() + span.bytes as usize / 2 + 1;
        if self.commands.capacity() < needed {
            self.commands
                .reserve(needed + span.bytes as usize / 4 + 16 - self.commands.len());
        }

        self.prepare_matcher(span.position as usize, span.bytes as usize);

        // The context mode is chosen over the whole pending meta-block, not
        // just this input block, and before any command is created.
        let context_mode = {
            let data = self.ringbuffer.buffer();
            let mask = self.ringbuffer.mask();
            self.params.choose_context_mode(
                data,
                wrap_position(self.last_flush_pos) as usize,
                mask,
                (self.input_pos - self.last_flush_pos) as usize,
            )
        };

        // A copy that ran off the end of the previous block continues into
        // this one; growing that command is cheaper than starting a new one.
        if !self.commands.is_empty() && self.references.last_insert_len == 0 {
            let Self {
                params,
                ringbuffer,
                references,
                commands,
                last_processed_pos,
                ..
            } = self;
            if let Some(command) = commands.last_mut() {
                extend_last_command(
                    command,
                    params.lgwin,
                    &params.dist,
                    references.dist_cache[0],
                    ringbuffer.buffer(),
                    ringbuffer.mask(),
                    *last_processed_pos,
                    &mut span,
                );
            }
        }

        self.create_references(span);

        {
            let max_length = self.params.max_metablock_size();
            let max_literals = max_length / 8;
            let max_commands = max_length / 8;
            let processed_bytes = (self.input_pos - self.last_flush_pos) as usize;
            let next_input_fits_metablock =
                processed_bytes + self.params.input_block_size() <= max_length;
            if !is_last
                && next_input_fits_metablock
                && self.references.num_literals < max_literals
                && self.commands.len() < max_commands
            {
                // Merge with the next input block instead.
                if self.update_last_processed_pos() {
                    self.is_prepared = false;
                }
                return Ok(());
            }
        }

        if self.references.last_insert_len > 0 {
            self.commands
                .push(Command::insert_only(self.references.last_insert_len));
            self.references.num_literals += self.references.last_insert_len;
            self.references.last_insert_len = 0;
        }

        if !is_last && self.input_pos == self.last_flush_pos {
            return Ok(());
        }
        debug_assert!(self.input_pos >= self.last_flush_pos);

        let metablock_size = (self.input_pos - self.last_flush_pos) as usize;
        self.reserve_storage(metablock_size)?;
        self.seed_storage_with_the_partial_byte();

        let position = self.write_meta_block(metablock_size, is_last, context_mode)?;

        let complete = position >> 3;
        self.last_bytes = u16::from(self.storage.get(complete).copied().unwrap_or(0));
        self.last_bytes_bits = (position & 7) as u32;
        self.last_flush_pos = self.input_pos;
        if self.update_last_processed_pos() {
            self.is_prepared = false;
        }
        let mask = self.ringbuffer.mask();
        if self.last_flush_pos > 0 {
            self.prev_byte = self
                .ringbuffer
                .buffer()
                .get(((self.last_flush_pos as u32).wrapping_sub(1) as usize) & mask)
                .copied()
                .unwrap_or(0);
        }
        if self.last_flush_pos > 1 {
            self.prev_byte2 = self
                .ringbuffer
                .buffer()
                .get(((self.last_flush_pos as u32).wrapping_sub(2) as usize) & mask)
                .copied()
                .unwrap_or(0);
        }
        self.commands.clear();
        self.references.num_literals = 0;
        self.saved_dist_cache = self.references.dist_cache;
        self.output_len = complete;
        self.finished = is_last;
        Ok(())
    }

    /// Sets the matcher up and hands it the block boundary positions.
    ///
    /// Mirrors `InitOrStitchToPreviousBlock`.
    fn prepare_matcher(&mut self, position: usize, input_size: usize) {
        let Self {
            level,
            ringbuffer,
            matcher,
            is_prepared,
            ..
        } = self;
        let data = ringbuffer.buffer();
        let mask = ringbuffer.mask();
        if !*is_prepared {
            matcher.prepare();
            *is_prepared = true;
        }
        dispatch!(*level, simd => matcher.stitch_to_previous_block(
            simd, input_size, position, data, mask
        ));
    }

    /// Writes one meta-block, returning the bit position after it.
    ///
    /// Mirrors `WriteMetaBlockInternal` for the high-quality builder.
    fn write_meta_block(
        &mut self,
        bytes: usize,
        is_last: bool,
        context_mode: ContextMode,
    ) -> BrotliResult<usize> {
        let wrapped_last_flush_pos = wrap_position(self.last_flush_pos) as usize;
        let Self {
            params,
            ringbuffer,
            commands,
            references,
            builder,
            saved_dist_cache,
            prev_byte,
            prev_byte2,
            last_bytes_bits,
            storage,
            writer,
            ..
        } = self;
        let data = ringbuffer.buffer();
        let mask = ringbuffer.mask();

        let mut w = BitWriter::new(storage, *last_bytes_bits as usize);
        if bytes == 0 {
            // Write the ISLAST and ISEMPTY bits and stop.
            w.write(2, 3);
            w.align();
            let position = w.position();
            if w.overflowed() {
                return Err(BrotliCompressError::BufferOverflow);
            }
            return Ok(position);
        }

        if !should_compress(
            data,
            mask,
            self.last_flush_pos,
            bytes,
            references.num_literals,
            commands.len(),
        ) {
            references.dist_cache = *saved_dist_cache;
            store_uncompressed_meta_block(
                is_last,
                data,
                wrapped_last_flush_pos,
                mask,
                bytes,
                &mut w,
            );
            let position = w.position();
            if w.overflowed() {
                return Err(BrotliCompressError::BufferOverflow);
            }
            return Ok(position);
        }

        let saved_last_bytes = u16::from(w.byte(1)) << 8 | u16::from(w.byte(0));
        let saved_last_bytes_bits = *last_bytes_bits as usize;

        // The builder re-tunes the distance alphabet for this block and
        // rewrites the commands' prefixes to match, so the writer has to be
        // handed the alphabet it settled on rather than the encoder's.
        let mut block_dist: DistanceParams = params.dist;
        let mut mb = MetaBlockSplit::default();
        builder.build(
            data,
            wrapped_last_flush_pos,
            mask,
            params,
            *prev_byte,
            *prev_byte2,
            commands,
            context_mode,
            &mut block_dist,
            &mut mb,
        );
        optimize_histograms(block_dist.alphabet_size_limit as usize, &mut mb);
        writer.store_meta_block(
            data,
            wrapped_last_flush_pos,
            bytes,
            mask,
            *prev_byte,
            *prev_byte2,
            is_last,
            context_mode,
            &block_dist,
            commands,
            &mb,
            &mut w,
        );

        if bytes + 4 < (w.position() >> 3) {
            // Compressing made it bigger; store the bytes as they are.
            references.dist_cache = *saved_dist_cache;
            w.rewind(saved_last_bytes_bits);
            w.set_byte(0, saved_last_bytes as u8);
            w.set_byte(1, (saved_last_bytes >> 8) as u8);
            store_uncompressed_meta_block(
                is_last,
                data,
                wrapped_last_flush_pos,
                mask,
                bytes,
                &mut w,
            );
        }
        let position = w.position();
        if w.overflowed() {
            return Err(BrotliCompressError::BufferOverflow);
        }
        Ok(position)
    }
}

/// Decides whether a meta-block is worth compressing at all.
///
/// Mirrors `ShouldCompress`. Even at these qualities the reference refuses to
/// spend a prefix code on data its sample says is noise.
fn should_compress(
    data: &[u8],
    mask: usize,
    last_flush_pos: u64,
    bytes: usize,
    num_literals: usize,
    num_commands: usize,
) -> bool {
    if bytes <= 2 {
        return false;
    }
    if num_commands >= (bytes >> 8) + 2 {
        return true;
    }
    if num_literals as f64 <= LITERAL_FRACTION * bytes as f64 {
        return true;
    }
    let mut literal_histo = HistogramLiteral::default();
    let bit_cost_threshold = bytes as f64 * MIN_ENTROPY / f64::from(SAMPLE_RATE);
    let samples = bytes.div_ceil(SAMPLE_RATE as usize);
    let mut pos = last_flush_pos as u32;
    for _ in 0..samples {
        literal_histo.add(usize::from(
            data.get(pos as usize & mask).copied().unwrap_or(0),
        ));
        pos = pos.wrapping_add(SAMPLE_RATE);
    }
    bits_entropy(&literal_histo.data) <= bit_cost_threshold
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compressor::{QualityLevel, WindowBits};

    /// Builds an encoder for one quality.
    fn encoder(quality: QualityLevel) -> HqEncoder {
        let params = CompressParams::new(quality, WindowBits::DEFAULT);
        HqEncoder::new(Level::new(), &params).expect("supported quality")
    }

    /// Compresses `data` in blocks and returns the whole stream.
    fn compress(quality: QualityLevel, data: &[u8]) -> Vec<u8> {
        compress_with(Level::new(), quality, data)
    }

    /// As [`compress`], on a chosen SIMD backend.
    fn compress_with(level: Level, quality: QualityLevel, data: &[u8]) -> Vec<u8> {
        let params = CompressParams::new(quality, WindowBits::DEFAULT);
        let mut encoder = HqEncoder::new(level, &params).expect("supported quality");
        let limit = encoder.block_size_limit();
        let mut out = Vec::new();
        let mut offset = 0usize;
        loop {
            let take = (data.len() - offset).min(limit);
            let is_last = offset + take == data.len();
            out.extend_from_slice(
                encoder
                    .encode_block(&data[offset..offset + take], is_last)
                    .expect("encoding failed"),
            );
            offset += take;
            if is_last {
                break;
            }
        }
        out
    }

    #[test]
    fn the_window_header_matches_the_reference_encoding() {
        let header = |lgwin| {
            HqParams::new(&CompressParams::new(
                QualityLevel::Q11,
                WindowBits::standard(lgwin).expect("a legal window"),
            ))
            .expect("a supported quality")
            .window
            .header()
        };
        assert_eq!(header(16), (0, 1));
        assert_eq!(header(17), (1, 7));
        assert_eq!(header(22), (11, 4));
        assert_eq!(header(10), (0x21, 7));
    }

    #[test]
    fn a_tiny_block_is_never_compressed() {
        assert!(!should_compress(&[1, 2], usize::MAX, 0, 2, 2, 0));
    }

    #[test]
    fn random_literals_are_stored_uncompressed() {
        let mut rng = 0x1234_5678u64;
        let data: Vec<u8> = (0..200_000)
            .map(|_| {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                (rng >> 24) as u8
            })
            .collect();
        assert!(!should_compress(
            &data,
            usize::MAX,
            0,
            data.len(),
            data.len(),
            1
        ));
    }

    #[test]
    fn both_qualities_produce_a_non_empty_stream() {
        for quality in [QualityLevel::Q10, QualityLevel::Q11] {
            let stream = compress(quality, b"hello hello hello hello hello hello");
            assert!(!stream.is_empty(), "quality {quality:?} produced nothing");
        }
    }

    #[test]
    fn an_empty_stream_is_two_bits() {
        for quality in [QualityLevel::Q10, QualityLevel::Q11] {
            let mut encoder = encoder(quality);
            let stream = encoder.encode_block(&[], true).expect("encoding failed");
            assert!(!stream.is_empty());
            assert!(encoder.is_finished());
        }
    }

    #[test]
    fn the_block_size_limit_follows_the_window() {
        // The default window is twenty-two bits, so `ComputeLgBlock` gives
        // eighteen at both qualities.
        assert_eq!(encoder(QualityLevel::Q10).block_size_limit(), 1 << 18);
        assert_eq!(encoder(QualityLevel::Q11).block_size_limit(), 1 << 18);
    }

    #[test]
    fn every_backend_produces_the_same_stream() {
        let data: Vec<u8> = (0..120_000u32).map(|i| (i * 7 % 253) as u8).collect();
        for quality in [QualityLevel::Q10, QualityLevel::Q11] {
            let mut streams = Vec::new();
            for level in [Level::new(), Level::baseline(), Level::fallback()] {
                streams.push(compress_with(level, quality, &data));
            }
            assert!(
                streams.windows(2).all(|pair| pair[0] == pair[1]),
                "quality {quality:?} differed between backends"
            );
        }
    }

    /// Compresses `data` with the pinned C encoder.
    fn c_compress(quality: i32, lgwin: i32, input: &[u8]) -> Vec<u8> {
        // SAFETY: the output buffer is sized by the reference's own bound and
        // `size` is updated in place to what was written.
        let capacity = unsafe { google_brotli_ffi::BrotliEncoderMaxCompressedSize(input.len()) }
            .max(64)
            + 1024;
        let mut output = vec![0u8; capacity];
        let mut size = output.len();
        let ok = unsafe {
            google_brotli_ffi::BrotliEncoderCompress(
                quality,
                lgwin,
                google_brotli_ffi::BROTLI_DEFAULT_MODE,
                input.len(),
                input.as_ptr(),
                &raw mut size,
                output.as_mut_ptr(),
            )
        };
        assert_eq!(ok, google_brotli_ffi::BROTLI_TRUE, "the C encoder failed");
        output.truncate(size);
        output
    }

    /// The shortest prefix of a hash-collision fixture that once diverged from
    /// the reference.
    ///
    /// Five identical bytes then a random one, so every position shares a
    /// four-byte prefix: the binary tree runs deep and the dynamic program's
    /// choices come down to fractions of a bit. It caught
    /// `ComputeDistanceCache` refilling from the wrong end of the saved cache,
    /// which cost a short distance code on the last command of the block. Kept
    /// verbatim, because regenerating it from a seed would not survive a change
    /// to whatever produced it.
    const COLLISION_REGRESSION: &[u8] = include_bytes!("collision_regression.bin");

    #[test]
    fn the_collision_regression_matches_the_c_encoder() {
        for quality in [QualityLevel::Q10, QualityLevel::Q11] {
            let expected = c_compress(usize::from(quality) as i32, 22, COLLISION_REGRESSION);
            let actual = compress(quality, COLLISION_REGRESSION);
            assert_eq!(
                actual,
                expected,
                "quality {quality:?}: {} bytes against {}",
                actual.len(),
                expected.len()
            );
        }
    }

    #[test]
    fn a_collision_pattern_matches_the_c_encoder() {
        // Five identical bytes then a random one: every position shares a
        // four-byte prefix, which is what makes the binary tree deep and the
        // dynamic program's choices delicate.
        let mut rng = 0x243F_6A88_85A3_08D3u64;
        let mut data = Vec::new();
        while data.len() < 4000 {
            data.extend_from_slice(b"AAAAA");
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            data.push((rng >> 24) as u8);
        }
        for len in (1800..2000).chain([3000, 4000]) {
            let prefix = &data[..len];
            for quality in [QualityLevel::Q10, QualityLevel::Q11] {
                let expected = c_compress(usize::from(quality) as i32, 22, prefix);
                let actual = compress(quality, prefix);
                assert_eq!(
                    actual,
                    expected,
                    "length {len}, quality {quality:?}: {} bytes against {}",
                    actual.len(),
                    expected.len()
                );
            }
        }
    }

    #[test]
    fn quality_eleven_compresses_at_least_as_well_as_quality_ten() {
        let mut data = Vec::new();
        while data.len() < 200_000 {
            data.extend_from_slice(
                b"The quick brown fox jumps over the lazy dog. Pack my box with five dozen jugs. ",
            );
        }
        let ten = compress(QualityLevel::Q10, &data);
        let eleven = compress(QualityLevel::Q11, &data);
        assert!(
            eleven.len() <= ten.len(),
            "quality eleven produced {} bytes against quality ten's {}",
            eleven.len(),
            ten.len()
        );
    }
}
