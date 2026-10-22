#![no_main]
//! Huff0 codec robustness: the generic round trip, plus the R64 weight
//! parity class — arbitrary bytes parsed as Huffman weight descriptions (the
//! raw and FSE forms behind `HUF_readStats`) with truncated and padded
//! variants; every malformed shape must fail cleanly, no panics. The R65
//! alphabet-cap seed pins the declared-never-emitted maxSV >= 12 header only
//! the FSE-table cap rejects.

use libfuzzer_sys::fuzz_target;
use zstdx::{
    decoding::errors::{FSETableError, HuffmanTableError},
    huff0::{HuffmanTable, round_trip},
};

fuzz_target!(|data: &[u8]| {
    round_trip(data);
    weight_description_parse(data);
    weight_alphabet_cap_seed(data);
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

/// The R65 alphabet-cap class as a constructed seed: a weights NCount
/// declaring a symbol at 12/13 with a single state that the emitted weights
/// never use (the emitted alphabet is 0..=2). No Kraft/tableLog bound on the
/// decoded weight set can reject a declared-but-never-emitted symbol — only
/// `HuffmanTable`'s FSETable(11) cap catches it, at the same layer as
/// libzstd's HUF_readStats workspace check (tableLog_tooLarge).
fn weight_alphabet_cap_seed(data: &[u8]) {
    // 64 states at accuracy log 6: 40/20/3 for the emitted weights 0/1/2
    // plus 1 for the declared-but-never-emitted symbol.
    let declared = 12 + (data.first().copied().unwrap_or(0) & 1) as usize;
    let mut norm = [0i32; 14];
    norm[0] = 40;
    norm[1] = 20;
    norm[2] = 3;
    norm[declared] = 1;
    let ncount = zstdx::fse::serialize_ncount(&norm, 6);
    let mut src = Vec::with_capacity(ncount.len() + 1 + data.len());
    src.push(ncount.len() as u8); // header byte: FSE-compressed weights size
    src.extend_from_slice(&ncount);
    src.extend_from_slice(data); // arbitrary tail in place of the streams
    let mut table = HuffmanTable::new();
    assert!(
        table.build_decoder(&src).is_err(),
        "declared symbol {declared} must be rejected"
    );

    // Control: the same distribution declaring symbol 11 passes the cap —
    // libzstd accepts every header the workspace check lets through — so
    // any failure must come from the (absent) weight streams, not
    // TooManySymbols.
    let mut norm = [0i32; 12];
    norm[0] = 40;
    norm[1] = 20;
    norm[2] = 3;
    norm[11] = 1;
    let ncount = zstdx::fse::serialize_ncount(&norm, 6);
    let mut src = Vec::with_capacity(ncount.len() + 1);
    src.push(ncount.len() as u8);
    src.extend_from_slice(&ncount);
    match HuffmanTable::new().build_decoder(&src) {
        Err(HuffmanTableError::FSETableError(FSETableError::TooManySymbols { .. })) => {
            panic!("declared symbol 11 must pass the alphabet cap")
        },
        _ => {},
    }
}
