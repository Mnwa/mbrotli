//! Entropy-coding tables translated from Google's Brotli reference encoder.
//!
//! Source: <https://github.com/google/brotli/tree/028fb5a> (v1.2.0), files
//! `c/enc/entropy_encode_static.h`, `c/enc/entropy_encode.c`,
//! `c/enc/brotli_bit_stream.c` and `c/enc/fast_log.c`. Distributed by Google
//! under the MIT licence; see `brotli-ffi/vendor/brotli/LICENSE`.
//!
//! Tables that upstream ships pre-expanded are regenerated here by `const fn`
//! from the same algorithm, and pinned by golden checksums in the tests below.

use super::constants::{
    CODE_LENGTH_CODES, NUM_COMMAND_SYMBOLS, REPEAT_PREVIOUS_CODE_LENGTH, REPEAT_ZERO_CODE_LENGTH,
};

/// Bit depths of the static code-length code (`kCodeLengthDepth`).
pub(crate) const CODE_LENGTH_DEPTH: [u8; CODE_LENGTH_CODES] =
    [4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 5, 5, 0, 4, 4];

/// Bit patterns of the static code-length code (`kCodeLengthBits`).
pub(crate) const CODE_LENGTH_BITS: [u32; CODE_LENGTH_CODES] =
    [0, 8, 4, 12, 2, 10, 6, 14, 1, 9, 5, 13, 3, 15, 31, 0, 11, 7];

/// Number of bits in the serialised static code-length code.
pub(crate) const STATIC_CODE_LENGTH_CODE_BITS: u32 = 40;

/// Serialised static code-length code (`StoreStaticCodeLengthCode`).
pub(crate) const STATIC_CODE_LENGTH_CODE: u64 = 0x0000_00FF_5555_5554;

/// Order in which code-length code lengths are stored (`kStorageOrder`).
pub(crate) const STORAGE_ORDER: [usize; CODE_LENGTH_CODES] =
    [1, 2, 3, 4, 0, 5, 17, 6, 16, 7, 8, 9, 10, 11, 12, 13, 14, 15];

/// Symbols of the fixed code that compresses code-length code lengths.
pub(crate) const CODE_LENGTH_HUFFMAN_SYMBOLS: [u8; 6] = [0, 7, 3, 2, 1, 15];

/// Bit depths of the fixed code that compresses code-length code lengths.
pub(crate) const CODE_LENGTH_HUFFMAN_DEPTHS: [u8; 6] = [2, 4, 3, 2, 2, 4];

/// Gap sequence of the reference shell sort (`kBrotliShellGaps`).
pub(crate) const SHELL_GAPS: [usize; 6] = [132, 57, 23, 10, 4, 1];

/// Pre-reversed four-bit values used by `reverse_bits`.
pub(crate) const REVERSE_LUT: [u16; 16] = [
    0x00, 0x08, 0x04, 0x0C, 0x02, 0x0A, 0x06, 0x0E, 0x01, 0x09, 0x05, 0x0D, 0x03, 0x0B, 0x07, 0x0F,
];

/// Serialised code-length run for a stretch of zero bit depths.
pub(crate) const ZERO_REPS_BITS: [u64; NUM_COMMAND_SYMBOLS] = REPS_TABLES.0;

/// Length in bits of the corresponding [`ZERO_REPS_BITS`] entry.
pub(crate) const ZERO_REPS_DEPTH: [u32; NUM_COMMAND_SYMBOLS] = REPS_TABLES.1;

/// Serialised code-length run for a stretch of equal non-zero bit depths.
pub(crate) const NON_ZERO_REPS_BITS: [u64; NUM_COMMAND_SYMBOLS] = REPS_TABLES.2;

/// Length in bits of the corresponding [`NON_ZERO_REPS_BITS`] entry.
pub(crate) const NON_ZERO_REPS_DEPTH: [u32; NUM_COMMAND_SYMBOLS] = REPS_TABLES.3;

type RepsTables = (
    [u64; NUM_COMMAND_SYMBOLS],
    [u32; NUM_COMMAND_SYMBOLS],
    [u64; NUM_COMMAND_SYMBOLS],
    [u32; NUM_COMMAND_SYMBOLS],
);

const REPS_TABLES: RepsTables = build_reps_tables();

/// Longest code-length run either repetition encoding can produce.
const MAX_REPS_SYMBOLS: usize = 16;

/// Appends one code-length symbol, and its extra bits, to an accumulator.
const fn push_symbol(acc: u64, len: u32, symbol: usize, extra: u8, extra_bits: u32) -> (u64, u32) {
    let acc = acc | ((CODE_LENGTH_BITS[symbol] as u64) << len);
    let len = len + CODE_LENGTH_DEPTH[symbol] as u32;
    (acc | ((extra as u64) << len), len + extra_bits)
}

/// Reverses the first `end` entries of `extras`.
const fn reverse_extras(mut extras: [u8; MAX_REPS_SYMBOLS], end: usize) -> [u8; MAX_REPS_SYMBOLS] {
    let mut low = 0;
    let mut high = end - 1;
    while low < high {
        let extra = extras[low];
        extras[low] = extras[high];
        extras[high] = extra;
        low += 1;
        high -= 1;
    }
    extras
}

/// Reverses `symbols[start..end]` and `extras[start..end]` in lockstep.
const fn reverse_runs(
    mut symbols: [u8; MAX_REPS_SYMBOLS],
    mut extras: [u8; MAX_REPS_SYMBOLS],
    start: usize,
    end: usize,
) -> ([u8; MAX_REPS_SYMBOLS], [u8; MAX_REPS_SYMBOLS]) {
    let mut low = start;
    let mut high = end - 1;
    while low < high {
        let symbol = symbols[low];
        symbols[low] = symbols[high];
        symbols[high] = symbol;
        let extra = extras[low];
        extras[low] = extras[high];
        extras[high] = extra;
        low += 1;
        high -= 1;
    }
    (symbols, extras)
}

/// Rebuilds the four repetition tables upstream ships pre-expanded.
///
/// Mirrors `BrotliWriteHuffmanTreeRepetitionsZeros` and
/// `BrotliWriteHuffmanTreeRepetitions` followed by the bit emission performed
/// by `BrotliStoreHuffmanTreeToBitMask` with the static code-length code.
const fn build_reps_tables() -> RepsTables {
    let mut zero_bits = [0u64; NUM_COMMAND_SYMBOLS];
    let mut zero_depth = [0u32; NUM_COMMAND_SYMBOLS];
    let mut non_zero_bits = [0u64; NUM_COMMAND_SYMBOLS];
    let mut non_zero_depth = [0u32; NUM_COMMAND_SYMBOLS];

    let mut reps = 0;
    while reps < NUM_COMMAND_SYMBOLS {
        let mut symbols = [0u8; MAX_REPS_SYMBOLS];
        let mut extras = [0u8; MAX_REPS_SYMBOLS];
        let mut count = 0;
        let mut left = reps;

        if left == 11 {
            symbols[count] = 0;
            count += 1;
            left -= 1;
        }
        if left < 3 {
            let mut i = 0;
            while i < left {
                symbols[count] = 0;
                count += 1;
                i += 1;
            }
        } else {
            let start = count;
            left -= 3;
            loop {
                symbols[count] = REPEAT_ZERO_CODE_LENGTH as u8;
                extras[count] = (left & 0x7) as u8;
                count += 1;
                left >>= 3;
                if left == 0 {
                    break;
                }
                left -= 1;
            }
            (symbols, extras) = reverse_runs(symbols, extras, start, count);
        }

        let mut acc = 0u64;
        let mut len = 0u32;
        let mut i = 0;
        while i < count {
            let symbol = symbols[i] as usize;
            let extra_bits = if symbol == REPEAT_ZERO_CODE_LENGTH {
                3
            } else {
                0
            };
            (acc, len) = push_symbol(acc, len, symbol, extras[i], extra_bits);
            i += 1;
        }
        zero_bits[reps] = acc;
        zero_depth[reps] = len;

        let mut extras = [0u8; MAX_REPS_SYMBOLS];
        let mut count = 0;
        let mut left = reps;
        loop {
            extras[count] = (left & 0x3) as u8;
            count += 1;
            left >>= 2;
            if left == 0 {
                break;
            }
            left -= 1;
        }
        extras = reverse_extras(extras, count);

        let mut acc = 0u64;
        let mut len = 0u32;
        let mut i = 0;
        while i < count {
            (acc, len) = push_symbol(acc, len, REPEAT_PREVIOUS_CODE_LENGTH, extras[i], 2);
            i += 1;
        }
        non_zero_bits[reps] = acc;
        non_zero_depth[reps] = len;

        reps += 1;
    }

    (zero_bits, zero_depth, non_zero_bits, non_zero_depth)
}

/// Reference logarithm table (`kBrotliLog2Table`).
///
/// Upstream spells the entries as `float` literals inside a `double` array, so
/// every value is the single-precision rounding of `log2(i)` widened back to
/// double. The exact widened values are reproduced here.
pub(crate) const LOG2_TABLE: [f64; 256] = [
    0.0_f64,
    0.0_f64,
    1.0_f64,
    1.5849624872207642_f64,
    2.0_f64,
    2.321928024291992_f64,
    2.5849626064300537_f64,
    2.8073549270629883_f64,
    3.0_f64,
    3.1699249744415283_f64,
    3.321928024291992_f64,
    3.4594316482543945_f64,
    3.5849626064300537_f64,
    3.700439691543579_f64,
    3.8073549270629883_f64,
    3.906890630722046_f64,
    4.0_f64,
    4.087462902069092_f64,
    4.169925212860107_f64,
    4.247927665710449_f64,
    4.321928024291992_f64,
    4.392317295074463_f64,
    4.4594316482543945_f64,
    4.523561954498291_f64,
    4.584962368011475_f64,
    4.643856048583984_f64,
    4.700439929962158_f64,
    4.754887580871582_f64,
    4.807354927062988_f64,
    4.857981204986572_f64,
    4.906890392303467_f64,
    4.954196453094482_f64,
    5.0_f64,
    5.044394016265869_f64,
    5.087462902069092_f64,
    5.1292829513549805_f64,
    5.169925212860107_f64,
    5.209453582763672_f64,
    5.247927665710449_f64,
    5.285402297973633_f64,
    5.321928024291992_f64,
    5.3575520515441895_f64,
    5.392317295074463_f64,
    5.426264762878418_f64,
    5.4594316482543945_f64,
    5.4918532371521_f64,
    5.523561954498291_f64,
    5.554588794708252_f64,
    5.584962368011475_f64,
    5.614709854125977_f64,
    5.643856048583984_f64,
    5.672425270080566_f64,
    5.700439929962158_f64,
    5.7279205322265625_f64,
    5.754887580871582_f64,
    5.781359672546387_f64,
    5.807354927062988_f64,
    5.832890033721924_f64,
    5.857981204986572_f64,
    5.882643222808838_f64,
    5.906890392303467_f64,
    5.930737495422363_f64,
    5.954196453094482_f64,
    5.977280139923096_f64,
    6.0_f64,
    6.02236795425415_f64,
    6.044394016265869_f64,
    6.066089153289795_f64,
    6.087462902069092_f64,
    6.108524322509766_f64,
    6.1292829513549805_f64,
    6.149746894836426_f64,
    6.169925212860107_f64,
    6.18982458114624_f64,
    6.209453582763672_f64,
    6.228818893432617_f64,
    6.247927665710449_f64,
    6.266786575317383_f64,
    6.285402297973633_f64,
    6.303780555725098_f64,
    6.321928024291992_f64,
    6.339849948883057_f64,
    6.3575520515441895_f64,
    6.375039577484131_f64,
    6.392317295074463_f64,
    6.409390926361084_f64,
    6.426264762878418_f64,
    6.442943572998047_f64,
    6.4594316482543945_f64,
    6.475733280181885_f64,
    6.4918532371521_f64,
    6.5077948570251465_f64,
    6.523561954498291_f64,
    6.539158821105957_f64,
    6.554588794708252_f64,
    6.569855690002441_f64,
    6.584962368011475_f64,
    6.599912643432617_f64,
    6.614709854125977_f64,
    6.629356384277344_f64,
    6.643856048583984_f64,
    6.658211708068848_f64,
    6.672425270080566_f64,
    6.686500549316406_f64,
    6.700439929962158_f64,
    6.714245319366455_f64,
    6.7279205322265625_f64,
    6.741466999053955_f64,
    6.754887580871582_f64,
    6.768184185028076_f64,
    6.781359672546387_f64,
    6.7944159507751465_f64,
    6.807354927062988_f64,
    6.820178985595703_f64,
    6.832890033721924_f64,
    6.845489978790283_f64,
    6.857981204986572_f64,
    6.870364665985107_f64,
    6.882643222808838_f64,
    6.89481782913208_f64,
    6.906890392303467_f64,
    6.918863296508789_f64,
    6.930737495422363_f64,
    6.942514419555664_f64,
    6.954196453094482_f64,
    6.965784072875977_f64,
    6.977280139923096_f64,
    6.98868465423584_f64,
    7.0_f64,
    7.011227130889893_f64,
    7.02236795425415_f64,
    7.033422946929932_f64,
    7.044394016265869_f64,
    7.0552825927734375_f64,
    7.066089153289795_f64,
    7.076815605163574_f64,
    7.087462902069092_f64,
    7.098031997680664_f64,
    7.108524322509766_f64,
    7.118941307067871_f64,
    7.1292829513549805_f64,
    7.139551162719727_f64,
    7.149746894836426_f64,
    7.1598711013793945_f64,
    7.169925212860107_f64,
    7.1799092292785645_f64,
    7.18982458114624_f64,
    7.199672222137451_f64,
    7.209453582763672_f64,
    7.219168663024902_f64,
    7.228818893432617_f64,
    7.238404750823975_f64,
    7.247927665710449_f64,
    7.257387638092041_f64,
    7.266786575317383_f64,
    7.276124477386475_f64,
    7.285402297973633_f64,
    7.294620513916016_f64,
    7.303780555725098_f64,
    7.312882900238037_f64,
    7.321928024291992_f64,
    7.330916881561279_f64,
    7.339849948883057_f64,
    7.348728179931641_f64,
    7.3575520515441895_f64,
    7.366322040557861_f64,
    7.375039577484131_f64,
    7.38370418548584_f64,
    7.392317295074463_f64,
    7.400879383087158_f64,
    7.409390926361084_f64,
    7.417852401733398_f64,
    7.426264762878418_f64,
    7.434628009796143_f64,
    7.442943572998047_f64,
    7.451210975646973_f64,
    7.4594316482543945_f64,
    7.4676055908203125_f64,
    7.475733280181885_f64,
    7.483815670013428_f64,
    7.4918532371521_f64,
    7.4998459815979_f64,
    7.5077948570251465_f64,
    7.515699863433838_f64,
    7.523561954498291_f64,
    7.531381607055664_f64,
    7.539158821105957_f64,
    7.546894550323486_f64,
    7.554588794708252_f64,
    7.56224250793457_f64,
    7.569855690002441_f64,
    7.577428817749023_f64,
    7.584962368011475_f64,
    7.592456817626953_f64,
    7.599912643432617_f64,
    7.607330322265625_f64,
    7.614709854125977_f64,
    7.62205171585083_f64,
    7.629356384277344_f64,
    7.636624813079834_f64,
    7.643856048583984_f64,
    7.6510515213012695_f64,
    7.658211708068848_f64,
    7.6653361320495605_f64,
    7.672425270080566_f64,
    7.679480075836182_f64,
    7.686500549316406_f64,
    7.693487167358398_f64,
    7.700439929962158_f64,
    7.707359313964844_f64,
    7.714245319366455_f64,
    7.721099376678467_f64,
    7.7279205322265625_f64,
    7.734709739685059_f64,
    7.741466999053955_f64,
    7.74819278717041_f64,
    7.754887580871582_f64,
    7.761551380157471_f64,
    7.768184185028076_f64,
    7.774786949157715_f64,
    7.781359672546387_f64,
    7.787902355194092_f64,
    7.7944159507751465_f64,
    7.800899982452393_f64,
    7.807354927062988_f64,
    7.813781261444092_f64,
    7.820178985595703_f64,
    7.8265485763549805_f64,
    7.832890033721924_f64,
    7.839203834533691_f64,
    7.845489978790283_f64,
    7.851748943328857_f64,
    7.857981204986572_f64,
    7.8641862869262695_f64,
    7.870364665985107_f64,
    7.876516819000244_f64,
    7.882643222808838_f64,
    7.8887434005737305_f64,
    7.89481782913208_f64,
    7.900866985321045_f64,
    7.906890392303467_f64,
    7.91288948059082_f64,
    7.918863296508789_f64,
    7.924812316894531_f64,
    7.930737495422363_f64,
    7.936637878417969_f64,
    7.942514419555664_f64,
    7.948367118835449_f64,
    7.954196453094482_f64,
    7.9600019454956055_f64,
    7.965784072875977_f64,
    7.971543788909912_f64,
    7.977280139923096_f64,
    7.9829936027526855_f64,
    7.98868465423584_f64,
    7.994353294372559_f64,
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden checksums taken from `c/enc/entropy_encode_static.h` of the
    /// pinned reference (`google/brotli` v1.2.0, commit `028fb5a`).
    #[test]
    fn repetition_tables_match_the_reference_checksums() {
        let zero_bits: u64 = ZERO_REPS_BITS.iter().sum();
        let zero_depth: u64 = ZERO_REPS_DEPTH.iter().map(|&d| u64::from(d)).sum();
        let non_zero_bits: u64 = NON_ZERO_REPS_BITS.iter().sum();
        let non_zero_depth: u64 = NON_ZERO_REPS_DEPTH.iter().map(|&d| u64::from(d)).sum();

        assert_eq!(zero_bits, 15_851_288_660);
        assert_eq!(zero_depth, 14_989);
        assert_eq!(non_zero_bits, 219_502_909_920);
        assert_eq!(non_zero_depth, 18_432);
    }

    #[test]
    fn repetition_tables_match_the_reference_prefixes() {
        assert_eq!(
            &ZERO_REPS_BITS[..16],
            &[
                0, 0, 0, 7, 23, 39, 55, 71, 87, 103, 119, 1904, 2951, 4999, 7047, 9095
            ]
        );
        assert_eq!(
            &ZERO_REPS_DEPTH[..16],
            &[0, 4, 8, 7, 7, 7, 7, 7, 7, 7, 7, 11, 14, 14, 14, 14]
        );
        assert_eq!(
            &NON_ZERO_REPS_BITS[..16],
            &[
                11, 27, 43, 59, 715, 1739, 2763, 3787, 731, 1755, 2779, 3803, 747, 1771, 2795, 3819
            ]
        );
        assert_eq!(
            &NON_ZERO_REPS_DEPTH[..16],
            &[6, 6, 6, 6, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12]
        );
    }

    #[test]
    fn repetition_tables_match_the_reference_suffixes() {
        assert_eq!(
            &ZERO_REPS_BITS[700..],
            &[49_924_999, 83_479_431, 117_033_863, 150_588_295]
        );
        assert_eq!(&ZERO_REPS_DEPTH[700..], &[28, 28, 28, 28]);
        assert_eq!(
            &NON_ZERO_REPS_BITS[700..],
            &[195_999_451, 464_434_907, 732_870_363, 1_001_305_819]
        );
        assert_eq!(&NON_ZERO_REPS_DEPTH[700..], &[30, 30, 30, 30]);
    }

    #[test]
    fn the_generator_is_reproducible_at_run_time() {
        // The tables are `const`, so the generator normally only ever runs in
        // const evaluation. Calling it again here keeps it exercised at run
        // time and pins the constants against their own generator.
        let (zero_bits, zero_depth, non_zero_bits, non_zero_depth) = build_reps_tables();
        assert_eq!(zero_bits, ZERO_REPS_BITS);
        assert_eq!(zero_depth, ZERO_REPS_DEPTH);
        assert_eq!(non_zero_bits, NON_ZERO_REPS_BITS);
        assert_eq!(non_zero_depth, NON_ZERO_REPS_DEPTH);
    }

    #[test]
    fn the_generator_primitives_behave_as_documented() {
        let (accumulator, width) = push_symbol(0, 0, REPEAT_ZERO_CODE_LENGTH, 5, 3);
        assert_eq!(
            width,
            u32::from(CODE_LENGTH_DEPTH[REPEAT_ZERO_CODE_LENGTH]) + 3
        );
        assert_eq!(
            accumulator,
            u64::from(CODE_LENGTH_BITS[REPEAT_ZERO_CODE_LENGTH]) | (5 << 4)
        );

        let mut symbols = [0u8; MAX_REPS_SYMBOLS];
        let mut extras = [0u8; MAX_REPS_SYMBOLS];
        symbols[..3].copy_from_slice(&[1, 2, 3]);
        extras[..3].copy_from_slice(&[7, 8, 9]);
        let (symbols, extras) = reverse_runs(symbols, extras, 0, 3);
        assert_eq!(&symbols[..3], &[3, 2, 1]);
        assert_eq!(&extras[..3], &[9, 8, 7]);

        let mut extras = [0u8; MAX_REPS_SYMBOLS];
        extras[..4].copy_from_slice(&[1, 2, 3, 4]);
        let extras = reverse_extras(extras, 4);
        assert_eq!(&extras[..4], &[4, 3, 2, 1]);
    }

    #[test]
    fn repetition_lengths_fit_a_single_bit_writer_call() {
        assert!(ZERO_REPS_DEPTH.iter().all(|&d| d <= 56));
        assert!(NON_ZERO_REPS_DEPTH.iter().all(|&d| d <= 56));
    }
    #[test]
    fn log2_table_reproduces_single_precision_logarithms() {
        assert_eq!(LOG2_TABLE.len(), 256);
        assert_eq!(LOG2_TABLE[0], 0.0);
        assert_eq!(LOG2_TABLE[1], 0.0);
        assert_eq!(LOG2_TABLE[2], 1.0);
        for (value, &entry) in LOG2_TABLE.iter().enumerate().skip(1) {
            assert_eq!(entry, f64::from((value as f64).log2() as f32));
        }
    }
}
