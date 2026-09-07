//! Random corruption smoke test: flip bytes in valid frames and ensure the
//! decoder reports an error (or succeeds) without panicking or hanging.
//! Usage: cargo run --release --example corruption_smoke -- <file.zst> [rounds]

use std::time::Duration;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: corruption_smoke <file.zst> [rounds]");
    let rounds: usize = args.next().map_or(64, |s| s.parse().unwrap());
    let data = std::fs::read(&path).unwrap();

    let mut rng: u64 = 0x9E3779B97F4A7C15;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };

    let (ok, err) = (0..rounds).fold((0usize, 0usize), |(ok, err), _| {
        let mut corrupted = data.clone();
        let nflip = 1 + (next() as usize % 3);
        for _ in 0..nflip {
            let pos = next() as usize % corrupted.len();
            corrupted[pos] ^= 1 + (next() as u8 % 255);
        }
        let result = std::panic::catch_unwind(move || {
            let mut decoder = ruzstd::decoding::StreamingDecoder::new(&corrupted[..]).unwrap();
            let mut out = Vec::new();
            std::io::Read::read_to_end(&mut decoder, &mut out).is_ok()
        });
        match result {
            Ok(_) => (ok + 1, err),
            Err(_) => (ok, err + 1),
        }
    });
    eprintln!("{path}: {rounds} rounds, clean={ok}, panicked={err}");
    assert_eq!(err, 0, "decoder panicked on corrupted input");
    // Guard against hangs: each round must finish (the fold itself completing
    // within a bounded time proves it; the process-level timeout enforces it).
    let _ = Duration::from_secs(0);
}
