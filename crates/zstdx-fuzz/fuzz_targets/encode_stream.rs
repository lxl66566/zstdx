#![no_main]
//! Streaming encoder coverage: the first input byte picks the ladder level
//! and the second the write chunk size, so block/job boundaries land at
//! many different phases. The frame must decode back through both decoders.
//!
//! Byte identity with the bulk path is deliberately NOT asserted here: the
//! matcher's pooled state survives between frames, so the same input can
//! produce different (equally valid) frames depending on what ran before
//! in the process. Cross-build byte comparisons must use fresh processes
//! (the zstdx-bench `dump` tool).

use libfuzzer_sys::fuzz_target;
use std::io::Write;
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
    let chunk = 1 + data.get(1).copied().unwrap_or(0) as usize % 4096;

    let mut enc = zstdx::stream::write::Encoder::new(Vec::new(), level).unwrap();
    for c in data.chunks(chunk) {
        enc.write_all(c).unwrap();
    }
    let comp = enc.finish().unwrap();

    let mut back = Vec::with_capacity(data.len() + 16);
    zstdx::decoding::FrameDecoder::new()
        .decode_all_to_vec(&comp, &mut back)
        .unwrap();
    assert_eq!(&back[..], data, "zstdx decode of streamed output differs");

    let mut zback = Vec::new();
    zstd::stream::copy_decode(comp.as_slice(), &mut zback).unwrap();
    assert_eq!(&zback[..], data, "zstd decode of streamed output differs");
});
