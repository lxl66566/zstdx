#![no_main]
//! Encoder roundtrip across the whole ladder: the first input byte picks
//! the level so every strategy (including the optimal parsers) gets
//! coverage, and the frame must decode back to the input through both our
//! decoder and the reference implementation.

use libfuzzer_sys::fuzz_target;
use zstdx::decoding::FrameDecoder;
use zstdx::Level;

const LADDER: [Level; 7] = [
    Level::Uncompressed,
    Level::Fastest,
    Level::Fast,
    Level::Balanced,
    Level::Best,
    Level::Opt,
    Level::Ultra,
];

fuzz_target!(|data: &[u8]| {
    let level = LADDER[data.first().copied().unwrap_or(0) as usize % LADDER.len()];
    let comp = zstdx::bulk::compress(data, level);

    let mut back = Vec::with_capacity(data.len() + 16);
    FrameDecoder::new()
        .decode_all_to_vec(&comp, &mut back)
        .unwrap();
    assert_eq!(&back[..], data, "zstdx decode of own output differs");

    let mut zback = Vec::new();
    zstd::stream::copy_decode(comp.as_slice(), &mut zback).unwrap();
    assert_eq!(&zback[..], data, "zstd decode of zstdx output differs");
});
