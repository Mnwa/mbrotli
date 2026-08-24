//! Randomised property tests over structured inputs.
//!
//! The generator is seeded deterministically, so a failure reproduces exactly.
//! It mixes literal runs, back-references, incompressible noise and boundary
//! lengths, which is where the fast encoders make their interesting decisions.

mod support;

use mbrotli::Brotli;
use support::{
    IMPLEMENTED_QUALITIES, Rng, c_compress, c_decompress, host_levels, params, prefix_for,
    quality_number,
};

/// Builds one pseudo-random input from a mixture of shapes.
fn generate(rng: &mut Rng, max_len: usize) -> Vec<u8> {
    let target = (rng.next_u64() as usize) % (max_len + 1);
    let mut data = Vec::with_capacity(target);
    while data.len() < target {
        match rng.next_u8() % 6 {
            // Literal noise.
            0 => {
                let run = 1 + (rng.next_u64() as usize) % 512;
                for _ in 0..run {
                    data.push(rng.next_u8());
                }
            }
            // A run of one byte value.
            1 => {
                let byte = rng.next_u8();
                let run = 1 + (rng.next_u64() as usize) % 2048;
                data.extend(std::iter::repeat_n(byte, run));
            }
            // A periodic pattern.
            2 => {
                let period = 1 + (rng.next_u64() as usize) % 32;
                let run = 1 + (rng.next_u64() as usize) % 4096;
                for index in 0..run {
                    data.push((index % period) as u8);
                }
            }
            // A back-reference to earlier data.
            3 if !data.is_empty() => {
                let start = (rng.next_u64() as usize) % data.len();
                let run = 1 + (rng.next_u64() as usize) % 1024;
                for index in 0..run {
                    let source = data[(start + index) % data.len()];
                    data.push(source);
                }
            }
            // A small alphabet.
            4 => {
                let alphabet = 1 + u16::from(rng.next_u8() % 8);
                let run = 1 + (rng.next_u64() as usize) % 1024;
                data.extend(rng.bytes(run, alphabet));
            }
            // Ascii text.
            _ => {
                let run = 1 + (rng.next_u64() as usize) % 256;
                for _ in 0..run {
                    data.push(b'a' + rng.next_u8() % 26);
                }
            }
        }
    }
    data.truncate(target);
    data
}

#[test]
fn random_inputs_match_the_c_encoder_and_round_trip() {
    let compressor = Brotli::default().compressor();
    let mut rng = Rng::new(0xC0FF_EE00_1234_5678);
    for case in 0..400u32 {
        let data = generate(&mut rng, 300_000);
        let lgwin = 10 + (rng.next_u64() as usize) % 15;
        for quality in IMPLEMENTED_QUALITIES {
            let data = prefix_for(quality, &data);
            let expected = c_compress(quality_number(quality), lgwin as i32, data);
            let actual = compressor
                .compress(params(quality, lgwin), data)
                .expect("compression failed");
            assert_eq!(
                actual,
                expected,
                "case {case}, quality {}, lgwin {lgwin}, {} input bytes",
                usize::from(quality),
                data.len()
            );
            let decoded = c_decompress(&actual, data.len())
                .unwrap_or_else(|| panic!("case {case}: the decoder rejected the stream"));
            assert_eq!(decoded, data, "case {case}");
        }
    }
}

#[test]
fn random_inputs_agree_across_backends() {
    let levels = host_levels();
    let mut rng = Rng::new(0x1234_5678_9ABC_DEF0);
    for case in 0..150u32 {
        let data = generate(&mut rng, 200_000);
        let lgwin = 10 + (rng.next_u64() as usize) % 15;
        for quality in IMPLEMENTED_QUALITIES {
            let data = prefix_for(quality, &data);
            let mut reference: Option<Vec<u8>> = None;
            for &(level_name, level) in &levels {
                let actual = Brotli::from(level)
                    .compressor()
                    .compress(params(quality, lgwin), data)
                    .expect("compression failed");
                match &reference {
                    None => reference = Some(actual),
                    Some(expected) => assert_eq!(
                        &actual, expected,
                        "case {case}, backend {level_name}, lgwin {lgwin}"
                    ),
                }
            }
        }
    }
}

#[test]
fn short_random_inputs_match_the_c_encoder() {
    let compressor = Brotli::default().compressor();
    let mut rng = Rng::new(0x0F0F_0F0F_0F0F_0F0F);
    for case in 0..3_000u32 {
        let data = generate(&mut rng, 512);
        let lgwin = 10 + (rng.next_u64() as usize) % 15;
        for quality in IMPLEMENTED_QUALITIES {
            let data = prefix_for(quality, &data);
            let expected = c_compress(quality_number(quality), lgwin as i32, data);
            let actual = compressor
                .compress(params(quality, lgwin), data)
                .expect("compression failed");
            assert_eq!(
                actual,
                expected,
                "case {case}, quality {}, lgwin {lgwin}, {} input bytes",
                usize::from(quality),
                data.len()
            );
        }
    }
}
