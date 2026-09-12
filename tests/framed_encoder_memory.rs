#![cfg(all(feature = "compression", feature = "experimental"))]
use mbrotli::RetentionPolicy;
use mbrotli::framing::*;
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

fn fixture<'a>(data: &'a [u8], fields: &'a [MetadataField<'a>]) -> [FramedItem<'a>; 3] {
    [
        FramedItem::Metadata {
            kind: MetadataKind::Resource,
            fields,
            options: Default::default(),
        },
        FramedItem::Resource(FramedResource {
            data,
            options: Default::default(),
            stream: Default::default(),
            encoding: ResourceEncoding::Uncompressed,
        }),
        FramedItem::Padding { bytes: 3 },
    ]
}
fn config() -> FramedEncodeConfig {
    FramedEncodeConfig::default().with_framing_config(FramingConfig {
        chunk_bytes: 31,
        repeat_metadata: true,
        ..Default::default()
    })
}
#[test]
fn every_framing_allocation_failure_rolls_back_and_owner_remains_reusable() {
    let data = [b'x'; 93];
    let fields = [MetadataField {
        code: *b"id",
        value: b"name",
    }];
    let items = fixture(&data, &fields);
    let codes = [*b"id"];
    let input = FramedInput {
        items: &items,
        repeat_metadata_fields: Some(&codes),
    };
    let expected = FramedCompressor::new(config())
        .unwrap()
        .compress(input)
        .unwrap();
    let mut completed = false;
    for fail_after in 0..200 {
        let mut owner = FramedCompressor::new(config()).unwrap();
        let mut dst = b"prefix".to_vec();
        FAIL.set(Some(fail_after));
        let result = owner.compress_into(input, &mut dst);
        FAIL.set(None);
        match result {
            Ok(range) => {
                assert_eq!(&dst[range], expected);
                completed = true;
                break;
            }
            Err(error) => {
                assert!(
                    matches!(error, FramedEncodeError::AllocationFailed),
                    "{error}"
                );
                assert_eq!(dst, b"prefix");
                assert_eq!(owner.compress(input).unwrap(), expected);
            }
        }
    }
    assert!(completed, "all allocation boundaries exhausted");
}
#[test]
fn retained_and_peak_storage_are_measured_without_whole_payload_staging() {
    let data = vec![b'x'; 100_000];
    let fields = [MetadataField {
        code: *b"id",
        value: b"name",
    }];
    let items = fixture(&data, &fields);
    let config = FramedEncodeConfig::default().with_framing_config(FramingConfig {
        chunk_bytes: 4096,
        repeat_metadata: true,
        ..Default::default()
    });
    let baseline = LIVE.get();
    PEAK.set(baseline);
    let mut owner = FramedCompressor::new(config).unwrap();
    assert_eq!(LIVE.get(), baseline);
    let output = owner.compress(items.as_slice().into()).unwrap();
    drop(output);
    assert_eq!(owner.retained_bytes(), LIVE.get() - baseline);
    assert!(owner.retained_bytes() < data.len());
    let retained = owner.retained_bytes();
    let peak = PEAK.get() - baseline;
    assert!(peak > owner.retained_bytes());
    owner
        .reconfigure(config.with_framing_config(FramingConfig {
            chunk_bytes: 1,
            max_buffer_bytes: 8196,
            ..Default::default()
        }))
        .unwrap();
    owner.trim(RetentionPolicy::ReleaseAll);
    assert_eq!(owner.retained_bytes(), 0);
    assert_eq!(LIVE.get(), baseline);
    eprintln!(
        "stored payload={} retained={retained} peak_with_returned_output={peak}",
        data.len()
    );
}

#[test]
fn raw_and_framing_retention_are_counted_once_without_counting_dictionary_storage() {
    let dictionary = mbrotli::dictionary::DictionaryBuilder::new()
        .add_prefix(&b"dictionary and payload repeated"[..])
        .build()
        .unwrap();
    let references = [DictionaryReference::PrefixId(DictionaryId([7; 32]))];
    let items = [FramedItem::Resource(FramedResource {
        data: b"dictionary and payload repeated",
        options: Default::default(),
        stream: Default::default(),
        encoding: ResourceEncoding::Shared {
            dictionary: &dictionary,
            references: &references,
        },
    })];
    // Warm optional instrumentation storage outside the measured owner lifetime.
    drop(
        FramedCompressor::new(config())
            .unwrap()
            .compress(items.as_slice().into())
            .unwrap(),
    );
    let baseline = LIVE.get();
    let mut owner = FramedCompressor::new(config()).unwrap();
    drop(owner.compress(items.as_slice().into()).unwrap());
    assert_eq!(owner.retained_bytes(), LIVE.get() - baseline);
    owner.recover();
    assert_eq!(LIVE.get(), baseline);
    assert_eq!(owner.retained_bytes(), 0);
}

#[test]
fn compatible_reconfiguration_and_exact_retention_ceiling_keep_capacities() {
    let data = [b'x'; 100];
    let fields = [MetadataField {
        code: *b"id",
        value: b"name",
    }];
    let items = fixture(&data, &fields);
    let mut owner = FramedCompressor::new(config()).unwrap();
    drop(owner.compress(items.as_slice().into()).unwrap());
    let retained = owner.retained_bytes();
    assert!(retained > 0);
    owner.reconfigure(config()).unwrap();
    assert_eq!(owner.retained_bytes(), retained);
    owner.trim(RetentionPolicy::Bounded {
        max_bytes: retained,
    });
    assert_eq!(owner.retained_bytes(), retained);
    owner.trim(RetentionPolicy::Bounded {
        max_bytes: retained - 1,
    });
    assert_eq!(owner.retained_bytes(), 0);
}
