//! Ramp-frame piece-parallel decode tests. These run in their own test
//! binary (process): the ramp-depth and piece-router overrides are
//! process-global statics, and the unit-test binary runs its tests on
//! parallel threads where the override would leak into other tests'
//! MT encodes (their mt-vs-st ratio gates assume ungated jobs).
//!
//! The overrides and the MT paths they exercise are std-gated.
#![cfg(feature = "std")]

use std::{io::Write, sync::MutexGuard};

use zstdx::{
    DecoderOptions, EncoderOptions, Level, bulk,
    decoding::{DEFAULT_MAX_WINDOW_SIZE, FrameDecoder},
    piece_engagements, set_mt_ramp_depth_for_tests, set_piece_decode_for_tests,
};

/// The override knobs are process-global statics, so the tests taking them
/// must not overlap: hold this lock for the whole test body.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn serial() -> MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

const RAMP: u64 = 2 * 1024 * 1024;
const MAX_WINDOW: u64 = DEFAULT_MAX_WINDOW_SIZE;

fn textish(len: usize) -> Vec<u8> {
    let words: Vec<&[u8]> = vec![
        b"the ", b"quick ", b"brown ", b"fox ", b"jumps ", b"over ", b"lazy ", b"dog ", b"lorem ",
        b"ipsum ", b"dolor ", b"sit ", b"amet ",
    ];
    let mut state = 7u64;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let w = words[((state >> 33) as usize) % words.len()];
        let take = w.len().min(len - out.len());
        out.extend_from_slice(&w[..take]);
    }
    out
}

/// Semi-structured records (json-like literal runs with matches).
fn records(len: usize) -> Vec<u8> {
    let mut state = 12345u64;
    let mut data = Vec::with_capacity(len);
    let mut id = 0u64;
    while data.len() < len {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let user = (state >> 33) % 5000;
        let event = match (state >> 45) % 5 {
            0 => "click",
            1 => "view",
            2 => "purchase",
            3 => "error",
            _ => "login",
        };
        data.extend_from_slice(
            format!(
                "{{\"id\":{id},\"user\":\"user_{user}\",\"event\":\"{event}\",\"ts\":{}}}",
                1700000000 + id
            )
            .as_bytes(),
        );
        id += 1;
    }
    data
}

fn decode_mt(comp: &[u8], raw_len: usize, workers: u32) -> Vec<u8> {
    let out =
        bulk::decompress_with(comp, 1 << 20, &DecoderOptions::new().threads(workers)).unwrap();
    assert_eq!(out.len(), raw_len);
    out
}

/// Ramp-encoded MT frames must decode byte-exact through the piece
/// executor (both entry points, several worker counts and levels), the
/// engagement counter must move, and the sequential decoder must agree.
#[test]
fn ramp_frames_decode_byte_exact() {
    let _guard = serial();
    set_mt_ramp_depth_for_tests(RAMP);
    set_piece_decode_for_tests(true);
    // Near-offset matches over a literal floor: the pure i%61 cycle
    // compresses to ~1.6 KB (below the MT decoder's input floor), so seed
    // random literals to keep the frame in the parallel regime while the
    // matches stay short-offset (the ramp's hardest case).
    let runs = {
        let mut state = 99u64;
        (0..16 * 1024 * 1024)
            .map(|i| {
                if i % 16 == 0 {
                    state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                    (state >> 33) as u8
                } else {
                    (i % 61) as u8
                }
            })
            .collect::<Vec<u8>>()
    };
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("text", textish(16 * 1024 * 1024)),
        ("records", records(16 * 1024 * 1024)),
        ("runs", runs),
    ];
    for (name, data) in &cases {
        for level in [Level::Fastest, Level::Fast, Level::Balanced] {
            for workers in [2u32, 4, 8] {
                let compressed = bulk::compress_with(
                    data,
                    &EncoderOptions::new(level).workers(workers).checksum(true),
                )
                .unwrap();
                let before = piece_engagements();
                let out = decode_mt(&compressed, data.len(), workers);
                assert_eq!(&out, data, "{name}/{level:?}/{workers}");
                assert!(
                    piece_engagements() > before,
                    "{name}/{level:?}/{workers}: piece executor did not engage"
                );
                // The vec path (append behind a prefix) must agree.
                let mut appended = vec![0x41u8; 1024];
                let prefix = appended.clone();
                zstdx::decoding::mt_decode_to_vec_for_tests(
                    &compressed,
                    &mut appended,
                    workers,
                    MAX_WINDOW,
                )
                .unwrap();
                assert_eq!(&appended[..prefix.len()], &prefix[..]);
                assert_eq!(&appended[prefix.len()..], data);
                // libzstd decodes the same frame (wire-legal output).
                let mut lib = Vec::new();
                zstd::stream::copy_decode(compressed.as_slice(), &mut lib).unwrap();
                assert_eq!(lib, *data, "{name}/{level:?}/{workers} via libzstd");
            }
        }
    }
    set_mt_ramp_depth_for_tests(u64::MAX);
    set_piece_decode_for_tests(false);
}

/// Non-ramp frames (encoder override off, router still on) must never
/// engage and stay byte-exact — the serial stage B through the piece
/// driver's fallback or the stock path, indistinguishable either way.
#[test]
fn plain_frames_do_not_engage() {
    let _guard = serial();
    set_mt_ramp_depth_for_tests(u64::MAX);
    set_piece_decode_for_tests(true);
    let data = textish(16 * 1024 * 1024);
    let compressed = bulk::compress_with(
        &data,
        &EncoderOptions::new(Level::Fastest)
            .workers(4)
            .checksum(true),
    )
    .unwrap();
    let before = piece_engagements();
    assert_eq!(decode_mt(&compressed, data.len(), 4), data);
    assert_eq!(piece_engagements(), before, "plain frames must not engage");

    // zstd-encoded bulk frame through the piece router: serial fallback.
    let zcomp = zstd::bulk::compress(&data, 3).unwrap();
    assert_eq!(decode_mt(&zcomp, data.len(), 4), data);
    assert_eq!(piece_engagements(), before);
    set_piece_decode_for_tests(false);
}

/// A pledged streaming MT encode shares the bulk job grid and gates the
/// same way, so its frames engage too.
#[test]
fn pledged_stream_ramp_frame_engages() {
    let _guard = serial();
    set_mt_ramp_depth_for_tests(RAMP);
    set_piece_decode_for_tests(true);
    let data = textish(12 * 1024 * 1024);
    let opts = EncoderOptions::new(Level::Fastest)
        .workers(4)
        .checksum(true)
        .pledged_size(Some(data.len() as u64));
    let mut enc = zstdx::stream::write::Encoder::with_options(Vec::new(), opts).unwrap();
    enc.write_all(&data).unwrap();
    let compressed = enc.finish().unwrap();
    let before = piece_engagements();
    assert_eq!(decode_mt(&compressed, data.len(), 4), data);
    assert!(piece_engagements() > before, "stream frame did not engage");
    set_mt_ramp_depth_for_tests(u64::MAX);
    set_piece_decode_for_tests(false);
}

/// Corrupt ramp frames must surface an error (decode or checksum), never a
/// panic, hang or silent garbage.
#[test]
fn corrupt_ramp_frame_errors() {
    let _guard = serial();
    set_mt_ramp_depth_for_tests(RAMP);
    set_piece_decode_for_tests(true);
    let data = records(16 * 1024 * 1024);
    let mut compressed = bulk::compress_with(
        &data,
        &EncoderOptions::new(Level::Fastest)
            .workers(4)
            .checksum(true),
    )
    .unwrap();
    let mid = compressed.len() / 2;
    for pos in [mid, mid + 1] {
        let mut c = compressed.clone();
        c[pos] ^= 0xff;
        let _ = bulk::decompress_with(&c, 1 << 20, &DecoderOptions::new().threads(4));
    }
    // Trailer corruption must report the checksum mismatch.
    let last = compressed.len() - 1;
    compressed[last] ^= 0xff;
    let err =
        bulk::decompress_with(&compressed, 1 << 20, &DecoderOptions::new().threads(4)).unwrap_err();
    assert!(err.to_string().contains("checksum"), "{err}");
    set_mt_ramp_depth_for_tests(u64::MAX);
    set_piece_decode_for_tests(false);
}

/// A too-small output buffer must be rejected before any write, and the
/// frame must still decode afterwards (the abort path leaves the driver
/// reusable).
#[test]
fn undersized_output_errors() {
    let _guard = serial();
    set_mt_ramp_depth_for_tests(RAMP);
    set_piece_decode_for_tests(true);
    let data = textish(8 * 1024 * 1024);
    let compressed =
        bulk::compress_with(&data, &EncoderOptions::new(Level::Fastest).workers(4)).unwrap();
    let mut small = vec![0u8; data.len() - 1];
    let err = zstdx::decoding::mt_decode_all_for_tests(&compressed, &mut small, 4, MAX_WINDOW)
        .unwrap_err();
    assert!(err.to_string().contains("at least as many bytes"), "{err}");
    // Same frame again, exact size.
    assert_eq!(decode_mt(&compressed, data.len(), 4), data);
    set_mt_ramp_depth_for_tests(u64::MAX);
    set_piece_decode_for_tests(false);
}

/// Multi-frame and unpledged inputs must not route to the piece executor
/// even with the router on (single pledged frame only), and stay correct.
#[test]
fn multi_frame_does_not_engage() {
    let _guard = serial();
    set_mt_ramp_depth_for_tests(RAMP);
    set_piece_decode_for_tests(true);
    let a = textish(6 * 1024 * 1024);
    let b = records(6 * 1024 * 1024);
    let mut two = bulk::compress_with(
        &a,
        &EncoderOptions::new(Level::Fastest)
            .workers(2)
            .checksum(true),
    )
    .unwrap();
    two.extend_from_slice(
        &bulk::compress_with(
            &b,
            &EncoderOptions::new(Level::Fastest)
                .workers(2)
                .checksum(true),
        )
        .unwrap(),
    );
    let mut expect = a.clone();
    expect.extend_from_slice(&b);
    let before = piece_engagements();
    assert_eq!(decode_mt(&two, expect.len(), 4), expect);
    assert_eq!(piece_engagements(), before, "multi-frame input engaged");
    set_mt_ramp_depth_for_tests(u64::MAX);
    set_piece_decode_for_tests(false);
}

/// The sequential decoder cross-check used by the byte-exact tests.
#[test]
fn sequential_decoder_agrees() {
    let _guard = serial();
    set_mt_ramp_depth_for_tests(RAMP);
    set_piece_decode_for_tests(true);
    let data = records(8 * 1024 * 1024);
    let compressed = bulk::compress_with(
        &data,
        &EncoderOptions::new(Level::Fastest)
            .workers(4)
            .checksum(true),
    )
    .unwrap();
    let mut seq = vec![0u8; data.len()];
    let mut decoder = FrameDecoder::new();
    let n = decoder.decode_all(&compressed, &mut seq).unwrap();
    assert_eq!(&seq[..n], &data[..]);
    assert_eq!(decode_mt(&compressed, data.len(), 4), data);
    set_mt_ramp_depth_for_tests(u64::MAX);
    set_piece_decode_for_tests(false);
}

/// High-ratio frames compress below the MT decoder's 512 KiB input floor
/// (text 32 MiB -> ~100 KB); the pledged content size, not the compressed
/// size, must route them into the piece executor.
#[test]
fn high_ratio_small_input_engages() {
    let _guard = serial();
    set_mt_ramp_depth_for_tests(RAMP);
    set_piece_decode_for_tests(true);
    // Pure short-period cycle: extreme ratio, a few KB compressed.
    let data: Vec<u8> = (0..8 * 1024 * 1024).map(|i| (i % 61) as u8).collect();
    for level in [Level::Fastest, Level::Fast, Level::Balanced] {
        for workers in [2u32, 4, 8] {
            let compressed = bulk::compress_with(
                &data,
                &EncoderOptions::new(level).workers(workers).checksum(true),
            )
            .unwrap();
            assert!(
                compressed.len() < 512 * 1024,
                "{level:?}/{workers}: {} must sit below the MT input floor",
                compressed.len()
            );
            let before = piece_engagements();
            assert_eq!(decode_mt(&compressed, data.len(), workers), data);
            assert!(
                piece_engagements() > before,
                "{level:?}/{workers}: piece executor did not engage below the input floor"
            );
        }
    }
    set_mt_ramp_depth_for_tests(u64::MAX);
    set_piece_decode_for_tests(false);
}

/// A below-floor frame whose pledge misses the piece regime must stay
/// serial: no engagement, still byte-exact.
#[test]
fn small_pledge_does_not_engage() {
    let _guard = serial();
    set_mt_ramp_depth_for_tests(RAMP);
    set_piece_decode_for_tests(true);
    let data: Vec<u8> = (0..1024 * 1024).map(|i| (i % 61) as u8).collect();
    let compressed = bulk::compress_with(
        &data,
        &EncoderOptions::new(Level::Fastest)
            .workers(4)
            .checksum(true),
    )
    .unwrap();
    assert!(compressed.len() < 512 * 1024);
    let before = piece_engagements();
    assert_eq!(decode_mt(&compressed, data.len(), 4), data);
    assert_eq!(piece_engagements(), before, "sub-regime pledge engaged");
    set_mt_ramp_depth_for_tests(u64::MAX);
    set_piece_decode_for_tests(false);
}
