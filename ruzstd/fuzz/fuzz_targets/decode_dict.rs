#![no_main]
#[macro_use]
extern crate libfuzzer_sys;
extern crate zstdx;

fuzz_target!(|data: &[u8]| {
    zstdx::decoding::Dictionary::decode_dict(data).ok(); // may return error on invalid data, may not panic.
});
