#![no_main]
//! Pooled dictionary encode identity + roundtrip over the dictionary
//! attachment surfaces (R55-R58): `bulk::compress_with` with a raw and a
//! parsed dictionary, the compat `bulk::Compressor` raw attachment
//! (repeat calls, re-set, error replay), and a reused `FrameCompressor`
//! interleaving `set_dictionary`/`set_parsed_dictionary` across frames
//! (the seed-image cache's displacement paths).
//!
//! Properties: an invalid dictionary surfaces as the documented error on
//! every fallible surface (never a panic; the low-level compressor's raw
//! form documents a compress-time panic, so it only ever sees
//! pre-validated bytes); every produced frame decodes back to the payload
//! through our decoder with the dictionary attached; the second identical
//! call's bytes equal the first call's (fresh-seed determinism on the raw
//! path, image-cache restore exactness on the parsed path); the cached
//! path's frame equals the fresh-parse path's.

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;
use zstdx::{
    DecoderOptions, EncoderDictionary, EncoderOptions, Level, bulk, compat,
    encoding::{FrameCompressor, MatchGeneratorDriver},
};

const LADDER: [Level; 4] = [Level::Fastest, Level::Fast, Level::Balanced, Level::Best];

fn u16_at(data: &[u8], i: usize) -> usize {
    usize::from(u16::from_le_bytes([
        data.get(i).copied().unwrap_or(0),
        data.get(i + 1).copied().unwrap_or(0),
    ]))
}

/// Cyclic fill from a template so short inputs still yield full-length
/// buffers (an empty template degenerates to zeros).
fn fill(template: &[u8], len: usize) -> Vec<u8> {
    if template.is_empty() {
        return vec![0u8; len];
    }
    (0..len).map(|i| template[i % template.len()]).collect()
}

/// The template slice starting at `off` (cyclically).
fn region(pool: &[u8], off: usize) -> &[u8] {
    if pool.is_empty() {
        &[]
    } else {
        &pool[off % pool.len()..]
    }
}

fn decode_with_dict(frame: &[u8], dict: &[u8], payload: &[u8], what: &str) {
    let out = bulk::decompress_with(frame, 0, &DecoderOptions::new().dictionary(dict))
        .unwrap_or_else(|e| panic!("{what} does not decode through our decoder: {e}"));
    assert_eq!(&out[..], payload, "{what} roundtrip differs");
}

fn decode_plain(frame: &[u8], payload: &[u8], what: &str) {
    let out = bulk::decompress(frame, 0)
        .unwrap_or_else(|e| panic!("{what} does not decode through our decoder: {e}"));
    assert_eq!(&out[..], payload, "{what} roundtrip differs");
}

/// One frame out of a reused compressor (the drain is swapped fresh and
/// retrieved after `compress`).
fn fc_frame<'a>(
    fc: &mut FrameCompressor<Cursor<&'a [u8]>, Vec<u8>, MatchGeneratorDriver>,
    payload: &'a [u8],
) -> Vec<u8> {
    let _old = fc.set_drain(Vec::new());
    fc.set_source(Cursor::new(payload));
    fc.compress();
    fc.take_drain().expect("drain was just set")
}

fuzz_target!(|data: &[u8]| {
    // Parameter head; short inputs default everything.
    let level = LADDER[data.first().copied().unwrap_or(0) as usize % LADDER.len()];
    let other_level = LADDER[(data.first().copied().unwrap_or(0) as usize
        + 1
        + data.get(12).copied().unwrap_or(0) as usize % (LADDER.len() - 1))
        % LADDER.len()];
    let mut mask = data.get(1).copied().unwrap_or(0);
    if mask == 0 {
        mask = 0b111;
    }
    let shape = data.get(2).copied().unwrap_or(0);
    let plen = u16_at(data, 3) % (64 * 1024);
    // At least 2 bytes: the truncation shape halves the length, and an
    // empty dictionary is a legitimate parse whose attach/detach
    // semantics differ per surface (options attach, compat detaches) —
    // the cross-surface byte identity below assumes a non-empty one.
    let dlen = 2 + u16_at(data, 5) % (32 * 1024);
    let pool = &data[13.min(data.len())..];

    // The dictionary blob: a slice of the input at a mutated offset/width,
    // then the shape's truncation/flip (formatted-shaped slices come from
    // the seed corpus; any bytes form a raw-content dictionary).
    let mut dict: Vec<u8> = fill(region(pool, u16_at(data, 9)), dlen);
    match (shape >> 1) % 4 {
        1 => dict.truncate(dict.len() / 2),
        2 => {
            let mid = dict.len() / 2;
            dict[mid] ^= 0xff;
        },
        3 => {
            let mid = dict.len() / 2;
            dict[mid] ^= 0xff;
            dict.truncate(dict.len().saturating_sub(dict.len() / 4));
        },
        _ => {},
    }
    // A second, differently-sized slice of the same bytes for the
    // displacement pattern (its parse validity is checked before use).
    let dict2: Vec<u8> = fill(&dict, 1 + dict.len() * (1 + shape as usize % 7) / 8);

    // The payload: either pure input bytes or half dictionary-correlated
    // (the correlated half exercises dictionary matches).
    let template = region(pool, u16_at(data, 7));
    let payload: Vec<u8> = if shape & 1 != 0 && !dict.is_empty() {
        let head = fill(&dict, plen / 2);
        let mut p = head;
        p.extend(fill(template, plen - p.len()));
        p
    } else {
        fill(template, plen)
    };
    let pledge = Some(payload.len() as u64);

    let parsed = EncoderDictionary::parse(&dict);
    let parsed2 = EncoderDictionary::parse(&dict2);

    match parsed {
        // The documented error surfaces: `compress_with` fails, the compat
        // attachment errors at the first compress and replays at every
        // later one, and detaching heals.
        Err(parse_err) => {
            let opts = EncoderOptions::new(level).dictionary(&dict);
            match bulk::compress_with(&payload, &opts) {
                Err(e) => assert!(
                    matches!(e, zstdx::Error::Dictionary(_)),
                    "wrong error for an invalid dictionary: {e} (parse said {parse_err})"
                ),
                Ok(frame) => panic!(
                    "compress_with accepted a dictionary the parse rejected: {} bytes",
                    frame.len()
                ),
            }
            let li = level.as_i32();
            let mut c = compat::bulk::Compressor::with_dictionary(li, &dict).unwrap();
            assert!(c.compress(&payload).is_err(), "first compress must fail");
            assert!(c.compress(&payload).is_err(), "the failure must replay");
            c.set_dictionary(li, &dict).unwrap();
            assert!(
                c.compress(&payload).is_err(),
                "a re-set attachment must re-parse and fail again"
            );
            c.set_dictionary(li, &[]).unwrap();
            let plain = c
                .compress(&payload)
                .expect("detaching must heal the compressor");
            decode_plain(&plain, &payload, "post-detach plain frame");
        },
        Ok(parsed) => {
            // (a) bulk: raw and parsed attachments, repeat calls.
            let opts_raw = EncoderOptions::new(level)
                .dictionary(&dict)
                .pledged_size(pledge);
            let r1 = bulk::compress_with(&payload, &opts_raw).unwrap();
            let r2 = bulk::compress_with(&payload, &opts_raw).unwrap();
            assert_eq!(r1, r2, "raw-path repeat call diverged");

            let opts_par = EncoderOptions::new(level)
                .parsed_dictionary(&parsed)
                .pledged_size(pledge);
            let p1 = bulk::compress_with(&payload, &opts_par).unwrap();
            let p2 = bulk::compress_with(&payload, &opts_par).unwrap();
            assert_eq!(
                p1, p2,
                "cached-path frame differs from the fresh-parse frame"
            );
            assert_eq!(
                r1, p1,
                "raw and parsed attachments produce different frames"
            );
            decode_with_dict(&r1, &dict, &payload, "raw-path frame");
            decode_with_dict(&p2, &dict, &payload, "cached-path frame");

            // (b) the compat attachment: repeat calls ride the cached
            // parse, a re-set discards it wholesale.
            if mask & 0b10 != 0 {
                let li = level.as_i32();
                let mut c = compat::bulk::Compressor::with_dictionary(li, &dict).unwrap();
                let c1 = c.compress(&payload).unwrap();
                let c2 = c.compress(&payload).unwrap();
                assert_eq!(c1, c2, "compat repeat call diverged");
                assert_eq!(
                    c1, r1,
                    "compat frame differs from the equivalent compress_with call"
                );
                c.set_dictionary(li, &dict).unwrap();
                let c3 = c.compress(&payload).unwrap();
                assert_eq!(c3, c1, "re-set attachment diverged");
                decode_with_dict(&c3, &dict, &payload, "compat frame");
                c.set_dictionary(li, &[]).unwrap();
                let plain = c.compress(&payload).unwrap();
                decode_plain(&plain, &payload, "compat detached plain frame");
            }

            // (c) a reused low-level compressor: parsed and raw
            // attachments displaced across sequential frames (the raw
            // form re-parses per frame, so every swap displaces the seed
            // image), with a second compressor at another level
            // interleaved; identical repeats must stay byte-identical.
            // The source length is always declared: the unpledged raw
            // form sizes its window from the dictionary content alone and
            // emits blocks past it (engine bug, referred — see the R59B
            // campaign notes), so the documented shape-declaring use is
            // what runs here.
            if mask & 0b100 != 0 {
                let shape_len = zstdx::InputShape::default().with_len(payload.len() as u64);
                let mut fc: FrameCompressor<Cursor<&[u8]>, Vec<u8>, MatchGeneratorDriver> =
                    FrameCompressor::new(level);
                fc.set_input_shape(shape_len);
                let mut fc2: FrameCompressor<Cursor<&[u8]>, Vec<u8>, MatchGeneratorDriver> =
                    FrameCompressor::new(other_level);
                fc2.set_input_shape(shape_len);

                let f1 = {
                    fc.set_parsed_dictionary(&parsed);
                    fc_frame(&mut fc, &payload)
                };
                let f2 = {
                    fc.set_dictionary(&dict);
                    fc_frame(&mut fc, &payload)
                };
                let f3 = {
                    fc.set_parsed_dictionary(&parsed);
                    fc_frame(&mut fc, &payload)
                };
                let f4 = match &parsed2 {
                    Ok(p2d) => {
                        fc2.set_parsed_dictionary(p2d);
                        Some(fc_frame(&mut fc2, &payload))
                    },
                    // The raw form documents a compress-time panic on
                    // invalid bytes, so only pre-validated dicts go in.
                    Err(_) => None,
                };
                // Consecutive identical calls: a guaranteed seed-image
                // cache hit on the second.
                let f5 = fc_frame(&mut fc, &payload);
                let f6 = fc_frame(&mut fc, &payload);

                assert_eq!(f5, f6, "seed-image cache restore is not exact");
                assert_eq!(f1, f3, "parsed frame changed across displacements");
                assert_eq!(f1, f5, "displacement changed the frame bytes");
                assert_eq!(f2, f1, "raw and parsed low-level frames differ");
                for (f, what) in [
                    (&f1, "low-level parsed frame"),
                    (&f2, "low-level raw frame"),
                    (&f5, "low-level cached frame"),
                ] {
                    decode_with_dict(f, &dict, &payload, what);
                }
                if let Some(f4) = &f4 {
                    decode_with_dict(f4, &dict2, &payload, "low-level second-dict frame");
                }
            }
        },
    }
});
