#![no_main]
#[macro_use]
extern crate libfuzzer_sys;
extern crate ruzstd;
use ruzstd::{encoding::compress_to_vec, Level};

fuzz_target!(|data: &[u8]| {
    let output = compress_to_vec(data, Level::Uncompressed);

    let mut decoded = Vec::with_capacity(data.len());
    let mut decoder = ruzstd::decoding::FrameDecoder::new();
    decoder.decode_all_to_vec(&output, &mut decoded).unwrap();
    assert_eq!(data, &decoded);

    let output = compress_to_vec(data, Level::Fastest);

    let mut decoded = Vec::with_capacity(data.len());
    let mut decoder = ruzstd::decoding::FrameDecoder::new();
    decoder.decode_all_to_vec(&output, &mut decoded).unwrap();
    assert_eq!(data, &decoded);
});
