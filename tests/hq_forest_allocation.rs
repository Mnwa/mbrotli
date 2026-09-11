#![cfg(feature = "compression")]
//! The window-sized match forest must be taken from the allocator's zeroing
//! path, not written byte by byte.
//!
//! Qualities ten and eleven size the binary-tree forest by the window whenever
//! the stream is not a single final block, exactly as the C reference does.
//! The reference leaves those pages untouched, so a short stream with a wide
//! window never faults them in. Filling the same allocation with zeros instead
//! materialises every page: at a thirty-bit window that is eight gibibytes of
//! resident memory and seconds of kernel time for a payload of a few kibibytes,
//! which is what an AFL campaign reported as a hang.
//!
//! A global allocator that keeps the two paths apart turns that into a
//! deterministic check: with a window of twenty-four bits the forest is one
//! allocation of 128 MiB, far above any other the encoder makes here, and it
//! must arrive zeroed rather than as plain storage the encoder then writes.

use mbrotli::{BlockBits, BlockSize, Compressor, EncoderConfig, Quality, Window};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Allocations at least this large are the forest and nothing else: the ring
/// buffer grows only to the bytes written, and every table is far smaller.
const LARGE: usize = 64 * 1024 * 1024;

static WATCHING: AtomicBool = AtomicBool::new(false);
static LARGE_PLAIN: AtomicUsize = AtomicUsize::new(0);
static LARGE_ZEROED: AtomicUsize = AtomicUsize::new(0);

struct SortingAllocator;

// SAFETY: every request and pointer is forwarded unchanged to System, which
// owns the memory; the counters are lock-free integers that allocate nothing.
unsafe impl GlobalAlloc for SortingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if WATCHING.load(Ordering::Relaxed) && layout.size() >= LARGE {
            LARGE_PLAIN.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: GlobalAlloc's caller supplies a valid layout.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if WATCHING.load(Ordering::Relaxed) && layout.size() >= LARGE {
            LARGE_ZEROED.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: as above; System zeroes the block it returns.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: pointer and layout identify the forwarded System allocation.
        unsafe { System.dealloc(pointer, layout) };
    }
}

#[global_allocator]
static ALLOCATOR: SortingAllocator = SortingAllocator;

#[test]
fn the_match_forest_is_allocated_zeroed_rather_than_filled() {
    // One byte over the pinned block size, so the first block is not the last
    // and the forest is sized by the window rather than by the payload.
    let payload = vec![0u8; (1 << 16) + 1];
    let config = EncoderConfig::default()
        .with_quality(Quality::Q10)
        .with_window(Window::standard(24).expect("twenty-four is a legal window"))
        .with_block_size(BlockSize::Bits(
            BlockBits::try_from(16).expect("sixteen is a legal block size"),
        ));

    WATCHING.store(true, Ordering::Relaxed);
    let compressed = Compressor::new(config)
        .expect("configuration is supported")
        .compress(&payload)
        .expect("compression succeeds");
    WATCHING.store(false, Ordering::Relaxed);

    assert!(!compressed.is_empty(), "the encoder produced no output");
    assert_eq!(
        LARGE_PLAIN.load(Ordering::Relaxed),
        0,
        "a window-sized allocation was taken as plain storage, so every page of \
         it is written before the encoder can use it"
    );
    assert!(
        LARGE_ZEROED.load(Ordering::Relaxed) >= 1,
        "the window-sized forest was not allocated at all; the test no longer \
         reaches the path it guards"
    );
}
