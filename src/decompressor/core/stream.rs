//! Meta-block regeneration: a byte-exact resumable state machine plus a fast
//! path that decodes whole commands while a word-sized refill is possible.
//!
//! The fast path keeps the same state fields as the resumable stages and
//! returns to them at every point where input or output may run short, so
//! any chunking of input and output produces identical results and errors.

use super::super::{DecodeError, DecoderConfig, InvalidDataKind, OutputSize};
use super::{
    bits::{Bits, Input},
    block::Block,
    context_map::ContextMap,
    dictionary,
    distance::DistanceLayout,
    header::{self, MetaBlock},
    huffman::{self, Builder, Group},
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

/// Smallest ring allocation; growth doubles up to the window size.
const MIN_RING: u64 = 64;

const fn lsb6_lut() -> [u8; 512] {
    let mut lut = [0u8; 512];
    let mut i = 0;
    while i < 256 {
        lut[i] = (i & 63) as u8;
        i += 1;
    }
    lut
}

const fn msb6_lut() -> [u8; 512] {
    let mut lut = [0u8; 512];
    let mut i = 0;
    while i < 256 {
        lut[i] = (i >> 2) as u8;
        i += 1;
    }
    lut
}

const CONTEXT_LUT_LSB6: [u8; 512] = lsb6_lut();
const CONTEXT_LUT_MSB6: [u8; 512] = msb6_lut();

/// Context lookup for a literal block's context mode. Each mode combines
/// the two previous bytes as `lut[p1] | lut[256 + p2]`.
const fn context_lut(mode: u8) -> &'static [u8; 512] {
    match mode {
        0 => &CONTEXT_LUT_LSB6,
        1 => &CONTEXT_LUT_MSB6,
        2 => &CONTEXT_LUT_UTF8,
        _ => &CONTEXT_LUT_SIGNED,
    }
}

const CELLS: [usize; 11] = [0, 1, 0, 1, 8, 9, 2, 16, 10, 17, 18];

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
    DistanceExtraHigh(usize, u64),
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

/// Why the fast literal loop stopped before the insert run ended.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum Pause {
    Done,
    Input,
    Output,
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

    /// Largest `produced` this call may reach without per-byte policy checks.
    fn fast_end(&self) -> usize {
        let mut budget = u64::MAX - self.total_before;
        if let Some(limit) = self.limit {
            budget = budget.min(limit.saturating_sub(self.total_before));
        }
        if let OutputSize::Exact(expected) = self.exact {
            budget = budget.min(expected.saturating_sub(self.total_before));
        }
        usize::try_from(budget).map_or(self.bytes.len(), |budget| budget.min(self.bytes.len()))
    }
}

/// Delivers ring bytes decoded since `flushed`. The pending region never
/// crosses the ring end: writers flush whenever a write reaches it.
fn flush_ring(ring: &[u8], position: u64, flushed: &mut u64, output: &mut Output<'_>) {
    let pending = (position - *flushed) as usize;
    if pending != 0 {
        let start = (*flushed & (ring.len() as u64 - 1)) as usize;
        output.bytes[output.produced..output.produced + pending]
            .copy_from_slice(&ring[start..start + pending]);
        output.produced += pending;
        *flushed = position;
    }
}

/// Copies `length` bytes from `distance` back, in ring pieces that neither
/// wrap nor need per-byte handling. The ring already holds `position + length`.
fn copy_ring(
    ring: &mut [u8],
    position: &mut u64,
    distance: u64,
    mut length: usize,
    flushed: &mut u64,
    output: &mut Output<'_>,
) {
    let size = ring.len();
    let mask = size as u64 - 1;
    while length != 0 {
        let dst = (*position & mask) as usize;
        let src = ((*position - distance) & mask) as usize;
        let piece = length.min(size - dst).min(size - src);
        if src < dst && (distance as usize) < piece {
            // Overlapping pattern: each chunk doubles the replicated prefix.
            let mut done = 0;
            let mut chunk = distance as usize;
            while done < piece {
                let n = chunk.min(piece - done);
                ring.copy_within(src..src + n, dst + done);
                done += n;
                chunk <<= 1;
            }
        } else if piece <= 16
            && let Some(&word) = ring.get(src..src + 16).and_then(|s| s.first_chunk::<16>())
            && let Some(target) = ring.get_mut(dst..dst + 16)
        {
            // The slots past `piece` are the format's 16 unreachable bytes
            // ahead of the window, or still unwritten ring space.
            target.copy_from_slice(&word);
        } else {
            ring.copy_within(src..src + piece, dst);
        }
        *position += piece as u64;
        length -= piece;
        if dst + piece == size {
            flush_ring(ring, *position, flushed, output);
        }
    }
}

/// Writes `bytes` at `position` in ring pieces. The ring already holds them.
fn write_ring(
    ring: &mut [u8],
    position: &mut u64,
    mut bytes: &[u8],
    mut flushed: Option<(&mut u64, &mut Output<'_>)>,
) {
    let size = ring.len();
    let mask = size as u64 - 1;
    while !bytes.is_empty() {
        let dst = (*position & mask) as usize;
        let piece = bytes.len().min(size - dst);
        ring[dst..dst + piece].copy_from_slice(&bytes[..piece]);
        bytes = &bytes[piece..];
        *position += piece as u64;
        if dst + piece == size
            && let Some((flushed, output)) = flushed.as_mut()
        {
            flush_ring(ring, *position, flushed, output);
        }
    }
}

#[derive(Debug)]
pub(crate) struct Stream {
    bits: Bits,
    stage: Stage,
    memory: Memory,
    /// Power-of-two history ring, grown on output up to the window size.
    /// Its length never shrinks between operations; stale bytes are never
    /// addressable because references stay within the current position.
    ring: Vec<u8>,
    // A prefix-crossing reference can exceed the sliding window. Preserve only
    // the original history bytes that would be overwritten before being read.
    prefix_history: Vec<u8>,
    position: u64,
    window_size: u64,
    max_backward: u64,
    pub(crate) window: Option<Window>,
    large: bool,
    last: bool,
    remaining: u64,
    blocks: [Block; 3],
    builder: Builder,
    modes: [u8; 256],
    maps: [ContextMap; 2],
    trees: [Group; 3],
    distances: DistanceLayout,
    literals: u64,
    copy: u64,
    implicit: bool,
    distance: u64,
    distance_code: usize,
    cache: [u64; 4],
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
        self.prefix_history.clear();
        self.position = 0;
        self.window = None;
        self.cache = [4, 11, 15, 16];
        self.memory.limit = config.limits().max_workspace_bytes();
    }

    /// Grows the ring so positions below `end` are addressable, doubling up
    /// to the window size. A full ring wraps instead.
    fn ensure_ring(&mut self, end: u64) -> Result<(), DecodeError> {
        let len = self.ring.len() as u64;
        if end <= len || len >= self.window_size {
            return Ok(());
        }
        let desired = end
            .checked_next_power_of_two()
            .unwrap_or(u64::MAX)
            .max(len.saturating_mul(2))
            .max(MIN_RING)
            .min(self.window_size);
        let desired = usize::try_from(desired).map_err(|_| DecodeError::SizeOverflow)?;
        self.memory.resize(&mut self.ring, desired)
    }

    fn ring_mask(&self) -> u64 {
        self.ring.len() as u64 - 1
    }

    fn emit(&mut self, byte: u8, output: &mut Output<'_>) -> Result<(), DecodeError> {
        let next = self
            .position
            .checked_add(1)
            .ok_or(DecodeError::SizeOverflow)?;
        self.ensure_ring(next)?;
        let index = (self.position & self.ring_mask()) as usize;
        self.ring[index] = byte;
        self.position = next;
        output.bytes[output.produced] = byte;
        output.produced += 1;
        self.remaining -= 1;
        Ok(())
    }

    /// The two most recent output bytes, zero before any output.
    fn previous_bytes(&self) -> (u8, u8) {
        if self.position == 0 {
            return (0, 0);
        }
        let mask = self.ring_mask();
        let p1 = self.ring[((self.position - 1) & mask) as usize];
        let p2 = if self.position >= 2 {
            self.ring[((self.position - 2) & mask) as usize]
        } else {
            0
        };
        (p1, p2)
    }

    fn context(&self) -> usize {
        let (p1, p2) = self.previous_bytes();
        let lut = context_lut(self.modes[self.blocks[0].current]);
        usize::from(lut[usize::from(p1)] | lut[256 + usize::from(p2)])
    }

    #[cfg_attr(all(feature = "hotpath", not(feature = "no_std")), hotpath::measure)]
    pub(crate) fn run(
        &mut self,
        input: &mut Input<'_>,
        output: &mut Output<'_>,
        config: DecoderConfig,
        dictionary: Option<DictionaryRef<'_>>,
    ) -> Result<Stop, DecodeError> {
        let result = self.run_stages(input, output, config, dictionary);
        // The fast path may pull whole speculative bytes into the reservoir.
        // Across an input/output pause they persist in `self.bits`, so `consumed`
        // legitimately counts them. At a member boundary the session resets the
        // workspace and discards the reservoir, so any whole bytes belonging to
        // a following member must return to the input to be read again.
        if matches!(result, Ok(Stop::Member)) {
            self.bits.unread(input);
        }
        result
    }

    /// Decodes whole commands while whole-word refills and output space
    /// allow, then leaves the resumable stage the byte-exact path continues from.
    fn fast(
        &mut self,
        input: &mut Input<'_>,
        output: &mut Output<'_>,
        fast_end: usize,
        out_end: usize,
        dictionary: Option<DictionaryRef<'_>>,
    ) -> Result<(), DecodeError> {
        let mut bits = self.bits;
        let mut flushed = self.position;
        macro_rules! pause {
            ($stage:expr) => {{
                flush_ring(&self.ring, self.position, &mut flushed, output);
                self.bits = bits;
                self.stage = $stage;
                return Ok(());
            }};
        }
        macro_rules! check {
            ($result:expr) => {
                match $result {
                    Ok(value) => value,
                    Err(error) => {
                        flush_ring(&self.ring, self.position, &mut flushed, output);
                        self.bits = bits;
                        return Err(error);
                    }
                }
            };
        }
        loop {
            // Deliver bytes decoded so far so `space` reflects true output room.
            // Without this, a ring that never wraps accumulates the whole output
            // as pending and forces every copy onto the byte-exact slow path.
            flush_ring(&self.ring, self.position, &mut flushed, output);
            if self.remaining == 0 || !bits.refill(input, fast_end) {
                pause!(Stage::Command);
            }
            if self.blocks[1].remaining == 0 {
                if !self.blocks[1].switch_ready() {
                    pause!(Stage::Command);
                }
                self.blocks[1].switch_fast(&mut bits);
                if !bits.refill(input, fast_end) {
                    pause!(Stage::Command);
                }
            }
            let symbol =
                huffman::decode_fast(self.trees[1].codes(self.blocks[1].current), &mut bits);
            self.blocks[1].remaining -= 1;
            let cell = CELLS[symbol >> 6];
            let insert = (cell & 24) + ((symbol >> 3) & 7);
            let copy = ((cell << 3) & 24) + (symbol & 7);
            self.implicit = symbol < 128;
            self.literals = u64::from(INS_BASE[insert]) + bits.take(INS_EXTRA[insert]);
            if self.literals > self.remaining {
                check!(Err(InvalidDataKind::MetaBlock.into()));
            }
            if bits.count() < 24 && !bits.refill(input, fast_end) {
                pause!(Stage::CopyExtra(copy));
            }
            self.copy = u64::from(COPY_BASE[copy]) + bits.take(COPY_EXTRA[copy]);
            if self.literals != 0 {
                let end = check!(
                    self.position
                        .checked_add(self.literals)
                        .ok_or(DecodeError::SizeOverflow)
                );
                check!(self.ensure_ring(end));
                match self.fast_literals(&mut bits, input, output, fast_end, out_end, &mut flushed)
                {
                    Pause::Done => {}
                    Pause::Input | Pause::Output => pause!(Stage::Literals),
                }
            }
            if self.remaining == 0 {
                pause!(Stage::Command);
            }
            let distance = if self.implicit {
                self.distance_code = 0;
                self.cache[0]
            } else {
                if bits.count() < 54 && !bits.refill(input, fast_end) {
                    pause!(Stage::Distance);
                }
                if self.blocks[2].remaining == 0 {
                    if !self.blocks[2].switch_ready() {
                        pause!(Stage::Distance);
                    }
                    self.blocks[2].switch_fast(&mut bits);
                    if !bits.refill(input, fast_end) {
                        pause!(Stage::Distance);
                    }
                }
                let context = self.copy.saturating_sub(2).min(3) as usize;
                let tree = usize::from(self.maps[1].values[self.blocks[2].current * 4 + context]);
                let symbol = huffman::decode_fast(self.trees[2].codes(tree), &mut bits);
                self.blocks[2].remaining -= 1;
                self.distance_code = symbol;
                let width = self.distances.extra_bits(symbol);
                let extra = if width > 32 {
                    if input.consumed + 16 > fast_end || !bits.refill(input, fast_end) {
                        pause!(Stage::DistanceExtra(symbol));
                    }
                    let low = bits.take(32);
                    if !bits.refill(input, fast_end) {
                        check!(Err(DecodeError::InternalInvariant));
                    }
                    low | (bits.take(width - 32) << 32)
                } else {
                    bits.take(width)
                };
                check!(self.distances.resolve(symbol, extra, &self.cache))
            };
            self.distance = distance;
            let available = self.position.min(self.max_backward);
            let pending = (self.position - flushed) as usize;
            let space = out_end - output.produced - pending;
            if distance > available {
                let prefix_len = dictionary.map_or(0, DictionaryRef::prefix_len);
                if distance - available <= prefix_len {
                    pause!(Stage::Resolve);
                }
                let context = self.context();
                let length = check!(dictionary::resolve(
                    dictionary,
                    distance - available - prefix_len - 1,
                    self.copy as usize,
                    context,
                    &mut self.scratch,
                ));
                if length as u64 > self.remaining {
                    check!(Err(InvalidDataKind::MetaBlock.into()));
                }
                if length == 0 && distance <= 120 {
                    check!(Err(InvalidDataKind::DictionaryReference.into()));
                }
                self.scratch_len = length;
                self.scratch_pos = 0;
                if length > space {
                    pause!(Stage::Dictionary);
                }
                let end = check!(
                    self.position
                        .checked_add(length as u64)
                        .ok_or(DecodeError::SizeOverflow)
                );
                check!(self.ensure_ring(end));
                write_ring(
                    &mut self.ring,
                    &mut self.position,
                    &self.scratch[..length],
                    Some((&mut flushed, &mut *output)),
                );
                self.remaining -= length as u64;
            } else {
                if self.copy > self.remaining {
                    check!(Err(InvalidDataKind::MetaBlock.into()));
                }
                if self.distance_code != 0 {
                    self.cache.rotate_right(1);
                    self.cache[0] = distance;
                }
                if self.copy > space as u64 {
                    pause!(Stage::Copy);
                }
                let end = check!(
                    self.position
                        .checked_add(self.copy)
                        .ok_or(DecodeError::SizeOverflow)
                );
                check!(self.ensure_ring(end));
                copy_ring(
                    &mut self.ring,
                    &mut self.position,
                    distance,
                    self.copy as usize,
                    &mut flushed,
                    output,
                );
                self.remaining -= self.copy;
            }
        }
    }

    /// Decodes the pending insert run into the ring. The ring already holds
    /// every literal; output space is checked per byte against `out_end`.
    fn fast_literals(
        &mut self,
        bits: &mut Bits,
        input: &mut Input<'_>,
        output: &mut Output<'_>,
        fast_end: usize,
        out_end: usize,
        flushed: &mut u64,
    ) -> Pause {
        let (mut p1, mut p2) = self.previous_bytes();
        let Self {
            ring,
            maps,
            trees,
            blocks,
            modes,
            ..
        } = self;
        let ring = ring.as_mut_slice();
        let size = ring.len();
        let mask = size as u64 - 1;
        let map = maps[0].values.as_slice();
        let group = &trees[0];
        let block = &mut blocks[0];
        let mut position = self.position;
        let mut literals = self.literals;
        let mut remaining = self.remaining;
        let mut index = (position & mask) as usize;
        let mut block_remaining = block.remaining;
        let mut lut = context_lut(modes[block.current]);
        let mut map_base = block.current * 64;
        let mut space = out_end - output.produced - (position - *flushed) as usize;
        let outcome = loop {
            if literals == 0 {
                break Pause::Done;
            }
            if space == 0 {
                break Pause::Output;
            }
            if bits.count() < 15 && !bits.refill(input, fast_end) {
                break Pause::Input;
            }
            if block_remaining == 0 {
                if !block.switch_ready() || (bits.count() < 54 && !bits.refill(input, fast_end)) {
                    break Pause::Input;
                }
                block.remaining = 0;
                block.switch_fast(bits);
                block_remaining = block.remaining;
                lut = context_lut(modes[block.current]);
                map_base = block.current * 64;
                continue;
            }
            if index == size {
                flush_ring(ring, position, flushed, output);
                index = 0;
            }
            let context = usize::from(lut[usize::from(p1)] | lut[256 + usize::from(p2)]);
            let tree = usize::from(map[map_base + context]);
            let byte = huffman::decode_fast(group.codes(tree), bits) as u8;
            ring[index] = byte;
            index += 1;
            position += 1;
            literals -= 1;
            remaining -= 1;
            block_remaining -= 1;
            space -= 1;
            p2 = p1;
            p1 = byte;
        };
        block.remaining = block_remaining;
        self.position = position;
        self.literals = literals;
        self.remaining = remaining;
        outcome
    }

    fn run_stages(
        &mut self,
        input: &mut Input<'_>,
        output: &mut Output<'_>,
        config: DecoderConfig,
        dictionary: Option<DictionaryRef<'_>>,
    ) -> Result<Stop, DecodeError> {
        let fast_end = input.fast_end();
        let out_end = output.fast_end();
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
                    self.window_size = 1u64 << window.bits();
                    self.max_backward = self.window_size - 16;
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
                            for block in &mut self.blocks {
                                block.reset();
                            }
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
                    if self.bits.count() >= 8 {
                        self.bits.take(8);
                        self.remaining -= 1;
                        continue;
                    }
                    let skip = self.remaining.min((fast_end - input.consumed) as u64) as usize;
                    if skip == 0 {
                        read!(8);
                        self.remaining -= 1;
                        continue;
                    }
                    input.consumed += skip;
                    self.remaining -= skip as u64;
                }
                Stage::Raw => {
                    if self.remaining == 0 {
                        self.stage = Stage::EndBlock;
                        continue;
                    }
                    if !output.ready()? {
                        return Ok(Stop::Output);
                    }
                    if self.bits.count() >= 8 {
                        let byte = self.bits.take(8) as u8;
                        self.emit(byte, output)?;
                        continue;
                    }
                    let count = self
                        .remaining
                        .min((fast_end - input.consumed) as u64)
                        .min((out_end - output.produced) as u64)
                        as usize;
                    if count == 0 {
                        let byte = read!(8) as u8;
                        self.emit(byte, output)?;
                        continue;
                    }
                    let end = self
                        .position
                        .checked_add(count as u64)
                        .ok_or(DecodeError::SizeOverflow)?;
                    self.ensure_ring(end)?;
                    let bytes = &input.bytes[input.consumed..input.consumed + count];
                    output.bytes[output.produced..output.produced + count].copy_from_slice(bytes);
                    write_ring(&mut self.ring, &mut self.position, bytes, None);
                    input.consumed += count;
                    output.produced += count;
                    self.remaining -= count as u64;
                }
                Stage::Blocks(i) => {
                    if !self.blocks[i].header(
                        &mut self.bits,
                        input,
                        &mut self.builder,
                        &mut self.memory,
                    )? {
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
                        let counts = [self.maps[0].trees, self.blocks[1].count, self.maps[1].trees];
                        let alphabets = [256, 704, self.distances.alphabet()];
                        for (group, (count, alphabet)) in
                            self.trees.iter_mut().zip(counts.into_iter().zip(alphabets))
                        {
                            group.prepare(count, alphabet, &mut self.memory)?;
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
                    if !self
                        .builder
                        .read(alphabet, &mut self.bits, input, &mut self.memory)?
                    {
                        return Ok(Stop::Input);
                    }
                    let max_symbol =
                        self.builder
                            .build_slot(alphabet, &mut self.trees[group], index)?;
                    if group == 2 {
                        self.distances.validate_symbol(max_symbol)?;
                    }
                    self.stage = if index + 1 < self.trees[group].count() {
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
                    self.fast(input, output, fast_end, out_end, dictionary)?;
                    if !matches!(self.stage, Stage::Command) {
                        continue;
                    }
                    if self.remaining == 0 {
                        self.stage = Stage::EndBlock;
                        continue;
                    }
                    if !self.blocks[1].prepare(&mut self.bits, input)? {
                        return Ok(Stop::Input);
                    }
                    let Some(symbol) = huffman::decode(
                        self.trees[1].codes(self.blocks[1].current),
                        &mut self.bits,
                        input,
                    )?
                    else {
                        return Ok(Stop::Input);
                    };
                    self.blocks[1].advance();
                    let cell = CELLS[symbol >> 6];
                    let insert = (cell & 24) + ((symbol >> 3) & 7);
                    let copy = ((cell << 3) & 24) + (symbol & 7);
                    self.implicit = symbol < 128;
                    self.stage = Stage::InsertExtra(insert, copy);
                }
                Stage::InsertExtra(insert, copy) => {
                    self.literals = u64::from(INS_BASE[insert]) + read!(INS_EXTRA[insert]);
                    if self.literals > self.remaining {
                        return Err(InvalidDataKind::MetaBlock.into());
                    }
                    self.stage = Stage::CopyExtra(copy);
                }
                Stage::CopyExtra(copy) => {
                    self.copy = u64::from(COPY_BASE[copy]) + read!(COPY_EXTRA[copy]);
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
                    let Some(byte) =
                        huffman::decode(self.trees[0].codes(tree), &mut self.bits, input)?
                    else {
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
                    let Some(symbol) =
                        huffman::decode(self.trees[2].codes(tree), &mut self.bits, input)?
                    else {
                        return Ok(Stop::Input);
                    };
                    self.blocks[2].advance();
                    self.distance_code = symbol;
                    self.stage = Stage::DistanceExtra(symbol);
                }
                Stage::DistanceExtra(symbol) => {
                    let width = self.distances.extra_bits(symbol);
                    if width > 32 {
                        let low = read!(32);
                        self.stage = Stage::DistanceExtraHigh(symbol, low);
                        continue;
                    }
                    let extra = read!(width);
                    self.distance = self.distances.resolve(symbol, extra, &self.cache)?;
                    self.stage = Stage::Resolve;
                }
                Stage::DistanceExtraHigh(symbol, low) => {
                    let high = read!(self.distances.extra_bits(symbol) - 32);
                    let extra = low | (high << 32);
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
                            if length != 0 {
                                let mask = self.ring_mask();
                                let start = self.position - available;
                                for (i, byte) in self.prefix_history.iter_mut().enumerate() {
                                    *byte = self.ring[((start + i as u64) & mask) as usize];
                                }
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
                        let context = self.context();
                        self.scratch_len = dictionary::resolve(
                            dictionary,
                            self.distance - available - prefix_len - 1,
                            self.copy as usize,
                            context,
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
                    // Bulk-copy the run even when the fast path could not run
                    // (for example near the end of a small, highly expanding
                    // input). `out_end` already folds in slice length, output
                    // limits and the exact-size contract.
                    let n = self.copy.min((out_end - output.produced) as u64);
                    let end = self
                        .position
                        .checked_add(n)
                        .ok_or(DecodeError::SizeOverflow)?;
                    self.ensure_ring(end)?;
                    let mut flushed = self.position;
                    copy_ring(
                        &mut self.ring,
                        &mut self.position,
                        self.distance,
                        n as usize,
                        &mut flushed,
                        output,
                    );
                    flush_ring(&self.ring, self.position, &mut flushed, output);
                    self.copy -= n;
                    self.remaining -= n;
                    if self.copy != 0 {
                        output.ready()?;
                        return Ok(Stop::Output);
                    }
                    self.stage = Stage::Command;
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
                    // Emit a contiguous prefix run rather than one byte at a
                    // time. The run stops at its segment end, the copy length,
                    // the prefix end and the current output room.
                    let run = dictionary.map_or(&[][..], |value| value.prefix_run(offset));
                    if run.is_empty() {
                        return Err(InvalidDataKind::DictionaryReference.into());
                    }
                    let n = (run.len() as u64)
                        .min(self.copy)
                        .min((out_end - output.produced) as u64)
                        as usize;
                    let end = self
                        .position
                        .checked_add(n as u64)
                        .ok_or(DecodeError::SizeOverflow)?;
                    self.ensure_ring(end)?;
                    let mut flushed = self.position;
                    write_ring(
                        &mut self.ring,
                        &mut self.position,
                        &run[..n],
                        Some((&mut flushed, &mut *output)),
                    );
                    flush_ring(&self.ring, self.position, &mut flushed, output);
                    self.copy -= n as u64;
                    self.remaining -= n as u64;
                    self.stage = Stage::Prefix {
                        offset: offset + n as u64,
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
            ring: Vec::new(),
            prefix_history: Vec::new(),
            position: 0,
            window_size: 0,
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
        for (window_size, maximum) in [(8u64, 4u64), (1024, 1008)] {
            let dictionary = DecodeDictionary::new(
                &[DictionaryAttachment::Raw(b"xy")],
                DecodeDictionaryLimits::default(),
            )
            .unwrap();
            let mut ring = b"abcd".to_vec();
            ring.resize(8, 0);
            let mut stream = Stream {
                stage: Stage::Resolve,
                window_size,
                max_backward: maximum,
                position: 4,
                ring,
                distance: 6,
                copy: 15,
                remaining: 15,
                last: true,
                ..Stream::default()
            };
            stream.memory.live = stream.ring.capacity();
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

    #[test]
    fn ring_copies_replicate_patterns_and_wrap_without_per_byte_work() {
        // Reference: byte-at-a-time copy through a masked ring.
        let mut rng = 0x2545_f491_4f6c_dd1du64;
        let mut random = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        for size in [64usize, 256] {
            for _ in 0..400 {
                let mut expected = alloc::vec![0u8; size];
                for byte in &mut expected {
                    *byte = random() as u8;
                }
                let mut ring = expected.clone();
                let position = 3 * size as u64 + (random() % size as u64);
                let distance = 1 + random() % (size as u64 - 16);
                let length = 1 + (random() as usize) % 100;
                let mut reference_position = position;
                let mut produced = alloc::vec![0u8; length];
                for byte in &mut produced {
                    let source =
                        expected[((reference_position - distance) & (size as u64 - 1)) as usize];
                    expected[(reference_position & (size as u64 - 1)) as usize] = source;
                    // The decoder emits each byte before its ring slot is reused.
                    *byte = source;
                    reference_position += 1;
                }
                let mut sink = alloc::vec![0u8; length];
                let mut output = Output {
                    bytes: &mut sink,
                    produced: 0,
                    total_before: 0,
                    limit: None,
                    exact: OutputSize::Unknown,
                };
                let mut fast_position = position;
                let mut flushed = position;
                copy_ring(
                    &mut ring,
                    &mut fast_position,
                    distance,
                    length,
                    &mut flushed,
                    &mut output,
                );
                flush_ring(&ring, fast_position, &mut flushed, &mut output);
                assert_eq!(fast_position, reference_position);
                assert_eq!(output.produced, length);
                // Only the copied bytes and the 16 unreachable slots ahead may differ.
                for (i, (a, b)) in ring.iter().zip(&expected).enumerate() {
                    let ahead = (i as u64).wrapping_sub(fast_position) & (size as u64 - 1);
                    assert!(a == b || ahead < 16, "slot {i} differs");
                }
                assert_eq!(sink, produced);
            }
        }
    }

    #[test]
    fn context_lookup_tables_match_the_format_modes() {
        // Modes 0 and 1 are the low and high six bits of the previous byte.
        assert_eq!(context_lut(0)[0xff], 63);
        assert_eq!(context_lut(0)[256 + 0xff], 0);
        assert_eq!(context_lut(1)[0xff], 63);
        assert_eq!(context_lut(1)[0x04], 1);
        // Modes 2 and 3 are the shared UTF8 and signed tables verbatim.
        assert_eq!(context_lut(2) as &[u8], CONTEXT_LUT_UTF8.as_slice());
        assert_eq!(context_lut(3) as &[u8], CONTEXT_LUT_SIGNED.as_slice());
        // A stream with no output yet has zero context in every mode.
        let stream = Stream::default();
        assert_eq!(stream.previous_bytes(), (0, 0));
        assert_eq!(stream.context(), 0);
    }
}
