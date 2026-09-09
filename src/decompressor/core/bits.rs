//! Demand-driven bit input over a 64-bit reservoir.
//!
//! The slow path accepts one byte at a time and never accepts a byte beyond
//! the field that needs it, so limits and progress stay byte-exact. The fast
//! path loads whole words while at least eight acceptable bytes remain; whole
//! bytes still buffered when a call returns are handed back with [`Bits::unread`],
//! which restores the byte-exact accounting the slow path guarantees.

use super::super::{DecodeError, InvalidDataKind};

/// Widest field either path reads in one step.
pub(super) const MAX_PEEK: u32 = 56;

#[derive(Debug, Default, Clone, Copy)]
pub(super) struct Bits {
    value: u64,
    count: u32,
}

pub(crate) struct Input<'a> {
    pub(crate) bytes: &'a [u8],
    pub(crate) consumed: usize,
    pub(crate) total_before: u64,
    pub(crate) limit: Option<u64>,
}

impl Input<'_> {
    /// End of the prefix this call may accept without per-byte limit checks.
    pub(super) fn fast_end(&self) -> usize {
        let budget = self
            .limit
            .map_or(u64::MAX, |limit| limit.saturating_sub(self.total_before))
            .min(u64::MAX - self.total_before);
        usize::try_from(budget).map_or(self.bytes.len(), |budget| budget.min(self.bytes.len()))
    }
}

#[inline(always)]
pub(super) const fn mask(count: u32) -> u64 {
    if count == 0 {
        0
    } else {
        u64::MAX >> (64 - count)
    }
}

impl Bits {
    /// Buffered bits.
    #[inline(always)]
    pub(super) const fn count(&self) -> u32 {
        self.count
    }

    /// Buffered bits, least significant first; bits above `count` are zero.
    #[inline(always)]
    pub(super) const fn value(&self) -> u64 {
        self.value
    }

    /// Accepts one byte, or reports exhausted input.
    pub(super) fn load_byte(&mut self, input: &mut Input<'_>) -> Result<bool, DecodeError> {
        let Some(&byte) = input.bytes.get(input.consumed) else {
            return Ok(false);
        };
        let next = input
            .total_before
            .checked_add(input.consumed as u64)
            .and_then(|value| value.checked_add(1))
            .ok_or(DecodeError::SizeOverflow)?;
        if let Some(limit) = input.limit
            && next > limit
        {
            return Err(DecodeError::InputLimitExceeded { limit });
        }
        self.value |= u64::from(byte) << self.count;
        self.count += 8;
        input.consumed += 1;
        Ok(true)
    }

    pub(super) fn peek(
        &mut self,
        count: u32,
        input: &mut Input<'_>,
    ) -> Result<Option<u64>, DecodeError> {
        debug_assert!(count <= MAX_PEEK);
        while self.count < count {
            if !self.load_byte(input)? {
                return Ok(None);
            }
        }
        Ok(Some(self.value & mask(count)))
    }

    /// Loads whole bytes until at least 57 bits are buffered. Returns false,
    /// leaving the reservoir unchanged, when fewer than eight acceptable bytes
    /// remain before `fast_end`.
    #[inline(always)]
    pub(super) fn refill(&mut self, input: &mut Input<'_>, fast_end: usize) -> bool {
        if self.count > MAX_PEEK {
            return true;
        }
        let end = input.consumed + 8;
        if end > fast_end {
            return false;
        }
        let Some(chunk) = input
            .bytes
            .get(input.consumed..end)
            .and_then(|chunk| chunk.first_chunk::<8>())
        else {
            return false;
        };
        let bytes = (64 - self.count) >> 3;
        let keep = bytes * 8;
        let word = u64::from_le_bytes(*chunk) & (u64::MAX >> (64 - keep));
        self.value |= word << self.count;
        self.count += keep;
        input.consumed += bytes as usize;
        true
    }

    /// Returns every whole buffered byte to the input, keeping fewer than
    /// eight bits. Only bytes accepted during the current call can be whole.
    pub(super) fn unread(&mut self, input: &mut Input<'_>) {
        let whole = self.count / 8;
        input.consumed -= whole as usize;
        self.count -= whole * 8;
        self.value &= mask(self.count);
    }

    #[inline(always)]
    pub(super) const fn drop(&mut self, count: u32) {
        self.value >>= count;
        self.count -= count;
    }

    /// Reads `count` already buffered bits.
    #[inline(always)]
    pub(super) const fn take(&mut self, count: u32) -> u64 {
        debug_assert!(count <= self.count);
        let value = self.value & mask(count);
        self.drop(count);
        value
    }

    pub(super) fn read(
        &mut self,
        count: u32,
        input: &mut Input<'_>,
    ) -> Result<Option<u64>, DecodeError> {
        let value = self.peek(count, input)?;
        if value.is_some() {
            self.drop(count);
        }
        Ok(value)
    }

    pub(super) fn align(&mut self) -> Result<(), DecodeError> {
        let padding = self.count % 8;
        if self.value & mask(padding) != 0 {
            return Err(InvalidDataKind::Padding.into());
        }
        self.drop(padding);
        Ok(())
    }

    pub(super) fn uint8(&mut self, input: &mut Input<'_>) -> Result<Option<usize>, DecodeError> {
        let Some(first) = self.peek(1, input)? else {
            return Ok(None);
        };
        if first == 0 {
            self.drop(1);
            return Ok(Some(0));
        }
        let Some(head) = self.peek(4, input)? else {
            return Ok(None);
        };
        let n = (head >> 1) as u32;
        if n == 0 {
            self.drop(4);
            return Ok(Some(1));
        }
        let Some(all) = self.peek(4 + n, input)? else {
            return Ok(None);
        };
        self.drop(4 + n);
        Ok(Some((1usize << n) + (all >> 4) as usize))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn probe(bytes: &[u8]) -> Input<'_> {
        Input {
            bytes,
            consumed: 0,
            total_before: 0,
            limit: None,
        }
    }
    #[test]
    fn cumulative_input_overflow_is_reported_before_accepting_a_byte() {
        let mut input = Input {
            bytes: &[0],
            consumed: 0,
            total_before: u64::MAX,
            limit: None,
        };
        assert!(matches!(
            Bits::default().read(1, &mut input),
            Err(DecodeError::SizeOverflow)
        ));
        assert_eq!(input.consumed, 0);
        assert_eq!(input.fast_end(), 0);
    }
    #[test]
    fn partial_fields_retain_bytes_but_never_read_past_the_requested_field() {
        let mut bits = Bits::default();
        let mut first = probe(&[0x01]);
        assert_eq!(bits.read(12, &mut first).unwrap(), None);
        let mut second = Input {
            bytes: &[0x02, 0xaa],
            consumed: 0,
            total_before: 1,
            limit: None,
        };
        assert_eq!(bits.read(12, &mut second).unwrap(), Some(0x201));
        assert_eq!(second.consumed, 1);
        bits.align().unwrap();
        assert_eq!(bits.read(8, &mut second).unwrap(), Some(0xaa));
    }
    #[test]
    fn refill_loads_whole_words_and_unread_returns_unused_bytes() {
        let bytes: alloc::vec::Vec<u8> = (1..=20).collect();
        let mut bits = Bits::default();
        let mut input = probe(&bytes);
        assert!(bits.read(3, &mut input).unwrap().is_some());
        assert!(bits.refill(&mut input, bytes.len()));
        assert_eq!((bits.count(), input.consumed), (61, 8));
        assert!(bits.refill(&mut input, bytes.len()));
        assert_eq!(bits.count(), 61);
        assert_eq!(bits.take(13), (0x03_02_01u64 >> 3) & mask(13));
        bits.unread(&mut input);
        assert_eq!(bits.count(), 0);
        assert_eq!(input.consumed, 2);
        assert_eq!(bits.read(8, &mut input).unwrap(), Some(3));
        // Fewer than eight acceptable bytes: the reservoir is left alone.
        assert!(!bits.refill(&mut input, 9));
        assert_eq!(bits.count(), 0);
        // A slice shorter than the fast end cannot supply a whole word either.
        input.consumed = bytes.len() - 3;
        assert!(!bits.refill(&mut input, bytes.len() + 5));
        assert_eq!(bits.count(), 0);
    }
    #[test]
    fn fast_end_respects_limits_and_slice_length() {
        let bytes = [0u8; 10];
        let mut probe = probe(&bytes);
        assert_eq!(probe.fast_end(), 10);
        probe.limit = Some(7);
        assert_eq!(probe.fast_end(), 7);
        probe.total_before = 9;
        assert_eq!(probe.fast_end(), 0);
        probe.limit = None;
        probe.total_before = u64::MAX - 3;
        assert_eq!(probe.fast_end(), 3);
    }
    #[test]
    fn full_reservoir_refill_and_uint8_decode() {
        let bytes = [0xff; 16];
        let mut bits = Bits::default();
        let mut input = probe(&bytes);
        assert!(bits.refill(&mut input, 16));
        assert_eq!((bits.count(), input.consumed), (64, 8));
        bits.drop(64 - 7);
        assert!(bits.refill(&mut input, 16));
        assert_eq!((bits.count(), input.consumed), (63, 15));
        assert_eq!(
            Bits::default().uint8(&mut probe(&[0b0001_0011])).unwrap(),
            Some(3)
        );
        assert_eq!(
            Bits::default().uint8(&mut probe(&[0b0000_0001])).unwrap(),
            Some(1)
        );
        assert_eq!(Bits::default().uint8(&mut probe(&[0])).unwrap(), Some(0));
    }
}
