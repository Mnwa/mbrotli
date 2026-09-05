//! Greedy block splitting.
//!
//! Ports `c/enc/metablock_inc.h` and the `ContextBlockSplitter` of
//! `c/enc/metablock.c` from the pinned reference (`google/brotli` v1.2.0,
//! commit `028fb5a`).
//!
//! A splitter consumes symbols in order and decides, every time it has
//! collected a target number of them, whether the block it just gathered is
//! different enough from the previous one to deserve its own prefix code. The
//! decision is a comparison of estimated entropies, so the arithmetic and the
//! order of the comparisons are part of the format contract.

use crate::compressor::core::shared::block_split::{BlockSplit, MAX_NUMBER_OF_BLOCK_TYPES};
use crate::compressor::core::shared::format::MAX_STATIC_CONTEXTS;
use crate::compressor::core::shared::histogram::{Histogram, HistogramLiteral, bits_entropy};

/// How much better the second-last block has to look to be reused.
const SECOND_LAST_ADVANTAGE: f64 = 20.0;

/// Greedy splitter for one symbol stream (`BlockSplitter`).
pub(crate) struct BlockSplitter<const N: usize> {
    alphabet_size: usize,
    min_block_size: usize,
    split_threshold: f64,
    num_blocks: usize,
    /// The split being built.
    pub(crate) split: BlockSplit,
    /// One histogram per block type, plus the one being gathered.
    pub(crate) histograms: Vec<Histogram<N>>,
    histograms_size: usize,
    combined: [Histogram<N>; 2],
    target_block_size: usize,
    block_size: usize,
    curr_histogram_ix: usize,
    last_histogram_ix: [usize; 2],
    last_entropy: [f64; 2],
    merge_last_count: usize,
}

impl<const N: usize> BlockSplitter<N> {
    /// Creates a splitter for `num_symbols` symbols (`InitBlockSplitter`).
    ///
    /// `min_block_size` is the smallest block the splitter will emit and
    /// `split_threshold` how much entropy a new block type has to save.
    #[cfg(test)]
    pub(crate) fn new(
        alphabet_size: usize,
        min_block_size: usize,
        split_threshold: f64,
        num_symbols: usize,
    ) -> Self {
        Self::with_storage(
            alphabet_size,
            min_block_size,
            split_threshold,
            num_symbols,
            BlockSplit::default(),
            Vec::new(),
        )
    }

    /// Initializes a splitter using the previous meta-block's backing buffers.
    pub(crate) fn with_storage(
        alphabet_size: usize,
        min_block_size: usize,
        split_threshold: f64,
        num_symbols: usize,
        mut split: BlockSplit,
        mut histograms: Vec<Histogram<N>>,
    ) -> Self {
        let max_num_blocks = num_symbols / min_block_size + 1;
        // One more than the maximum number of block types, for the histogram
        // still being gathered when a meta-block runs out of types.
        let max_num_types = max_num_blocks.min(MAX_NUMBER_OF_BLOCK_TYPES + 1);
        split.reserve(max_num_blocks);
        split.num_types = 0;
        histograms.clear();
        histograms.resize(max_num_types + 1, Histogram::default());
        Self {
            alphabet_size,
            min_block_size,
            split_threshold,
            num_blocks: 0,
            split,
            histograms,
            histograms_size: max_num_types,
            combined: [Histogram::default(), Histogram::default()],
            target_block_size: min_block_size,
            block_size: 0,
            curr_histogram_ix: 0,
            last_histogram_ix: [0, 0],
            last_entropy: [0.0, 0.0],
            merge_last_count: 0,
        }
    }

    /// Counts one symbol, closing the block when it is full.
    #[inline(always)]
    pub(crate) fn add_symbol(&mut self, symbol: usize) {
        if let Some(histogram) = self.histograms.get_mut(self.curr_histogram_ix) {
            histogram.add(symbol);
        }
        self.block_size += 1;
        if self.block_size == self.target_block_size {
            self.finish_block(false);
        }
    }

    /// Advances to the next histogram slot, clearing it if it exists.
    fn advance_histogram(&mut self) {
        self.curr_histogram_ix += 1;
        if self.curr_histogram_ix < self.histograms_size
            && let Some(histogram) = self.histograms.get_mut(self.curr_histogram_ix)
        {
            histogram.clear();
        }
    }

    /// Decides what to do with the block that has just been gathered.
    ///
    /// Mirrors `BlockSplitterFinishBlock`: it either opens a new block type,
    /// reuses the second-last one, or merges into the last one.
    pub(crate) fn finish_block(&mut self, is_final: bool) {
        self.block_size = self.block_size.max(self.min_block_size);
        if self.num_blocks == 0 {
            self.split.lengths[0] = self.block_size as u32;
            self.split.types[0] = 0;
            self.last_entropy[0] = bits_entropy(&self.histograms[0].data[..self.alphabet_size]);
            self.last_entropy[1] = self.last_entropy[0];
            self.num_blocks += 1;
            self.split.num_types += 1;
            self.advance_histogram();
            self.block_size = 0;
        } else if self.block_size > 0 {
            let entropy =
                bits_entropy(&self.histograms[self.curr_histogram_ix].data[..self.alphabet_size]);
            let mut combined_entropy = [0.0f64; 2];
            let mut diff = [0.0f64; 2];
            for j in 0..2 {
                let last = self.last_histogram_ix[j];
                self.combined[j] = self.histograms[self.curr_histogram_ix].clone();
                let other = self.histograms[last].clone();
                self.combined[j].add_histogram(&other);
                combined_entropy[j] = bits_entropy(&self.combined[j].data[..self.alphabet_size]);
                diff[j] = combined_entropy[j] - entropy - self.last_entropy[j];
            }

            if self.split.num_types < MAX_NUMBER_OF_BLOCK_TYPES
                && diff[0] > self.split_threshold
                && diff[1] > self.split_threshold
            {
                // The block looks nothing like either neighbour: give it a
                // type of its own.
                self.split.lengths[self.num_blocks] = self.block_size as u32;
                self.split.types[self.num_blocks] = self.split.num_types as u8;
                self.last_histogram_ix[1] = self.last_histogram_ix[0];
                // The reference narrows this to a byte, which only matters at
                // the very last usable type.
                self.last_histogram_ix[0] = self.split.num_types as u8 as usize;
                self.last_entropy[1] = self.last_entropy[0];
                self.last_entropy[0] = entropy;
                self.num_blocks += 1;
                self.split.num_types += 1;
                self.advance_histogram();
                self.block_size = 0;
                self.merge_last_count = 0;
                self.target_block_size = self.min_block_size;
            } else if diff[1] < diff[0] - SECOND_LAST_ADVANTAGE {
                // It looks like the block before last: reuse that type.
                self.split.lengths[self.num_blocks] = self.block_size as u32;
                self.split.types[self.num_blocks] = self.split.types[self.num_blocks - 2];
                self.last_histogram_ix.swap(0, 1);
                self.histograms[self.last_histogram_ix[0]] = self.combined[1].clone();
                self.last_entropy[1] = self.last_entropy[0];
                self.last_entropy[0] = combined_entropy[1];
                self.num_blocks += 1;
                self.block_size = 0;
                if let Some(histogram) = self.histograms.get_mut(self.curr_histogram_ix) {
                    histogram.clear();
                }
                self.merge_last_count = 0;
                self.target_block_size = self.min_block_size;
            } else {
                // It looks like the last block: extend it, and gather more
                // symbols next time before asking again.
                self.split.lengths[self.num_blocks - 1] += self.block_size as u32;
                self.histograms[self.last_histogram_ix[0]] = self.combined[0].clone();
                self.last_entropy[0] = combined_entropy[0];
                if self.split.num_types == 1 {
                    self.last_entropy[1] = self.last_entropy[0];
                }
                self.block_size = 0;
                if let Some(histogram) = self.histograms.get_mut(self.curr_histogram_ix) {
                    histogram.clear();
                }
                self.merge_last_count += 1;
                if self.merge_last_count > 1 {
                    self.target_block_size += self.min_block_size;
                }
            }
        }
        if is_final {
            self.histograms_size = self.split.num_types;
            self.histograms.truncate(self.split.num_types);
            self.split.num_blocks = self.num_blocks;
            self.split.types.truncate(self.num_blocks);
            self.split.lengths.truncate(self.num_blocks);
        }
    }
}

/// Greedy splitter for literals gathered per context (`ContextBlockSplitter`).
///
/// Each block type owns `num_contexts` histograms, and the split decision is
/// made on the total entropy change across all of them.
pub(crate) struct ContextBlockSplitter {
    alphabet_size: usize,
    num_contexts: usize,
    max_block_types: usize,
    min_block_size: usize,
    split_threshold: f64,
    num_blocks: usize,
    /// The split being built.
    pub(crate) split: BlockSplit,
    /// `num_contexts` histograms per block type, in type-major order.
    pub(crate) histograms: Vec<HistogramLiteral>,
    histograms_size: usize,
    target_block_size: usize,
    block_size: usize,
    curr_histogram_ix: usize,
    last_histogram_ix: [usize; 2],
    last_entropy: [f64; 2 * MAX_STATIC_CONTEXTS],
    merge_last_count: usize,
}

impl ContextBlockSplitter {
    /// Creates a splitter over `num_contexts` contexts.
    ///
    /// Mirrors `InitContextBlockSplitter`.
    #[cfg(test)]
    pub(crate) fn new(
        alphabet_size: usize,
        num_contexts: usize,
        min_block_size: usize,
        split_threshold: f64,
        num_symbols: usize,
    ) -> Self {
        Self::with_storage(
            alphabet_size,
            num_contexts,
            min_block_size,
            split_threshold,
            num_symbols,
            BlockSplit::default(),
            Vec::new(),
        )
    }

    /// Initializes context splitting with retained output storage.
    pub(crate) fn with_storage(
        alphabet_size: usize,
        num_contexts: usize,
        min_block_size: usize,
        split_threshold: f64,
        num_symbols: usize,
        mut split: BlockSplit,
        mut histograms: Vec<HistogramLiteral>,
    ) -> Self {
        let max_num_blocks = num_symbols / min_block_size + 1;
        let max_block_types = MAX_NUMBER_OF_BLOCK_TYPES / num_contexts;
        let max_num_types = max_num_blocks.min(max_block_types + 1);
        split.reserve(max_num_blocks);
        split.num_types = 0;
        let histograms_size = max_num_types * num_contexts;
        histograms.clear();
        histograms.resize(histograms_size + num_contexts, HistogramLiteral::default());
        Self {
            alphabet_size,
            num_contexts,
            max_block_types,
            min_block_size,
            split_threshold,
            num_blocks: 0,
            split,
            histograms,
            histograms_size,
            target_block_size: min_block_size,
            block_size: 0,
            curr_histogram_ix: 0,
            last_histogram_ix: [0, 0],
            last_entropy: [0.0; 2 * MAX_STATIC_CONTEXTS],
            merge_last_count: 0,
        }
    }

    /// Counts one literal in `context`, closing the block when it is full.
    #[inline(always)]
    pub(crate) fn add_symbol(&mut self, symbol: usize, context: usize) {
        if let Some(histogram) = self.histograms.get_mut(self.curr_histogram_ix + context) {
            histogram.add(symbol);
        }
        self.block_size += 1;
        if self.block_size == self.target_block_size {
            self.finish_block(false);
        }
    }

    /// Advances past this block type's histograms, clearing the next set.
    fn advance_histograms(&mut self) {
        self.curr_histogram_ix += self.num_contexts;
        if self.curr_histogram_ix < self.histograms_size {
            for index in 0..self.num_contexts {
                if let Some(histogram) = self.histograms.get_mut(self.curr_histogram_ix + index) {
                    histogram.clear();
                }
            }
        }
    }

    /// Decides what to do with the block that has just been gathered.
    ///
    /// Mirrors `ContextBlockSplitterFinishBlock`.
    pub(crate) fn finish_block(&mut self, is_final: bool) {
        let contexts = self.num_contexts;
        self.block_size = self.block_size.max(self.min_block_size);
        if self.num_blocks == 0 {
            self.split.lengths[0] = self.block_size as u32;
            self.split.types[0] = 0;
            for index in 0..contexts {
                self.last_entropy[index] =
                    bits_entropy(&self.histograms[index].data[..self.alphabet_size]);
                self.last_entropy[contexts + index] = self.last_entropy[index];
            }
            self.num_blocks += 1;
            self.split.num_types += 1;
            self.advance_histograms();
            self.block_size = 0;
        } else if self.block_size > 0 {
            let mut entropy = [0.0f64; MAX_STATIC_CONTEXTS];
            let mut combined = core::array::from_fn::<_, { 2 * MAX_STATIC_CONTEXTS }, _>(|_| {
                HistogramLiteral::default()
            });
            let mut combined_entropy = [0.0f64; 2 * MAX_STATIC_CONTEXTS];
            let mut diff = [0.0f64; 2];
            for (index, entropy) in entropy.iter_mut().enumerate().take(contexts) {
                let curr = self.curr_histogram_ix + index;
                *entropy = bits_entropy(&self.histograms[curr].data[..self.alphabet_size]);
                for (j, diff) in diff.iter_mut().enumerate() {
                    let jx = j * contexts + index;
                    let last = self.last_histogram_ix[j] + index;
                    combined[jx] = self.histograms[curr].clone();
                    let other = self.histograms[last].clone();
                    combined[jx].add_histogram(&other);
                    combined_entropy[jx] = bits_entropy(&combined[jx].data[..self.alphabet_size]);
                    *diff += combined_entropy[jx] - *entropy - self.last_entropy[jx];
                }
            }

            if self.split.num_types < self.max_block_types
                && diff[0] > self.split_threshold
                && diff[1] > self.split_threshold
            {
                self.split.lengths[self.num_blocks] = self.block_size as u32;
                self.split.types[self.num_blocks] = self.split.num_types as u8;
                self.last_histogram_ix[1] = self.last_histogram_ix[0];
                self.last_histogram_ix[0] = self.split.num_types * contexts;
                for (index, &entropy) in entropy.iter().enumerate().take(contexts) {
                    self.last_entropy[contexts + index] = self.last_entropy[index];
                    self.last_entropy[index] = entropy;
                }
                self.num_blocks += 1;
                self.split.num_types += 1;
                self.advance_histograms();
                self.block_size = 0;
                self.merge_last_count = 0;
                self.target_block_size = self.min_block_size;
            } else if diff[1] < diff[0] - SECOND_LAST_ADVANTAGE {
                self.split.lengths[self.num_blocks] = self.block_size as u32;
                self.split.types[self.num_blocks] = self.split.types[self.num_blocks - 2];
                self.last_histogram_ix.swap(0, 1);
                for index in 0..contexts {
                    self.histograms[self.last_histogram_ix[0] + index] =
                        combined[contexts + index].clone();
                    self.last_entropy[contexts + index] = self.last_entropy[index];
                    self.last_entropy[index] = combined_entropy[contexts + index];
                    if let Some(histogram) = self.histograms.get_mut(self.curr_histogram_ix + index)
                    {
                        histogram.clear();
                    }
                }
                self.num_blocks += 1;
                self.block_size = 0;
                self.merge_last_count = 0;
                self.target_block_size = self.min_block_size;
            } else {
                self.split.lengths[self.num_blocks - 1] += self.block_size as u32;
                for index in 0..contexts {
                    self.histograms[self.last_histogram_ix[0] + index] = combined[index].clone();
                    self.last_entropy[index] = combined_entropy[index];
                    if self.split.num_types == 1 {
                        self.last_entropy[contexts + index] = self.last_entropy[index];
                    }
                    if let Some(histogram) = self.histograms.get_mut(self.curr_histogram_ix + index)
                    {
                        histogram.clear();
                    }
                }
                self.block_size = 0;
                self.merge_last_count += 1;
                if self.merge_last_count > 1 {
                    self.target_block_size += self.min_block_size;
                }
            }
        }
        if is_final {
            self.histograms_size = self.split.num_types * contexts;
            self.histograms.truncate(self.histograms_size);
            self.split.num_blocks = self.num_blocks;
            self.split.types.truncate(self.num_blocks);
            self.split.lengths.truncate(self.num_blocks);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compressor::core::shared::constants::NUM_LITERAL_SYMBOLS;

    /// Runs a plain splitter over `symbols` and returns the finished split.
    fn split_literals(symbols: &[u8]) -> BlockSplitter<NUM_LITERAL_SYMBOLS> {
        let mut splitter =
            BlockSplitter::<NUM_LITERAL_SYMBOLS>::new(256, 512, 400.0, symbols.len());
        for &symbol in symbols {
            splitter.add_symbol(usize::from(symbol));
        }
        splitter.finish_block(true);
        splitter
    }

    #[test]
    fn a_uniform_stream_stays_one_block() {
        let symbols: Vec<u8> = (0..4000u32).map(|i| (i % 7) as u8).collect();
        let splitter = split_literals(&symbols);
        assert_eq!(splitter.split.num_types, 1);
        assert_eq!(splitter.split.num_blocks, 1);
        assert_eq!(splitter.histograms.len(), 1);
    }

    #[test]
    fn block_lengths_sum_to_the_symbol_count() {
        for length in [0usize, 1, 511, 512, 513, 5000, 20_000] {
            let symbols: Vec<u8> = (0..length)
                .map(|i| if i % 3000 < 1500 { (i % 5) as u8 } else { 200 })
                .collect();
            let splitter = split_literals(&symbols);
            let total: u64 = splitter.split.lengths.iter().map(|&l| u64::from(l)).sum();
            // The first block is padded up to the minimum block size, so a
            // short stream reports at least that many symbols.
            assert!(total >= length as u64);
            assert_eq!(
                splitter.split.types.len(),
                splitter.split.num_blocks,
                "length {length}"
            );
            assert_eq!(splitter.split.lengths.len(), splitter.split.num_blocks);
        }
    }

    #[test]
    fn a_stream_that_changes_alphabet_is_split() {
        let mut symbols = vec![b'a'; 8000];
        symbols.extend((0..8000u32).map(|i| (i % 251) as u8).collect::<Vec<u8>>());
        symbols.extend(vec![b'a'; 8000]);
        let splitter = split_literals(&symbols);
        assert!(
            splitter.split.num_types > 1,
            "the splitter kept one type for a stream that changes character"
        );
        assert_eq!(splitter.histograms.len(), splitter.split.num_types);
    }

    #[test]
    fn every_block_type_is_within_the_type_count() {
        let mut symbols = Vec::new();
        for round in 0..40u32 {
            let filler = if round % 2 == 0 { 3u8 } else { 250 };
            symbols.extend(std::iter::repeat_n(filler, 700));
        }
        let splitter = split_literals(&symbols);
        assert!(
            splitter
                .split
                .types
                .iter()
                .all(|&t| usize::from(t) < splitter.split.num_types)
        );
        assert!(splitter.split.num_types <= MAX_NUMBER_OF_BLOCK_TYPES);
    }

    #[test]
    fn the_context_splitter_keeps_one_histogram_set_per_type() {
        let mut splitter = ContextBlockSplitter::new(256, 3, 512, 400.0, 6000);
        for index in 0..6000usize {
            splitter.add_symbol(index % 251, index % 3);
        }
        splitter.finish_block(true);
        assert_eq!(splitter.histograms.len(), splitter.split.num_types * 3);
        assert!(splitter.split.num_types >= 1);
    }

    #[test]
    fn the_context_splitter_separates_a_changing_stream() {
        let mut splitter = ContextBlockSplitter::new(256, 2, 512, 400.0, 24_000);
        for index in 0..8000usize {
            splitter.add_symbol(b'a' as usize, index % 2);
        }
        for index in 0..8000usize {
            splitter.add_symbol(index % 251, index % 2);
        }
        for index in 0..8000usize {
            splitter.add_symbol(b'a' as usize, index % 2);
        }
        splitter.finish_block(true);
        assert!(splitter.split.num_types > 1);
        assert_eq!(splitter.histograms.len(), splitter.split.num_types * 2);
    }

    #[test]
    fn a_context_splitter_caps_types_by_the_context_count() {
        let splitter = ContextBlockSplitter::new(256, 13, 512, 400.0, 1000);
        assert_eq!(splitter.max_block_types, MAX_NUMBER_OF_BLOCK_TYPES / 13);
    }

    #[test]
    fn a_fresh_split_reserves_room_for_every_block() {
        let mut split = BlockSplit::default();
        split.reserve(5);
        assert_eq!(split.num_blocks, 5);
        assert_eq!(split.types.len(), 5);
        assert_eq!(split.lengths.len(), 5);
    }
}
