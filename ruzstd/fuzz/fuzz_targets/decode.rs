#![no_main]
#[macro_use]
extern crate libfuzzer_sys;
extern crate zstdx;
use std::io::Read;

fuzz_target!(|data: &[u8]| {
    if let Ok(mut decoder) = zstdx::decoding::StreamingDecoder::new(data) {
        let mut output = Vec::new();
        _ = decoder.read_to_end(&mut output);
    }
});
