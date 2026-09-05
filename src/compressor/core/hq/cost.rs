//! What the dynamic program believes each command would cost.
//!
//! Ports `ZopfliCostModel` and its two initialisers from
//! `c/enc/backward_references_hq.c` of the pinned reference (`google/brotli`
//! v1.2.0, commit `028fb5a`).
//!
//! Everything here is `f32`, in the reference's operation order, because the
//! dynamic program compares these numbers with a strict `<` and a difference in
//! the last bit changes which command it picks. Two details carry that weight
//! and are easy to lose in translation:
//!
//! * `FastLog2` returns a `double` in the reference and is narrowed to `f32`
//!   at each use, so the intermediate is computed at full width and rounded
//!   once.
//! * The cumulative literal costs are built with an explicit carry, which
//!   recovers the precision a running `f32` sum would throw away. The carry is
//!   not an optimisation: remove it and the prices drift, and with them the
//!   chosen commands.

use super::literal_cost::{LiteralCostArena, estimate_bit_costs_for_literals};
use super::nodes::INFINITY;
use crate::compressor::core::shared::command::Command;
use crate::compressor::core::shared::constants::{NUM_COMMAND_SYMBOLS, NUM_LITERAL_SYMBOLS};
use crate::compressor::core::shared::distance::NUM_HISTOGRAM_DISTANCE_SYMBOLS;
use crate::compressor::core::shared::fast_log::fast_log2;

/// Prior the literal-cost model uses for command symbols: `log2(11 + symbol)`.
const COMMAND_PRIOR_OFFSET: usize = 11;

/// Prior the literal-cost model uses for distance symbols: `log2(20 + symbol)`.
const DISTANCE_PRIOR_OFFSET: usize = 20;

/// Turns a histogram into per-symbol costs (`SetCost`).
///
/// A symbol that never occurred is priced as if it had, at the cost of two
/// extra bits; for a non-literal histogram the missing symbols are also counted
/// into the total first, which makes an unused symbol dearer the more of them
/// there are.
fn set_cost(histogram: &[u32], literal_histogram: bool, cost: &mut [f32]) {
    let sum: usize = histogram.iter().map(|&count| count as usize).sum();
    let log2sum = fast_log2(sum) as f32;

    let mut missing_symbol_sum = sum;
    if !literal_histogram {
        missing_symbol_sum += histogram.iter().filter(|&&count| count == 0).count();
    }
    let missing_symbol_cost = fast_log2(missing_symbol_sum) as f32 + 2.0;

    for (slot, &count) in cost.iter_mut().zip(histogram) {
        if count == 0 {
            *slot = missing_symbol_cost;
            continue;
        }
        // Shannon bits for this symbol, floored at one: no prefix code can
        // spend less.
        let bits = log2sum - fast_log2(count as usize) as f32;
        *slot = if bits < 1.0 { 1.0 } else { bits };
    }
}

/// The prices the dynamic program decides on (`ZopfliCostModel`).
pub(crate) struct ZopfliCostModel {
    cost_cmd: Vec<f32>,
    cost_dist: Vec<f32>,
    /// Cumulative literal cost: `literal_costs[to] - literal_costs[from]` is
    /// what coding `from..to` as literals would cost.
    literal_costs: Vec<f32>,
    min_cost_cmd: f32,
    distance_histogram_size: usize,
    histogram_literal: Vec<u32>,
    histogram_cmd: Vec<u32>,
    histogram_dist: Vec<u32>,
    cost_literal: Vec<f32>,
    literal_arena: LiteralCostArena,
}

impl ZopfliCostModel {
    /// Allocates a model for blocks of at most `max_bytes` and `alphabet_size`
    /// distance symbols.
    ///
    /// Sized once for the largest meta-block the encoder can emit, so no block
    /// allocates.
    pub(crate) fn new(max_bytes: usize, alphabet_size: usize) -> Self {
        Self {
            cost_cmd: vec![0f32; NUM_COMMAND_SYMBOLS],
            cost_dist: vec![0f32; alphabet_size.max(1)],
            literal_costs: vec![0f32; max_bytes + 2],
            min_cost_cmd: 0.0,
            distance_histogram_size: alphabet_size,
            histogram_literal: vec![0u32; NUM_LITERAL_SYMBOLS],
            histogram_cmd: vec![0u32; NUM_COMMAND_SYMBOLS],
            histogram_dist: vec![0u32; NUM_HISTOGRAM_DISTANCE_SYMBOLS],
            cost_literal: vec![0f32; NUM_LITERAL_SYMBOLS],
            literal_arena: LiteralCostArena::default(),
        }
    }

    /// Returns the bytes this cost model keeps allocated.
    pub(crate) fn retained_bytes(&self) -> usize {
        (self.cost_cmd.capacity()
            + self.cost_dist.capacity()
            + self.literal_costs.capacity()
            + self.cost_literal.capacity())
            * size_of::<f32>()
            + (self.histogram_literal.capacity()
                + self.histogram_cmd.capacity()
                + self.histogram_dist.capacity())
                * size_of::<u32>()
            + self.literal_arena.retained_bytes()
    }

    /// Makes sure the model can price a block of `num_bytes` bytes.
    ///
    /// Growth is the only allocation the model ever does after construction,
    /// and it happens outside the dynamic program.
    pub(crate) fn reserve(&mut self, num_bytes: usize, alphabet_size: usize) {
        if self.literal_costs.len() < num_bytes + 2 {
            self.literal_costs.resize(num_bytes + 2, 0.0);
        }
        if self.cost_dist.len() < alphabet_size {
            self.cost_dist.resize(alphabet_size, 0.0);
        }
        self.distance_histogram_size = alphabet_size;
    }

    /// Prices a block from the literal-cost estimator alone.
    ///
    /// Mirrors `ZopfliCostModelSetFromLiteralCosts`, the model the first — and
    /// for quality ten only — pass uses. Commands and distances get flat
    /// logarithmic priors, because nothing is yet known about them.
    pub(crate) fn set_from_literal_costs(
        &mut self,
        position: usize,
        ringbuffer: &[u8],
        mask: usize,
        num_bytes: usize,
    ) {
        estimate_bit_costs_for_literals(
            position,
            num_bytes,
            mask,
            ringbuffer,
            &mut self.literal_arena,
            &mut self.literal_costs[1..],
        );
        accumulate_literal_costs(&mut self.literal_costs, num_bytes);

        for (symbol, slot) in self.cost_cmd.iter_mut().enumerate() {
            *slot = fast_log2(COMMAND_PRIOR_OFFSET + symbol) as f32;
        }
        for (symbol, slot) in self
            .cost_dist
            .iter_mut()
            .take(self.distance_histogram_size)
            .enumerate()
        {
            *slot = fast_log2(DISTANCE_PRIOR_OFFSET + symbol) as f32;
        }
        self.min_cost_cmd = fast_log2(COMMAND_PRIOR_OFFSET) as f32;
    }

    /// Prices a block from the commands a previous pass produced.
    ///
    /// Mirrors `ZopfliCostModelSetFromCommands`, which quality eleven uses for
    /// its second pass: now that a plausible command sequence exists, its own
    /// symbol frequencies are a far better price list than the priors.
    pub(crate) fn set_from_commands(
        &mut self,
        position: usize,
        ringbuffer: &[u8],
        mask: usize,
        commands: &[Command],
        last_insert_len: usize,
        num_bytes: usize,
    ) {
        self.histogram_literal.fill(0);
        self.histogram_cmd.fill(0);
        self.histogram_dist.fill(0);

        let mut pos = position - last_insert_len;
        for command in commands {
            let inslength = command.insert_len as usize;
            let copylength = command.copy_len() as usize;
            let distcode = usize::from(command.distance_code());
            let cmdcode = usize::from(command.cmd_prefix);

            self.histogram_cmd[cmdcode] += 1;
            if cmdcode >= 128
                && let Some(slot) = self.histogram_dist.get_mut(distcode)
            {
                *slot += 1;
            }
            for offset in 0..inslength {
                let literal = ringbuffer.get((pos + offset) & mask).copied().unwrap_or(0);
                self.histogram_literal[usize::from(literal)] += 1;
            }
            pos += inslength + copylength;
        }

        set_cost(&self.histogram_literal, true, &mut self.cost_literal);
        set_cost(&self.histogram_cmd, false, &mut self.cost_cmd);
        set_cost(
            &self.histogram_dist[..self.distance_histogram_size],
            false,
            &mut self.cost_dist[..self.distance_histogram_size],
        );

        self.min_cost_cmd = self.cost_cmd.iter().copied().fold(INFINITY, f32::min);

        // Spread the per-symbol literal costs over the block, then accumulate.
        for index in 0..num_bytes {
            let literal = ringbuffer
                .get((position + index) & mask)
                .copied()
                .unwrap_or(0);
            self.literal_costs[index + 1] = self.cost_literal[usize::from(literal)];
        }
        accumulate_literal_costs(&mut self.literal_costs, num_bytes);
    }

    /// Returns what coding command symbol `cmdcode` would cost.
    #[inline(always)]
    pub(crate) fn command_cost(&self, cmdcode: u16) -> f32 {
        self.cost_cmd
            .get(usize::from(cmdcode))
            .copied()
            .unwrap_or(INFINITY)
    }

    /// Returns what coding distance symbol `distcode` would cost.
    #[inline(always)]
    pub(crate) fn distance_cost(&self, distcode: usize) -> f32 {
        self.cost_dist.get(distcode).copied().unwrap_or(INFINITY)
    }

    /// Returns what coding `from..to` as literals would cost.
    #[inline(always)]
    pub(crate) fn literal_costs(&self, from: usize, to: usize) -> f32 {
        let at = |index: usize| self.literal_costs.get(index).copied().unwrap_or(0.0);
        at(to) - at(from)
    }

    /// Returns the cheapest command symbol in the model.
    #[inline(always)]
    pub(crate) const fn min_cost_cmd(&self) -> f32 {
        self.min_cost_cmd
    }
}

/// Turns per-byte literal costs into a cumulative sum, with carry.
///
/// `costs[1..=num_bytes]` holds one cost per byte on entry and the running
/// total on exit, with `costs[0]` zero. The carry keeps the part of each
/// addend that the running total was too large to represent, and feeds it back
/// in; without it the sum drifts low over a long block and the dynamic program
/// starts preferring literals it should not.
fn accumulate_literal_costs(costs: &mut [f32], num_bytes: usize) {
    let mut literal_carry = 0f32;
    costs[0] = 0.0;
    for index in 0..num_bytes {
        literal_carry += costs[index + 1];
        costs[index + 1] = costs[index] + literal_carry;
        literal_carry -= costs[index + 1] - costs[index];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compressor::core::shared::distance::DistanceParams;

    /// The distance alphabet a default-parameter stream uses.
    fn alphabet() -> usize {
        DistanceParams::default().alphabet_size_limit as usize
    }

    #[test]
    fn a_missing_symbol_costs_more_than_a_rare_one() {
        let mut histogram = vec![0u32; 8];
        histogram[0] = 1000;
        histogram[1] = 1;
        let mut cost = vec![0f32; 8];
        set_cost(&histogram, false, &mut cost);
        assert!(cost[0] < cost[1], "{:?}", cost);
        assert!(cost[1] < cost[2], "a missing symbol was not the dearest");
    }

    #[test]
    fn a_literal_histogram_does_not_inflate_its_missing_symbols() {
        // The non-literal form counts the missing symbols into the total
        // first, so it prices them higher.
        let mut histogram = vec![0u32; 64];
        histogram[0] = 100;
        let mut literal = vec![0f32; 64];
        let mut other = vec![0f32; 64];
        set_cost(&histogram, true, &mut literal);
        set_cost(&histogram, false, &mut other);
        assert!(literal[1] < other[1]);
    }

    #[test]
    fn no_symbol_costs_less_than_one_bit() {
        let mut histogram = vec![0u32; 4];
        histogram[0] = 1_000_000;
        histogram[1] = 1;
        let mut cost = vec![0f32; 4];
        set_cost(&histogram, true, &mut cost);
        assert_eq!(cost[0], 1.0);
    }

    #[test]
    fn the_literal_prior_prices_every_command_symbol() {
        let mut model = ZopfliCostModel::new(4096, alphabet());
        let data = vec![b'a'; 4096];
        model.set_from_literal_costs(0, &data, usize::MAX, 4096);
        // `log2(11 + symbol)`, narrowed once.
        assert_eq!(model.command_cost(0), fast_log2(11) as f32);
        assert_eq!(model.command_cost(5), fast_log2(16) as f32);
        assert_eq!(model.distance_cost(0), fast_log2(20) as f32);
        assert_eq!(model.min_cost_cmd(), fast_log2(11) as f32);
    }

    #[test]
    fn cumulative_literal_costs_are_monotone_and_additive() {
        let mut model = ZopfliCostModel::new(4096, alphabet());
        let data: Vec<u8> = (0..4096u32).map(|i| (i * 31 % 256) as u8).collect();
        model.set_from_literal_costs(0, &data, usize::MAX, 4096);

        assert_eq!(model.literal_costs(0, 0), 0.0);
        let whole = model.literal_costs(0, 4096);
        let first = model.literal_costs(0, 2000);
        let second = model.literal_costs(2000, 4096);
        assert!(whole > 0.0);
        // Additive to within one rounding of the running sum.
        assert!((whole - (first + second)).abs() <= whole.abs() * f32::EPSILON * 4.0);
        for split in [1usize, 100, 1000, 4095] {
            assert!(model.literal_costs(0, split) <= model.literal_costs(0, split + 1));
        }
    }

    #[test]
    fn the_carry_beats_a_plain_running_sum() {
        // Over a long block of small equal costs a naive `f32` sum stalls once
        // the total outgrows the addend; the carry does not.
        let count = 200_000usize;
        // A cost with no exact binary representation, so the running sum loses
        // a little of every addend once it has grown.
        let addend = 3.7f32;
        let mut with_carry = vec![0f32; count + 2];
        for slot in with_carry[1..=count].iter_mut() {
            *slot = addend;
        }
        accumulate_literal_costs(&mut with_carry, count);

        let mut naive = 0f32;
        for _ in 0..count {
            naive += addend;
        }
        let exact = f64::from(addend) * count as f64;
        let carried_error = (f64::from(with_carry[count]) - exact).abs();
        let naive_error = (f64::from(naive) - exact).abs();
        assert!(
            naive_error > carried_error * 100.0,
            "carry {} (error {carried_error}), naive {naive} (error {naive_error})",
            with_carry[count]
        );
    }

    #[test]
    fn a_command_model_prices_the_symbols_it_saw() {
        let dist = DistanceParams::default();
        let data = vec![b'z'; 4096];
        // Twenty identical commands: their symbols become cheap and everything
        // else becomes the missing-symbol price.
        let commands: Vec<Command> = (0..20)
            .map(|_| Command::new(&dist, 4, 20, 0, 100))
            .collect();

        let mut model = ZopfliCostModel::new(4096, alphabet());
        model.set_from_commands(200, &data, usize::MAX, &commands, 0, 2048);

        let used = commands[0].cmd_prefix;
        let unused = (0..NUM_COMMAND_SYMBOLS as u16)
            .find(|&symbol| symbol != used)
            .expect("another symbol exists");
        assert!(model.command_cost(used) < model.command_cost(unused));
        assert!(model.min_cost_cmd() <= model.command_cost(used));
    }

    #[test]
    fn an_empty_command_list_still_prices_every_symbol() {
        let data = vec![b'q'; 1024];
        let mut model = ZopfliCostModel::new(1024, alphabet());
        model.set_from_commands(0, &data, usize::MAX, &[], 0, 1024);
        for symbol in [0u16, 1, 100, (NUM_COMMAND_SYMBOLS - 1) as u16] {
            assert!(model.command_cost(symbol).is_finite());
        }
        assert!(model.literal_costs(0, 1024).is_finite());
    }

    #[test]
    fn rebuilding_the_model_does_not_carry_state_over() {
        let dist = DistanceParams::default();
        let data = vec![b'k'; 4096];
        let commands: Vec<Command> = (0..8).map(|_| Command::new(&dist, 2, 10, 0, 60)).collect();

        let mut fresh = ZopfliCostModel::new(4096, alphabet());
        fresh.set_from_commands(100, &data, usize::MAX, &commands, 0, 1024);

        let mut reused = ZopfliCostModel::new(4096, alphabet());
        reused.set_from_literal_costs(0, &data, usize::MAX, 4096);
        reused.set_from_commands(100, &data, usize::MAX, &commands, 0, 1024);

        for symbol in 0..NUM_COMMAND_SYMBOLS as u16 {
            assert_eq!(fresh.command_cost(symbol), reused.command_cost(symbol));
        }
        assert_eq!(fresh.literal_costs(0, 1024), reused.literal_costs(0, 1024));
    }

    #[test]
    fn reserving_grows_the_model_without_changing_its_prices() {
        let data: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
        let mut small = ZopfliCostModel::new(1024, alphabet());
        small.reserve(8192, alphabet());
        small.set_from_literal_costs(0, &data, usize::MAX, 8192);

        let mut large = ZopfliCostModel::new(8192, alphabet());
        large.set_from_literal_costs(0, &data, usize::MAX, 8192);

        assert_eq!(small.literal_costs(0, 8192), large.literal_costs(0, 8192));
    }
}
