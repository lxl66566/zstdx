#![no_main]
//! MT decoder robustness and parity: every input must decode identically
//! through the parallel entry points (segment executor; piece executor via
//! the router override, on since this is a dedicated process) and the
//! sequential decoder, or fail cleanly on both. Any acceptance asymmetry
//! or byte divergence is a crash artifact.

use libfuzzer_sys::fuzz_target;
use zstdx::{
    DecoderOptions, EncoderOptions, Level, bulk,
    decoding::{FrameDecoder, errors::FrameDecoderError},
    set_mt_ramp_depth_for_tests, set_piece_decode_for_tests,
};

fn check(data: &[u8]) {
    let mt = bulk::decompress_with(data, 1 << 20, &DecoderOptions::new().threads(4));

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
        let mut state = 12345u64;
        let mut data = Vec::with_capacity(8 * 1024 * 1024);
        let mut id = 0u64;
        while data.len() < 8 * 1024 * 1024 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let user = (state >> 33) % 5000;
            let event = match (state >> 45) % 5 {
                0 => "click",
                1 => "view",
                2 => "purchase",
                3 => "error",
                _ => "login",
            };
            let payload_len = ((state >> 25) % 24) as usize;
            data.extend_from_slice(
                format!(
                    "{{\"id\":{id},\"user\":\"user_{user}\",\"event\":\"{event}\",\"ts\":{},\"\
                     payload\":\"",
                    1700000000 + id
                )
                .as_bytes(),
            );
            data.resize(data.len() + payload_len, b'x');
            data.extend_from_slice(b"\",\"score\":0.5}\n");
            id += 1;
        }
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

fuzz_target!(|data: &[u8]| {
    // Process-global static; set before the first decode so every
    // qualifying pledged frame routes through the piece machinery.
    set_piece_decode_for_tests(true);
    mt_frames_seed(data);
    check(data);
});
