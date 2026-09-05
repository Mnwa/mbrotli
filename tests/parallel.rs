//! One-stream interoperability, scheduler determinism, resource ownership and faults.
mod support;
use mbrotli::compressor::parallel::*;
use mbrotli::{Backend, CompressionMode, EncoderConfig, Quality, Window};
use std::{
    error::Error,
    io::{self, Write},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

fn config() -> ParallelConfig {
    ParallelConfig::from(SegmentSize::try_from(64 << 10).unwrap())
        .with_minimum_parallel_size(0)
        .with_max_retained_workers(4)
}
fn compressor(q: u8) -> ParallelCompressor {
    ParallelCompressor::new(
        EncoderConfig::default().with_quality(Quality::try_from(q).unwrap()),
        config(),
    )
    .unwrap()
}
fn batch(n: usize) -> BatchConfig {
    BatchConfig::memory(TaskCount::try_from(n).unwrap(), 32 << 20)
}
fn data() -> Vec<u8> {
    let mut v =
        b"\xff\x00testing the compression dictionary: encyclopedia \xc3\xa9 abcdefghijklmnop "
            .repeat(3200);
    v.resize(3 * (64 << 10) + 1, 0);
    v
}
fn inline(c: &mut ParallelCompressor, input: &[u8], n: usize) -> Vec<u8> {
    let mut b = c.prepare_slice(input, batch(n)).unwrap();
    b.run_inline().unwrap();
    let mut out = b"prefix".to_vec();
    let result = b.finish_into(&mut out).unwrap();
    assert_eq!(result.range, 6..out.len());
    assert_eq!(result.stats.output_bytes as usize, out.len() - 6);
    assert_eq!(result.stats.input_bytes as usize, input.len());
    out.split_off(6)
}
#[test]
fn all_qualities_task_counts_modes_and_backends_produce_one_identical_stream() {
    let input = data();
    for q in 0..=11 {
        let mut c = compressor(q);
        let expected = inline(&mut c, &input, 1);
        assert_eq!(
            support::c_decompress(&expected, input.len()).unwrap(),
            input,
            "q{q}"
        );
        for n in [2, 3, 4, 8] {
            assert_eq!(inline(&mut c, &input, n), expected, "q{q} tasks{n}");
        }
        for backend in Backend::available() {
            for mode in [
                CompressionMode::Generic,
                CompressionMode::Text,
                CompressionMode::Font,
            ] {
                let cfg = EncoderConfig::default()
                    .with_quality(Quality::try_from(q).unwrap())
                    .with_mode(mode);
                let mut c = ParallelCompressor::with_backend(cfg, config(), backend).unwrap();
                let mut b = c.prepare_slice(&input, batch(3)).unwrap();
                std::thread::scope(|scope| {
                    for job in b.take_tasks().unwrap().into_iter().rev() {
                        scope.spawn(move || job.run());
                    }
                });
                let mut out = Vec::new();
                b.finish_into(&mut out).unwrap();
                assert_eq!(out, inline(&mut c, &input, 1));
                assert_eq!(support::c_decompress(&out, input.len()).unwrap(), input);
            }
        }
    }
}
#[test]
fn detached_and_single_thread_rayon_schedulers_need_no_coordinator_drain() {
    fn send_static<T: Send + 'static>() {}
    send_static::<OwnedParallelTask>();
    send_static::<ParallelCompressor>();
    send_static::<FileSource>();
    let input = data();
    let mut c = compressor(5);
    let expected = inline(&mut c, &input, 1);
    let source = Arc::new(ArcBytesSource::from(Arc::<[u8]>::from(input.clone())));
    assert_eq!(source.as_ref().as_ref(), input);
    assert!(!source.is_empty().unwrap());
    for rayon in [false, true] {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let mut b = c.prepare_source(source.clone(), batch(3)).unwrap();
        let jobs = b.take_tasks().unwrap();
        for job in jobs.into_iter().rev() {
            if rayon {
                pool.spawn(move || job.run());
            } else {
                std::thread::spawn(move || job.run());
            }
        }
        b.wait().unwrap();
        let mut output = Vec::new();
        b.finish_into(&mut output).unwrap();
        assert_eq!(output, expected);
        let mut b = c.prepare_slice(&input, batch(3)).unwrap();
        let jobs = b.take_tasks().unwrap();
        pool.scope(|scope| {
            for job in jobs {
                scope.spawn(move |_| job.run());
            }
        });
        assert_eq!(b.wait_timeout(Duration::ZERO).unwrap(), WaitStatus::Ready);
        let (out, _) = b.finish_to_writer(Vec::new()).unwrap();
        assert_eq!(out, expected);
    }
}
#[test]
fn empty_tiny_and_segment_edges_preserve_bytes_and_serial_fallback() {
    for q in 0..=11 {
        let mut c = compressor(q);
        for n in [0, 1, 2, 3, 4, 65535, 65536, 65537] {
            let input = vec![b'x'; n];
            let out = inline(&mut c, &input, 8);
            assert_eq!(support::c_decompress(&out, n).unwrap(), input);
        }
        c.reconfigure_parallel(ParallelConfig::default());
        let input = b"small serial fallback";
        let out = inline(&mut c, input, 8);
        let serial = mbrotli::Compressor::new(*c.encoder_config())
            .unwrap()
            .compress(input)
            .unwrap();
        assert_eq!(out, serial);
    }
}
#[test]
fn every_standard_window_and_incompressible_parts_round_trip() {
    let mut seed = 1u64;
    let bytes: Vec<_> = (0..140000)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as u8
        })
        .collect();
    for q in [0, 1, 2, 5, 9, 10, 11] {
        for bits in 10..=24 {
            let cfg = EncoderConfig::default()
                .with_quality(Quality::try_from(q).unwrap())
                .with_window(Window::standard(bits).unwrap());
            let mut c = ParallelCompressor::new(cfg, config()).unwrap();
            let out = inline(&mut c, &bytes, 3);
            assert_eq!(
                support::c_decompress(&out, bytes.len()).unwrap(),
                bytes,
                "q{q} win{bits}"
            );
        }
    }
}
#[test]
fn configuration_bounds_and_retention_are_explicit() {
    assert!(SegmentSize::try_from(0).is_err());
    assert!(SegmentSize::try_from(65535).is_err());
    assert!(SegmentSize::try_from((16 << 20) + 1).is_err());
    assert_eq!(SegmentSize::default().get(), 4 << 20);
    assert_eq!(TaskCount::ONE.get(), 1);
    assert!(TaskCount::try_from(0).is_err());
    assert!(TaskCount::try_from(usize::MAX).is_err());
    assert!(TaskCount::available().unwrap().get() > 0);
    let mut c = compressor(5);
    assert_eq!(c.retained_worker_count(), 0);
    assert_eq!(c.retained_bytes(), 0);
    let e = c
        .estimate_source(
            (1u64 << 40) + 1,
            &BatchConfig::directory(TaskCount::ONE, "/tmp"),
        )
        .unwrap();
    assert!(e.segment_count > 1);
    assert!(e.maximum_final_bytes >= e.input_bytes);
    assert!(e.estimated_active_workspace_bytes < 2usize.pow(35));
    assert!(c.estimate_source(u64::MAX, &batch(1)).is_err());
    assert!(matches!(
        c.prepare_slice(&data(), BatchConfig::memory(TaskCount::ONE, 1)),
        Err(ParallelEncodeError::MemoryStagingLimit)
    ));
    c.reconfigure_parallel(config().with_aggregate_memory_limit(Some(1)));
    assert!(matches!(
        c.estimate_source(65536, &batch(1)),
        Err(ParallelEncodeError::WorkerMemoryLimit)
    ));
    c.reconfigure_parallel(config());
    let _ = inline(&mut c, &data(), 3);
    assert_eq!(c.retained_worker_count(), 3);
    assert!(c.retained_bytes() > 0);
    c.trim(ParallelRetentionPolicy::Aggressive);
    c.trim(ParallelRetentionPolicy::Bounded { max_bytes: 0 });
    assert_eq!(c.retained_bytes(), 0);
    let _ = inline(&mut c, &data(), 1);
    c.trim(ParallelRetentionPolicy::ReleaseAll);
    assert_eq!(c.retained_bytes(), 0);
    assert!(format!("{c:?}").contains("ParallelCompressor"));
    assert_eq!(c.parallel_config(), &config());
    let invalid = EncoderConfig::default().with_window(Window::large(25).unwrap());
    assert!(matches!(
        ParallelCompressor::new(invalid, config()),
        Err(ParallelConfigError::UnsupportedParallelWindow)
    ));
    let memory = Staging::Memory(MemoryStaging::from(1000000));
    let directory = Staging::Directory(DirectoryStaging::from(std::path::PathBuf::from("/tmp")));
    assert!(format!("{memory:?} {directory:?}").contains("Directory"));
}
#[test]
fn extraction_timeout_abandonment_cancellation_and_parent_reuse() {
    let mut c = compressor(5);
    let input = data();
    let expected = inline(&mut c, &input, 1);
    let mut b = c.prepare_slice(&input, batch(3)).unwrap();
    assert!(matches!(b.wait(), Err(ParallelEncodeError::NotReady)));
    assert!(matches!(
        b.poll().unwrap(),
        BatchPoll::Pending {
            completed: 0,
            total: 3
        }
    ));
    let jobs = b.take_tasks().unwrap();
    assert!(b.take_tasks().is_err());
    assert_eq!(
        b.wait_timeout(Duration::ZERO).unwrap(),
        WaitStatus::TimedOut
    );
    assert!(format!("{b:?} {:?}", jobs[0]).contains("ScopedParallelTask"));
    assert_eq!(u32::from(jobs[0].id()), 0);
    assert_eq!(u64::from(jobs[0].segment_range().start), 0);
    drop(jobs);
    let error = b.wait().unwrap_err();
    assert!(error.to_string().contains("abandoned"));
    assert!(error.source().is_some());
    let mut dst = b"unchanged".to_vec();
    assert!(b.finish_into(&mut dst).is_err());
    assert_eq!(dst, b"unchanged");
    assert_eq!(inline(&mut c, &input, 3), expected);
    let mut b = c.prepare_slice(&input, batch(3)).unwrap();
    b.cancel();
    b.cancel();
    assert!(b.run_inline().is_err());
    drop(b);
    let mut b = c.prepare_slice(&input, batch(3)).unwrap();
    let jobs = b.take_tasks().unwrap();
    drop(b);
    assert_eq!(inline(&mut c, &input, 1), expected);
    for job in jobs {
        job.run();
    }
}
#[test]
fn files_directory_spools_and_file_writer_match_memory_and_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("input");
    let dest = dir.path().join("output");
    let input = data();
    std::fs::write(&source, &input).unwrap();
    let mut c = compressor(5);
    let expected = inline(&mut c, &input, 2);
    assert!(FileSource::open(dir.path()).is_err());
    let file = FileSource::try_from(std::fs::File::open(&source).unwrap()).unwrap();
    let mut b = c
        .prepare_file(
            file,
            BatchConfig::directory(TaskCount::try_from(3).unwrap(), dir.path()),
        )
        .unwrap();
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 4);
    for task in b.take_tasks().unwrap().into_iter().rev() {
        std::thread::spawn(move || task.run());
    }
    let (file, stats) = b
        .finish_to_writer(std::fs::File::create(&dest).unwrap())
        .unwrap();
    file.sync_all().unwrap();
    assert_eq!(stats.output_bytes as usize, expected.len());
    assert_eq!(std::fs::read(&dest).unwrap(), expected);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    let b = c
        .prepare_file(
            FileSource::open(&source).unwrap(),
            BatchConfig::directory(TaskCount::ONE, dir.path()),
        )
        .unwrap();
    drop(b);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    assert!(
        c.prepare_slice(
            &input,
            BatchConfig::directory(TaskCount::ONE, dir.path().join("missing"))
        )
        .is_err()
    );
    let mut b = c
        .prepare_file(FileSource::open(&source).unwrap(), batch(1))
        .unwrap();
    b.run_inline().unwrap();
    std::fs::write(&source, vec![1; input.len()]).unwrap();
    assert!(matches!(
        b.finish_into(&mut Vec::new()),
        Err(ParallelEncodeError::SourceChanged)
    ));
}

#[derive(Debug)]
struct Sink {
    bytes: Vec<u8>,
    fail_at: usize,
    interrupted: bool,
    zero: bool,
}
impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if !self.interrupted {
            self.interrupted = true;
            return Err(io::ErrorKind::Interrupted.into());
        }
        if self.bytes.len() == self.fail_at {
            return if self.zero {
                Ok(0)
            } else {
                Err(io::ErrorKind::BrokenPipe.into())
            };
        }
        let n = bytes.len().min(7).min(self.fail_at - self.bytes.len());
        self.bytes.extend_from_slice(&bytes[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        panic!("finish must not flush")
    }
}
#[test]
fn writer_failures_preserve_ownership_and_exact_accepted_prefix() {
    let input = vec![b'a'; 65537];
    let mut c = compressor(1);
    let expected = inline(&mut c, &input, 2);
    for at in 0..expected.len() {
        let mut b = c.prepare_slice(&input, batch(2)).unwrap();
        b.run_inline().unwrap();
        let error = b
            .finish_to_writer(Sink {
                bytes: Vec::new(),
                fail_at: at,
                interrupted: false,
                zero: at % 2 == 0,
            })
            .unwrap_err();
        assert_eq!(error.bytes_written, at as u64);
        assert_eq!(error.writer.bytes, expected[..at]);
        assert!(error.source().is_some());
        assert!(error.to_string().contains("assembly"));
    }
    let mut b = c.prepare_slice(&input, batch(2)).unwrap();
    b.run_inline().unwrap();
    let (sink, _) = b
        .finish_to_writer(Sink {
            bytes: Vec::new(),
            fail_at: usize::MAX,
            interrupted: false,
            zero: false,
        })
        .unwrap();
    assert_eq!(sink.bytes, expected);
}

struct FaultSource {
    length: AtomicU64,
    panic: bool,
    fail: bool,
}
impl RandomAccessSource for FaultSource {
    fn len(&self) -> io::Result<u64> {
        Ok(self.length.load(Ordering::Relaxed))
    }
    fn read_exact_at(&self, _: u64, dst: &mut [u8]) -> io::Result<()> {
        assert!(!self.panic, "injected source panic");
        if self.fail {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        dst.fill(1);
        Ok(())
    }
}
#[test]
fn source_errors_panics_and_length_changes_never_mutate_destination() {
    let mut c = compressor(5);
    for panic in [false, true] {
        let source = Arc::new(FaultSource {
            length: AtomicU64::new(65537),
            panic,
            fail: !panic,
        });
        let mut b = c.prepare_source(source, batch(2)).unwrap();
        for t in b.take_tasks().unwrap() {
            t.run();
        }
        let error = b.finish_to_writer(Vec::new()).unwrap_err();
        assert!(error.writer.is_empty());
        assert_eq!(error.bytes_written, 0);
        assert!(
            error
                .to_string()
                .contains(if panic { "panicked" } else { "source read" })
        );
    }
    for policy in [
        SourceConsistency::VerifyLength,
        SourceConsistency::AssumeImmutable,
    ] {
        c.reconfigure_parallel(config().with_source_consistency(policy));
        let source = Arc::new(FaultSource {
            length: AtomicU64::new(65537),
            panic: false,
            fail: false,
        });
        let mut b = c.prepare_source(source.clone(), batch(2)).unwrap();
        b.run_inline().unwrap();
        source.length.store(1, Ordering::Relaxed);
        let result = b.finish_into(&mut Vec::new());
        assert_eq!(result.is_ok(), policy == SourceConsistency::AssumeImmutable);
    }
    let source = ArcBytesSource::from(Arc::<[u8]>::from(b"abc".as_slice()));
    assert!(source.read_exact_at(u64::MAX, &mut [0; 1]).is_err());
    assert!(source.read_exact_at(2, &mut [0; 2]).is_err());
    assert_eq!(SourceIdentity::from(vec![1]), SourceIdentity::from(vec![1]));
}

#[test]
fn fast_explicit_short_copies_survive_adaptive_trees_and_large_segments() {
    let text = std::fs::read("brotli-ffi/vendor/brotli/tests/testdata/alice29.txt").unwrap();
    let input: Vec<_> = text.iter().copied().cycle().take(1 << 20).collect();
    for q in [0, 1] {
        let mut c = compressor(q);
        c.reconfigure_parallel(
            ParallelConfig::from(SegmentSize::try_from(256 << 10).unwrap())
                .with_minimum_parallel_size(0),
        );
        let out = inline(&mut c, &input, 4);
        assert_eq!(
            support::c_decompress(&out, input.len())
                .expect("explicit short copies need nonzero command depths"),
            input,
            "q{q}"
        );
    }
}

#[test]
fn q1_explicit_two_byte_remainder_uses_the_canonical_command_alias() {
    let seed = include_bytes!("../fuzz/afl/regressions/parallel/explicit-copy-two.bin");
    let input = &seed[2..];
    let out = inline(&mut compressor(1), input, 1);
    assert_eq!(
        support::c_decompress(&out, input.len()).expect("q1 two-byte remainder"),
        input
    );
}

#[test]
fn directory_staging_processes_more_than_four_gib_with_bounded_reads() {
    struct ZeroSource {
        largest_read: AtomicU64,
        final_offset: AtomicU64,
    }
    const LEN: u64 = (1u64 << 32) + 3;
    impl RandomAccessSource for ZeroSource {
        fn len(&self) -> io::Result<u64> {
            Ok(LEN)
        }
        fn read_exact_at(&self, offset: u64, dst: &mut [u8]) -> io::Result<()> {
            assert!(dst.len() <= 4 << 20);
            assert!(offset + dst.len() as u64 <= LEN);
            self.largest_read
                .fetch_max(dst.len() as u64, Ordering::Relaxed);
            self.final_offset.fetch_max(offset, Ordering::Relaxed);
            dst.fill(u8::from(offset >= 1u64 << 32));
            Ok(())
        }
    }
    let source = Arc::new(ZeroSource {
        largest_read: AtomicU64::new(0),
        final_offset: AtomicU64::new(0),
    });
    let dir = tempfile::tempdir().unwrap();
    let mut c = ParallelCompressor::new(
        EncoderConfig::default().with_quality(Quality::Q0),
        ParallelConfig::default(),
    )
    .unwrap();
    let mut b = c
        .prepare_source(
            source.clone(),
            BatchConfig::directory(TaskCount::try_from(4).unwrap(), dir.path()),
        )
        .unwrap();
    assert_eq!(b.segment_count(), 1025);
    std::thread::scope(|scope| {
        for task in b.take_tasks().unwrap() {
            scope.spawn(move || task.run());
        }
    });
    let path = dir.path().join("output.br");
    let (_, stats) = b
        .finish_to_writer(std::fs::File::create(&path).unwrap())
        .unwrap();
    assert_eq!(stats.input_bytes, LEN);
    assert_eq!(source.final_offset.load(Ordering::Relaxed), 1u64 << 32);
    assert_eq!(source.largest_read.load(Ordering::Relaxed), 4 << 20);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    let compressed = std::fs::read(path).unwrap();
    let mut output = vec![0; 128 << 10];
    let mut total = 0u64;
    // SAFETY: decoder state is created and destroyed once. Input and output
    // allocations remain live and distinct, and all available lengths fit them.
    unsafe {
        use google_brotli_ffi as ffi;
        let state = ffi::BrotliDecoderCreateInstance(None, None, std::ptr::null_mut());
        assert!(!state.is_null());
        let mut available_in = compressed.len();
        let mut next_in = compressed.as_ptr();
        loop {
            let mut available_out = output.len();
            let mut next_out = output.as_mut_ptr();
            let result = ffi::BrotliDecoderDecompressStream(
                state,
                &mut available_in,
                &mut next_in,
                &mut available_out,
                &mut next_out,
                std::ptr::null_mut(),
            );
            let n = output.len() - available_out;
            let zeros = (1u64 << 32).saturating_sub(total).min(n as u64) as usize;
            assert!(output[..zeros].iter().all(|&x| x == 0));
            assert!(output[zeros..n].iter().all(|&x| x == 1));
            total += n as u64;
            if result == ffi::BROTLI_DECODER_RESULT_SUCCESS {
                break;
            }
            assert_eq!(result, ffi::BROTLI_DECODER_RESULT_NEEDS_MORE_OUTPUT);
        }
        ffi::BrotliDecoderDestroyInstance(state);
        assert_eq!(available_in, 0);
    }
    assert_eq!(total, LEN);
}

#[test]
fn q1_two_byte_copy_keeps_canonical_huffman_order_with_other_short_copies() {
    let seed = include_bytes!("../fuzz/afl/regressions/parallel/explicit-copy-two-order.bin");
    let input = &seed[2..];
    let out = inline(&mut compressor(1), input, 1);
    assert_eq!(
        support::c_decompress(&out, input.len()).expect("q1 canonical copy order"),
        input
    );
}
