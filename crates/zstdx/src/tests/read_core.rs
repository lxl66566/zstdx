//! Read-side encode core edges: frames out of the generic
//! `FrameCompressor<M>` loop (the `encoding::compress*` entry points and
//! `FrameCompressor::new_with_matcher`) across forced small windows and
//! custom matchers, gated through both decoders.

use alloc::vec::Vec;

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
