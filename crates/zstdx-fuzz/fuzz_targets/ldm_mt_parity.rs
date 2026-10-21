#![no_main]
//! LdmMode x workers decode parity: a generated shape (far-repeat units
//! interleaved with incompressible regions, all parameters driven by the
//! fuzzed input) is encoded with a random `{Auto, Enable, Disable}` x
//! `{0, 1, 4}` x pledged/open combination through the bulk and the
//! streaming core. Every produced frame must decode byte-for-byte to the
//! original through our sequential decoder, our parallel decoder and the
//! reference zstd decoder (the window-declaration proxy: a frame whose
//! declared window is smaller than the matches used fails there), and the
//! MT/stream-MT frame's decode must equal the ST frame's decode of the
//! same options.

use libfuzzer_sys::fuzz_target;
use std::io::Write;
use zstdx::{DecoderOptions, EncoderOptions, LdmMode, Level, bulk, stream};

const LADDER: [Level; 3] = [Level::Fastest, Level::Fast, Level::Balanced];
const MODES: [LdmMode; 3] = [LdmMode::Auto, LdmMode::Enable, LdmMode::Disable];
const WORKERS: [u32; 3] = [0, 1, 4];

/// The generated shape: `plen` bytes alternating between repetitions of
/// one random unit (the far-repeat source) and fresh random noise (the
/// incompressible regions).
fn generate(plen: usize, unit_log: u32, seed: u64) -> Vec<u8> {
    let mut s = seed | 1;
    let mut rand = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let unit_len = 1usize << unit_log;
    let unit: Vec<u8> = (0..unit_len).map(|_| rand() as u8).collect();
    let mut payload = Vec::with_capacity(plen);
    let mut repeat = false;
    while payload.len() < plen {
        let take = unit_len.min(plen - payload.len());
        if repeat {
            payload.extend_from_slice(&unit[..take]);
        } else {
            payload.extend((0..take).map(|_| rand() as u8));
        }
        repeat = !repeat;
    }
    payload
}

fuzz_target!(|data: &[u8]| {
    let mode = MODES[data.first().copied().unwrap_or(0) as usize % MODES.len()];
    let workers = WORKERS[data.get(1).copied().unwrap_or(0) as usize % WORKERS.len()];
    let flags = data.get(2).copied().unwrap_or(0);
    let pledged = flags & 1 != 0;
    let via_stream = flags & 2 != 0;
    let level = LADDER[data.get(3).copied().unwrap_or(0) as usize % LADDER.len()];
    let plen = usize::from(u16::from_le_bytes([
        data.get(4).copied().unwrap_or(0),
        data.get(5).copied().unwrap_or(0),
    ])) % (256 * 1024);
    let unit_log = 6 + u32::from(data.get(6).copied().unwrap_or(0)) % 11;
    let chunk = 1 + data.get(7).copied().unwrap_or(0) as usize % 4096;

    // The generator seed: the input's entropy tail, so mutations reshape
    // the payload.
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    for &b in data.get(8..32).unwrap_or(&[]) {
        seed ^= u64::from(b);
        seed = seed.rotate_left(8);
    }
    let payload = generate(plen, unit_log, seed);

    let what = format!(
        "mode {mode:?} workers {workers} pledged {pledged} stream {via_stream} level {level:?} \
         len {plen} unit_log {unit_log}"
    );

    // The single-threaded reference frame of the same options.
    let st_opts = EncoderOptions::new(level).long_distance_matching(mode);
    let st = bulk::compress_with(&payload, &st_opts).unwrap();

    // The swept frame: bulk MT or the streaming core (pledged/open).
    let mt_frame = if via_stream {
        let opts = st_opts
            .clone()
            .pledged_size(pledged.then_some(plen as u64))
            .workers(workers);
        let mut enc = stream::write::Encoder::with_options(Vec::new(), opts).unwrap();
        for c in payload.chunks(chunk) {
            enc.write_all(c).unwrap();
        }
        enc.finish().unwrap()
    } else {
        let opts = st_opts
            .clone()
            .pledged_size(pledged.then_some(plen as u64))
            .workers(workers);
        bulk::compress_with(&payload, &opts).unwrap()
    };

    let check = |frame: &[u8], which: &str| {
        let seq = bulk::decompress_with(frame, 0, &DecoderOptions::new())
            .unwrap_or_else(|e| panic!("{which} ({what}) does not decode sequentially: {e}"));
        assert_eq!(&seq[..], &payload[..], "{which} ({what}) sequential roundtrip differs");
        let mt = bulk::decompress_with(frame, 0, &DecoderOptions::new().threads(4))
            .unwrap_or_else(|e| panic!("{which} ({what}) does not decode through the parallel decoder: {e}"));
        assert_eq!(&mt[..], &seq[..], "{which} ({what}) parallel decode disagrees");
        let mut zback = Vec::new();
        zstd::stream::copy_decode(frame, &mut zback)
            .unwrap_or_else(|e| panic!("{which} ({what}) rejected by the reference decoder: {e}"));
        assert_eq!(&zback[..], &seq[..], "{which} ({what}) reference decode disagrees");
    };
    check(&st, "ST frame");
    check(&mt_frame, if via_stream { "stream-MT frame" } else { "bulk-MT frame" });
});
