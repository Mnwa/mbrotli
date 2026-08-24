//! Least-significant-bit-first bit writer used by the fast encoders.
//!
//! The layout matches `BrotliWriteBits` in `c/enc/write_bits.h` of the pinned
//! reference: bits are written into increasing byte addresses and, within a
//! byte, from the least significant bit upwards. Every write also clears the
//! bits above the new position in the byte it lands in, so the writer never
//! needs a separate "prepare storage" step after the first byte.

/// Largest number of bits a single [`BitWriter::write`] call accepts.
pub(crate) const MAX_BITS_PER_WRITE: u32 = 56;

/// Bytes a write touches, and therefore the headroom the buffer must keep.
pub(crate) const WRITE_SLACK: usize = 8;

/// Cursor into a caller-owned byte buffer that appends individual bits.
pub(crate) struct BitWriter<'a> {
    storage: &'a mut [u8],
    position: usize,
    overflowed: bool,
}

impl<'a> BitWriter<'a> {
    /// Creates a writer that resumes at bit `position` of `storage`.
    ///
    /// The caller owns the invariant that `storage[position >> 3]` already
    /// holds the partially filled byte, exactly as the reference encoder seeds
    /// the first two bytes with the stream header.
    pub(crate) const fn new(storage: &'a mut [u8], position: usize) -> Self {
        Self {
            storage,
            position,
            overflowed: false,
        }
    }

    /// Returns the current bit position.
    pub(crate) const fn position(&self) -> usize {
        self.position
    }

    /// Returns `true` when a write was refused because the buffer was full.
    ///
    /// A writer that has overflowed keeps its position advancing so callers can
    /// still finish their control flow, but the produced bytes are meaningless
    /// and the encoder turns this into an error.
    pub(crate) const fn overflowed(&self) -> bool {
        self.overflowed
    }

    /// Returns the byte at `index`, or zero when it lies past the buffer.
    pub(crate) fn byte(&self, index: usize) -> u8 {
        match self.storage.get(index) {
            Some(&byte) => byte,
            None => 0,
        }
    }

    /// Overwrites the byte at `index`, ignoring an index past the buffer.
    ///
    /// The encoder uses this to restore the partial byte it carried into a
    /// meta-block after deciding to store that meta-block uncompressed.
    pub(crate) fn set_byte(&mut self, index: usize, value: u8) {
        match self.storage.get_mut(index) {
            Some(byte) => *byte = value,
            None => self.overflowed = true,
        }
    }

    /// Appends the `n_bits` low bits of `bits`.
    ///
    /// Like the reference writer this materialises a whole machine word, which
    /// clears the seven bytes that follow the new position. Callers therefore
    /// have to provide [`WRITE_SLACK`] bytes of headroom past the largest
    /// position they will reach.
    ///
    /// # Panics
    ///
    /// Never panics; a write past the end of the buffer sets the overflow flag
    /// instead.
    #[inline(always)]
    pub(crate) fn write(&mut self, n_bits: u32, bits: u64) {
        debug_assert!(n_bits <= MAX_BITS_PER_WRITE);
        debug_assert!(n_bits == 64 || (bits >> n_bits) == 0);
        let start = self.position >> 3;
        let Some(window) = self.storage.get_mut(start..start + WRITE_SLACK) else {
            self.overflowed = true;
            self.position += n_bits as usize;
            return;
        };
        let value = u64::from(window[0]) | (bits << (self.position & 7));
        window.copy_from_slice(&value.to_le_bytes());
        self.position += n_bits as usize;
    }

    /// Overwrites `n_bits` bits that were already emitted at `position`.
    ///
    /// Mirrors `UpdateBits`; quality 0 uses it to widen the `MLEN` field of a
    /// meta-block it decided to extend.
    pub(crate) fn update(&mut self, mut n_bits: u32, mut bits: u32, mut position: usize) {
        while n_bits > 0 {
            let byte_position = position >> 3;
            let unchanged = (position & 7) as u32;
            let changed = n_bits.min(8 - unchanged);
            let total = unchanged + changed;
            let Some(byte) = self.storage.get_mut(byte_position) else {
                self.overflowed = true;
                return;
            };
            let mask = !((1u32 << total) - 1) | ((1u32 << unchanged) - 1);
            let kept = u32::from(*byte) & mask;
            let fresh = bits & ((1u32 << changed) - 1);
            *byte = ((fresh << unchanged) | kept) as u8;
            n_bits -= changed;
            bits >>= changed;
            position += changed as usize;
        }
    }

    /// Drops everything written after bit `position` and resumes there.
    pub(crate) fn rewind(&mut self, position: usize) {
        let mask = (1u16 << (position & 7)) - 1;
        if let Some(byte) = self.storage.get_mut(position >> 3) {
            *byte &= mask as u8;
        } else {
            self.overflowed = true;
        }
        self.position = position;
    }

    /// Advances to the next byte boundary, leaving the skipped bits zero.
    pub(crate) const fn align(&mut self) {
        self.position = (self.position + 7) & !7;
    }

    /// Advances to the next byte boundary and clears the byte landed on.
    ///
    /// Mirrors `JumpToByteBoundary`: the caller reads that byte back as the
    /// partial byte carried into the next meta-block, so in a reused buffer it
    /// has to be cleared rather than left holding an older stream's bits.
    pub(crate) fn jump_to_byte_boundary(&mut self) {
        self.align();
        self.prepare_storage();
    }

    /// Clears the byte at the current, byte-aligned position.
    ///
    /// Mirrors `BrotliWriteBitsPrepareStorage`.
    pub(crate) fn prepare_storage(&mut self) {
        debug_assert_eq!(self.position & 7, 0);
        match self.storage.get_mut(self.position >> 3) {
            Some(byte) => *byte = 0,
            None => self.overflowed = true,
        }
    }

    /// Copies `data` verbatim at the current, byte-aligned position.
    pub(crate) fn write_bytes(&mut self, data: &[u8]) {
        debug_assert_eq!(self.position & 7, 0);
        let start = self.position >> 3;
        let Some(window) = self.storage.get_mut(start..start + data.len()) else {
            self.overflowed = true;
            self.position += data.len() << 3;
            return;
        };
        window.copy_from_slice(data);
        self.position += data.len() << 3;
        match self.storage.get_mut(self.position >> 3) {
            Some(byte) => *byte = 0,
            None => self.overflowed = true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writer_output(bits: &[(u32, u64)]) -> (Vec<u8>, usize) {
        let mut storage = vec![0u8; 64];
        let mut writer = BitWriter::new(&mut storage, 0);
        for &(n, value) in bits {
            writer.write(n, value);
        }
        let position = writer.position();
        assert!(!writer.overflowed());
        (storage, position)
    }

    #[test]
    fn writes_bits_least_significant_first() {
        let (storage, position) = writer_output(&[(1, 1), (2, 2), (5, 0b10101)]);
        assert_eq!(position, 8);
        assert_eq!(storage[0], 0b1010_1101);
    }

    #[test]
    fn writes_across_a_byte_boundary() {
        let (storage, position) = writer_output(&[(4, 0xF), (8, 0xA5)]);
        assert_eq!(position, 12);
        assert_eq!(storage[0], 0x5F);
        assert_eq!(storage[1], 0x0A);
    }

    #[test]
    fn writes_the_widest_supported_field() {
        let value = (1u64 << 56) - 1;
        let (storage, position) = writer_output(&[(3, 5), (56, value)]);
        assert_eq!(position, 59);
        assert_eq!(storage[0], 0b1111_1101);
        assert_eq!(&storage[1..7], &[0xFF; 6]);
        assert_eq!(storage[7], 0b0000_0111);
    }

    #[test]
    fn zero_width_writes_leave_the_stream_untouched() {
        let (storage, position) = writer_output(&[(3, 5), (0, 0), (3, 5)]);
        assert_eq!(position, 6);
        assert_eq!(storage[0], 0b0010_1101);
    }

    #[test]
    fn clears_the_bytes_that_follow_the_write() {
        let mut storage = vec![0xFFu8; 16];
        storage[0] = 0;
        let mut writer = BitWriter::new(&mut storage, 0);
        writer.write(4, 0);
        assert_eq!(&storage[..8], &[0u8; 8]);
        assert_eq!(&storage[8..], &[0xFFu8; 8]);
    }

    #[test]
    fn update_rewrites_an_already_emitted_field() {
        let mut storage = vec![0u8; 16];
        let mut writer = BitWriter::new(&mut storage, 0);
        writer.write(3, 0b101);
        let patched = writer.position();
        writer.write(20, 0);
        writer.write(4, 0xF);
        writer.update(20, 0x0F_F0F, patched);
        assert_eq!(writer.position(), 27);
        let value = u32::from_le_bytes([storage[0], storage[1], storage[2], storage[3]]);
        assert_eq!(value & 0b111, 0b101);
        assert_eq!((value >> 3) & 0xF_FFFF, 0x0F_F0F);
        assert_eq!((value >> 23) & 0xF, 0xF);
    }

    #[test]
    fn rewind_discards_bits_written_after_the_mark() {
        let mut storage = vec![0u8; 16];
        let mut writer = BitWriter::new(&mut storage, 0);
        writer.write(5, 0b10101);
        let mark = writer.position();
        writer.write(20, 0xF_FFFF);
        writer.rewind(mark);
        assert_eq!(writer.position(), 5);
        assert_eq!(storage[0], 0b0001_0101);
    }

    #[test]
    fn align_moves_to_the_next_byte_boundary() {
        let mut storage = vec![0u8; 8];
        let mut writer = BitWriter::new(&mut storage, 0);
        writer.write(3, 0b111);
        writer.align();
        assert_eq!(writer.position(), 8);
        writer.align();
        assert_eq!(writer.position(), 8);
    }

    #[test]
    fn jumping_to_a_byte_boundary_clears_the_byte_landed_on() {
        let mut storage = vec![0xFFu8; 16];
        storage[0] = 0;
        let mut writer = BitWriter::new(&mut storage, 0);
        writer.write(3, 0b101);
        writer.jump_to_byte_boundary();
        assert_eq!(writer.position(), 8);
        assert_eq!(storage[0], 0b101);
        assert_eq!(storage[1], 0);
    }

    #[test]
    fn preparing_storage_clears_only_the_current_byte() {
        let mut storage = vec![0xFFu8; 4];
        let mut writer = BitWriter::new(&mut storage, 16);
        writer.prepare_storage();
        assert_eq!(storage, vec![0xFF, 0xFF, 0x00, 0xFF]);
    }

    #[test]
    fn preparing_storage_past_the_buffer_reports_an_overflow() {
        let mut storage = vec![0u8; 1];
        let mut writer = BitWriter::new(&mut storage, 64);
        writer.prepare_storage();
        assert!(writer.overflowed());
    }

    #[test]
    fn write_bytes_copies_verbatim_and_clears_the_next_byte() {
        let mut storage = vec![0xFFu8; 16];
        storage[0] = 0;
        let mut writer = BitWriter::new(&mut storage, 0);
        writer.write(8, 0x41);
        writer.write_bytes(&[1, 2, 3]);
        let position = writer.position();
        assert_eq!(&storage[..5], &[0x41, 1, 2, 3, 0]);
        assert_eq!(position, 32);
    }

    #[test]
    fn reports_overflow_instead_of_panicking() {
        let mut storage = vec![0u8; WRITE_SLACK + 1];
        let mut writer = BitWriter::new(&mut storage, 0);
        writer.write(8, 0xFF);
        assert!(!writer.overflowed());
        writer.write(8, 0xFF);
        assert!(!writer.overflowed());
        writer.write(8, 0xFF);
        assert!(writer.overflowed());
    }

    #[test]
    fn overflow_is_reported_for_rewind_update_and_byte_copies() {
        let mut storage = vec![0u8; 1];
        let mut writer = BitWriter::new(&mut storage, 0);
        writer.write_bytes(&[1, 2, 3]);
        assert!(writer.overflowed());

        let mut storage = vec![0u8; 1];
        let mut writer = BitWriter::new(&mut storage, 0);
        writer.update(8, 0xFF, 64);
        assert!(writer.overflowed());

        let mut storage = vec![0u8; 1];
        let mut writer = BitWriter::new(&mut storage, 0);
        writer.rewind(64);
        assert!(writer.overflowed());
    }

    #[test]
    fn set_byte_overwrites_and_reports_an_out_of_range_index() {
        let mut storage = vec![0u8; 2];
        let mut writer = BitWriter::new(&mut storage, 0);
        writer.set_byte(1, 0xAB);
        assert!(!writer.overflowed());
        writer.set_byte(9, 0xCD);
        assert!(writer.overflowed());
        assert_eq!(storage[1], 0xAB);
    }

    #[test]
    fn byte_reads_past_the_buffer_as_zero() {
        let mut storage = vec![7u8; 1];
        let writer = BitWriter::new(&mut storage, 0);
        assert_eq!(writer.byte(0), 7);
        assert_eq!(writer.byte(9), 0);
    }
}
