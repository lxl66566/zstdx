#![no_main]
//! Cross-implementation decoding: frames produced by the reference zstd
//! crate (level swept from the first input byte) must decode to the
//! original bytes through both of our decode paths.

use libfuzzer_sys::fuzz_target;
use std::io::Read;
use zstdx::decoding::{BlockDecodingStrategy, FrameDecoder, StreamingDecoder};

fn decode_streaming(mut data: &[u8]) -> Vec<u8> {
    let mut decoder = StreamingDecoder::new(&mut data).unwrap();
    let mut result = Vec::new();
    decoder.read_to_end(&mut result).expect("decoding failed");
    result
}

fn decode_pull(mut data: &[u8]) -> Vec<u8> {
    let mut decoder = FrameDecoder::new();
    decoder.reset(&mut data).unwrap();
    let mut result = Vec::new();
    while !decoder.is_finished() || decoder.can_collect() > 0 {
        decoder
            .decode_blocks(&mut data, BlockDecodingStrategy::UptoBytes(1024 * 1024))
            .unwrap();
        decoder.collect_to_writer(&mut result).unwrap();
    }
    result
}

fuzz_target!(|data: &[u8]| {
    let z = 1 + i32::from(data.first().copied().unwrap_or(0)) % 22;
    let comp = zstd::stream::encode_all(std::io::Cursor::new(data), z).unwrap();

    assert_eq!(decode_streaming(comp.as_slice()), data);
    assert_eq!(decode_pull(comp.as_slice()), data);
});
