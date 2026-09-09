use super::super::DecodeError;
use super::{
    bits::{Bits, Input},
    huffman::{Builder, Huffman},
};
use crate::shared::format::PREFIX_CODE_RANGES;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum Stage {
    TypeCount,
    TypeTree,
    LengthTree,
    LengthSymbol,
    LengthExtra { symbol: usize },
    Ready,
}

/// Independent block-switch state for one of literals, commands or distances.
/// The two previous types are initialized to the format's virtual types 1 and 0.
#[derive(Debug)]
pub(super) struct Block {
    pub(super) count: usize,
    pub(super) current: usize,
    remaining: u64,
    previous: usize,
    stage: Stage,
    types: Huffman,
    lengths: Huffman,
}

impl Default for Block {
    fn default() -> Self {
        Self {
            count: 1,
            current: 0,
            previous: 1,
            remaining: u64::MAX,
            stage: Stage::TypeCount,
            types: Huffman::default(),
            lengths: Huffman::default(),
        }
    }
}

impl Block {
    pub(super) fn header(
        &mut self,
        bits: &mut Bits,
        input: &mut Input<'_>,
        builder: &mut Builder,
    ) -> Result<bool, DecodeError> {
        loop {
            match self.stage {
                Stage::TypeCount => {
                    let Some(count) = bits.uint8(input)? else {
                        return Ok(false);
                    };
                    self.count = count + 1;
                    if count == 0 {
                        self.stage = Stage::Ready;
                        return Ok(true);
                    }
                    self.stage = Stage::TypeTree;
                }
                Stage::TypeTree => {
                    if !builder.read(self.count + 2, bits, input, &mut self.types)? {
                        return Ok(false);
                    }
                    self.stage = Stage::LengthTree;
                }
                Stage::LengthTree => {
                    if !builder.read(26, bits, input, &mut self.lengths)? {
                        return Ok(false);
                    }
                    self.stage = Stage::LengthSymbol;
                }
                Stage::LengthSymbol | Stage::LengthExtra { .. } | Stage::Ready => {
                    return self.length(bits, input);
                }
            }
        }
    }

    fn length(&mut self, bits: &mut Bits, input: &mut Input<'_>) -> Result<bool, DecodeError> {
        if self.stage == Stage::LengthSymbol {
            let Some(symbol) = self.lengths.read(bits, input)? else {
                return Ok(false);
            };
            self.stage = Stage::LengthExtra { symbol };
        }
        if let Stage::LengthExtra { symbol } = self.stage {
            let (base, width) = PREFIX_CODE_RANGES[symbol];
            let Some(extra) = bits.read(width as u8, input)? else {
                return Ok(false);
            };
            self.remaining = u64::from(base) + extra;
            self.stage = Stage::Ready;
        }
        Ok(true)
    }

    pub(super) fn prepare(
        &mut self,
        bits: &mut Bits,
        input: &mut Input<'_>,
    ) -> Result<bool, DecodeError> {
        if self.remaining != 0 {
            return Ok(true);
        }
        if self.stage == Stage::Ready {
            let Some(symbol) = self.types.read(bits, input)? else {
                return Ok(false);
            };
            let next = match symbol {
                0 => self.previous,
                1 => self.current + 1,
                _ => symbol - 2,
            } % self.count;
            self.previous = self.current;
            self.current = next;
            self.stage = Stage::LengthSymbol;
        }
        self.length(bits, input)
    }

    pub(super) const fn advance(&mut self) {
        self.remaining -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures;
    use super::*;

    #[test]
    fn encoded_switches_use_previous_incremented_and_explicit_types() {
        for symbol in [0, 1, 3] {
            let bytes = fixtures::fields(&[
                (4, 1), // two block types
                (2, 1),
                (2, 0),
                (2, symbol), // single type-switch symbol
                (2, 1),
                (2, 0),
                (5, 0), // single length symbol, base 1 + 2 bits
                (2, 0),
                (2, 0),
                (2, 0),
            ]);
            let mut bits = Bits::default();
            let mut input = fixtures::input(&bytes);
            let mut block = Block::default();
            assert!(
                block
                    .header(&mut bits, &mut input, &mut Builder::default())
                    .unwrap()
            );
            assert_eq!((block.count, block.current, block.remaining), (2, 0, 1));
            block.advance();
            assert!(block.prepare(&mut bits, &mut input).unwrap());
            assert_eq!((block.current, block.remaining), (1, 1));
            block.advance();
            assert!(block.prepare(&mut bits, &mut input).unwrap());
            assert_eq!(block.current, usize::from(symbol == 3));
        }
    }
}
