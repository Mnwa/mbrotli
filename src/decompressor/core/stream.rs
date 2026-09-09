use super::super::{DecodeError, DecoderConfig, InvalidDataKind, OutputSize};
use super::{
    bits::{Bits, Input},
    block::Block,
    context_map::ContextMap,
    dictionary,
    distance::DistanceLayout,
    header::{self, MetaBlock},
    huffman::{Builder, Huffman},
    memory::Memory,
};
use crate::shared::dictionary::transform::SCRATCH_BYTES;
use crate::{
    Window, WindowEncoding,
    dictionary::DictionaryRef,
    shared::format::{
        CONTEXT_LUT_SIGNED, CONTEXT_LUT_UTF8, COPY_BASE, COPY_EXTRA, INS_BASE, INS_EXTRA,
    },
};
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, Default)]
enum Stage {
    #[default]
    Window,
    Meta,
    Metadata,
    Raw,
    Blocks(usize),
    DistanceParams,
    Modes(usize),
    Maps(usize),
    Trees(usize, usize),
    Command,
    InsertExtra(usize, usize),
    CopyExtra(usize),
    Literals,
    Distance,
    DistanceExtra(usize),
    Resolve,
    Copy,
    Dictionary,
    Prefix {
        offset: u64,
        start: u64,
    },
    PrefixHistory {
        offset: usize,
        start: u64,
    },
    EndBlock,
    End,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum Stop {
    Input,
    Output,
    Member,
}

pub(crate) struct Output<'a> {
    pub(crate) bytes: &'a mut [u8],
    pub(crate) produced: usize,
    pub(crate) total_before: u64,
    pub(crate) limit: Option<u64>,
    pub(crate) exact: OutputSize,
}

impl Output<'_> {
    fn ready(&self) -> Result<bool, DecodeError> {
        let next = self
            .total_before
            .checked_add(self.produced as u64)
            .and_then(|v| v.checked_add(1))
            .ok_or(DecodeError::SizeOverflow)?;
        if let Some(limit) = self.limit
            && next > limit
        {
            return Err(DecodeError::OutputLimitExceeded { limit });
        }
        if let OutputSize::Exact(expected) = self.exact
            && next > expected
        {
            return Err(DecodeError::OutputSizeMismatch {
                expected,
                actual: next,
            });
        }
        Ok(self.produced < self.bytes.len())
    }
}

#[derive(Debug)]
pub(crate) struct Stream {
    bits: Bits,
    stage: Stage,
    memory: Memory,
    history: Vec<u8>,
    // A prefix-crossing reference can exceed the sliding window. Preserve only
    // the original history bytes that would be overwritten before being read.
    prefix_history: Vec<u8>,
    position: u64,
    max_backward: u64,
    pub(crate) window: Option<Window>,
    large: bool,
    last: bool,
    remaining: u64,
    blocks: [Block; 3],
    builder: Builder,
    modes: [u8; 256],
    maps: [ContextMap; 2],
    trees: [Vec<Huffman>; 3],
    distances: DistanceLayout,
    literals: u64,
    copy: u64,
    implicit: bool,
    distance: u64,
    distance_code: usize,
    cache: [u64; 4],
    previous: [u8; 2],
    scratch: [u8; SCRATCH_BYTES],
    scratch_pos: usize,
    scratch_len: usize,
}

impl Stream {
    pub(crate) const fn retained_bytes(&self) -> usize {
        self.memory.live
    }

    pub(crate) fn reset(&mut self, config: DecoderConfig) {
        self.bits = Bits::default();
        self.stage = Stage::Window;
        self.builder.reset();
        self.history.clear();
        self.prefix_history.clear();
        self.position = 0;
        self.window = None;
        self.cache = [4, 11, 15, 16];
        self.previous = [0, 0];
        self.memory.limit = config.limits().max_workspace_bytes();
    }

    fn emit(&mut self, byte: u8, output: &mut Output<'_>) -> Result<(), DecodeError> {
        let index = usize::try_from(self.position % self.max_backward)
            .map_err(|_| DecodeError::SizeOverflow)?;
        if index >= self.history.len() {
            let length = index.checked_add(1).ok_or(DecodeError::SizeOverflow)?;
            if length > self.history.capacity() {
                let desired = length
                    .max(self.history.capacity().saturating_mul(2))
                    .min(usize::try_from(self.max_backward).unwrap_or(usize::MAX));
                self.memory.resize(&mut self.history, desired)?;
            } else {
                self.history.resize(length, 0);
            }
        }
        self.history[index] = byte;
        self.position = self
            .position
            .checked_add(1)
            .ok_or(DecodeError::SizeOverflow)?;
        self.previous = [byte, self.previous[0]];
        output.bytes[output.produced] = byte;
        output.produced += 1;
        self.remaining -= 1;
        Ok(())
    }

    fn context(&self) -> usize {
        let [a, b] = self.previous;
        usize::from(match self.modes[self.blocks[0].current] {
            0 => a & 63,
            1 => a >> 2,
            2 => CONTEXT_LUT_UTF8[usize::from(a)] | CONTEXT_LUT_UTF8[256 + usize::from(b)],
            _ => CONTEXT_LUT_SIGNED[usize::from(a)] | CONTEXT_LUT_SIGNED[256 + usize::from(b)],
        })
    }

    #[cfg_attr(all(feature = "hotpath", not(feature = "no_std")), hotpath::measure)]
    pub(crate) fn run(
        &mut self,
        input: &mut Input<'_>,
        output: &mut Output<'_>,
        config: DecoderConfig,
        dictionary: Option<DictionaryRef<'_>>,
    ) -> Result<Stop, DecodeError> {
        macro_rules! read {
            ($n:expr) => {
                match self.bits.read($n, input)? {
                    Some(v) => v,
                    None => return Ok(Stop::Input),
                }
            };
        }
        loop {
            match self.stage {
                Stage::Window => {
                    let Some(window) =
                        header::window(&mut self.bits, input, config.window_limit())?
                    else {
                        return Ok(Stop::Input);
                    };
                    self.large = window.encoding() == WindowEncoding::Large;
                    self.max_backward = (1u64 << window.bits()) - 16;
                    self.window = Some(window);
                    self.stage = Stage::Meta;
                }
                Stage::Meta => {
                    let Some(header) = header::metablock(&mut self.bits, input)? else {
                        return Ok(Stop::Input);
                    };
                    match header {
                        MetaBlock::End => self.stage = Stage::End,
                        MetaBlock::Uncompressed { length } => {
                            self.remaining = length;
                            self.last = false;
                            self.stage = Stage::Raw;
                        }
                        MetaBlock::Metadata { length, last } => {
                            self.remaining = length;
                            self.last = last;
                            self.stage = Stage::Metadata;
                        }
                        MetaBlock::Compressed { length, last } => {
                            self.remaining = length;
                            self.last = last;
                            self.blocks = Default::default();
                            for map in &mut self.maps {
                                map.reset();
                            }
                            self.stage = Stage::Blocks(0);
                        }
                    }
                }
                Stage::Metadata => {
                    if self.remaining == 0 {
                        self.stage = Stage::EndBlock;
                        continue;
                    }
                    read!(8);
                    self.remaining -= 1;
                }
                Stage::Raw => {
                    if self.remaining == 0 {
                        self.stage = Stage::EndBlock;
                        continue;
                    }
                    if !output.ready()? {
                        return Ok(Stop::Output);
                    }
                    let byte = read!(8) as u8;
                    self.emit(byte, output)?;
                }
                Stage::Blocks(i) => {
                    if !self.blocks[i].header(&mut self.bits, input, &mut self.builder)? {
                        return Ok(Stop::Input);
                    }
                    self.stage = if i == 2 {
                        Stage::DistanceParams
                    } else {
                        Stage::Blocks(i + 1)
                    };
                }
                Stage::DistanceParams => {
                    let params = read!(6);
                    self.distances = DistanceLayout::from_header(params as u8, self.large);
                    self.stage = Stage::Modes(0);
                }
                Stage::Modes(i) => {
                    self.modes[i] = read!(2) as u8;
                    self.stage = if i + 1 == self.blocks[0].count {
                        Stage::Maps(0)
                    } else {
                        Stage::Modes(i + 1)
                    };
                }
                Stage::Maps(i) => {
                    let size =
                        self.blocks[if i == 0 { 0 } else { 2 }].count << if i == 0 { 6 } else { 2 };
                    if !self.maps[i].read(
                        size,
                        &mut self.bits,
                        input,
                        &mut self.builder,
                        &mut self.memory,
                    )? {
                        return Ok(Stop::Input);
                    }
                    if i == 0 {
                        self.stage = Stage::Maps(1);
                    } else {
                        for (i, n) in [self.maps[0].trees, self.blocks[1].count, self.maps[1].trees]
                            .into_iter()
                            .enumerate()
                        {
                            self.memory.resize(&mut self.trees[i], n)?;
                        }
                        self.stage = Stage::Trees(0, 0);
                    }
                }
                Stage::Trees(group, index) => {
                    let alphabet = match group {
                        0 => 256,
                        1 => 704,
                        _ => self.distances.alphabet(),
                    };
                    if !self.builder.read(
                        alphabet,
                        &mut self.bits,
                        input,
                        &mut self.trees[group][index],
                    )? {
                        return Ok(Stop::Input);
                    }
                    if group == 2 {
                        self.distances
                            .validate_symbol(self.trees[group][index].max_symbol())?;
                    }
                    self.stage = if index + 1 < self.trees[group].len() {
                        Stage::Trees(group, index + 1)
                    } else if group < 2 {
                        Stage::Trees(group + 1, 0)
                    } else {
                        Stage::Command
                    };
                }
                Stage::Command => {
                    if self.remaining == 0 {
                        self.stage = Stage::EndBlock;
                        continue;
                    }
                    if !self.blocks[1].prepare(&mut self.bits, input)? {
                        return Ok(Stop::Input);
                    }
                    let Some(symbol) =
                        self.trees[1][self.blocks[1].current].read(&mut self.bits, input)?
                    else {
                        return Ok(Stop::Input);
                    };
                    self.blocks[1].advance();
                    const CELLS: [usize; 11] = [0, 1, 0, 1, 8, 9, 2, 16, 10, 17, 18];
                    let cell = CELLS[symbol >> 6];
                    let insert = (cell & 24) + ((symbol >> 3) & 7);
                    let copy = ((cell << 3) & 24) + (symbol & 7);
                    self.implicit = symbol < 128;
                    self.stage = Stage::InsertExtra(insert, copy);
                }
                Stage::InsertExtra(insert, copy) => {
                    self.literals = u64::from(INS_BASE[insert]) + read!(INS_EXTRA[insert] as u8);
                    if self.literals > self.remaining {
                        return Err(InvalidDataKind::MetaBlock.into());
                    }
                    self.stage = Stage::CopyExtra(copy);
                }
                Stage::CopyExtra(copy) => {
                    self.copy = u64::from(COPY_BASE[copy]) + read!(COPY_EXTRA[copy] as u8);
                    self.stage = Stage::Literals;
                }
                Stage::Literals => {
                    if self.literals == 0 {
                        self.stage = if self.remaining == 0 {
                            Stage::EndBlock
                        } else {
                            Stage::Distance
                        };
                        continue;
                    }
                    if !output.ready()? {
                        return Ok(Stop::Output);
                    }
                    if !self.blocks[0].prepare(&mut self.bits, input)? {
                        return Ok(Stop::Input);
                    }
                    let tree = usize::from(
                        self.maps[0].values[self.blocks[0].current * 64 + self.context()],
                    );
                    let Some(byte) = self.trees[0][tree].read(&mut self.bits, input)? else {
                        return Ok(Stop::Input);
                    };
                    self.blocks[0].advance();
                    self.emit(byte as u8, output)?;
                    self.literals -= 1;
                }
                Stage::Distance => {
                    if self.implicit {
                        self.distance_code = 0;
                        self.distance = self.cache[0];
                        self.stage = Stage::Resolve;
                        continue;
                    }
                    if !self.blocks[2].prepare(&mut self.bits, input)? {
                        return Ok(Stop::Input);
                    }
                    let context = self.copy.saturating_sub(2).min(3) as usize;
                    let tree =
                        usize::from(self.maps[1].values[self.blocks[2].current * 4 + context]);
                    let Some(symbol) = self.trees[2][tree].read(&mut self.bits, input)? else {
                        return Ok(Stop::Input);
                    };
                    self.blocks[2].advance();
                    self.distance_code = symbol;
                    self.stage = Stage::DistanceExtra(symbol);
                }
                Stage::DistanceExtra(symbol) => {
                    let extra = read!(self.distances.extra_bits(symbol));
                    self.distance = self.distances.resolve(symbol, extra, &self.cache)?;
                    self.stage = Stage::Resolve;
                }
                Stage::Resolve => {
                    let available = self.position.min(self.max_backward);
                    let prefix_len = dictionary.map_or(0, DictionaryRef::prefix_len);
                    if self.distance > available && self.distance - available <= prefix_len {
                        let offset = prefix_len - (self.distance - available);
                        if self.copy > self.remaining {
                            return Err(InvalidDataKind::DictionaryReference.into());
                        }
                        if self.distance > self.max_backward {
                            let crossing = self.copy.saturating_sub(prefix_len - offset);
                            let length = usize::try_from(crossing.min(available))
                                .map_err(|_| DecodeError::SizeOverflow)?;
                            self.memory.resize(&mut self.prefix_history, length)?;
                            for (i, byte) in self.prefix_history.iter_mut().enumerate() {
                                let index =
                                    (self.position - available + i as u64) % self.max_backward;
                                *byte = self.history[index as usize];
                            }
                        }
                        if self.distance_code != 0 {
                            self.cache.rotate_right(1);
                            self.cache[0] = self.distance;
                        }
                        self.stage = Stage::Prefix {
                            offset,
                            start: offset,
                        };
                    } else if self.distance > available {
                        self.scratch_len = dictionary::resolve(
                            dictionary,
                            self.distance - available - prefix_len - 1,
                            self.copy as usize,
                            self.context(),
                            &mut self.scratch,
                        )?;
                        if self.scratch_len as u64 > self.remaining {
                            return Err(InvalidDataKind::MetaBlock.into());
                        }
                        // RFC/C reject these zero-output references: they can
                        // otherwise repeat using exclusively zero-bit trees.
                        if self.scratch_len == 0 && self.distance <= 120 {
                            return Err(InvalidDataKind::DictionaryReference.into());
                        }
                        self.scratch_pos = 0;
                        self.stage = Stage::Dictionary;
                    } else {
                        if self.copy > self.remaining {
                            return Err(InvalidDataKind::MetaBlock.into());
                        }
                        if self.distance_code != 0 {
                            self.cache.rotate_right(1);
                            self.cache[0] = self.distance;
                        }
                        self.stage = Stage::Copy;
                    }
                }
                Stage::Copy => {
                    if self.copy == 0 {
                        self.stage = Stage::Command;
                        continue;
                    }
                    if !output.ready()? {
                        return Ok(Stop::Output);
                    }
                    let index = ((self.position - self.distance) % self.max_backward) as usize;
                    let byte = self.history[index];
                    self.emit(byte, output)?;
                    self.copy -= 1;
                }
                Stage::Prefix { offset, start } => {
                    if self.copy == 0 {
                        self.stage = Stage::Command;
                        continue;
                    }
                    if offset == dictionary.map_or(0, DictionaryRef::prefix_len) {
                        self.stage = if self.distance <= self.max_backward {
                            Stage::Copy
                        } else {
                            Stage::PrefixHistory { offset: 0, start }
                        };
                        continue;
                    }
                    if !output.ready()? {
                        return Ok(Stop::Output);
                    }
                    let byte = dictionary
                        .and_then(|value| value.prefix_byte(offset))
                        .ok_or(DecodeError::InvalidData {
                            kind: InvalidDataKind::DictionaryReference,
                        })?;
                    self.emit(byte, output)?;
                    self.copy -= 1;
                    self.stage = Stage::Prefix {
                        offset: offset + 1,
                        start,
                    };
                }
                Stage::PrefixHistory { offset, start } => {
                    if self.copy == 0 {
                        self.stage = Stage::Command;
                        continue;
                    }
                    if offset == self.prefix_history.len() {
                        self.stage = Stage::Prefix {
                            offset: start,
                            start,
                        };
                        continue;
                    }
                    if !output.ready()? {
                        return Ok(Stop::Output);
                    }
                    self.emit(self.prefix_history[offset], output)?;
                    self.copy -= 1;
                    self.stage = Stage::PrefixHistory {
                        offset: offset + 1,
                        start,
                    };
                }
                Stage::Dictionary => {
                    if self.scratch_pos == self.scratch_len {
                        self.stage = Stage::Command;
                        continue;
                    }
                    if !output.ready()? {
                        return Ok(Stop::Output);
                    }
                    let byte = self.scratch[self.scratch_pos];
                    self.emit(byte, output)?;
                    self.scratch_pos += 1;
                }
                Stage::EndBlock => {
                    self.stage = if self.last { Stage::End } else { Stage::Meta };
                }
                Stage::End => {
                    self.bits.align()?;
                    return Ok(Stop::Member);
                }
            }
        }
    }
}

impl Default for Stream {
    fn default() -> Self {
        Self {
            bits: Bits::default(),
            stage: Stage::default(),
            memory: Memory::default(),
            history: Vec::new(),
            prefix_history: Vec::new(),
            position: 0,
            max_backward: 0,
            window: None,
            large: false,
            last: false,
            remaining: 0,
            blocks: Default::default(),
            builder: Builder::default(),
            modes: [0; 256],
            maps: Default::default(),
            trees: Default::default(),
            distances: DistanceLayout::default(),
            literals: 0,
            copy: 0,
            implicit: false,
            distance: 0,
            distance_code: 0,
            cache: [4, 11, 15, 16],
            previous: [0; 2],
            scratch: [0; SCRATCH_BYTES],
            scratch_pos: 0,
            scratch_len: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dictionary::{DecodeDictionary, DecodeDictionaryLimits, DictionaryAttachment};

    #[test]
    fn prefix_references_continue_through_history_and_overlap() {
        for maximum in [4, 1024] {
            let dictionary = DecodeDictionary::new(
                &[DictionaryAttachment::Raw(b"xy")],
                DecodeDictionaryLimits::default(),
            )
            .unwrap();
            let mut stream = Stream {
                stage: Stage::Resolve,
                max_backward: maximum,
                position: 4,
                history: b"abcd".to_vec(),
                distance: 6,
                copy: 15,
                remaining: 15,
                last: true,
                ..Stream::default()
            };
            stream.memory.live = stream.history.capacity();
            let mut input = Input {
                bytes: &[],
                consumed: 0,
                total_before: 0,
                limit: None,
            };
            let mut decoded = Vec::new();
            loop {
                let mut bytes = [0; 1];
                let mut output = Output {
                    bytes: &mut bytes,
                    produced: 0,
                    total_before: decoded.len() as u64,
                    limit: None,
                    exact: OutputSize::Unknown,
                };
                let result = stream
                    .run(
                        &mut input,
                        &mut output,
                        DecoderConfig::default(),
                        Some((&dictionary).into()),
                    )
                    .unwrap();
                decoded.extend_from_slice(&output.bytes[..output.produced]);
                if result == Stop::Member {
                    break;
                }
                assert_eq!(result, Stop::Output);
            }
            assert_eq!(decoded, b"xyabcdxyabcdxya");
        }
    }
}
