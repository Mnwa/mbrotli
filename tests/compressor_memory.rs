//! Thread-local allocator accounting checks the public retention contract.

use mbrotli::{Compressor, EncoderConfig, Quality, RetentionPolicy};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct CountingAllocator;
thread_local! {
    static LIVE: Cell<usize> = const { Cell::new(0) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    static FAIL_NEXT: Cell<bool> = const { Cell::new(false) };
}

// SAFETY: successful requests and pointers are forwarded unchanged to System;
// returning null simulates a permitted allocation failure.
// Const-initialized thread-local counters allocate nothing and do not recurse.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if FAIL_NEXT
            .try_with(|flag| flag.replace(false))
            .unwrap_or(false)
        {
            return std::ptr::null_mut();
        }
        // SAFETY: GlobalAlloc's caller supplies a valid layout.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            let _ = LIVE.try_with(|live| live.set(live.get().wrapping_add(layout.size())));
            let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        let _ = LIVE.try_with(|live| live.set(live.get().wrapping_sub(layout.size())));
        // SAFETY: pointer and layout identify the forwarded System allocation.
        unsafe { System.dealloc(pointer, layout) };
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[test]
fn recoverable_allocation_failures_do_not_poison_the_compressor() {
    let mut compressor =
        Compressor::new(EncoderConfig::default().with_quality(Quality::Q5)).expect("config");
    let mut output = Vec::new();
    FAIL_NEXT.set(true);
    let outcome = compressor.compress_into(b"payload", &mut output);
    FAIL_NEXT.set(false);
    assert!(matches!(
        outcome,
        Err(mbrotli::EncodeError::AllocationFailed { .. })
    ));
    assert!(output.is_empty());
    compressor
        .compress_into(b"payload", &mut output)
        .expect("warm encoder");
    FAIL_NEXT.set(true);
    let outcome = compressor.start(mbrotli::InputSize::Exact(7).into());
    FAIL_NEXT.set(false);
    assert!(matches!(
        outcome,
        Err(mbrotli::EncodeError::AllocationFailed { .. })
    ));
    drop(outcome);
    compressor
        .compress_into(b"payload", &mut output)
        .expect("recovered");
}

#[test]
fn reported_retention_counts_every_owned_heap_allocation() {
    let data = b"the quick brown fox jumps over the lazy dog 0123456789\n".repeat(1300);
    for number in 0..=11 {
        let quality = Quality::try_from(number).expect("quality");
        let mut output =
            Vec::with_capacity(Compressor::max_compressed_size(data.len()).expect("bound"));
        let before = LIVE.get();
        let mut compressor =
            Compressor::new(EncoderConfig::default().with_quality(quality)).expect("config");
        compressor
            .compress_into(&data, &mut output)
            .expect("compress");
        let measured = LIVE.get().wrapping_sub(before);
        let reported = compressor.retained_bytes();
        assert_eq!(
            reported, measured,
            "q{number}: retained bytes omit owned storage"
        );
        compressor.trim(RetentionPolicy::Bounded {
            max_bytes: reported - 1,
        });
        assert_eq!(
            LIVE.get(),
            before,
            "q{number}: trim did not release storage"
        );
    }
}

#[test]
fn warmed_compression_reuses_all_allocations() {
    let data = b"the quick brown fox jumps over the lazy dog 0123456789\n".repeat(1300);
    for number in 0..=11 {
        let quality = Quality::try_from(number).expect("quality");
        let mut output =
            Vec::with_capacity(Compressor::max_compressed_size(data.len()).expect("bound"));
        let mut compressor =
            Compressor::new(EncoderConfig::default().with_quality(quality)).expect("config");
        compressor
            .compress_into(&data, &mut output)
            .expect("warm up");
        for _ in 0..3 {
            output.clear();
            let before = ALLOCATIONS.get();
            compressor
                .compress_into(&data, &mut output)
                .expect("compress");
            assert_eq!(
                ALLOCATIONS.get() - before,
                0,
                "q{number}: warmed call allocated"
            );
        }
    }
}

#[test]
fn warmed_binary_and_multiblock_compression_reuses_all_allocations() {
    let mut random = 17u32;
    let binary: Vec<u8> = (0..130_000)
        .map(|_| {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            random as u8
        })
        .collect();
    let mixed: Vec<u8> = binary.iter().copied().cycle().take(1_100_000).collect();
    for data in [&binary, &mixed] {
        for number in 0..=11 {
            let mut compressor = Compressor::new(
                EncoderConfig::default().with_quality(Quality::try_from(number).expect("quality")),
            )
            .expect("config");
            let mut output =
                Vec::with_capacity(Compressor::max_compressed_size(data.len()).expect("bound"));
            compressor
                .compress_into(data, &mut output)
                .expect("warm up");
            output.clear();
            let before = ALLOCATIONS.get();
            compressor
                .compress_into(data, &mut output)
                .expect("compress");
            assert_eq!(
                ALLOCATIONS.get() - before,
                0,
                "q{number}, {} bytes: warmed binary call allocated",
                data.len()
            );
        }
    }
}

#[test]
fn warmed_dictionary_compression_reuses_prefix_and_merge_storage() {
    use mbrotli::dictionary::DictionaryBuilder;
    let prefix = b"common dictionary phrases shared between independent messages ".repeat(70);
    let dictionary = DictionaryBuilder::new()
        .add_prefix(prefix.as_slice())
        .build()
        .expect("dictionary");
    let data = prefix.repeat(18);
    for number in 5..=11 {
        let mut compressor = Compressor::new(
            EncoderConfig::default().with_quality(Quality::try_from(number).expect("quality")),
        )
        .expect("config");
        let mut output =
            Vec::with_capacity(Compressor::max_compressed_size(data.len()).expect("bound"));
        compressor
            .compress_with_dictionary_into(&dictionary, &data, &mut output)
            .expect("warm up");
        output.clear();
        let before = ALLOCATIONS.get();
        compressor
            .compress_with_dictionary_into(&dictionary, &data, &mut output)
            .expect("compress");
        assert_eq!(
            ALLOCATIONS.get() - before,
            0,
            "q{number}: dictionary call allocated"
        );
    }
}

#[test]
fn warmed_sessions_reuse_staging_and_pending_storage() {
    let data = b"streaming text with repeated words and a block boundary\n".repeat(1400);
    let mut output = [0; 97];
    for number in 0..=11 {
        let mut compressor = Compressor::new(
            EncoderConfig::default().with_quality(Quality::try_from(number).expect("quality")),
        )
        .expect("config");
        for iteration in 0..2 {
            let before = ALLOCATIONS.get();
            let mut session = compressor
                .start(mbrotli::InputSize::Exact(data.len() as u64).into())
                .expect("session");
            let mut input = data.as_slice();
            loop {
                let progress = session
                    .process(input, &mut output, mbrotli::Operation::Finish)
                    .expect("process");
                input = &input[progress.consumed..];
                if progress.status == mbrotli::EncoderStatus::Finished {
                    break;
                }
            }
            drop(session);
            if iteration == 1 {
                assert_eq!(
                    ALLOCATIONS.get() - before,
                    0,
                    "q{number}: warmed session allocated"
                );
            }
        }
    }
}

#[test]
fn a_warmed_incompressible_small_window_slice_does_not_allocate() {
    let mut random = 41u32;
    let input: Vec<u8> = (0..70_000)
        .map(|_| {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            random as u8
        })
        .collect();
    let mut output = vec![0; Compressor::max_compressed_size(input.len()).expect("bound")];
    for quality in [Quality::Q0, Quality::Q1] {
        let config = EncoderConfig::default()
            .with_quality(quality)
            .with_window(mbrotli::Window::standard(10).expect("window"));
        let mut compressor = Compressor::new(config).expect("config");
        compressor
            .compress_to_slice(&input, &mut output)
            .expect("warm up");
        let before = ALLOCATIONS.get();
        compressor
            .compress_to_slice(&input, &mut output)
            .expect("incompressible stream");
        assert_eq!(
            ALLOCATIONS.get() - before,
            0,
            "{quality:?}: warmed slice allocated"
        );
    }
}
