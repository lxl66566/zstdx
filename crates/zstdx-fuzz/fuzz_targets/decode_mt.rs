#![no_main]
//! MT decoder robustness and parity: every input must decode identically
//! through the parallel entry points (segment executor; piece executor via
//! the router override, on since this is a dedicated process) and the
//! sequential decoder, or fail cleanly on both. Any acceptance asymmetry
//! or byte divergence is a crash artifact. Committed seed classes cover
//! the machinery mutations cannot assemble: multi-frame streams for the
//! frame-lane executor, job-grid and piece shapes for the segment
//! executor, and the restart-poor libzstd -9 epoch class.

use libfuzzer_sys::fuzz_target;
use zstdx::{
    DecoderOptions, EncoderOptions, Level, bulk,
    decoding::{FrameDecoder, errors::FrameDecoderError},
    frame_lane_engagements, set_mt_ramp_depth_for_tests, set_piece_decode_for_tests,
};

fn check_with(data: &[u8], workers: u32) {
    let mt = bulk::decompress_with(data, 1 << 20, &DecoderOptions::new().threads(workers));

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
}

fn check(data: &[u8]) {
    check_with(data, 4);
}

/// One committed restart-poor frame per process: libzstd -9 over
/// semi-structured records emits table-carrying epochs far longer than
/// the segment target, so the planner cuts mid-epoch and every later
/// lease primes its scratch from the recorded table epoch — the class
/// the mid-epoch equivalence tests pin in-process. The -9 build over
/// 8 MiB (a frame around 830 KiB, safely above the parallel floor) is
/// too expensive per input, so it parks in a process-wide OnceLock.
fn restart_poor_frame() -> &'static [u8] {
    static FRAME: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    FRAME.get_or_init(|| {
        let mut rng = Rng(12345);
        let data = records(&mut rng, 8 * 1024 * 1024);
        zstd::bulk::compress(&data, 9).unwrap()
    })
}

/// Minimal xorshift for deterministic payloads.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

const WORDS: [&[u8]; 12] = [
    b"frame ",
    b"match ",
    b"distance ",
    b"window ",
    b"encode ",
    b"stream ",
    b"prefix ",
    b"check ",
    b"level ",
    b"table ",
    b"block ",
    b"hash ",
];

/// Text-ish payload with a fuzz-seeded word mix.
fn textish(rng: &mut Rng, len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        let w = WORDS[(rng.next() >> 33) as usize % WORDS.len()];
        out.extend_from_slice(w);
    }
    out.truncate(len);
    out
}

/// Semi-structured records (json-like literal runs with matches).
fn records(rng: &mut Rng, len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut id = 0u64;
    while out.len() < len {
        let state = rng.next();
        let user = (state >> 33) % 5000;
        let event = match (state >> 45) % 5 {
            0 => "click",
            1 => "view",
            2 => "purchase",
            3 => "error",
            _ => "login",
        };
        let payload_len = ((state >> 25) % 24) as usize;
        out.extend_from_slice(
            format!(
                "{{\"id\":{id},\"user\":\"user_{user}\",\"event\":\"{event}\",\"ts\":{},\"\
                 payload\":\"",
                1700000000 + id
            )
            .as_bytes(),
        );
        out.resize(out.len() + payload_len, b'x');
        out.extend_from_slice(b"\",\"score\":0.5}\n");
        id += 1;
    }
    out.truncate(len);
    out
}

/// Pseudo-random bytes: incompressible content, so the frame carries raw
/// blocks and clears the parallel decode floor at raw size.
fn noise(rng: &mut Rng, len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    for chunk in out.chunks_mut(8) {
        chunk.copy_from_slice(&rng.next().to_le_bytes()[..chunk.len()]);
    }
    out
}

/// Deterministic MT-frame seeds (R66 constructor pattern): frames from our
/// own MT encoder, so the segment scan sees job-boundary restart cuts at
/// varying alignments, plus libzstd -9 restart-poor frames, so the
/// planner also cuts mid-epoch and primes leases from the folded table
/// epoch. The ramp override arms the deep-offset job guarantee once per
/// process, letting the piece planner validate real job grids (pledged
/// high-ratio shapes below the 512-KiB compressed floor).
fn mt_frames_seed(data: &[u8]) {
    let byte = |i: usize| data.get(i).copied().unwrap_or(0x5a);
    let mut rng = Rng(u64::from(byte(1)) | 0x0123_4567_89ab_cdef);
    // 512 KiB clears the piece validator's 3/8-of-job-size lag bar for
    // every candidate job size at or under ~1.3 MiB.
    set_mt_ramp_depth_for_tests(512 * 1024);

    // Segment-parallel regime: 600 KiB of mixed text over the MT floor.
    let len = 600 * 1024 + byte(2) as usize % 4096;
    let payload = textish(&mut rng, len);
    let frame = bulk::compress_with(
        &payload,
        &EncoderOptions::new(Level::Fastest)
            .workers(2 + byte(3) as u32 % 3)
            .checksum(byte(4) & 1 == 0),
    )
    .unwrap();
    check(&frame);

    // Piece regime (1/4 of inputs): a pledged high-ratio frame below the
    // compressed floor, ramp-armed so job grids carry the guarantee.
    if byte(0) & 3 == 0 {
        let len = 2 * 1024 * 1024 + byte(5) as usize * 8192;
        let tile = textish(&mut rng, 128 * 1024);
        let mut payload = Vec::with_capacity(len);
        while payload.len() < len {
            payload.extend_from_slice(&tile);
        }
        payload.truncate(len);
        let frame = bulk::compress_with(
            &payload,
            &EncoderOptions::new(Level::Fastest)
                .workers(4)
                .pledged_size(Some(len as u64)),
        )
        .unwrap();
        check(&frame);
    }

    // Restart-poor epoch class (the mid-epoch table priming): the frame
    // parks per process, and every qualifying input re-decodes it through
    // both entry points — the re-decode probes the staging pool's residue
    // reuse, the class's own hazard.
    if byte(0) & 7 == 0 {
        check(restart_poor_frame());
    }

    // Fallback regime: a small frame under the MT floor must decode
    // identically through both entry points.
    let payload = textish(&mut rng, 4096 + byte(6) as usize * 64);
    let frame = bulk::compress_with(
        &payload,
        &EncoderOptions::new(Level::from_zstd(byte(7) as i32 % 8 + 1)),
    )
    .unwrap();
    check(&frame);
}

/// Concatenated-frame seed class: the frame-lane executor runs on
/// multi-frame streams only, which mutation never assembles. The frame
/// count, per-frame size, content, checksum, level and encoder worker
/// count all derive from the input bytes; skippable frames interleave and
/// frames past the raw budget park as empty frames (the tail class). A
/// noise frame anchors any stream that would otherwise sit below the
/// 512-KiB parallel floor, so the lane path engages on every qualifying
/// input — pinned by the engagement counter — while the decode worker
/// count rides spare bits, running the lanes grouped and per-frame at
/// 2/4/8/12 workers.
fn multiframe_seed(data: &[u8]) {
    let byte = |i: usize| data.get(i).copied().unwrap_or(0x5a);
    let workers = [2u32, 4, 8, 12][byte(9) as usize & 3];
    let n_frames = 2 + byte(8) as usize % 15;
    let mut rng = Rng(u64::from(byte(10)) | 0x0fed_cba9_8765_4321);
    // Sizes span empty and sub-floor frames up to multi-MiB frames; the
    // raw budget bounds the per-input cost.
    const SIZES: [usize; 6] = [0, 4096, 64 * 1024, 256 * 1024, 1024 * 1024, 2 * 1024 * 1024];
    const BUDGET: usize = 3 * 1024 * 1024;
    let mut spent = 0usize;
    let mut stream = Vec::new();
    for _ in 0..n_frames {
        if rng.next() & 7 == 0 {
            // Skippable frame: magic 0x184D2A50 + LE length + payload.
            let len = 16 + (rng.next() >> 40) as usize % 48;
            stream.extend_from_slice(&[0x50, 0x2a, 0x4d, 0x18]);
            stream.extend_from_slice(&(len as u32).to_le_bytes());
            stream.resize(stream.len() + len, 0x5a);
        }
        let len = if spent < BUDGET {
            SIZES[(rng.next() >> 36) as usize % SIZES.len()].min(BUDGET - spent)
        } else {
            0
        };
        spent += len;
        let payload = match (rng.next() >> 40) % 4 {
            0 => textish(&mut rng, len),
            1 => records(&mut rng, len),
            2 => vec![b'k'; len],
            _ => noise(&mut rng, len),
        };
        let frame = bulk::compress_with(
            &payload,
            &EncoderOptions::new(Level::from_zstd(1 + (rng.next() >> 36) as i32 % 4))
                .checksum(rng.next() & 1 == 0)
                .workers(1 + (rng.next() >> 40) as u32 % 3),
        )
        .unwrap();
        stream.extend_from_slice(&frame);
    }
    if stream.len() < 600 * 1024 {
        // A noise frame clears the 512-KiB floor at raw size, engaging the
        // lane path for otherwise-sub-floor streams.
        let pad = 600 * 1024 - stream.len() + 4096;
        let frame = bulk::compress_with(&noise(&mut rng, pad), &EncoderOptions::new(Level::Fastest))
            .unwrap();
        stream.extend_from_slice(&frame);
    }
    let before = frame_lane_engagements();
    check_with(&stream, workers);
    assert!(
        frame_lane_engagements() > before,
        "a multi-frame stream above the floor must engage the frame-lane executor"
    );
}

fuzz_target!(|data: &[u8]| {
    // Process-global static; set before the first decode so every
    // qualifying pledged frame routes through the piece machinery.
    set_piece_decode_for_tests(true);
    mt_frames_seed(data);
    // The frame-lane executor needs committed multi-frame streams.
    if data.first().copied().unwrap_or(0x5a) & 3 == 2 {
        multiframe_seed(data);
    }
    check(data);
});
