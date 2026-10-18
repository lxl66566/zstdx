#![no_main]
//! MT decoder robustness and parity: every input must decode identically
//! through the parallel entry points (segment executor; piece executor via
//! the router override, on since this is a dedicated process) and the
//! sequential decoder, or fail cleanly on both. Any acceptance asymmetry
//! or byte divergence is a crash artifact.

use libfuzzer_sys::fuzz_target;
use zstdx::{
    DecoderOptions, bulk,
    decoding::{FrameDecoder, errors::FrameDecoderError},
    set_piece_decode_for_tests,
};

fuzz_target!(|data: &[u8]| {
    // Process-global static; set before the first decode so every
    // qualifying pledged frame routes through the piece machinery.
    set_piece_decode_for_tests(true);
    let mt = bulk::decompress_with(data, 1 << 20, &DecoderOptions::new().threads(4));

    // Sequential reference with geometric growth (bounded: RLE blocks cap
    // the expansion of small inputs well below this).
    let mut seq = Vec::new();
    let mut step = 1usize << 16;
    let reference = loop {
        let mut fr = FrameDecoder::new();
        match fr.decode_all_to_vec(data, &mut seq) {
            Ok(()) => break Some(()),
            Err(FrameDecoderError::TargetTooSmall) => {
                assert!(step <= 1 << 30, "frame expands beyond 1 GiB");
                seq.reserve(step);
                step *= 2;
            },
            Err(_) => break None,
        }
    };

    match (mt, reference) {
        (Ok(a), Some(())) => assert_eq!(&a[..], &seq[..], "MT and sequential decodes disagree"),
        (Err(_), None) => {},
        (Err(e), Some(())) => panic!("sequential decode accepted what MT rejected: {e}"),
        (Ok(a), None) => panic!(
            "MT decode accepted what sequential rejected: {} bytes",
            a.len()
        ),
    }
});
