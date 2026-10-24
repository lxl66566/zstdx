//! Encoder edge differential against the reference libzstd: crafted payload
//! shapes (boundary sizes, run and period classes, window-straddling
//! repeats, dictionary echoes) across the full numeric level ladder and the
//! option combinations (checksum, pledged size, forced window, LDM,
//! dictionary, workers) must decode back to the exact input through the
//! reference decoder — every wire-legality class (offset within the
//! declared window, block-size caps, header field sizes) is enforced by
//! that decode — and our own flat decoder must agree on the same bytes.

use alloc::{format, vec::Vec};

/// Deterministic xorshift64 payload source.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn byte(&mut self) -> u8 {
        (self.next() >> 33) as u8
    }
}

/// One crafted payload class: the wire edges concentrate at size-format,
/// literals-section and block boundaries, and at run/period shapes that
/// drive repcodes, RLE literals and long matches.
fn shape(kind: &str, len: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(len);
    match kind {
        "zeros" => data.resize(len, 0),
        "text" => {
            let template = b"{\"id\":123,\"name\":\"user\",\"tags\":[\"a\",\"b\"],\"score\":42}\n";
            while data.len() < len {
                data.extend_from_slice(&template[..template.len().min(len - data.len())]);
            }
        },
        "period4" | "period8" | "period1000" | "period131063" => {
            let period = match kind {
                "period4" => 4,
                "period8" => 8,
                "period1000" => 1000,
                _ => 131063,
            };
            let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
            let unit: Vec<u8> = (0..period).map(|_| rng.byte()).collect();
            while data.len() < len {
                data.extend_from_slice(&unit[..unit.len().min(len - data.len())]);
            }
        },
        // Beyond-window redundancy with no near twin (the todo-26
        // reproducer classes): `farfrags` spreads a small fragment pool
        // so every twin sits a few KiB out (past the small forced
        // windows), `twinhalves` repeats its whole first half a
        // half-payload later (past a w16 window).
        "farfrags" => {
            let mut rng = Rng(0xf00d_f00d_f00d_f00d);
            let frags: alloc::vec::Vec<alloc::vec::Vec<u8>> = (0..40)
                .map(|_| (0..24).map(|_| rng.byte()).collect())
                .collect();
            let mut rng = Rng(0xfeed_face_cafe_babe);
            let mut rng = Rng(0xfeed_face_cafe_babe);
            while data.len() < len {
                let n = 1 + rng.next() as usize % 3;
                for _ in 0..n {
                    data.extend_from_slice(&frags[rng.next() as usize % frags.len()]);
                    let noise = 8 + rng.next() as usize % 40;
                    for _ in 0..noise {
                        data.push(rng.byte());
                    }
                }
            }
            data.truncate(len);
        },
        "twinhalves" => {
            let mut rng = Rng(0x5eed_5eed_5eed_5eed);
            let frags: alloc::vec::Vec<alloc::vec::Vec<u8>> = (0..64)
                .map(|_| (0..40).map(|_| rng.byte()).collect())
                .collect();
            let mut rng = Rng(0x1bad_b002_bad0_dad1);
            while data.len() < len / 2 {
                data.extend_from_slice(&frags[rng.next() as usize % frags.len()]);
                let noise = 20 + rng.next() as usize % 30;
                for _ in 0..noise {
                    data.push(rng.byte());
                }
            }
            // The second half replays the first half's span with fresh
            // noise between: long matches at ~len/2 distance, none nearer.
            let first = data.clone();
            let mut i = 0usize;
            while data.len() < len {
                let end = (i + 40).min(first.len());
                data.extend_from_slice(&first[i..end]);
                i = end;
                if i >= first.len() {
                    i = 0;
                }
                let noise = 20 + rng.next() as usize % 30;
                for _ in 0..noise {
                    data.push(rng.byte());
                }
            }
            data.truncate(len);
        },
        "run_mix" => {
            // Long uniform runs interleaved with fresh noise: RLE literals,
            // long matches, and entropy tails in one payload.
            let mut rng = Rng(0x0123_4567_89ab_cdef);
            while data.len() < len {
                let run = 1 + rng.next() as usize % 3000;
                let byte = rng.byte();
                data.resize((data.len() + run).min(len), byte);
                let noise = 1 + rng.next() as usize % 300;
                for _ in 0..noise.min(len - data.len()) {
                    data.push(rng.byte());
                }
            }
        },
        _ => {
            let mut rng = Rng(0x1234_5678_9abc_def0);
            for _ in 0..len {
                data.push(rng.byte());
            }
        },
    }
    data
}

/// The differential core: compress through `zstdx::bulk::compress_with`,
/// decode through the reference and through our flat decoder, compare both
/// with the input. `dict` attaches the frame's dictionary on the reference
/// side (an id-carrying frame is rejected without it, a suppressed-id frame
/// needs it to decode at all).
fn check(payload: &[u8], label: &str, opts: &crate::EncoderOptions) {
    check_with_dict(payload, label, opts, None);
}

fn check_with_dict(payload: &[u8], label: &str, opts: &crate::EncoderOptions, dict: Option<&[u8]>) {
    let compressed = crate::bulk::compress_with(payload, opts)
        .unwrap_or_else(|e| panic!("{label}: compress failed: {e:?}"));
    let mut reference = Vec::new();
    match dict {
        None => {
            zstd::stream::copy_decode(compressed.as_slice(), &mut reference)
                .unwrap_or_else(|e| panic!("{label}: reference decode failed: {e:?}"));
        },
        Some(dict) => {
            let mut zdec = zstd::bulk::Decompressor::with_dictionary(dict)
                .unwrap_or_else(|e| panic!("{label}: reference decoder init failed: {e:?}"));
            let mut out = alloc::vec![0u8; payload.len()];
            let n = zdec
                .decompress_to_buffer(&compressed, &mut out)
                .unwrap_or_else(|e| panic!("{label}: reference decode failed: {e:?}"));
            reference.extend_from_slice(&out[..n]);
        },
    }
    assert_eq!(reference, payload, "{label}: reference bytes differ");
    let ours = match dict {
        None => crate::bulk::decompress(&compressed, payload.len())
            .unwrap_or_else(|e| panic!("{label}: own decode failed: {e:?}")),
        Some(dict) => {
            let dec_opts = crate::DecoderOptions::new().dictionary(dict);
            crate::bulk::decompress_with(&compressed, payload.len(), &dec_opts)
                .unwrap_or_else(|e| panic!("{label}: own decode failed: {e:?}"))
        },
    };
    assert_eq!(ours, payload, "{label}: own bytes differ");
}

/// Tiny and size-boundary payloads through the full numeric ladder: every
/// strategy row (fast, dfast, row, chain, btlazy, btopt, btultra, the wide
/// rows 20-22) must produce frames the reference decodes byte-exactly.
#[test]
fn size_edges_full_ladder() {
    let sizes = [
        0usize, 1, 2, 3, 4, 5, 7, 8, 9, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 128, 129, 255,
        256, 257, 1023, 1024, 1025, 4095, 4096, 4097,
    ];
    for kind in ["text", "random", "period4", "zeros"] {
        for &len in &sizes {
            let payload = shape(kind, len);
            for level in 1..=22 {
                let opts = crate::EncoderOptions::new(crate::Level::from_zstd(level));
                check(&payload, &format!("{kind}/{len}/l{level}"), &opts);
            }
        }
    }
}

/// Block-boundary sizes (64/128 KiB cuts and the maximum encodable match
/// length) over the repcode-heavy and mixed shapes.
#[test]
fn block_edges() {
    let sizes = [
        65535usize, 65536, 65537, 131071, 131072, 131073, 131074, 131075, 262145,
    ];
    for kind in ["period8", "run_mix", "text"] {
        for &len in &sizes {
            let payload = shape(kind, len);
            let levels = if len >= 262_145 {
                &[1i32, 3, 9, 19][..]
            } else {
                &[1, 3, 9, 13, 19, 22][..]
            };
            for &level in levels {
                let opts = crate::EncoderOptions::new(crate::Level::from_zstd(level));
                check(&payload, &format!("{kind}/{len}/l{level}"), &opts);
            }
        }
    }
}

/// Option combinations over shapes that stress their specific wire class:
/// the forced small window must keep every emitted offset inside it, the
/// raised window and LDM feeds must stay inside the format's reach, the
/// pledge must land in the header exactly, and the checksum must verify.
#[test]
fn option_combos() {
    let periodic = shape("period1000", 300 * 1024);
    for window_log in [10u32, 12] {
        for level in [1i32, 3, 9, 13] {
            let opts = crate::EncoderOptions::new(crate::Level::from_zstd(level))
                .with_input_shape(crate::InputShape::default().with_window_log(window_log));
            check(&periodic, &format!("window{window_log}/l{level}"), &opts);
        }
    }

    let text = shape("text", 70_000);
    for level in [1i32, 9, 19] {
        let base = crate::EncoderOptions::new(crate::Level::from_zstd(level));
        check(
            &text,
            &format!("checksum/l{level}"),
            &base.clone().checksum(true),
        );
        check(
            &text,
            &format!("pledged/l{level}"),
            &base.clone().pledged_size(Some(text.len() as u64)),
        );
        let small = shape("text", 300);
        check(
            &small,
            &format!("pledged-small/l{level}"),
            &crate::EncoderOptions::new(crate::Level::from_zstd(level))
                .checksum(true)
                .pledged_size(Some(small.len() as u64)),
        );
    }

    let far = shape("period131063", 600 * 1024);
    for level in [1i32, 3, 9, 17] {
        let opts = crate::EncoderOptions::new(crate::Level::from_zstd(level))
            .long_distance_matching(crate::LdmMode::Enable);
        check(&far, &format!("ldm-enable/l{level}"), &opts);
    }
    let opts = crate::EncoderOptions::new(crate::Level::from_zstd(19))
        .long_distance_matching(crate::LdmMode::Disable);
    check(&far, "ldm-disable/l19", &opts);

    let wide = shape("text", 100 * 1024);
    for level in [1i32, 3] {
        let opts = crate::EncoderOptions::new(crate::Level::from_zstd(level))
            .with_input_shape(crate::InputShape::default().with_window_log(27));
        check(&wide, &format!("window27/l{level}"), &opts);
    }
}

/// Window and LDM classes whose wire legality is the point: repeats at a
/// period the forced window barely covers (or just exceeds — the encoder
/// must not reference them), windows below the format's 1 KiB floor (the
/// descriptor clamps up, offsets stay inside), pledges on windowed frames,
/// and LDM forced onto shapes auto-arming would never touch (tiny, empty,
/// uniform).
#[test]
fn window_and_ldm_edges() {
    // A noisy unit so the far repeats are matches, not RLE short-circuits.
    let mut unit = shape("random", 4000);
    let periods = [1000usize, 1024, 1025, 2000, 2048, 2049];
    for &period in &periods {
        unit.truncate(period);
        let mut payload = Vec::with_capacity(300 * 1024);
        while payload.len() < 300 * 1024 {
            payload.extend_from_slice(&unit);
        }
        payload.truncate(300 * 1024);
        for window_log in [10u32, 11] {
            for level in [1i32, 3, 9] {
                let opts = crate::EncoderOptions::new(crate::Level::from_zstd(level))
                    .with_input_shape(crate::InputShape::default().with_window_log(window_log));
                check(
                    &payload,
                    &format!("period{period}/window{window_log}/l{level}"),
                    &opts,
                );
            }
        }
    }

    // Windows below the format floor and at the engine cap.
    let small = shape("period1000", 8192);
    for window_log in [7u32, 27] {
        for level in [1i32, 9] {
            let opts = crate::EncoderOptions::new(crate::Level::from_zstd(level))
                .with_input_shape(crate::InputShape::default().with_window_log(window_log));
            check(&small, &format!("window{window_log}/l{level}"), &opts);
        }
    }

    // A pledge on a windowed frame (declared past the window) and the
    // zero-length pledge classes with and without the checksum.
    let windowed = shape("text", 200 * 1024);
    for level in [1i32, 9, 19] {
        let opts = crate::EncoderOptions::new(crate::Level::from_zstd(level))
            .with_input_shape(crate::InputShape::default().with_window_log(10))
            .pledged_size(Some(windowed.len() as u64));
        check(&windowed, &format!("pledged-windowed/l{level}"), &opts);
    }
    for level in [1i32, 19] {
        for checksum in [true, false] {
            let opts = crate::EncoderOptions::new(crate::Level::from_zstd(level))
                .checksum(checksum)
                .pledged_size(Some(0));
            check(&[], &format!("pledged-empty/l{level}/c{checksum}"), &opts);
        }
    }

    // LDM forced where auto-arming never engages.
    for (label, payload, level) in [
        ("tiny", shape("period1000", 100), 1i32),
        ("tiny", shape("text", 8), 19),
        ("empty", Vec::new(), 9),
        ("zeros", shape("zeros", 512 * 1024), 1),
        ("random", shape("random", 512 * 1024), 3),
    ] {
        let opts = crate::EncoderOptions::new(crate::Level::from_zstd(level))
            .long_distance_matching(crate::LdmMode::Enable);
        check(&payload, &format!("ldm-forced/{label}/l{level}"), &opts);
    }
}

/// Forced windows on UNPLEDGED streams (todo 26): the unknown length
/// leaves the source clamp out of param resolution, so every row whose
/// search domain rides `chain_reach` (chain 9, btlazy 13-15, opt 16-19)
/// scanned past the declared window while the header kept declaring it —
/// frames the reference rejects (the bulk cells in `option_combos` stay
/// clamped through their known length, which is exactly why the grid
/// above never caught it). Both stream cores, no pledge, no length hint,
/// reference decode on every frame; rows 20-22 (no reach override) ride
/// along as the control.
#[cfg(feature = "std")]
#[test]
fn forced_window_unpledged_streams() {
    let shapes = [
        ("farfrags", shape("farfrags", 300 * 1024)),
        ("twinhalves", shape("twinhalves", 300 * 1024)),
    ];
    for (kind, payload) in &shapes {
        for window_log in [10u32, 16] {
            for level in [9i32, 13, 16, 19, 22] {
                let opts = crate::EncoderOptions::new(crate::Level::from_zstd(level))
                    .with_input_shape(crate::InputShape::default().with_window_log(window_log));
                let label = format!("{kind}/w{window_log}/l{level}");
                // write core: chunked writes, no pledge, no length hint.
                let mut sink = alloc::vec![];
                {
                    use crate::io::Write;
                    let mut enc =
                        crate::stream::write::Encoder::with_options(&mut sink, opts.clone())
                            .unwrap();
                    for piece in payload.chunks(7_000) {
                        enc.write_all(piece).unwrap();
                    }
                    enc.finish().unwrap();
                }
                zstd::stream::decode_all(&sink[..])
                    .unwrap_or_else(|e| panic!("{label}/write: reference decode failed: {e:?}"));
                assert_eq!(
                    crate::stream::decode_all(&sink[..]).unwrap(),
                    *payload,
                    "{label}/write: own bytes differ"
                );
                // read core: whole-frame pull, no pledge, no length hint.
                let mut compressed = alloc::vec![];
                {
                    use crate::io::Read;
                    let mut enc =
                        crate::stream::read::Encoder::with_options(payload.as_slice(), opts)
                            .unwrap();
                    enc.read_to_end(&mut compressed).unwrap();
                }
                zstd::stream::decode_all(&compressed[..])
                    .unwrap_or_else(|e| panic!("{label}/read: reference decode failed: {e:?}"));
                assert_eq!(
                    crate::stream::decode_all(&compressed[..]).unwrap(),
                    *payload,
                    "{label}/read: own bytes differ"
                );
            }
        }
    }
}

/// Forced windows on the small-src btlazy swap (levels 9/10, len <= 128
/// KiB, R73): the swap picks its own geometry on the known-length path
/// and pre-R73 ignored the forced window there (declared 1<<17 under a
/// forced w10 — legal but a fidelity deviation). C applies the user's
/// windowLog after the small-table pick (`ZSTD_overrideCParams` inside
/// `ZSTD_getCParamsFromCCtxParams`, then adjustCParams' source clamp),
/// so the swap must resolve under the forced window exactly like the
/// ladder rows: the header declares it, and the frame still
/// reference-decodes. A wide-alphabet head keeps the swap's row
/// (`small_src_btlazy_head`'s bar), so these cells pin the swap path
/// itself.
#[cfg(feature = "std")]
#[test]
fn forced_window_small_src_swap() {
    let payload = shape("text", 100 * 1024);
    let declared_window = |frame: &[u8]| {
        crate::decoding::frame::read_frame_header(frame)
            .unwrap()
            .0
            .window_size()
            .unwrap()
    };
    for window_log in [10u32, 12] {
        for level in [9i32, 10] {
            let opts = crate::EncoderOptions::new(crate::Level::from_zstd(level))
                .with_input_shape(crate::InputShape::default().with_window_log(window_log));
            let label = format!("w{window_log}/l{level}");
            // Bulk: the known-length path the swap rides.
            let bulk = crate::bulk::compress_with(&payload, &opts).unwrap();
            assert_eq!(declared_window(&bulk), 1u64 << window_log, "{label}/bulk");
            zstd::stream::decode_all(bulk.as_slice())
                .unwrap_or_else(|e| panic!("{label}/bulk: reference decode failed: {e:?}"));
            assert_eq!(
                crate::bulk::decompress(&bulk, payload.len()).unwrap(),
                payload
            );
            // Pledged stream twin: same declared window, same decode.
            let mut sink = alloc::vec![];
            {
                use crate::io::Write;
                let mut enc = crate::stream::write::Encoder::with_options(
                    &mut sink,
                    opts.clone().pledged_size(Some(payload.len() as u64)),
                )
                .unwrap();
                enc.write_all(&payload).unwrap();
                enc.finish().unwrap();
            }
            assert_eq!(declared_window(&sink), 1u64 << window_log, "{label}/stream");
            zstd::stream::decode_all(&sink[..])
                .unwrap_or_else(|e| panic!("{label}/stream: reference decode failed: {e:?}"));
            assert_eq!(crate::stream::decode_all(&sink[..]).unwrap(), payload);
        }
    }
}

/// Dictionary frames at the tiny-payload edge (the first block ≤ 8 bytes
/// class) and over the real `zstd --train` fixture: raw-content and
/// formatted dictionaries, the id-suppressed form, single- and
/// multithreaded.
#[test]
fn dictionary_edges() {
    let Some(dict) = super::fixture_bytes("./dict_tests/dictionary") else {
        return;
    };
    let raw: Vec<u8> = shape("text", 1024);
    for payload_len in [0usize, 1, 3, 7, 8, 9, 16, 100, 4096] {
        let payload = shape("text", payload_len);
        for level in [1i32, 3, 9, 13, 19, 22] {
            let opts = crate::EncoderOptions::new(crate::Level::from_zstd(level)).dictionary(&dict);
            check_with_dict(
                &payload,
                &format!("formatted-dict/{payload_len}/l{level}"),
                &opts,
                Some(&dict),
            );
            let opts = crate::EncoderOptions::new(crate::Level::from_zstd(level)).dictionary(&raw);
            check_with_dict(
                &payload,
                &format!("raw-dict/{payload_len}/l{level}"),
                &opts,
                Some(&raw),
            );
        }
    }
    let echo = shape("text", 32 * 1024);
    for level in [3i32, 9, 19] {
        let opts = crate::EncoderOptions::new(crate::Level::from_zstd(level)).dictionary(&dict);
        check_with_dict(
            &echo,
            &format!("formatted-dict-echo/l{level}"),
            &opts,
            Some(&dict),
        );
        let opts = crate::EncoderOptions::new(crate::Level::from_zstd(level))
            .dictionary(&dict)
            .include_dictid(false);
        check_with_dict(
            &echo,
            &format!("dict-suppressed/l{level}"),
            &opts,
            Some(&dict),
        );
    }
}

/// Multithreaded job decomposition on the option path: workers > 1 with and
/// without a dictionary must stay byte-legal (the reference decode) over
/// multi-job payloads.
#[test]
#[cfg(feature = "std")]
fn multithreaded_edges() {
    let payload = shape("run_mix", 2 * 1024 * 1024);
    for level in [1i32, 3, 9, 13] {
        let opts = crate::EncoderOptions::new(crate::Level::from_zstd(level)).workers(3);
        check(&payload, &format!("mt3/l{level}"), &opts);
    }
    let Some(dict) = super::fixture_bytes("./dict_tests/dictionary") else {
        return;
    };
    let echo = shape("text", 2 * 1024 * 1024);
    for level in [3i32, 9] {
        let opts = crate::EncoderOptions::new(crate::Level::from_zstd(level))
            .workers(3)
            .dictionary(&dict);
        check_with_dict(&echo, &format!("mt3-dict/l{level}"), &opts, Some(&dict));
    }
}

/// Pooled-driver table geometry across levels: the per-thread pooled slice
/// state carries `tables`/`second` between `bulk::compress` calls, and two
/// Row levels can share a row count at different hash logs once the
/// source-length clamp shrinks the head table (10's rowLog 5 at srcLog 11,
/// 5's rowLog 4 at srcLog 10 — both 128 rows). The rows-kept resize path
/// must keep `second` in lockstep with the head span; a stale larger
/// `second` left the next Fast frame's `tables[..second]` past its buffer.
#[test]
#[cfg(feature = "std")]
fn pooled_row_geometry() {
    let a = shape("text", 1500);
    let b = shape("text", 750);
    for (payload, level) in [(&a[..], 10i32), (&b[..], 5), (&a[..], 1)] {
        let opts = crate::EncoderOptions::new(crate::Level::from_zstd(level))
            .pledged_size(Some(payload.len() as u64));
        check(payload, &format!("pooled-geometry/l{level}"), &opts);
    }
}

/// Pooled table geometry across the FULL ladder at the `adjust_params`
/// clamp boundaries: the per-thread pooled slice state carries its tables
/// between `bulk::compress_with` calls, and the source-length clamp
/// retunes the table geometry per level (srcLog crossings at 2^9..2^12,
/// where Row hash logs and chain table sizes step), so every (len, level)
/// cell must re-derive its geometry from a pool the previous level left
/// behind and still roundtrip. Serial by construction: one thread, outer
/// loop over the lengths, inner loop over the whole ladder 0..=22.
#[test]
#[cfg(feature = "std")]
fn pooled_ladder_sweep() {
    let lens = [
        (1 << 9) - 1,
        1 << 9,
        (1 << 9) + 1,
        (1 << 10) - 1,
        1 << 10,
        (1 << 10) + 1,
        (1 << 11) - 1,
        1 << 11,
        (1 << 11) + 1,
        (1 << 12) - 1,
        1 << 12,
        (1 << 12) + 1,
        1500,
        750,
    ];
    // Rotate the payload class with the length so the sweep stresses the
    // hash tables (text), the chain/row fill (random), repcodes (period4)
    // and RLE literals (zeros) as it walks the ladder.
    for (idx, &len) in lens.iter().enumerate() {
        let kind = ["text", "random", "period4", "zeros"][idx % 4];
        let payload = shape(kind, len);
        for level in 0..=22 {
            let opts = crate::EncoderOptions::new(crate::Level::from_zstd(level));
            check(
                &payload,
                &format!("pooled-sweep/{kind}/{len}/l{level}"),
                &opts,
            );
        }
    }
}
