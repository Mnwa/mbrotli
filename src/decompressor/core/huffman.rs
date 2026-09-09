//! Canonical prefix codes, including resumable complex code-length descriptions.

use super::super::{DecodeError, InvalidDataKind};
use super::bits::{Bits, Input};

// RFC 9841: 16 short codes + 120 direct codes + (124 << 3).
const MAX_ALPHABET: usize = 1128;
const ORDER: [usize; 18] = [1, 2, 3, 4, 0, 5, 17, 6, 16, 7, 8, 9, 10, 11, 12, 13, 14, 15];

#[derive(Debug, Clone)]
pub(super) struct Huffman {
    counts: [u16; 16],
    symbols: [u16; MAX_ALPHABET],
    single: Option<u16>,
}

impl Default for Huffman {
    fn default() -> Self {
        Self {
            counts: [0; 16],
            symbols: [0; MAX_ALPHABET],
            single: None,
        }
    }
}

impl Huffman {
    /// Largest reachable symbol; zero-bit single-symbol trees count as reachable.
    pub(super) fn max_symbol(&self) -> usize {
        if let Some(symbol) = self.single {
            return usize::from(symbol);
        }
        let count: usize = self.counts.iter().map(|&n| usize::from(n)).sum();
        self.symbols[..count]
            .iter()
            .copied()
            .max()
            .map_or(0, usize::from)
    }

    fn build(&mut self, lengths: &[u8]) -> Result<(), DecodeError> {
        self.counts.fill(0);
        self.single = None;
        let mut total = 0;
        for (symbol, &length) in lengths.iter().enumerate() {
            if length > 15 {
                return Err(InvalidDataKind::Huffman.into());
            }
            if length != 0 {
                self.counts[usize::from(length)] += 1;
                total += 1;
                self.single = Some(symbol as u16);
            }
        }
        if total == 0 {
            return Err(InvalidDataKind::Huffman.into());
        }
        if total == 1 {
            return Ok(());
        }
        self.single = None;
        let mut space = 1i32;
        let mut offsets = [0usize; 16];
        for length in 1..16 {
            space = space * 2 - i32::from(self.counts[length]);
            if space < 0 {
                return Err(InvalidDataKind::Huffman.into());
            }
            if length < 15 {
                offsets[length + 1] = offsets[length] + usize::from(self.counts[length]);
            }
        }
        if space != 0 {
            return Err(InvalidDataKind::Huffman.into());
        }
        for (symbol, &length) in lengths.iter().enumerate() {
            if length != 0 {
                let offset = &mut offsets[usize::from(length)];
                self.symbols[*offset] = symbol as u16;
                *offset += 1;
            }
        }
        Ok(())
    }

    pub(super) fn read(
        &self,
        bits: &mut Bits,
        input: &mut Input<'_>,
    ) -> Result<Option<usize>, DecodeError> {
        if let Some(symbol) = self.single {
            return Ok(Some(usize::from(symbol)));
        }
        let mut first = 0usize;
        let mut offset = 0usize;
        for length in 1..=15u8 {
            let Some(value) = bits.peek(length, input)? else {
                return Ok(None);
            };
            let code = (value.reverse_bits() >> (64 - length)) as usize;
            let count = usize::from(self.counts[usize::from(length)]);
            if code >= first && code - first < count {
                bits.drop(length);
                return Ok(Some(usize::from(self.symbols[offset + code - first])));
            }
            first = (first + count) << 1;
            offset += count;
        }
        Err(InvalidDataKind::Huffman.into())
    }
}

#[derive(Debug, Clone, Copy, Default)]
enum Stage {
    #[default]
    Start,
    SimpleCount,
    SimpleSymbols,
    SimpleShape,
    CodeLengths,
    Symbols,
    Repeat(u8),
}

#[derive(Debug)]
pub(super) struct Builder {
    stage: Stage,
    lengths: [u8; MAX_ALPHABET],
    small: [u8; 18],
    code: Huffman,
    symbols: [usize; 4],
    count: usize,
    index: usize,
    space: i32,
    previous: u8,
    repeat_length: u8,
    repeat: usize,
}

impl Default for Builder {
    fn default() -> Self {
        Self {
            stage: Stage::Start,
            lengths: [0; MAX_ALPHABET],
            small: [0; 18],
            code: Huffman::default(),
            symbols: [0; 4],
            count: 0,
            index: 0,
            space: 0,
            previous: 8,
            repeat_length: 0,
            repeat: 0,
        }
    }
}

impl Builder {
    /// Abandons a partial description. The next start clears its scratch arrays.
    pub(super) const fn reset(&mut self) {
        self.stage = Stage::Start;
    }

    #[cfg_attr(all(feature = "hotpath", not(feature = "no_std")), hotpath::measure)]
    pub(super) fn read(
        &mut self,
        alphabet: usize,
        bits: &mut Bits,
        input: &mut Input<'_>,
        result: &mut Huffman,
    ) -> Result<bool, DecodeError> {
        if alphabet == 0 || alphabet > MAX_ALPHABET {
            return Err(InvalidDataKind::Huffman.into());
        }
        loop {
            match self.stage {
                Stage::Start => {
                    let Some(skip) = bits.read(2, input)? else {
                        return Ok(false);
                    };
                    self.lengths.fill(0);
                    self.small.fill(0);
                    self.index = skip as usize;
                    self.space = 32;
                    self.count = 0;
                    self.stage = if skip == 1 {
                        Stage::SimpleCount
                    } else {
                        Stage::CodeLengths
                    };
                }
                Stage::SimpleCount => {
                    let Some(count) = bits.read(2, input)? else {
                        return Ok(false);
                    };
                    self.count = count as usize + 1;
                    self.index = 0;
                    self.stage = Stage::SimpleSymbols;
                }
                Stage::SimpleSymbols => {
                    let width = (usize::BITS - (alphabet - 1).leading_zeros()) as u8;
                    while self.index < self.count {
                        let Some(symbol) = bits.read(width, input)? else {
                            return Ok(false);
                        };
                        let symbol = symbol as usize;
                        if symbol >= alphabet || self.symbols[..self.index].contains(&symbol) {
                            return Err(InvalidDataKind::Huffman.into());
                        }
                        self.symbols[self.index] = symbol;
                        self.index += 1;
                    }
                    self.stage = Stage::SimpleShape;
                }
                Stage::SimpleShape => {
                    let shape = if self.count == 4 {
                        let Some(shape) = bits.read(1, input)? else {
                            return Ok(false);
                        };
                        shape
                    } else {
                        0
                    };
                    let lengths = match (self.count, shape) {
                        (1, _) => [1, 0, 0, 0],
                        (2, _) => [1, 1, 0, 0],
                        (3, _) => [1, 2, 2, 0],
                        (_, 0) => [2, 2, 2, 2],
                        _ => [1, 2, 3, 3],
                    };
                    for (i, &length) in lengths.iter().enumerate().take(self.count) {
                        self.lengths[self.symbols[i]] = length;
                    }
                    result.build(&self.lengths[..alphabet])?;
                    self.stage = Stage::Start;
                    return Ok(true);
                }
                Stage::CodeLengths => {
                    while self.index < 18 && self.space > 0 {
                        let Some(first) = bits.peek(2, input)? else {
                            return Ok(false);
                        };
                        let (width, value) = match first {
                            0 => (2, 0),
                            1 => (2, 4),
                            2 => (2, 3),
                            _ => {
                                let Some(next) = bits.peek(3, input)? else {
                                    return Ok(false);
                                };
                                if next == 3 {
                                    (3, 2)
                                } else {
                                    let Some(last) = bits.peek(4, input)? else {
                                        return Ok(false);
                                    };
                                    (4, if last == 7 { 1 } else { 5 })
                                }
                            }
                        };
                        bits.drop(width);
                        self.small[ORDER[self.index]] = value;
                        self.index += 1;
                        if value != 0 {
                            self.count += 1;
                            self.space -= 32 >> value;
                        }
                    }
                    if self.space < 0 || (self.count != 1 && self.space != 0) {
                        return Err(InvalidDataKind::Huffman.into());
                    }
                    self.code.build(&self.small)?;
                    self.index = 0;
                    self.space = 32768;
                    self.previous = 8;
                    self.repeat = 0;
                    self.repeat_length = 0;
                    self.stage = Stage::Symbols;
                }
                Stage::Symbols => {
                    if self.space == 0 {
                        result.build(&self.lengths[..alphabet])?;
                        self.stage = Stage::Start;
                        return Ok(true);
                    }
                    if self.space < 0 || self.index == alphabet {
                        return Err(InvalidDataKind::Huffman.into());
                    }
                    let Some(symbol) = self.code.read(bits, input)? else {
                        return Ok(false);
                    };
                    if symbol >= 16 {
                        self.stage = Stage::Repeat(symbol as u8);
                        continue;
                    }
                    self.lengths[self.index] = symbol as u8;
                    self.index += 1;
                    self.repeat = 0;
                    if symbol != 0 {
                        self.previous = symbol as u8;
                        self.space -= 32768 >> symbol;
                    }
                }
                Stage::Repeat(symbol) => {
                    let width = symbol - 14;
                    let Some(extra) = bits.read(width, input)? else {
                        return Ok(false);
                    };
                    let length = if symbol == 16 { self.previous } else { 0 };
                    if self.repeat_length != length {
                        self.repeat = 0;
                        self.repeat_length = length;
                    }
                    let previous = self.repeat;
                    self.repeat = if previous == 0 {
                        0
                    } else {
                        (previous - 2) << width
                    };
                    self.repeat += extra as usize + 3;
                    let delta = self.repeat - previous;
                    let end = self.index + delta;
                    if end > alphabet {
                        return Err(InvalidDataKind::Huffman.into());
                    }
                    self.lengths[self.index..end].fill(length);
                    self.index = end;
                    if length != 0 {
                        self.space -= (delta as i32) << (15 - length);
                    }
                    self.stage = Stage::Symbols;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures;
    use super::*;

    #[test]
    fn complex_descriptions_repeat_previous_lengths_and_zero_lengths() {
        // Code-length trees name exactly {2,16} or {2,17}; both codes have
        // length one. The target tree has four length-two leaves.
        for repeat in [16, 17] {
            let mut fields = alloc::vec![(2, 0)];
            for &symbol in &ORDER {
                fields.push(if symbol == 2 || symbol == repeat {
                    (4, 7)
                } else {
                    (2, 0)
                });
                if symbol == repeat {
                    break;
                }
            }
            if repeat == 16 {
                fields.extend([(1, 0), (1, 1), (2, 0)]); // length 2, then repeat it 3 times
            } else {
                fields.extend([(1, 1), (3, 1), (4, 0)]); // four zeros, then four length 2s
            }
            fields.extend([(2, 0), (2, 2), (2, 1), (2, 3)]);
            let bytes = fixtures::fields(&fields);
            let mut bits = Bits::default();
            let mut input = fixtures::input(&bytes);
            let mut tree = Huffman::default();
            let alphabet = if repeat == 16 { 4 } else { 8 };
            assert!(
                Builder::default()
                    .read(alphabet, &mut bits, &mut input, &mut tree)
                    .unwrap()
            );
            assert_eq!(tree.counts[2], 4);
            for symbol in alphabet - 4..alphabet {
                assert_eq!(tree.read(&mut bits, &mut input).unwrap(), Some(symbol));
            }
        }
    }
}
