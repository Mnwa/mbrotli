//! Compress a regular file using caller-owned scoped threads and disk staging.
use mbrotli::compressor::parallel::{
    BatchConfig, FileSource, ParallelCompressor, ParallelConfig, TaskCount,
};
use mbrotli::{EncoderConfig, Quality};
#[cfg_attr(feature = "hotpath", hotpath::main)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<_> = std::env::args_os().collect();
    if arguments.len() != 3 {
        return Err("usage: cargo run --release --example parallel -- INPUT OUTPUT".into());
    }
    let scratch = std::env::temp_dir();
    let mut compressor = ParallelCompressor::new(
        EncoderConfig::default().with_quality(Quality::Q5),
        ParallelConfig::default(),
    )?;
    let mut batch = compressor.prepare_file(
        FileSource::open(&arguments[1])?,
        BatchConfig::directory(TaskCount::available()?, scratch),
    )?;
    let tasks = batch.take_tasks()?;
    std::thread::scope(|scope| {
        for task in tasks {
            scope.spawn(move || task.run());
        }
    });
    let output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&arguments[2])?;
    let (_, stats) = batch.finish_to_writer(output)?;
    println!(
        "{} input bytes -> {} compressed bytes in {} tasks",
        stats.input_bytes, stats.output_bytes, stats.effective_tasks
    );
    Ok(())
}
