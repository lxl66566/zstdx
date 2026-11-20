//! Read-side encode core edges: frames out of the generic
//! `FrameCompressor<M>` loop (the `encoding::compress*` entry points and
//! `FrameCompressor::new_with_matcher`) across forced small windows and
//! custom matchers, gated through both decoders.

use alloc::{string::ToString, vec::Vec};

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
