//! Allocation-backed regression for the public preparation peak budget.
#![cfg(feature = "experimental")]

use mbrotli::dictionary::{
    DictionaryBuilder, DictionaryLimits, SerializedDictionary, TransformList, TransformOperation,
    WordList,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

struct CountingAllocator;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: requests and pointers are forwarded unchanged to System; accounting
// uses only atomics and never allocates or changes allocation ownership.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the allocator caller supplies a valid layout.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            let live = LIVE.fetch_add(layout.size(), SeqCst) + layout.size();
            PEAK.fetch_max(live, SeqCst);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), SeqCst);
        // SAFETY: this pointer and layout came from the same forwarded System allocation.
        unsafe { System.dealloc(pointer, layout) };
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[test]
fn dictionary_preparation_obeys_the_measured_peak_budget() {
    let mut words = WordList::builder();
    for _ in 0..16384 {
        words = words.add_word(b"abcd");
    }
    let description = SerializedDictionary::builder()
        .add_word_list(words.build().expect("words"))
        .add_transform_list(
            TransformList::builder()
                .add_transform(b"", TransformOperation::Identity, b"")
                .build()
                .expect("transforms"),
        )
        .build()
        .expect("description");
    let retained = DictionaryBuilder::default()
        .add_serialized(&description)
        .build()
        .expect("prepare")
        .retained_bytes();
    for limit in [retained, retained * 2] {
        let baseline = LIVE.load(SeqCst);
        PEAK.store(baseline, SeqCst);
        let result = DictionaryBuilder::default()
            .with_limits(DictionaryLimits::default().with_max_retained_bytes(limit as u64))
            .add_serialized(&description)
            .build();
        let peak = PEAK.load(SeqCst).saturating_sub(baseline);
        assert_eq!(result.is_ok(), limit > retained);
        assert!(peak <= limit, "limit {limit}, observed heap peak {peak}");
    }
}
