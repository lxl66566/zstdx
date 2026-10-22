#![no_main]
//! Huff0 codec robustness: the generic round trip, plus the R64 weight
//! parity class — arbitrary bytes parsed as Huffman weight descriptions (the
//! raw and FSE forms behind `HUF_readStats`) with truncated and padded
//! variants; every malformed shape must fail cleanly, no panics.

use libfuzzer_sys::fuzz_target;
use zstdx::huff0::{HuffmanTable, round_trip};

fuzz_target!(|data: &[u8]| {
    round_trip(data);
    weight_description_parse(data);
});

fn weight_description_parse(data: &[u8]) {
    let mut table = HuffmanTable::new();
    let _ = table.build_decoder(data);
    if !data.is_empty() {
        let mut t = HuffmanTable::new();
        let _ = t.build_decoder(&data[..data.len() - 1]);
        let mut t = HuffmanTable::new();
        let _ = t.build_decoder(&data[..data.len() / 2]);
    }
    let mut padded = Vec::with_capacity(data.len() + 1);
    padded.extend_from_slice(data);
    padded.push(0);
    let mut t = HuffmanTable::new();
    let _ = t.build_decoder(&padded);
    // single-bit flips at the head: the raw/FSE form selector byte and the
    // weight stream's first bits
    for i in 0..data.len().min(4) {
        let mut flipped = padded.clone();
        flipped[i] ^= 1 << (i & 7);
        let mut t = HuffmanTable::new();
        let _ = t.build_decoder(&flipped);
    }
}
