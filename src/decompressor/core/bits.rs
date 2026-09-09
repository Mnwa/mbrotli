//! Demand-driven bit input: never accepts bytes beyond the requested field.

use super::super::{DecodeError, InvalidDataKind};

#[derive(Debug, Default)]
pub(super) struct Bits {
    value: u128,
    count: u8,
}

pub(crate) struct Input<'a> {
    pub(crate) bytes: &'a [u8],
    pub(crate) consumed: usize,
    pub(crate) total_before: u64,
    pub(crate) limit: Option<u64>,
}

impl Bits {
    pub(super) fn peek(
        &mut self,
        count: u8,
        input: &mut Input<'_>,
    ) -> Result<Option<u64>, DecodeError> {
        while self.count < count {
            let Some(&byte) = input.bytes.get(input.consumed) else {
                return Ok(None);
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
            self.value |= u128::from(byte) << self.count;
            self.count += 8;
            input.consumed += 1;
        }
        Ok(Some((self.value & ((1u128 << count) - 1)) as u64))
    }

    pub(super) const fn drop(&mut self, count: u8) {
        self.value >>= count;
        self.count -= count;
    }

    pub(super) fn read(
        &mut self,
        count: u8,
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
        if self.value & ((1u128 << padding) - 1) != 0 {
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
        let n = (head >> 1) as u8;
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
    }
    #[test]
    fn partial_fields_retain_bytes_but_never_read_past_the_requested_field() {
        let mut bits = Bits::default();
        let mut first = Input {
            bytes: &[0x01],
            consumed: 0,
            total_before: 0,
            limit: None,
        };
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
}
