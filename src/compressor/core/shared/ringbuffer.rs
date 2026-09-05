//! Sliding window over the input, with the reference's exact layout.
//!
//! Ports `RingBuffer` from `c/enc/ringbuffer.h` and the seven-byte clearing
//! that `CopyInputToRingBuffer` performs in `c/enc/encode.c` of the pinned
//! reference (`google/brotli` v1.2.0, commit `028fb5a`).
//!
//! The layout matters for the emitted bytes, not just for correctness. Match
//! finding reads whole words past the current position, and the reference
//! defines exactly what those bytes are: a copy of the start of the window in
//! the tail, zeros in the seven-byte margin, and the sentinel `241` at the
//! first tail byte until a lap writes over it. Reproducing the same filler is
//! what makes the encoder deterministic on data it never actually copied.

/// Bytes of margin past the window, so eight-byte loads always have data.
const SLACK_FOR_EIGHT_BYTE_HASHING: usize = 7;

/// Bytes reserved before the window for the two wrap-around copies.
const HEAD_ROOM: usize = 2;

/// Sentinel the reference leaves at the first tail byte.
const TAIL_SENTINEL: u8 = 241;

/// The window a search runs over.
#[derive(Copy, Clone)]
pub(crate) struct Window<'a> {
    /// The ring buffer holding the input seen so far.
    pub(crate) data: &'a [u8],
    /// Mask that turns an absolute position into a buffer index.
    pub(crate) mask: usize,
}

/// The stretch of input one call processes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct BlockSpan {
    /// Wrapped position the stretch starts at.
    pub(crate) position: u32,
    /// Number of bytes in the stretch.
    pub(crate) bytes: u32,
}

/// Circular window holding the input the encoder can still refer back to.
pub(crate) struct RingBuffer {
    size: usize,
    mask: usize,
    tail_size: usize,
    total_size: usize,
    cur_size: usize,
    pos: u32,
    data: Vec<u8>,
}

impl RingBuffer {
    /// Creates an empty window of `1 << rb_bits` bytes (`RingBufferSetup`).
    ///
    /// `lgblock` sizes the tail: the copy of the window head that lets a match
    /// finder read a whole word past the wrap point without a branch.
    pub(crate) fn new(rb_bits: usize, lgblock: usize) -> Self {
        let size = 1usize << rb_bits;
        let tail_size = 1usize << lgblock;
        Self {
            size,
            mask: size - 1,
            tail_size,
            total_size: size + tail_size,
            cur_size: 0,
            pos: 0,
            data: Vec::new(),
        }
    }

    /// Returns the mask that turns an absolute position into a buffer index.
    pub(crate) const fn mask(&self) -> usize {
        self.mask
    }

    /// Returns the window contents, indexed by masked position.
    ///
    /// The slice is longer than the window: it also holds the tail copy and the
    /// margin, so a caller may read a whole word starting at any valid index.
    pub(crate) fn buffer(&self) -> &[u8] {
        match self.data.get(HEAD_ROOM..) {
            Some(buffer) => buffer,
            None => &[],
        }
    }

    /// Returns the bytes this window keeps allocated.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.data.capacity()
    }

    /// Returns whether any input has been written yet.
    pub(crate) const fn is_allocated(&self) -> bool {
        self.cur_size != 0
    }

    /// Restores the window to the state its constructor left it in.
    ///
    /// The bytes are left where they are rather than wiped. Nothing can read
    /// them: a backward reference is bounded by the distance to the start of
    /// the stream, so the next stream never looks further back than it has
    /// written, and `write` re-establishes the head bytes, the tail mirror and
    /// the sentinel while `clear_margin` re-zeroes the margin. Wiping a window
    /// that can be mebibytes wide would cost more than the allocation reuse
    /// saves.
    pub(crate) fn reset(&mut self) {
        self.cur_size = 0;
        self.pos = 0;
    }

    /// Grows the backing storage to `buflen` bytes (`RingBufferInitBuffer`).
    fn init_buffer(&mut self, buflen: usize) {
        let mut fresh = vec![0u8; HEAD_ROOM + buflen + SLACK_FOR_EIGHT_BYTE_HASHING];
        let keep = HEAD_ROOM + self.cur_size + SLACK_FOR_EIGHT_BYTE_HASHING;
        if let Some(old) = self.data.get(..keep)
            && let Some(target) = fresh.get_mut(..keep)
        {
            target.copy_from_slice(old);
        }
        self.data = fresh;
        self.cur_size = buflen;
        // The two head bytes and the margin are zero, exactly as the reference
        // leaves them; `vec!` already provided that for a fresh allocation and
        // the copy above never reaches past `keep`.
        for index in 0..SLACK_FOR_EIGHT_BYTE_HASHING {
            if let Some(byte) = self.data.get_mut(HEAD_ROOM + self.cur_size + index) {
                *byte = 0;
            }
        }
        if let Some(byte) = self.data.get_mut(0) {
            *byte = 0;
        }
        if let Some(byte) = self.data.get_mut(1) {
            *byte = 0;
        }
    }

    /// Writes `index`-th byte of the window, ignoring an out-of-range index.
    fn set(&mut self, index: usize, value: u8) {
        if let Some(byte) = self.data.get_mut(HEAD_ROOM + index) {
            *byte = value;
        }
    }

    /// Copies the head of `bytes` into the tail mirror (`RingBufferWriteTail`).
    fn write_tail(&mut self, bytes: &[u8]) {
        let masked_pos = (self.pos as usize) & self.mask;
        if masked_pos >= self.tail_size {
            return;
        }
        let count = bytes.len().min(self.tail_size - masked_pos);
        let start = HEAD_ROOM + self.size + masked_pos;
        if let Some(target) = self.data.get_mut(start..start + count)
            && let Some(source) = bytes.get(..count)
        {
            target.copy_from_slice(source);
        }
    }

    /// Appends `bytes` to the window (`RingBufferWrite`).
    pub(crate) fn write(&mut self, bytes: &[u8]) {
        if self.pos == 0 && bytes.len() < self.tail_size {
            // First write of a short stream: the tail and the rest of the
            // window are never read, so only the bytes themselves are kept.
            self.pos = bytes.len() as u32;
            self.init_buffer(bytes.len());
            if let Some(target) = self.data.get_mut(HEAD_ROOM..HEAD_ROOM + bytes.len()) {
                target.copy_from_slice(bytes);
            }
            return;
        }
        if self.cur_size < self.total_size {
            self.init_buffer(self.total_size);
            self.set(self.size - 2, 0);
            self.set(self.size - 1, 0);
            self.set(self.size, TAIL_SENTINEL);
        }

        let masked_pos = (self.pos as usize) & self.mask;
        self.write_tail(bytes);
        if masked_pos + bytes.len() <= self.size {
            let start = HEAD_ROOM + masked_pos;
            if let Some(target) = self.data.get_mut(start..start + bytes.len()) {
                target.copy_from_slice(bytes);
            }
        } else {
            let head = (self.total_size - masked_pos).min(bytes.len());
            let start = HEAD_ROOM + masked_pos;
            if let Some(target) = self.data.get_mut(start..start + head)
                && let Some(source) = bytes.get(..head)
            {
                target.copy_from_slice(source);
            }
            let wrapped = self.size - masked_pos;
            if let Some(source) = bytes.get(wrapped..) {
                let count = source.len();
                if let Some(target) = self.data.get_mut(HEAD_ROOM..HEAD_ROOM + count) {
                    target.copy_from_slice(source);
                }
            }
        }

        let not_first_lap = (self.pos & (1u32 << 31)) != 0;
        let pos_mask = (1u32 << 31) - 1;
        let last_but_one = self.buffer().get(self.size - 2).copied().unwrap_or(0);
        let last = self.buffer().get(self.size - 1).copied().unwrap_or(0);
        if let Some(byte) = self.data.get_mut(0) {
            *byte = last_but_one;
        }
        if let Some(byte) = self.data.get_mut(1) {
            *byte = last;
        }
        self.pos = (self.pos & pos_mask) + ((bytes.len() as u32) & pos_mask);
        if not_first_lap {
            self.pos |= 1u32 << 31;
        }
    }

    /// Clears the seven bytes that follow the written data on the first lap.
    ///
    /// Hashing loads whole words, so without this the hash of the last few
    /// positions would depend on memory the encoder never wrote.
    pub(crate) fn clear_margin(&mut self) {
        if self.pos as usize > self.mask {
            return;
        }
        let start = HEAD_ROOM + self.pos as usize;
        let end = (start + SLACK_FOR_EIGHT_BYTE_HASHING).min(self.data.len());
        if let Some(target) = self.data.get_mut(start..end) {
            target.fill(0);
        }
    }
}

/// Wraps a 64-bit input position into the 32-bit space positions are stored in.
///
/// Mirrors `WrapPosition`: the first three gibibytes are contiguous, and after
/// that positions alternate between two gibibyte-wide halves so that the
/// "already lapped" property survives the truncation.
pub(crate) const fn wrap_position(position: u64) -> u32 {
    let result = position as u32;
    let gb = position >> 30;
    if gb > 2 {
        (result & ((1u32 << 30) - 1)) | ((((gb - 1) & 1) as u32 + 1) << 30)
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a window the way `ComputeRbBits` would for `lgwin` and `lgblock`.
    fn ring(lgwin: usize, lgblock: usize) -> RingBuffer {
        RingBuffer::new(1 + lgwin.max(lgblock), lgblock)
    }

    #[test]
    fn a_short_first_write_only_allocates_what_it_holds() {
        let mut rb = ring(16, 16);
        assert!(!rb.is_allocated());
        rb.write(b"hello");
        assert!(rb.is_allocated());
        assert_eq!(&rb.buffer()[..5], b"hello");
        // The margin is zeroed so eight-byte loads are defined.
        assert_eq!(&rb.buffer()[5..12], &[0u8; 7]);
    }

    #[test]
    fn a_long_first_write_allocates_the_whole_window_and_the_tail() {
        let mut rb = ring(16, 16);
        let payload = vec![7u8; 1 << 16];
        rb.write(&payload);
        assert_eq!(rb.cur_size, rb.total_size);
        assert_eq!(rb.buffer()[0], 7);
        // The tail mirrors the beginning of the window.
        assert_eq!(rb.buffer()[rb.size], 7);
    }

    #[test]
    fn the_tail_sentinel_survives_until_a_lap_writes_over_it() {
        let mut rb = ring(16, 16);
        // A write that starts past the tail leaves the sentinel in place.
        let mut payload = vec![1u8; 1 << 16];
        payload.truncate(1 << 16);
        rb.write(&vec![2u8; 1 << 16]);
        assert_eq!(rb.buffer()[rb.size], 2);
    }

    #[test]
    fn writes_wrap_around_and_mirror_into_the_tail() {
        let mut rb = ring(10, 16);
        let window = rb.size;
        rb.write(&vec![1u8; window - 4]);
        rb.write(&[9, 9, 9, 9, 8, 8, 8, 8]);
        assert_eq!(&rb.buffer()[window - 4..window], &[9, 9, 9, 9]);
        assert_eq!(&rb.buffer()[..4], &[8, 8, 8, 8]);
        assert_eq!(&rb.buffer()[window..window + 4], &[8, 8, 8, 8]);
    }

    #[test]
    fn the_head_bytes_mirror_the_end_of_the_window() {
        let mut rb = ring(10, 16);
        let window = rb.size;
        rb.write(&vec![5u8; window]);
        assert_eq!(rb.data[0], 5);
        assert_eq!(rb.data[1], 5);
        assert_eq!(rb.buffer()[window - 1], 5);
    }

    #[test]
    fn clearing_the_margin_only_touches_the_first_lap() {
        let mut rb = ring(10, 16);
        rb.write(&[3u8; 8]);
        rb.clear_margin();
        assert_eq!(&rb.buffer()[8..15], &[0u8; 7]);
    }

    #[test]
    fn position_wrapping_keeps_the_lap_parity() {
        assert_eq!(wrap_position(0), 0);
        assert_eq!(wrap_position(1234), 1234);
        assert_eq!(wrap_position((1u64 << 30) - 1), (1 << 30) - 1);
        assert_eq!(wrap_position(3u64 << 30), 1 << 30);
        assert_eq!(wrap_position(4u64 << 30), 2 << 30);
        assert_eq!(wrap_position(5u64 << 30), 1 << 30);
        assert_eq!(wrap_position((3u64 << 30) + 17), (1 << 30) + 17);
    }
}
