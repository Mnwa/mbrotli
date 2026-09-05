//! In-bounds tag filtering, preserving the bucket's newest-to-oldest order.

use fearless_simd::{Level, Simd, SimdBase, SimdMask, u8x16, u8x32};

/// One bit per equal byte, least significant bit for the first slot.
#[inline(always)]
fn matching_mask<S: Simd>(simd: S, bytes: &[u8; 16], tag: u8) -> u16 {
    u8x16::load_array_ref(simd, bytes)
        .simd_eq(u8x16::splat(simd, tag))
        .to_bitmask() as u16
}

/// Visits initialized circular slots in precisely the unfiltered scan order.
pub(super) struct Candidates {
    upper: usize,
    lower: usize,
    group: usize,
    matches: u32,
    slot_mask: usize,
}

impl Candidates {
    /// `count` retains the reference's wrapping u16 counter semantics.
    #[inline(always)]
    pub(super) fn new<S: Simd>(simd: S, count: u16, capacity: usize, tags: &[u8], tag: u8) -> Self {
        let upper = count as usize;
        let mut result = Self {
            upper,
            lower: upper.saturating_sub(capacity),
            group: 0,
            matches: 0,
            slot_mask: capacity - 1,
        };
        if !matches!(simd.level(), Level::Fallback(_)) && !tags.is_empty() {
            let mask = match capacity {
                16 => tags.first_chunk::<16>().map(|bytes| {
                    u32::from(matching_mask(simd, bytes, tag).rotate_right(count as u32 & 15)) << 16
                }),
                32 => tags.first_chunk::<32>().map(|bytes| {
                    (u8x32::load_array_ref(simd, bytes)
                        .simd_eq(u8x32::splat(simd, tag))
                        .to_bitmask() as u32)
                        .rotate_right(count as u32 & 31)
                }),
                _ => None,
            };
            if let Some(mask) = mask {
                // Rotate newest to the highest bit, then discard unwritten slots.
                // One vector comparison covers a complete q5/q6 bucket.
                let active = u32::MAX
                    .checked_shl(32 - upper.min(capacity) as u32)
                    .unwrap_or(0);
                result.matches = mask & active;
                result.group = upper;
                result.upper = result.lower;
            }
        }
        result
    }

    /// Returns a physical slot; empty tags select the unfiltered scan.
    #[inline(always)]
    pub(super) fn next<S: Simd>(&mut self, simd: S, tags: &[u8], tag: u8) -> Option<usize> {
        // Keep the original unfiltered scalar scan as an independent oracle.
        if tags.is_empty() || matches!(simd.level(), Level::Fallback(_)) {
            if self.upper == self.lower {
                return None;
            }
            self.upper -= 1;
            return Some(self.upper & self.slot_mask);
        }
        loop {
            if self.matches != 0 {
                let lane = 31 - self.matches.leading_zeros() as usize;
                self.matches &= !(1 << lane);
                return Some((self.group + lane) & self.slot_mask);
            }
            if self.upper == self.lower {
                return None;
            }
            let newest = (self.upper - 1) & self.slot_mask;
            self.group = newest & !15;
            let end = (newest & 15) + 1;
            let count = end.min(self.upper - self.lower);
            self.upper -= count;
            // Every group lies inside an initialized compact bucket. Taking
            // an array reference keeps vector loads bounded, even at its end.
            let bytes = tags[self.group..self.group + 16].first_chunk::<16>()?;
            let active = ((1u32 << end) - 1) ^ ((1u32 << (end - count)) - 1);
            self.matches = u32::from(matching_mask(simd, bytes, tag)) & active;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compressor::Backend;
    use fearless_simd::dispatch;

    #[test]
    fn masks_match_scalar_equality_for_every_byte_and_misalignment() {
        let bytes: Vec<u8> = (0..80).map(|i| (i * 17) as u8).collect();
        for backend in Backend::available() {
            for offset in 0..64 {
                let group = bytes[offset..].first_chunk::<16>().expect("group");
                for tag in 0..=255 {
                    let expected = group.iter().enumerate().fold(0u16, |mask, (i, &value)| {
                        mask | (u16::from(value == tag) << i)
                    });
                    assert_eq!(
                        dispatch!(backend.0, simd => matching_mask(simd, group, tag)),
                        expected
                    );
                }
            }
        }
    }

    #[test]
    fn circular_filter_preserves_order_at_partial_full_and_counter_wrap_boundaries() {
        for capacity in [16, 32, 64, 128, 256] {
            let tags: Vec<u8> = (0..capacity).map(|i| (i % 7) as u8).collect();
            for count in [0, 1, 15, 16, 17, 31, 32, 63, 64, 65, 255, 256, 257, 65535] {
                for backend in Backend::available() {
                    for tag in 0..=7 {
                        let expected: Vec<_> = ((count as usize).saturating_sub(capacity)
                            ..count as usize)
                            .rev()
                            .map(|index| index & (capacity - 1))
                            .filter(|&slot| backend == Backend::SCALAR || tags[slot] == tag)
                            .collect();
                        let mut candidates = dispatch!(backend.0, simd => Candidates::new(simd, count, capacity, &tags, tag));
                        let mut actual = Vec::new();
                        while let Some(slot) =
                            dispatch!(backend.0, simd => candidates.next(simd, &tags, tag))
                        {
                            actual.push(slot);
                        }
                        assert_eq!(
                            actual, expected,
                            "{backend}, capacity {capacity}, count {count}, tag {tag}"
                        );
                    }
                }
            }
        }
    }
}
