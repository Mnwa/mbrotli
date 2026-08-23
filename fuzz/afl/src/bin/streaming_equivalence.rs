//! Chunk boundaries must not change the stream the adapters produce.

use mbrotli::Brotli;
use mbrotli_afl::{assert_round_trip, decode_case};
use std::io::{Read, Write};

fn main() {
    let compressor = Brotli::default().compressor();
    afl::fuzz!(|input: &[u8]| {
        let case = decode_case(input);

        let mut sink = compressor.compress_writer(case.params, Vec::new());
        for piece in case.data.chunks(case.chunk) {
            sink.write_all(piece).expect("write failed");
        }
        let written = sink.finish().expect("finish failed");

        let mut source = compressor.compress_reader(case.params, case.data);
        let mut read = Vec::new();
        let mut buffer = vec![0u8; case.chunk];
        loop {
            let count = source.read(&mut buffer).expect("read failed");
            if count == 0 {
                break;
            }
            read.extend_from_slice(&buffer[..count]);
        }

        assert_eq!(written, read, "the writer and reader adapters disagree");
        assert_round_trip(case.data, &written);
    });
}
