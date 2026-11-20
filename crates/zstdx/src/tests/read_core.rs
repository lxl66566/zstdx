//! Read-side encode core edges: frames out of the generic
//! `FrameCompressor<M>` loop (the `encoding::compress*` entry points and
//! `FrameCompressor::new_with_matcher`) across forced small windows and
//! custom matchers, gated through both decoders.

use alloc::{format, string::ToString, vec::Vec};

/// Period-61 payload, 300 KiB: matches cross every block boundary a
/// forced small window produces.
fn periodic() -> Vec<u8> {
    (0..300u32 * 1024).map(|i| (i % 61) as u8).collect()
}

/// The generic loop must cap blocks at the matcher's declared window (RFC
/// 8878: Block_Maximum_Size = min(window, 128 KiB)): it resolved
/// `Matcher::block_size`'s 128 KiB default, so every forced window below
/// 128 KiB emitted oversized blocks that both decoders reject.
#[test]
fn block_cap_follows_the_declared_window() {
    let data = periodic();
    for window_log in [10u32, 12, 15, 16, 17] {
        let shape = crate::InputShape::default().with_window_log(window_log);
        let frame =
            crate::encoding::compress_to_vec_shaped(data.as_slice(), crate::Level::Fastest, shape);
        assert_eq!(
            zstd::stream::decode_all(frame.as_slice()).unwrap(),
            data,
            "window_log {window_log}: reference rejects the frame"
        );
        assert_eq!(
            crate::bulk::decompress(&frame, data.len()).unwrap(),
            data,
            "window_log {window_log}: own decoder rejects the frame"
        );
    }
}

/// A custom matcher staging a tail shorter than its own block cap must
/// still yield a valid frame: the loop clamps the read slice to the
/// staged tail instead of panicking on the overshoot.
#[test]
fn small_slice_matcher_does_not_panic() {
    let data = periodic();
    let matcher = crate::encoding::MatchGeneratorDriver::new(64 * 1024);
    let mut sink = Vec::new();
    let mut enc =
        crate::encoding::FrameCompressor::new_with_matcher(matcher, crate::Level::Fastest);
    enc.set_source(data.as_slice());
    enc.set_drain(&mut sink);
    enc.compress();
    assert_eq!(zstd::stream::decode_all(sink.as_slice()).unwrap(), data);
    assert_eq!(crate::bulk::decompress(&sink, data.len()).unwrap(), data);
}

/// `InputShape::len` is a non-binding hint (the stream cores'
/// `input_shape_len_is_a_hint_not_a_pledge` contract): the read-side core
/// once ended the frame at the declared length, silently dropping every
/// byte past a hint that landed on the block grid.
#[test]
fn input_hint_never_truncates_the_frame() {
    let data = periodic();
    // 128 KiB: the hint lands on the block grid, where the truncation cut.
    let hint = crate::InputShape::default().with_len(128 * 1024);
    let frame =
        crate::encoding::compress_to_vec_shaped(data.as_slice(), crate::Level::Fastest, hint);
    let decoded = zstd::stream::decode_all(frame.as_slice()).expect("frame decodes");
    assert_eq!(decoded, data);
    assert_eq!(crate::bulk::decompress(&frame, data.len()).unwrap(), data);
    // The hint sizes the row, never the header promise: no FCS field.
    assert_eq!(
        zstd::zstd_safe::get_frame_content_size(&frame).unwrap(),
        None
    );

    // An oversized hint must stay a valid frame too: it sized the window
    // around input that never arrives, never the frame's end.
    let over = crate::InputShape::default().with_len(1 << 20);
    let frame =
        crate::encoding::compress_to_vec_shaped(data.as_slice(), crate::Level::Fastest, over);
    assert_eq!(zstd::stream::decode_all(frame.as_slice()).unwrap(), data);
    assert_eq!(crate::bulk::decompress(&frame, data.len()).unwrap(), data);
}

/// Two phases alternating on a 64 KiB lattice, so every 128 KiB block
/// straddles an entropy shift: the content class whose pre-split cuts fire
/// (levels 5-12) with a leftover after nearly every decided block.
fn phase_shifted(len: usize) -> Vec<u8> {
    fn lcg(seed: u64) -> impl FnMut() -> u64 {
        let mut s = seed;
        move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        }
    }
    let mut r = lcg(0xdead_1010);
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        let phase = (out.len() / (64 * 1024)) % 2;
        let start = out.len();
        if phase == 0 {
            for _ in 0..(64 * 1024) {
                out.extend_from_slice(&r().to_le_bytes());
            }
        } else {
            for _ in 0..(64 * 1024) {
                out.extend_from_slice(b"{\"k\":1234567890,\"v\":[1,2,3,4,5]}\n");
                out.extend_from_slice(&(r() % 100).to_string().into_bytes());
            }
        }
        out.truncate(start + 64 * 1024);
    }
    out.truncate(len);
    out
}

/// The pre-split carry once stayed in the matcher's uncommitted tail across
/// the next `block_tail` reservation. Past `2 * window + slice_size`
/// committed bytes the reservation compacts the owned window, and the
/// compaction moves committed bytes only: the block staged right after it
/// spliced stale pre-compaction window bytes in front of its fresh ones.
/// The frame stayed structurally valid with a self-consistent checksum, so
/// both decoders returned the wrong payload without any error.
#[test]
fn pre_split_carry_survives_the_window_compaction() {
    let data = phase_shifted(1_750_000);
    let shape = crate::InputShape::default().with_window_log(18);
    let level = crate::Level::from_zstd(5);
    let frame = crate::encoding::compress_to_vec_shaped(data.as_slice(), level, shape);
    assert_eq!(
        zstd::stream::decode_all(frame.as_slice()).unwrap(),
        data,
        "reference decodes a wrong payload"
    );
    assert_eq!(
        crate::bulk::decompress(&frame, data.len()).unwrap(),
        data,
        "own decoder decodes a wrong payload"
    );
    // Cross-core control: the slice core takes the same window and the same
    // pre-split decisions, so its frame is the byte-exact reference for
    // what the read core must emit here.
    let bulk = crate::bulk::compress_with(
        data.as_slice(),
        &crate::EncoderOptions::new(level).with_input_shape(shape),
    )
    .unwrap();
    assert_eq!(
        zstd::stream::decode_all(bulk.as_slice()).unwrap(),
        data,
        "bulk control"
    );
    assert_eq!(frame, bulk, "read core diverged from the slice core");
}

/// The corruption above needs a pre-split leftover alive exactly when the
/// window compacts (first firing at 1703936 committed bytes on the wl18
/// level-5 row), so the boundary itself is the regression surface: sweep
/// across the trigger as an in-process sequence over the pooled state.
#[test]
fn carry_compaction_threshold_sweep() {
    for len in [1_600_000usize, 1_700_000, 1_703_936, 1_750_000, 2_500_000] {
        let data = phase_shifted(len);
        let shape = crate::InputShape::default().with_window_log(18);
        let frame = crate::encoding::compress_to_vec_shaped(
            data.as_slice(),
            crate::Level::from_zstd(5),
            shape,
        );
        assert_eq!(
            zstd::stream::decode_all(frame.as_slice()).unwrap(),
            data,
            "reference decodes a wrong payload at {len} bytes"
        );
        assert_eq!(
            crate::bulk::decompress(&frame, data.len()).unwrap(),
            data,
            "own decoder decodes a wrong payload at {len} bytes"
        );
    }
}

/// A dictionary echo (the dictionary's own content, then a near-twin at a
/// window-spanning distance) followed by fresh text: matches into the seed
/// and past it, on the read core's dictionary path.
fn dict_echo_payload(dict: &[u8], len: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(len);
    while data.len() < len {
        let take = dict.len().min(len - data.len());
        data.extend_from_slice(&dict[..take]);
        data.extend_from_slice(b"interlude with fresh vocabulary, ");
    }
    data
}

/// The generic `FrameCompressor` loop with a dictionary attached and a
/// forced window: the dictionary seeds the matcher's owned window, so the
/// blocks must still respect the declared window and the frame must decode
/// through both decoders with the dictionary — the read core's own dict
/// arm, which the bulk/stream dictionary grids do not drive.
#[test]
fn dictionary_attach_honors_a_forced_window() {
    let Some(dict) = super::fixture_bytes("./dict_tests/dictionary") else {
        return;
    };
    let data = dict_echo_payload(&dict, 300 * 1024);
    for window_log in [10u32, 12] {
        for level in [crate::Level::Fastest, crate::Level::from_zstd(9)] {
            let mut sink = Vec::new();
            let mut enc = crate::encoding::FrameCompressor::new(level);
            enc.set_dictionary(&dict);
            enc.set_input_shape(crate::InputShape::default().with_window_log(window_log));
            enc.set_source(data.as_slice());
            enc.set_drain(&mut sink);
            enc.compress();
            let label = format!("wl{window_log}/{level:?}");
            assert_eq!(
                zstd::bulk::Decompressor::with_dictionary(&dict)
                    .unwrap()
                    .decompress(&sink, data.len())
                    .unwrap(),
                data,
                "{label}: reference rejects the dict frame"
            );
            let dec_opts = crate::DecoderOptions::new().dictionary(&dict);
            assert_eq!(
                crate::bulk::decompress_with(&sink, data.len(), &dec_opts).unwrap(),
                data,
                "{label}: own decoder rejects the dict frame"
            );
        }
    }
}

/// The raw and parsed dictionary attachments must be one dictionary: the
/// raw form parses at compress time, the parsed one is shared, and the
/// frames they produce are byte-identical.
#[test]
fn parsed_dictionary_matches_the_raw_form() {
    let Some(dict) = super::fixture_bytes("./dict_tests/dictionary") else {
        return;
    };
    let data = dict_echo_payload(&dict, 128 * 1024);
    let mut raw_sink = Vec::new();
    let mut enc = crate::encoding::FrameCompressor::new(crate::Level::Fastest);
    enc.set_dictionary(&dict);
    enc.set_source(data.as_slice());
    enc.set_drain(&mut raw_sink);
    enc.compress();
    let mut parsed_sink = Vec::new();
    let mut enc = crate::encoding::FrameCompressor::new(crate::Level::Fastest);
    enc.set_parsed_dictionary(&crate::EncoderDictionary::parse(&dict).unwrap());
    enc.set_source(data.as_slice());
    enc.set_drain(&mut parsed_sink);
    enc.compress();
    assert_eq!(raw_sink, parsed_sink);
    let header_id = crate::EncoderDictionary::parse(&dict).unwrap().header_id();
    assert_eq!(
        zstd::zstd_safe::get_dict_id_from_frame(&raw_sink).map(|id| id.get()),
        header_id
    );
}

/// The read core's dictionary fold adds the dictionary content length to
/// the caller's size hint (the `compress`-time arm of the saturating fix
/// the bulk and stream grids pin): an absurd hint must keep the huge
/// shape, declaring the same window a merely-large hint gets, instead of
/// panicking under overflow checks or wrapping to a dict-clamped window.
#[test]
fn dictionary_absurd_hint_saturates_the_read_core_fold() {
    let Some(dict) = super::fixture_bytes("./dict_tests/dictionary") else {
        return;
    };
    let data = dict_echo_payload(&dict, 100 * 1024);
    let mut windows = Vec::new();
    for hint in [1u64 << 40, u64::MAX - 8, u64::MAX] {
        let mut sink = Vec::new();
        let mut enc = crate::encoding::FrameCompressor::new(crate::Level::Fastest);
        enc.set_dictionary(&dict);
        enc.set_input_shape(crate::InputShape {
            len: Some(hint),
            window_log: None,
        });
        enc.set_source(data.as_slice());
        enc.set_drain(&mut sink);
        enc.compress();
        let window = crate::decoding::frame::read_frame_header(&mut sink.as_slice())
            .unwrap()
            .0
            .window_size()
            .unwrap();
        windows.push(window);
        let dec_opts = crate::DecoderOptions::new().dictionary(&dict);
        assert_eq!(
            crate::bulk::decompress_with(&sink, data.len(), &dec_opts).unwrap(),
            data,
            "hint {hint}"
        );
    }
    assert_eq!(windows[0], windows[1], "u64::MAX - 8 must not wrap");
    assert_eq!(windows[0], windows[2], "u64::MAX must saturate");
}
