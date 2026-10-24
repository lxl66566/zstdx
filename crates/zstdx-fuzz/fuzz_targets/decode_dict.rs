#![no_main]
//! Dictionary decode paths (R73). Two surfaces:
//!
//! - robustness: arbitrary bytes parsed as a dictionary must fail cleanly, never panic (the
//!   original target surface, kept on the whole input and on the dictionary slice of the pair
//!   below);
//! - pair seeds: `[dlen_lo, dlen_hi, dict..., frame...]` with `dlen` a LE u16 clamped to the
//!   available bytes — the frame decodes with the dictionary attached the way the options-based
//!   entries attach it (own id and the null id, so id-suppressed frames decode too) through the
//!   streaming path (the generation model: dictionary content is a virtual generation-0 prefix that
//!   expires at the first ring wrap) and the flat whole-history path, each against its own
//!   reference counterpart with the dictionary — streaming vs the reference streaming decoder, flat
//!   vs the reference one-shot. The two zstdx paths are NOT compared with each other: they differ
//!   by design once the ring wraps (R72); see the `decode` target. Checks are one-directional
//!   (zstdx rejecting where the reference accepts is the stricter generation model), and the
//!   streamed byte-compare only runs when the reference's own streaming and one-shot outputs agree
//!   (layout-independent frames; on window-invalid aliased frames the accepted bytes are
//!   caller-layout defined — see the `decode` target's module doc). Oversized outputs skip the
//!   checks (decode bombs; see the `decode` target).
#[macro_use]
extern crate libfuzzer_sys;
extern crate zstdx;

use std::{io::Read, sync::Arc};

use zstdx::decoding::{Dictionary, FrameDecoder, StreamingDecoder, errors::FrameDecoderError};

/// Output past this bound skips every check.
const MAX_CHECK: usize = 32 << 20;

fuzz_target!(|data: &[u8]| {
    Dictionary::decode_dict(data).ok(); // may return error on invalid data, may not panic.

    let (head, rest) = match data {
        [a, b, rest @ ..] => (u16::from_le_bytes([*a, *b]) as usize, rest),
        _ => return,
    };
    let dlen = head.min(rest.len()).min(64 * 1024);
    let (dict, frame) = rest.split_at(dlen);
    Dictionary::decode_dict(dict).ok();
    if frame.is_empty() {
        return;
    }
    let Ok(dict) = Dictionary::load(dict) else {
        return;
    };
    let dict = Arc::new(dict);

    // Streaming pass; a mid-stream error leaves `streamed` unset (the
    // flat path still gets its own reference check — generation-model
    // reject/accept flips are by design).
    let mut streamed: Option<Vec<u8>> = None;
    if let Ok(mut dec) = StreamingDecoder::new_with_decoder(frame, {
        let mut d = FrameDecoder::new();
        d.add_options_dict(Arc::clone(&dict)).unwrap();
        d
    }) {
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

    let mut flat = Vec::new();
    let mut step = 1usize << 16;
    loop {
        let mut fr = FrameDecoder::new();
        fr.add_options_dict(Arc::clone(&dict)).unwrap();
        match fr.decode_all_to_vec(frame, &mut flat) {
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

    // Flat vs the reference one-shot with the dictionary
    // (ZSTD_decompress_usingDDict semantics, whole history). A reference
    // output larger than the buffer is a length divergence and fails the
    // expect. Runs before the streamed compare: agreement with the
    // reference streaming output is the layout-independence adjudicator.
    let mut rbuf = vec![0u8; flat.capacity().max(1 << 16)];
    let n = zstd::bulk::Decompressor::with_dictionary(&rest[..dlen])
        .expect("reference decoder init failed on a parsed dictionary")
        .decompress_to_buffer(frame, &mut rbuf)
        .expect("reference oneshot rejects a dict frame zstdx flat accepted");
    assert_eq!(
        &rbuf[..n],
        &flat[..],
        "reference and zstdx flat dict bytes differ"
    );

    if let Some(streamed) = &streamed {
        let mut zstream = Vec::new();
        let mut zdec =
            zstd::stream::Decoder::with_dictionary(std::io::Cursor::new(frame), &rest[..dlen])
                .expect("reference decoder init failed on a parsed dictionary");
        zdec.read_to_end(&mut zstream)
            .expect("reference rejects a dict frame zstdx streaming accepted");
        // Byte-equality only on layout-independent frames (module doc).
        if zstream.as_slice() == &rbuf[..n] {
            assert_eq!(
                zstream, *streamed,
                "reference and zstdx streamed dict bytes differ"
            );
        }
    }
});
