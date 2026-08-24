//! Greedy backward-reference search shared by qualities two to nine.
//!
//! Ports `CreateBackwardReferences` from `c/enc/backward_references_inc.h` and
//! `ComputeDistanceCode` from `c/enc/backward_references.c` of the pinned
//! reference (`google/brotli` v1.2.0, commit `028fb5a`), together with
//! `ExtendLastCommand` from `c/enc/encode.c`.
//!
//! The decision order here is the compression format's semantics, not an
//! implementation detail: which candidate wins, when a match is delayed by a
//! byte, which positions are stored and which are skipped all show up in the
//! emitted bytes. The match finder underneath may be accelerated freely, but
//! this sequence may not be reordered.

use fearless_simd::Simd;

use super::command::Command;
use super::hashers::{DistanceCache, MatchQuery, Matcher};
use super::params::{GreedyParams, NUM_DISTANCE_SHORT_CODES};
use super::score::{MIN_SCORE, SearchResult};
use crate::compressor::core::greedy::dictionary::DictionaryStats;

/// Score a delayed match has to beat the current one by (`cost_diff_lazy`).
const COST_DIFF_LAZY: usize = 175;

/// How many times in a row a match may be delayed by one byte.
const MAX_DELAYED_IN_A_ROW: usize = 4;

/// Returns the intermediate distance code for `distance`.
///
/// Mirrors `ComputeDistanceCode`. The first sixteen codes are short codes
/// relative to the distance cache; the two magic nibble tables spell out which
/// short code expresses "one less than the last distance", "two more", and so
/// on.
pub(crate) fn compute_distance_code(
    distance: usize,
    max_distance: usize,
    cache: &DistanceCache,
) -> usize {
    if distance <= max_distance {
        let distance_plus_3 = distance + 3;
        let offset0 = distance_plus_3.wrapping_sub(cache[0] as usize);
        let offset1 = distance_plus_3.wrapping_sub(cache[1] as usize);
        if distance == cache[0] as usize {
            return 0;
        }
        if distance == cache[1] as usize {
            return 1;
        }
        if offset0 < 7 {
            return (0x975_0468usize >> (4 * offset0)) & 0xF;
        }
        if offset1 < 7 {
            return (0xFDB_1ACEusize >> (4 * offset1)) & 0xF;
        }
        if distance == cache[2] as usize {
            return 2;
        }
        if distance == cache[3] as usize {
            return 3;
        }
    }
    distance + NUM_DISTANCE_SHORT_CODES as usize - 1
}

/// The window a search runs over.
#[derive(Copy, Clone)]
pub(crate) struct Window<'a> {
    /// The ring buffer holding the input seen so far.
    pub(crate) data: &'a [u8],
    /// Mask that turns an absolute position into a buffer index.
    pub(crate) mask: usize,
}

/// The stretch of input one call processes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct BlockSpan {
    /// Wrapped position the stretch starts at.
    pub(crate) position: u32,
    /// Number of bytes in the stretch.
    pub(crate) bytes: u32,
}

/// State the reference search carries between input blocks.
pub(crate) struct ReferenceState {
    /// The four distances that have short codes.
    pub(crate) dist_cache: DistanceCache,
    /// Literals produced but not yet attached to a command.
    pub(crate) last_insert_len: usize,
    /// Literals in the commands emitted for the current meta-block.
    pub(crate) num_literals: usize,
    /// Whether probing the static dictionary is still paying off.
    pub(crate) dictionary: DictionaryStats,
}

impl Default for ReferenceState {
    /// Returns the state a fresh stream starts from.
    fn default() -> Self {
        Self {
            dist_cache: super::hashers::INITIAL_DISTANCE_CACHE,
            last_insert_len: 0,
            num_literals: 0,
            dictionary: DictionaryStats::default(),
        }
    }
}

/// Turns `num_bytes` of input at `position` into commands.
///
/// Appends to `commands` and updates `state`; literals that no command has
/// claimed yet stay in [`ReferenceState::last_insert_len`] for the next call.
#[hotpath::measure]
pub(crate) fn create_backward_references<S: Simd, M: Matcher>(
    simd: S,
    matcher: &mut M,
    params: &GreedyParams,
    window: Window<'_>,
    span: BlockSpan,
    state: &mut ReferenceState,
    commands: &mut Vec<Command>,
) {
    let Window {
        data: ringbuffer,
        mask,
    } = window;
    let num_bytes = span.bytes as usize;
    let mut position = span.position as usize;
    let max_backward_limit = params.max_backward_limit();
    let mut insert_length = state.last_insert_len;
    let pos_end = position + num_bytes;
    let store_end = if num_bytes >= M::STORE_LOOKAHEAD {
        position + num_bytes - M::STORE_LOOKAHEAD + 1
    } else {
        position
    };

    let window = params.random_heuristics_window_size();
    let mut apply_random_heuristics = position + window;
    let extensive = params.quality.extensive_reference_search();

    while position + M::HASH_TYPE_LENGTH < pos_end {
        let mut max_length = pos_end - position;
        let mut max_distance = position.min(max_backward_limit);
        let mut dictionary_start = position.min(max_backward_limit);
        let mut sr = SearchResult::empty();
        matcher.find_longest_match(
            simd,
            &mut state.dictionary,
            MatchQuery {
                data: ringbuffer,
                mask,
                cache: &state.dist_cache,
                cur_ix: position,
                max_length,
                max_backward: max_distance,
                dictionary_distance: dictionary_start,
                max_distance: params.dist.max_distance as usize,
            },
            &mut sr,
        );

        if !sr.is_match() {
            insert_length += 1;
            position += 1;
            if position <= apply_random_heuristics {
                continue;
            }
            // Nothing has matched for a long time. Storing every position of
            // incompressible data costs time and floods the table, so the scan
            // strides forward and only stores part of what it skips.
            let (stride, margin_floor) = if position > apply_random_heuristics + 4 * window {
                (4usize, 4usize)
            } else {
                (2usize, 2usize)
            };
            let margin = (M::STORE_LOOKAHEAD - 1).max(margin_floor);
            let pos_jump = (position + 4 * stride).min(pos_end.saturating_sub(margin));
            while position < pos_jump {
                matcher.store(ringbuffer, mask, position);
                insert_length += stride;
                position += stride;
            }
            continue;
        }

        // A match is available; look one byte ahead for a better one, up to
        // four times in a row.
        let mut delayed = 0usize;
        max_length -= 1;
        loop {
            let mut sr2 = SearchResult {
                // Below quality five the delayed search starts from the length
                // it already has, which lets the matcher reject most
                // candidates without measuring them. Quality five gives that
                // shortcut up and searches everything again.
                len: if extensive {
                    0
                } else {
                    (sr.len - 1).min(max_length)
                },
                distance: 0,
                score: MIN_SCORE,
                len_code_delta: 0,
            };
            max_distance = (position + 1).min(max_backward_limit);
            dictionary_start = (position + 1).min(max_backward_limit);
            matcher.find_longest_match(
                simd,
                &mut state.dictionary,
                MatchQuery {
                    data: ringbuffer,
                    mask,
                    cache: &state.dist_cache,
                    cur_ix: position + 1,
                    max_length,
                    max_backward: max_distance,
                    dictionary_distance: dictionary_start,
                    max_distance: params.dist.max_distance as usize,
                },
                &mut sr2,
            );
            if sr2.score >= sr.score + COST_DIFF_LAZY {
                // Emit one more literal and start the match a byte later.
                position += 1;
                insert_length += 1;
                sr = sr2;
                delayed += 1;
                if delayed < MAX_DELAYED_IN_A_ROW && position + M::HASH_TYPE_LENGTH < pos_end {
                    max_length -= 1;
                    continue;
                }
            }
            break;
        }

        apply_random_heuristics = position + 2 * sr.len + window;
        dictionary_start = position.min(max_backward_limit);
        let distance_code = compute_distance_code(sr.distance, dictionary_start, &state.dist_cache);
        if sr.distance <= dictionary_start && distance_code > 0 {
            state.dist_cache[3] = state.dist_cache[2];
            state.dist_cache[2] = state.dist_cache[1];
            state.dist_cache[1] = state.dist_cache[0];
            state.dist_cache[0] = sr.distance as i32;
        }
        commands.push(Command::new(
            &params.dist,
            insert_length,
            sr.len,
            sr.len_code_delta,
            distance_code,
        ));
        state.num_literals += insert_length;
        insert_length = 0;

        // Store the positions the match covered, skipping the ones a run-length
        // repeat would only poison the table with.
        let mut range_start = position + 2;
        let range_end = (position + sr.len).min(store_end);
        if sr.distance < (sr.len >> 2) {
            range_start = range_end.min(range_start.max(position + sr.len - (sr.distance << 2)));
        }
        matcher.store_range(ringbuffer, mask, range_start, range_end);
        position += sr.len;
    }

    insert_length += pos_end - position;
    state.last_insert_len = insert_length;
}

/// Grows the last command over input that continues its copy.
///
/// Mirrors `ExtendLastCommand`. When a block boundary falls in the middle of a
/// repeat, the bytes after it would otherwise start a fresh command; extending
/// the previous one instead costs nothing and saves a command.
///
/// Returns the number of bytes the command absorbed.
pub(crate) fn extend_last_command(
    command: &mut Command,
    params: &GreedyParams,
    dist_cache: &DistanceCache,
    window: Window<'_>,
    last_processed_pos: u64,
    span: &mut BlockSpan,
) {
    let Window {
        data: ringbuffer,
        mask,
    } = window;
    let max_backward_distance = (1u64 << params.lgwin) - 16;
    let last_copy_len = u64::from(command.copy_len());
    let last_processed_pos = last_processed_pos - last_copy_len;
    let max_distance = last_processed_pos.min(max_backward_distance);
    let cmd_dist = u64::from(dist_cache[0] as u32);
    let distance_code = command.restore_distance_code(&params.dist);

    if u64::from(distance_code) < u64::from(NUM_DISTANCE_SHORT_CODES)
        || u64::from(distance_code) - u64::from(NUM_DISTANCE_SHORT_CODES - 1) == cmd_dist
    {
        if cmd_dist <= max_distance {
            while span.bytes != 0 {
                let here = usize::try_from(u64::from(span.position)).unwrap_or(0);
                let there = usize::try_from(u64::from(span.position) - cmd_dist).unwrap_or(0);
                let Some(&current) = ringbuffer.get(here & mask) else {
                    break;
                };
                let Some(&previous) = ringbuffer.get(there & mask) else {
                    break;
                };
                if current != previous {
                    break;
                }
                command.copy_len += 1;
                span.bytes -= 1;
                span.position += 1;
            }
        }
        // The copy length changed, so the command symbol has to be recomputed.
        // `ExtendLastCommand` reads the length-code delta as an unsigned field
        // here, which agrees with the signed reading everywhere it can occur:
        // only a dictionary match sets it, and never to a negative value.
        command.cmd_prefix = super::command::length_code(
            command.insert_len as usize,
            (command.copy_len & 0x1FF_FFFF) as usize + (command.copy_len >> 25) as usize,
            command.distance_code() == 0,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compressor::core::greedy::hashers::INITIAL_DISTANCE_CACHE;
    use crate::compressor::core::greedy::hashers::QuickMatcher;
    use crate::compressor::core::greedy::params::GreedyQuality;
    use crate::compressor::{CompressParams, QualityLevel, WindowBits};
    use fearless_simd::{Level, dispatch};

    fn params(quality: QualityLevel) -> GreedyParams {
        let public = CompressParams::new(quality, WindowBits::DEFAULT);
        GreedyParams::new(&public, 0).expect("supported quality")
    }

    fn run(quality: QualityLevel, data: &[u8]) -> (Vec<Command>, ReferenceState) {
        let params = params(quality);
        let mut matcher = QuickMatcher::<16, 1, 5, false>::new();
        matcher.prepare(true, data.len(), data);
        let mut state = ReferenceState::default();
        let mut commands = Vec::new();
        let level = Level::new();
        let window = Window {
            data,
            mask: usize::MAX,
        };
        let span = BlockSpan {
            position: 0,
            bytes: data.len() as u32,
        };
        dispatch!(level, simd => create_backward_references(
            simd, &mut matcher, &params, window, span, &mut state, &mut commands,
        ));
        (commands, state)
    }

    /// Bytes the commands and the trailing literals account for.
    fn consumed(commands: &[Command], state: &ReferenceState) -> usize {
        commands
            .iter()
            .map(|command| command.insert_len as usize + command.copy_len() as usize)
            .sum::<usize>()
            + state.last_insert_len
    }

    #[test]
    fn every_input_byte_is_accounted_for() {
        for quality in [QualityLevel::Q3, QualityLevel::Q4, QualityLevel::Q5] {
            for payload in [
                b"abcabcabcabcabcabcabcabcabcabc".to_vec(),
                vec![b'z'; 5000],
                (0..5000u32).map(|i| (i % 251) as u8).collect(),
                Vec::new(),
                b"a".to_vec(),
            ] {
                let mut data = payload.clone();
                // The match finder loads whole words past the end.
                data.extend_from_slice(&[0u8; 8]);
                let params = params(quality);
                let mut matcher = QuickMatcher::<16, 1, 5, false>::new();
                matcher.prepare(true, payload.len(), &data);
                let mut state = ReferenceState::default();
                let mut commands = Vec::new();
                let level = Level::new();
                let window = Window {
                    data: &data,
                    mask: usize::MAX,
                };
                let span = BlockSpan {
                    position: 0,
                    bytes: payload.len() as u32,
                };
                dispatch!(level, simd => create_backward_references(
                    simd, &mut matcher, &params, window, span, &mut state, &mut commands,
                ));
                assert_eq!(
                    consumed(&commands, &state),
                    payload.len(),
                    "quality {quality:?}, {} bytes",
                    payload.len()
                );
            }
        }
    }

    #[test]
    fn a_repeated_string_becomes_one_long_copy() {
        let mut data = b"the quick brown fox ".repeat(40);
        data.extend_from_slice(&[0u8; 8]);
        let payload = data.len() - 8;
        let params = params(QualityLevel::Q3);
        let mut matcher = QuickMatcher::<16, 1, 5, false>::new();
        matcher.prepare(true, payload, &data);
        let mut state = ReferenceState::default();
        let mut commands = Vec::new();
        let level = Level::new();
        let window = Window {
            data: &data,
            mask: usize::MAX,
        };
        let span = BlockSpan {
            position: 0,
            bytes: payload as u32,
        };
        dispatch!(level, simd => create_backward_references(
            simd, &mut matcher, &params, window, span, &mut state, &mut commands,
        ));
        assert!(!commands.is_empty());
        let longest = commands
            .iter()
            .map(|command| command.copy_len())
            .max()
            .unwrap_or(0);
        assert!(longest > 500, "longest copy was only {longest}");
    }

    #[test]
    fn incompressible_data_produces_no_commands() {
        let (commands, state) = run(QualityLevel::Q3, &[]);
        assert!(commands.is_empty());
        assert_eq!(state.last_insert_len, 0);
    }

    #[test]
    fn the_distance_cache_only_records_real_distances() {
        let mut data = b"abcdefgh".repeat(200);
        data.extend_from_slice(&[0u8; 8]);
        let payload = data.len() - 8;
        let params = params(QualityLevel::Q3);
        let mut matcher = QuickMatcher::<16, 1, 5, false>::new();
        matcher.prepare(true, payload, &data);
        let mut state = ReferenceState::default();
        let mut commands = Vec::new();
        let level = Level::new();
        let window = Window {
            data: &data,
            mask: usize::MAX,
        };
        let span = BlockSpan {
            position: 0,
            bytes: payload as u32,
        };
        dispatch!(level, simd => create_backward_references(
            simd, &mut matcher, &params, window, span, &mut state, &mut commands,
        ));
        assert!(state.dist_cache.iter().all(|&distance| distance > 0));
    }

    #[test]
    fn distance_codes_prefer_the_cache() {
        let cache: DistanceCache = INITIAL_DISTANCE_CACHE;
        assert_eq!(compute_distance_code(4, 1 << 20, &cache), 0);
        assert_eq!(compute_distance_code(11, 1 << 20, &cache), 1);
        assert_eq!(compute_distance_code(15, 1 << 20, &cache), 2);
        assert_eq!(compute_distance_code(16, 1 << 20, &cache), 3);
        // One less than the last distance has its own short code.
        assert_eq!(compute_distance_code(3, 1 << 20, &cache), 4);
        assert_eq!(compute_distance_code(5, 1 << 20, &cache), 5);
        // Anything else is spelled out.
        assert_eq!(compute_distance_code(1000, 1 << 20, &cache), 1015);
        // Beyond the window a distance is always spelled out.
        assert_eq!(compute_distance_code(4, 3, &cache), 19);
    }

    #[test]
    fn every_short_distance_code_is_in_range() {
        let cache: DistanceCache = INITIAL_DISTANCE_CACHE;
        for distance in 1usize..64 {
            let code = compute_distance_code(distance, 1 << 20, &cache);
            assert!(code < 16 || code == distance + 15, "distance {distance}");
        }
    }

    #[test]
    fn quality_five_searches_more_than_quality_four() {
        // The extensive search resets the delayed candidate length, so it can
        // pick a different, sometimes shorter but nearer, match.
        assert!(GreedyQuality::Q5.extensive_reference_search());
        assert!(!GreedyQuality::Q4.extensive_reference_search());
    }
}
