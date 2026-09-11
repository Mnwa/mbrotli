#![cfg(feature = "decompression")]
//! Allocator-observed storage and deterministic fallible-allocation paths.
mod support;
use mbrotli::{DecodeError, DecodeLimits, DecoderConfig, Decompressor, RetentionPolicy};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
struct Allocator;
thread_local! {
    static LIVE:Cell<usize>=const {Cell::new(0)};
    static PEAK:Cell<usize>=const {Cell::new(0)};
    static CALLS:Cell<usize>=const {Cell::new(0)};
    static FAIL:Cell<Option<usize>>=const {Cell::new(None)};
}
// SAFETY: allocation/deallocation is forwarded unchanged to System. TLS cells
// allocate nothing. The optional null return is a valid GlobalAlloc failure.
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if FAIL
            .try_with(|count| match count.get() {
                Some(0) => {
                    count.set(None);
                    true
                }
                Some(n) => {
                    count.set(Some(n - 1));
                    false
                }
                None => false,
            })
            .unwrap_or(false)
        {
            return std::ptr::null_mut();
        }
        // SAFETY: the allocator caller supplies a valid layout.
        let result = unsafe { System.alloc(layout) };
        if !result.is_null() {
            let _ = CALLS.try_with(|v| v.set(v.get() + 1));
            let _ = LIVE.try_with(|v| {
                v.set(v.get().wrapping_add(layout.size()));
                let _ = PEAK.try_with(|peak| peak.set(peak.get().max(v.get())));
            });
        }
        result
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        let _ = LIVE.try_with(|v| v.set(v.get().wrapping_sub(layout.size())));
        // SAFETY: pointer/layout belong to a live allocation forwarded to System.
        unsafe { System.dealloc(pointer, layout) };
    }
}
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

#[test]
fn retained_storage_matches_allocator_and_warm_calls_allocate_nothing() {
    let payload =
        b"warm decoder storage includes history, contexts and Huffman tables".repeat(2000);
    let compressed = support::c_compress_native_one_shot(5, 22, &payload);
    let mut output = vec![0; payload.len()];
    let mut warm = Decompressor::new(DecoderConfig::default()).unwrap();
    warm.decompress_to_slice(&compressed, &mut output).unwrap();
    drop(warm); // initialize optional profiling
    let before = LIVE.get();
    let calls_before = CALLS.get();
    PEAK.set(before);
    let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
    assert_eq!(decoder.retained_bytes(), 0);
    decoder
        .decompress_to_slice(&compressed, &mut output)
        .unwrap();
    assert_eq!(decoder.retained_bytes(), LIVE.get() - before);
    let retained = decoder.retained_bytes();
    let calls = CALLS.get();
    decoder
        .decompress_to_slice(&compressed, &mut output)
        .unwrap();
    assert_eq!(CALLS.get(), calls);
    assert_eq!(output, payload);
    let peak = PEAK.get() - before;
    decoder.trim(RetentionPolicy::ReleaseAll);
    assert_eq!(LIVE.get(), before);
    eprintln!(
        "decoded={} compressed={} allocations={} retained={} allocator_peak={}",
        payload.len(),
        compressed.len(),
        calls - calls_before,
        retained,
        peak
    );
    // A cold exact workspace budget succeeds; one byte below fails before
    // exceeding its allowed live heap allocation.
    for limit in [0, peak - 1, peak, peak + 1] {
        let mut decoder = Decompressor::new(
            DecoderConfig::default()
                .with_limits(DecodeLimits::default().with_max_workspace_bytes(Some(limit))),
        )
        .unwrap();
        let before = LIVE.get();
        PEAK.set(before);
        let result = decoder.decompress_to_slice(&compressed, &mut output);
        assert!(decoder.retained_bytes() <= limit);
        assert!(PEAK.get() - before <= limit);
        if limit < peak {
            assert!(matches!(
                result,
                Err(DecodeError::MemoryLimitExceeded { .. })
            ));
        } else {
            assert!(result.is_ok());
        }
    }
}

#[test]
fn each_decoder_allocation_failure_is_recoverable_and_append_is_atomic() {
    let compressed =
        support::c_compress_native_one_shot(5, 22, &b"allocation failure test".repeat(100));
    // Initialize optional profiler metadata before injecting failures into the
    // codec allocator. Profiler startup itself is outside the decoder contract.
    let mut warm = Decompressor::new(DecoderConfig::default()).unwrap();
    warm.decompress(&compressed).unwrap();
    drop(warm);
    let mut succeeded = false;
    for fail_after in 0..64 {
        let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
        let mut output = b"prefix".to_vec();
        FAIL.set(Some(fail_after));
        let result = decoder.decompress_into(&compressed, &mut output);
        FAIL.set(None);
        if result.is_ok() {
            succeeded = true;
            break;
        }
        assert!(matches!(result, Err(DecodeError::AllocationFailed)));
        assert_eq!(output, b"prefix");
        assert!(decoder.decompress(&compressed).is_ok());
    }
    assert!(succeeded);
}

#[test]
fn decode_only_dictionary_counts_storage_and_allocation_failures() {
    use mbrotli::dictionary::{
        DecodeDictionary, DecodeDictionaryError, DecodeDictionaryLimits, DictionaryAttachment,
    };
    let attachments = [
        DictionaryAttachment::Raw(b"one"),
        DictionaryAttachment::Raw(b"two"),
    ];
    for fail_after in 0..2 {
        FAIL.set(Some(fail_after));
        let result = DecodeDictionary::new(&attachments, DecodeDictionaryLimits::default());
        FAIL.set(None);
        assert!(matches!(
            result,
            Err(DecodeDictionaryError::AllocationFailed)
        ));
    }
    let before = LIVE.get();
    let dictionary =
        DecodeDictionary::new(&attachments, DecodeDictionaryLimits::default()).unwrap();
    assert_eq!(dictionary.retained_bytes(), LIVE.get() - before);
    assert_eq!(dictionary.retained_bytes(), 6);
}

#[cfg(feature = "experimental")]
fn framed_fixture() -> Vec<u8> {
    let mut bytes = vec![0x91, 10, 66, 82, 4];
    for _ in 0..12 {
        bytes.extend_from_slice(&[6, 1, 0, b'i', b'd', 1, b'x', 6, 2, 0, 0, b'a', b'b', b'c']);
    }
    bytes.extend_from_slice(&[3, 10, 0, 0]);
    bytes
}
#[cfg(feature = "experimental")]
#[test]
fn framed_append_rolls_back_at_every_allocation_failure() {
    use mbrotli::framing::{FramedDecodeError, FramedDecompressor};
    let bytes = framed_fixture();
    let mut warm = FramedDecompressor::new(Default::default()).unwrap();
    warm.decompress(&bytes).unwrap();
    drop(warm);
    let mut success = false;
    for fail_after in 0..512 {
        let mut d = FramedDecompressor::new(Default::default()).unwrap();
        let mut dst = b"prefix".to_vec();
        FAIL.set(Some(fail_after));
        let result = d.decompress_into(&bytes, &mut dst);
        FAIL.set(None);
        if result.is_ok() {
            success = true;
            break;
        }
        assert!(
            matches!(result, Err(FramedDecodeError::AllocationFailed)),
            "{result:?}"
        );
        assert_eq!(dst, b"prefix");
        assert!(d.decompress(&bytes).is_ok());
    }
    assert!(success);
}
#[cfg(feature = "experimental")]
#[test]
fn framed_workspace_ceiling_covers_peak_and_retained_storage() {
    use mbrotli::framing::{FramedDecodeConfig, FramedDecodeLimits, FramedDecompressor};
    let bytes = framed_fixture();
    let mut dst = [0; 36];
    let mut warm = FramedDecompressor::new(Default::default()).unwrap();
    drop(warm.decompress_to_slice(&bytes, &mut dst).unwrap());
    drop(warm);
    let before = LIVE.get();
    PEAK.set(before);
    let mut d = FramedDecompressor::new(Default::default()).unwrap();
    let output = d.decompress_to_slice(&bytes, &mut dst).unwrap();
    let peak = PEAK.get() - before;
    drop(output);
    assert_eq!(d.retained_bytes(), LIVE.get() - before);
    drop(d);
    for limit in [0, peak / 2, peak - 1, peak, peak + 1] {
        let mut d = FramedDecompressor::new(
            FramedDecodeConfig::default()
                .with_limits(FramedDecodeLimits::default().with_max_workspace_bytes(Some(limit))),
        )
        .unwrap();
        let before = LIVE.get();
        PEAK.set(before);
        let result = d.decompress_to_slice(&bytes, &mut dst);
        let measured = PEAK.get() - before;
        assert!(
            measured <= limit,
            "budget {limit}, observed peak {measured}, result {result:?}"
        );
        drop(result);
    }
}
