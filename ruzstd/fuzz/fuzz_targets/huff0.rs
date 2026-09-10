#![no_main]
#[macro_use]
extern crate libfuzzer_sys;
extern crate zstdx;
use zstdx::huff0::round_trip;

fuzz_target!(|data: &[u8]| {
    round_trip(data);
});
