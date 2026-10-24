#![no_main]
//! Decoder robustness and reference agreement: junk input must fail
//! cleanly, valid input must decode identically to the reference ON EACH
//! PATH — the streaming path (small reads, so block boundaries and
//! internal wrap-arounds are crossed many times) against the reference
//! streaming decoder, the flat `decode_all` path (whole-history one-shot
//! semantics, concatenated frames included) against the reference
//! one-shot `ZSTD_decompress`.
//!
//! The two zstdx paths are deliberately NOT compared with each other:
//! one-shot and streaming decode differ BY DESIGN (the streaming
//! generation model, R72) — a crafted wrapped-ring frame legitimately
//! produces different accept/bytes through the two paths, which is
//! exactly what the R73 campaign's first artifact was (the old
//! `streamed == flat` assert fired on it; the reference agreed with each
//! path separately). The checks are one-directional: zstdx rejecting
//! where the reference accepts is not an error (the deliberately
//! stricter generation model covers that direction); anything a zstdx
//! path accepts, its reference counterpart must accept with equal bytes
//! — except the streamed byte-compare, which only runs when the
//! reference's own streaming and one-shot outputs agree: on a
//! window-invalid aliased frame the accepted bytes are caller-layout
//! defined (libzstd's ring resolves the aliased offset against whatever
//! its own buffer geometry retains; the zstd crate's layout yielded
//! stale-ring bytes the CLI's does not — R73 artifact
//! crash-e9590bef, adjudicated not a zstdx defect), so no
//! cross-implementation byte oracle exists there. Accept/reject
//! agreement is still checked unconditionally.
//!
//! Decoded size is bounded: a 300 KB crafted input of RLE/match blocks
//! can declare gigabytes of output, and neither path's checks matter past
//! the point the box runs out of RAM — oversized frames are dropped from
//! the check instead.

use std::io::Read;

use libfuzzer_sys::fuzz_target;
use zstdx::decoding::{FrameDecoder, StreamingDecoder, errors::FrameDecoderError};

/// Output past this bound skips every check (see the module doc).
const MAX_CHECK: usize = 32 << 20;

fuzz_target!(|data: &[u8]| {
    // Streaming pass; a mid-stream error leaves `streamed` unset (the
    // flat path still gets its own reference check — a frame streaming
    // rejects but flat accepts is the by-design generation-model class).
    let mut streamed: Option<Vec<u8>> = None;
    if let Ok(mut dec) = StreamingDecoder::new(data) {
        let mut out = Vec::new();
        let mut sink = [0u8; 1024];
        loop {
            match dec.read(&mut sink) {
                Ok(0) => {
                    streamed = Some(out);
                    break;
                },
                Ok(n) => {
                    out.extend_from_slice(&sink[..n]);
                    if out.len() > MAX_CHECK {
                        return;
                    }
                },
                Err(_) => break,
            }
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
            },
            Err(_) => return,
        }
    }
    if flat.len() > MAX_CHECK {
        return;
    }

    // Flat vs the reference one-shot (ZSTD_decompress decodes every
    // concatenated frame whole-history, like decode_all). A reference
    // output larger than the buffer is a length divergence and fails the
    // expect; anything at most flat's size must match byte for byte.
    // Runs before the streamed compare: agreement with the reference
    // streaming output is the layout-independence adjudicator below.
    let mut rbuf = vec![0u8; flat.capacity().max(1 << 16)];
    let n = zstd::bulk::Decompressor::new()
        .expect("reference decoder init failed")
        .decompress_to_buffer(data, &mut rbuf)
        .expect("reference oneshot rejects a frame zstdx flat accepted");
    assert_eq!(
        &rbuf[..n],
        &flat[..],
        "reference and zstdx flat bytes differ"
    );

    if let Some(streamed) = &streamed {
        // Streaming vs the reference streaming decoder (multi-frame
        // inputs included: both sides decode concatenated frames).
        let mut zstream = Vec::new();
        zstd::stream::copy_decode(std::io::Cursor::new(data), &mut zstream)
            .expect("reference stream rejects a frame zstdx streaming accepted");
        // Byte-equality only on layout-independent frames (module doc).
        if zstream.as_slice() == &rbuf[..n] {
            assert_eq!(
                zstream, *streamed,
                "reference and zstdx streamed bytes differ"
            );
        }
    }
});
