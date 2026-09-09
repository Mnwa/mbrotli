//! Wire distance arithmetic, independent of host address size and history.

use super::super::{DecodeError, InvalidDataKind};

const SHORT_CODES: usize = 16;
const MAX_DISTANCE: u128 = (1u128 << 63) - 4;

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct DistanceLayout {
    postfix: u8,
    direct: usize,
    large: bool,
}

impl DistanceLayout {
    pub(super) const fn from_header(header: u8, large: bool) -> Self {
        let postfix = header & 3;
        Self {
            postfix,
            direct: ((header >> 2) as usize) << postfix,
            large,
        }
    }

    pub(super) const fn alphabet(self) -> usize {
        SHORT_CODES + self.direct + ((if self.large { 124 } else { 48 }) << self.postfix)
    }

    pub(super) const fn extra_bits(self, symbol: usize) -> u8 {
        if symbol < SHORT_CODES + self.direct {
            0
        } else {
            (1 + ((symbol - SHORT_CODES - self.direct) >> (self.postfix + 1))) as u8
        }
    }

    /// RFC 9841 constrains reachable symbols by their maximum possible value,
    /// even when the stream happens to select smaller extra bits.
    pub(super) fn validate_symbol(self, symbol: usize) -> Result<(), DecodeError> {
        if symbol >= self.alphabet() {
            return Err(InvalidDataKind::Distance.into());
        }
        if symbol >= SHORT_CODES
            && self.long_distance(symbol, (1u64 << self.extra_bits(symbol)) - 1) > MAX_DISTANCE
        {
            return Err(InvalidDataKind::Distance.into());
        }
        Ok(())
    }

    const fn long_distance(self, symbol: usize, extra: u64) -> u128 {
        if symbol < SHORT_CODES + self.direct {
            return (symbol - SHORT_CODES + 1) as u128;
        }
        let code = symbol - SHORT_CODES - self.direct;
        let high = code >> self.postfix;
        let low = code & ((1 << self.postfix) - 1);
        let offset = (((2 + (high & 1)) as u128) << self.extra_bits(symbol)) - 4;
        ((offset + extra as u128) << self.postfix) + low as u128 + self.direct as u128 + 1
    }

    pub(super) fn resolve(
        self,
        symbol: usize,
        extra: u64,
        cache: &[u64; 4],
    ) -> Result<u64, DecodeError> {
        if symbol >= SHORT_CODES {
            return u64::try_from(self.long_distance(symbol, extra))
                .map_err(|_| InvalidDataKind::Distance.into());
        }
        const INDEX: [usize; 16] = [0, 1, 2, 3, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1];
        const OFFSET: [i8; 16] = [0, 0, 0, 0, -1, 1, -2, 2, -3, 3, -1, 1, -2, 2, -3, 3];
        cache[INDEX[symbol]]
            .checked_add_signed(i64::from(OFFSET[symbol]))
            .filter(|&v| v != 0 && u128::from(v) <= MAX_DISTANCE)
            .ok_or_else(|| InvalidDataKind::Distance.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_layout_agrees_with_an_independent_wide_integer_model() {
        for header in 0..64 {
            let layout = DistanceLayout::from_header(header, true);
            let postfix = u32::from(header & 3);
            let direct = u128::from(header >> 2) << postfix;
            for symbol in 16..layout.alphabet() {
                let width = layout.extra_bits(symbol);
                for extra in [0, (1u64 << width) - 1] {
                    let expected = if (symbol as u128) < 16 + direct {
                        symbol as u128 - 15
                    } else {
                        let code = symbol as u128 - 16 - direct;
                        let bucket = code / (1 << postfix);
                        let low = code % (1 << postfix);
                        let n = 1 + bucket / 2;
                        ((2 + bucket % 2) * (1 << n) - 4 + u128::from(extra)) * (1 << postfix)
                            + low
                            + direct
                            + 1
                    };
                    assert_eq!(layout.long_distance(symbol, extra), expected);
                }
                let maximum = layout.long_distance(symbol, (1u64 << width) - 1);
                assert_eq!(
                    layout.validate_symbol(symbol).is_ok(),
                    maximum <= MAX_DISTANCE
                );
            }
            assert!(layout.validate_symbol(layout.alphabet()).is_err());
        }
    }
    #[test]
    fn cache_offsets_cannot_make_zero_or_overflowed_distances() {
        let layout = DistanceLayout::default();
        for symbol in [4, 6, 8, 10, 12, 14] {
            assert!(layout.resolve(symbol, 0, &[1; 4]).is_err());
        }
        assert!(layout.resolve(5, 0, &[u64::MAX; 4]).is_err());
        assert_eq!(layout.resolve(3, 0, &[4, 11, 15, 16]).unwrap(), 16);
        assert!(
            DistanceLayout::from_header(63, true)
                .resolve(1127, u64::MAX, &[4; 4])
                .is_err()
        );
    }
}
