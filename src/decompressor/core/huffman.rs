//! Canonical prefix codes as two-level lookup tables, plus the resumable
//! complex code-length description reader.
//!
//! Every table has a 256-entry root indexed by the next eight stream bits.
//! Codes longer than eight bits go through a second-level table whose root
//! entry carries the combined width and the absolute table offset. Complete
//! codes fill every entry, so a lookup never needs a validity check; a
//! single-symbol code fills the root with zero-width entries.

use super::super::{DecodeError, InvalidDataKind};
use super::bits::{Bits, Input, mask};
use super::memory::Memory;
use alloc::vec::Vec;

// RFC 9841: 16 short codes + 120 direct codes + (124 << 3).
pub(super) const MAX_ALPHABET: usize = 1128;
const MAX_LENGTH: usize = 15;
const ROOT_BITS: u32 = 8;
const ROOT_SIZE: usize = 1 << ROOT_BITS;
/// Fallback root used only if a table is malformed; decoding it yields a
/// zero-bit symbol 0 and cannot loop because callers bound their symbol counts.
const EMPTY_ROOT: [Code; ROOT_SIZE] = [Code { bits: 0, value: 0 }; ROOT_SIZE];
const ORDER: [usize; 18] = [1, 2, 3, 4, 0, 5, 17, 6, 16, 7, 8, 9, 10, 11, 12, 13, 14, 15];

/// Largest two-level table for an alphabet, indexed by `(alphabet + 31) / 32`.
/// [`table_size`] measures each built code; these bound a group's stride.
const MAX_TABLE_SIZE: [u16; 37] = [
    256, 402, 436, 468, 500, 534, 566, 598, 630, 662, 694, 726, 758, 790, 822, 854, 886, 920, 952,
    984, 1016, 1048, 1080, 1112, 1144, 1176, 1208, 1240, 1272, 1304, 1336, 1368, 1400, 1432, 1464,
    1496, 1528,
];

/// One table entry: a symbol with its code width, or a pointer whose width
/// exceeds the root width and whose value is the second-level table offset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Code {
    bits: u8,
    value: u16,
}

/// Canonical shape of a validated code: per-width counts and the symbols in
/// canonical order. A single-symbol code has `single` set and no counts.
struct Shape {
    counts: [u16; MAX_LENGTH + 1],
    single: Option<u16>,
    max_symbol: usize,
}

fn analyze(lengths: &[u8], sorted: &mut [u16; MAX_ALPHABET]) -> Result<Shape, DecodeError> {
    let mut counts = [0u16; MAX_LENGTH + 1];
    let mut total = 0usize;
    let mut last = 0usize;
    for (symbol, &length) in lengths.iter().enumerate() {
        if usize::from(length) > MAX_LENGTH {
            return Err(InvalidDataKind::Huffman.into());
        }
        if length != 0 {
            counts[usize::from(length)] += 1;
            total += 1;
            last = symbol;
        }
    }
    if total == 0 {
        return Err(InvalidDataKind::Huffman.into());
    }
    if total == 1 {
        return Ok(Shape {
            counts: [0; MAX_LENGTH + 1],
            single: Some(last as u16),
            max_symbol: last,
        });
    }
    let mut space = 1i32;
    let mut offsets = [0usize; MAX_LENGTH + 2];
    for length in 1..=MAX_LENGTH {
        space = space * 2 - i32::from(counts[length]);
        if space < 0 {
            return Err(InvalidDataKind::Huffman.into());
        }
        offsets[length + 1] = offsets[length] + usize::from(counts[length]);
    }
    if space != 0 {
        return Err(InvalidDataKind::Huffman.into());
    }
    for (symbol, &length) in lengths.iter().enumerate() {
        if length != 0 {
            let offset = &mut offsets[usize::from(length)];
            sorted[*offset] = symbol as u16;
            *offset += 1;
        }
    }
    Ok(Shape {
        counts,
        single: None,
        max_symbol: last,
    })
}

/// Reversed-bit increment of a `length`-bit canonical key.
const fn next_key(key: usize, length: usize) -> usize {
    let mut step = 1usize << (length - 1);
    while key & step != 0 {
        step >>= 1;
    }
    if step == 0 {
        0
    } else {
        (key & (step - 1)) + step
    }
}

/// Width of the second-level table starting at `length`, given the counts
/// still unplaced at each width.
const fn next_table_bits(remaining: &[u16; MAX_LENGTH + 1], mut length: usize) -> u32 {
    let mut left = 1i32 << (length - ROOT_BITS as usize);
    while length < MAX_LENGTH {
        left -= remaining[length] as i32;
        if left <= 0 {
            break;
        }
        length += 1;
        left <<= 1;
    }
    (length - ROOT_BITS as usize) as u32
}

/// Entries the table for `shape` occupies, counted without writing it.
fn table_size(shape: &Shape) -> usize {
    let mut remaining = shape.counts;
    let mut key = 0usize;
    let mut length = 1;
    while length <= ROOT_BITS as usize {
        for _ in 0..remaining[length] {
            key = next_key(key, length);
        }
        length += 1;
    }
    let mut total = ROOT_SIZE;
    let mut table_size = 0usize;
    let mut low = usize::MAX;
    for length in ROOT_BITS as usize + 1..=MAX_LENGTH {
        while remaining[length] != 0 {
            if key & (ROOT_SIZE - 1) != low {
                table_size = 1 << next_table_bits(&remaining, length);
                total += table_size;
                low = key & (ROOT_SIZE - 1);
            }
            remaining[length] -= 1;
            key = next_key(key, length);
        }
    }
    let _ = table_size;
    total
}

fn replicate(table: &mut [Code], start: usize, step: usize, code: Code) {
    let mut index = start;
    while index < table.len() {
        table[index] = code;
        index += step;
    }
}

/// Fills `codes` for a complete code. The caller sizes `codes` from
/// [`table_size`]; a shorter slice is a library defect.
fn fill(codes: &mut [Code], shape: &Shape, sorted: &[u16]) -> Result<(), DecodeError> {
    if let Some(symbol) = shape.single {
        let root = codes
            .get_mut(..ROOT_SIZE)
            .ok_or(DecodeError::InternalInvariant)?;
        root.fill(Code {
            bits: 0,
            value: symbol,
        });
        return Ok(());
    }
    if codes.len() < table_size(shape) {
        return Err(DecodeError::InternalInvariant);
    }
    let mut remaining = shape.counts;
    let max_length = (1..=MAX_LENGTH)
        .rev()
        .find(|&length| remaining[length] != 0)
        .unwrap_or(0);
    let mut table_bits = ROOT_BITS as usize;
    if max_length < table_bits {
        table_bits = max_length;
    }
    let mut current = 1usize << table_bits;
    let mut symbol = 0usize;
    let mut key = 0usize;
    let mut step = 2usize;
    let mut length = 1;
    while length <= table_bits {
        for _ in 0..remaining[length] {
            replicate(
                &mut codes[..current],
                key,
                step,
                Code {
                    bits: length as u8,
                    value: sorted[symbol],
                },
            );
            symbol += 1;
            key = next_key(key, length);
        }
        step <<= 1;
        length += 1;
    }
    while current != ROOT_SIZE {
        codes.copy_within(..current, current);
        current <<= 1;
    }
    let mut total = ROOT_SIZE;
    let mut table_start = 0usize;
    let mut low = usize::MAX;
    step = 2;
    for length in ROOT_BITS as usize + 1..=MAX_LENGTH {
        while remaining[length] != 0 {
            if key & (ROOT_SIZE - 1) != low {
                table_start += current;
                let bits = next_table_bits(&remaining, length);
                current = 1 << bits;
                total += current;
                low = key & (ROOT_SIZE - 1);
                codes[low] = Code {
                    bits: (bits + ROOT_BITS) as u8,
                    value: table_start as u16,
                };
            }
            replicate(
                &mut codes[table_start..table_start + current],
                key >> ROOT_BITS,
                step,
                Code {
                    bits: (length - ROOT_BITS as usize) as u8,
                    value: sorted[symbol],
                },
            );
            symbol += 1;
            remaining[length] -= 1;
            key = next_key(key, length);
        }
        step <<= 1;
    }
    debug_assert_eq!(total, table_size(shape));
    Ok(())
}

/// Decodes with at least fifteen buffered bits.
///
/// `codes` is a complete two-level table from [`fill`]: a 256-entry root
/// (accessed here as a fixed-size array so the root lookup carries no bounds
/// check) followed by the second-level tables it points into.
#[inline(always)]
pub(super) fn decode_fast(codes: &[Code], bits: &mut Bits) -> usize {
    let peek = bits.value();
    let root = codes.first_chunk::<ROOT_SIZE>().unwrap_or(&EMPTY_ROOT);
    let mut entry = root[(peek & (ROOT_SIZE as u64 - 1)) as usize];
    if entry.bits > ROOT_BITS as u8 {
        let index = usize::from(entry.value)
            + ((peek >> ROOT_BITS) & mask(u32::from(entry.bits) - ROOT_BITS)) as usize;
        bits.drop(ROOT_BITS);
        entry = codes[index];
    }
    bits.drop(u32::from(entry.bits));
    usize::from(entry.value)
}

/// Decodes with whatever is buffered; `None` needs at least one more byte.
fn try_decode(codes: &[Code], bits: &mut Bits) -> Option<usize> {
    let available = bits.count();
    let peek = bits.value();
    let mut entry = codes[(peek & (ROOT_SIZE as u64 - 1)) as usize];
    let mut width = u32::from(entry.bits);
    if width > ROOT_BITS {
        if available < ROOT_BITS {
            return None;
        }
        let index =
            usize::from(entry.value) + ((peek >> ROOT_BITS) & mask(width - ROOT_BITS)) as usize;
        entry = codes[index];
        width = ROOT_BITS + u32::from(entry.bits);
    }
    if width > available {
        return None;
    }
    bits.drop(width);
    Some(usize::from(entry.value))
}

/// Decodes one symbol, accepting bytes only as the code needs them.
pub(super) fn decode(
    codes: &[Code],
    bits: &mut Bits,
    input: &mut Input<'_>,
) -> Result<Option<usize>, DecodeError> {
    loop {
        if let Some(symbol) = try_decode(codes, bits) {
            return Ok(Some(symbol));
        }
        if !bits.load_byte(input)? {
            return Ok(None);
        }
    }
}

/// One owned prefix code whose storage is accounted like every workspace buffer.
#[derive(Debug, Default)]
pub(super) struct Huffman {
    codes: Vec<Code>,
}

impl Huffman {
    pub(super) fn codes(&self) -> &[Code] {
        &self.codes
    }

    pub(super) fn decode(
        &self,
        bits: &mut Bits,
        input: &mut Input<'_>,
    ) -> Result<Option<usize>, DecodeError> {
        decode(&self.codes, bits, input)
    }
}

/// Every prefix code of one kind for a meta-block, at a fixed stride.
#[derive(Debug, Default)]
pub(super) struct Group {
    codes: Vec<Code>,
    stride: usize,
}

impl Group {
    /// Reserves `count` tables for `alphabet` without building any.
    pub(super) fn prepare(
        &mut self,
        count: usize,
        alphabet: usize,
        memory: &mut Memory,
    ) -> Result<(), DecodeError> {
        let index = alphabet.div_ceil(32);
        let stride = MAX_TABLE_SIZE
            .get(index)
            .map(|&size| usize::from(size))
            .ok_or(InvalidDataKind::Huffman)?;
        let total = count.checked_mul(stride).ok_or(DecodeError::SizeOverflow)?;
        memory.resize(&mut self.codes, total)?;
        self.stride = stride;
        Ok(())
    }

    pub(super) fn count(&self) -> usize {
        self.codes.len().checked_div(self.stride).unwrap_or(0)
    }

    #[inline(always)]
    pub(super) fn codes(&self, tree: usize) -> &[Code] {
        let start = tree * self.stride;
        &self.codes[start..start + self.stride]
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

/// Resumable reader of one prefix-code description. After `read` reports a
/// complete description, `build` or `build_slot` turns it into a table.
#[derive(Debug)]
pub(super) struct Builder {
    stage: Stage,
    lengths: [u8; MAX_ALPHABET],
    sorted: [u16; MAX_ALPHABET],
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
            sorted: [0; MAX_ALPHABET],
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

    /// Builds the completed description into an exactly sized owned table.
    /// Returns the largest symbol with a code.
    pub(super) fn build(
        &mut self,
        alphabet: usize,
        memory: &mut Memory,
        target: &mut Huffman,
    ) -> Result<usize, DecodeError> {
        let shape = analyze(&self.lengths[..alphabet], &mut self.sorted)?;
        memory.resize(&mut target.codes, table_size(&shape))?;
        fill(&mut target.codes, &shape, &self.sorted)?;
        Ok(shape.max_symbol)
    }

    /// Builds the completed description into one of a group's fixed slots.
    pub(super) fn build_slot(
        &mut self,
        alphabet: usize,
        group: &mut Group,
        tree: usize,
    ) -> Result<usize, DecodeError> {
        let shape = analyze(&self.lengths[..alphabet], &mut self.sorted)?;
        let start = tree * group.stride;
        let slot = group
            .codes
            .get_mut(start..start + group.stride)
            .ok_or(DecodeError::InternalInvariant)?;
        fill(slot, &shape, &self.sorted)?;
        Ok(shape.max_symbol)
    }

    #[cfg_attr(all(feature = "hotpath", not(feature = "no_std")), hotpath::measure)]
    pub(super) fn read(
        &mut self,
        alphabet: usize,
        bits: &mut Bits,
        input: &mut Input<'_>,
        memory: &mut Memory,
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
                    let width = usize::BITS - (alphabet - 1).leading_zeros();
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
                    let shape = analyze(&self.small, &mut self.sorted)?;
                    memory.resize(&mut self.code.codes, table_size(&shape))?;
                    fill(&mut self.code.codes, &shape, &self.sorted)?;
                    self.index = 0;
                    self.space = 32768;
                    self.previous = 8;
                    self.repeat = 0;
                    self.repeat_length = 0;
                    self.stage = Stage::Symbols;
                }
                Stage::Symbols => {
                    if self.space == 0 {
                        self.stage = Stage::Start;
                        return Ok(true);
                    }
                    if self.space < 0 || self.index == alphabet {
                        return Err(InvalidDataKind::Huffman.into());
                    }
                    let Some(symbol) = self.code.decode(bits, input)? else {
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
                    let width = u32::from(symbol) - 14;
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

    fn build(lengths: &[u8]) -> Result<(Vec<Code>, usize), DecodeError> {
        let mut sorted = [0u16; MAX_ALPHABET];
        let shape = analyze(lengths, &mut sorted)?;
        let mut codes = alloc::vec![Code::default(); table_size(&shape)];
        fill(&mut codes, &shape, &sorted)?;
        Ok((codes, shape.max_symbol))
    }

    /// Reference decoder: walks the canonical code bit by bit.
    fn canonical(lengths: &[u8], mut next_bit: impl FnMut() -> u64) -> usize {
        let mut code = 0usize;
        let mut first = 0usize;
        let mut index = 0usize;
        let mut sorted: Vec<(u8, usize)> = lengths
            .iter()
            .enumerate()
            .filter(|&(_, &l)| l != 0)
            .map(|(s, &l)| (l, s))
            .collect();
        sorted.sort_unstable();
        for length in 1..=MAX_LENGTH as u8 {
            code |= next_bit() as usize;
            let count = sorted.iter().filter(|(l, _)| *l == length).count();
            if code - first < count {
                return sorted[index + code - first].1;
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        panic!("incomplete code");
    }

    #[test]
    fn tables_agree_with_bitwise_canonical_decoding_for_random_complete_codes() {
        let mut rng = 0x9e37_79b9_7f4a_7c15u64;
        let mut random = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        for alphabet in [2usize, 3, 4, 18, 26, 256, 258, 272, 704, 1128] {
            for _ in 0..40 {
                // Build a complete code by splitting leaves at random.
                let mut lengths = alloc::vec![0u8; alphabet];
                let symbols = 2 + (random() as usize) % (alphabet - 1);
                let mut lengths_pool: Vec<u8> = alloc::vec![1, 1];
                while lengths_pool.len() < symbols {
                    let pick = (random() as usize) % lengths_pool.len();
                    let length = lengths_pool[pick];
                    if length >= MAX_LENGTH as u8 {
                        continue;
                    }
                    lengths_pool.swap_remove(pick);
                    lengths_pool.push(length + 1);
                    lengths_pool.push(length + 1);
                }
                // Assign distinct symbols in random order.
                let mut candidates: Vec<usize> = (0..alphabet).collect();
                for i in (1..candidates.len()).rev() {
                    let j = (random() as usize) % (i + 1);
                    candidates.swap(i, j);
                }
                for (symbol, &length) in candidates.iter().zip(&lengths_pool) {
                    lengths[*symbol] = length;
                }
                let (codes, max_symbol) = build(&lengths).unwrap();
                assert_eq!(max_symbol, lengths.iter().rposition(|&l| l != 0).unwrap());
                let stride = usize::from(MAX_TABLE_SIZE[alphabet.div_ceil(32)]);
                assert!(codes.len() <= stride, "{alphabet}: {}", codes.len());
                for _ in 0..64 {
                    let word = random();
                    let word_bytes = word.to_le_bytes();
                    let mut bits = Bits::default();
                    let mut input = fixtures::input(&word_bytes);
                    bits.peek(56, &mut input).unwrap();
                    let before = bits.count();
                    let fast = decode_fast(&codes, &mut bits);
                    let used = before - bits.count();
                    let mut position = 0;
                    let expected = canonical(&lengths, || {
                        let bit = (word >> position) & 1;
                        position += 1;
                        bit
                    });
                    assert_eq!((fast, used as usize), (expected, position));
                    assert_eq!(used, u32::from(lengths[fast]));
                    // Byte-exact decoding reads only the bytes the code needs.
                    let mut slow = Bits::default();
                    let mut input = fixtures::input(&word_bytes);
                    assert_eq!(
                        decode(&codes, &mut slow, &mut input).unwrap(),
                        Some(expected)
                    );
                    assert_eq!(input.consumed, position.div_ceil(8));
                }
            }
        }
    }

    #[test]
    fn single_symbol_codes_consume_no_bits_and_bad_shapes_are_rejected() {
        let (codes, max_symbol) = build(&[0, 0, 7, 0]).unwrap();
        assert_eq!((codes.len(), max_symbol), (ROOT_SIZE, 2));
        let mut bits = Bits::default();
        let mut input = fixtures::input(&[]);
        assert_eq!(decode(&codes, &mut bits, &mut input).unwrap(), Some(2));
        assert!(build(&[0, 0]).is_err());
        assert!(build(&[1, 1, 1]).is_err());
        assert!(build(&[1, 2, 0]).is_err());
        assert!(build(&[16, 1]).is_err());
        let mut short = [Code::default(); 8];
        let mut sorted = [0u16; MAX_ALPHABET];
        let shape = analyze(&[1, 1], &mut sorted).unwrap();
        assert!(matches!(
            fill(&mut short, &shape, &sorted),
            Err(DecodeError::InternalInvariant)
        ));
        let single = analyze(&[0, 3], &mut sorted).unwrap();
        assert!(matches!(
            fill(&mut short, &single, &sorted),
            Err(DecodeError::InternalInvariant)
        ));
    }

    #[test]
    fn partial_input_decoding_waits_for_exactly_the_needed_bytes() {
        // Lengths 1 and 9..15 fill a second-level table.
        let mut lengths = alloc::vec![0u8; 16];
        lengths[0] = 1;
        for (i, length) in (2..=8).zip(&mut lengths[1..]) {
            *length = i;
        }
        lengths[8] = 8;
        let (codes, _) = build(&lengths).unwrap();
        // Symbol 8 has code 1111_1111 (8 bits); nothing resolves before it.
        let mut bits = Bits::default();
        let mut input = fixtures::input(&[0x7f]);
        assert_eq!(decode(&codes, &mut bits, &mut input).unwrap(), Some(7));
        assert_eq!(input.consumed, 1);
        let mut bits = Bits::default();
        assert_eq!(
            decode(&codes, &mut bits, &mut fixtures::input(&[])).unwrap(),
            None
        );
    }

    #[test]
    fn groups_reserve_fixed_strides_and_reject_impossible_alphabets() {
        let mut memory = Memory::default();
        let mut group = Group::default();
        assert_eq!(group.count(), 0);
        group.prepare(3, 256, &mut memory).unwrap();
        assert_eq!((group.count(), group.codes(2).len()), (3, 630));
        assert!(group.prepare(1, 4000, &mut memory).is_err());
        let mut builder = Builder::default();
        builder.lengths[..4].copy_from_slice(&[2, 2, 2, 2]);
        assert_eq!(builder.build_slot(4, &mut group, 1).unwrap(), 3);
        assert!(matches!(
            builder.build_slot(4, &mut group, 3),
            Err(DecodeError::InternalInvariant)
        ));
        let mut owned = Huffman::default();
        assert_eq!(builder.build(4, &mut memory, &mut owned).unwrap(), 3);
        assert_eq!(owned.codes().len(), ROOT_SIZE);
    }

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
            let mut memory = Memory::default();
            let mut builder = Builder::default();
            let alphabet = if repeat == 16 { 4 } else { 8 };
            assert!(
                builder
                    .read(alphabet, &mut bits, &mut input, &mut memory)
                    .unwrap()
            );
            let mut tree = Huffman::default();
            assert_eq!(
                builder.build(alphabet, &mut memory, &mut tree).unwrap(),
                alphabet - 1
            );
            for symbol in alphabet - 4..alphabet {
                assert_eq!(tree.decode(&mut bits, &mut input).unwrap(), Some(symbol));
            }
        }
    }
}
