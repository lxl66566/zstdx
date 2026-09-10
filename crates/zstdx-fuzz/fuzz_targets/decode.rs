#![no_main]
//! Decoder robustness and self-consistency: junk input must fail cleanly,
//! valid input must decode identically through the streaming path (small
//! reads, so block boundaries and internal wrap-arounds are crossed many
//! times) and the flat `decode_all` path.

use libfuzzer_sys::fuzz_target;
use std::io::Read;
use zstdx::decoding::errors::FrameDecoderError;
use zstdx::decoding::{FrameDecoder, StreamingDecoder};

fuzz_target!(|data: &[u8]| {
    let Ok(mut dec) = StreamingDecoder::new(data) else {
        return;
    };
    let mut streamed = Vec::new();
    let mut sink = [0u8; 1024];
    loop {
        match dec.read(&mut sink) {
            Ok(0) => break,
            Ok(n) => streamed.extend_from_slice(&sink[..n]),
            Err(_) => return,
        }
    }

    // decode_all_to_vec never grows its target; retry with geometric growth
    // (bounded: RLE blocks cap the expansion of small inputs well below
    // this).
    let mut flat = Vec::new();
    let mut step = 1usize << 16;
    loop {
        let mut fr = FrameDecoder::new();
        match fr.decode_all_to_vec(data, &mut flat) {
            Ok(()) => break,
            Err(FrameDecoderError::TargetTooSmall) => {
                assert!(step <= 1 << 30, "frame expands beyond 1 GiB");
                flat.reserve(step);
                step *= 2;
            }
            Err(_) => return,
        }
    }
    assert_eq!(streamed, flat, "streamed and flat decodes disagree");
});
