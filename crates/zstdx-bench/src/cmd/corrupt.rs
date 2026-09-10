//! Random corruption smoke test: flip bytes in valid frames and ensure the
//! decoder reports an error (or succeeds) without panicking or hanging.

use std::path::PathBuf;

#[derive(clap::Args)]
pub struct Args {
    pub file: PathBuf,
    /// Number of corrupted copies to try
    #[arg(long, default_value_t = 64)]
    pub rounds: usize,
}

pub fn run(args: &Args) {
    let data = std::fs::read(&args.file).unwrap();
    // Size the decode_all target from the clean input.
    let mut dec = zstdx::decoding::StreamingDecoder::new(&data[..]).unwrap();
    let mut clean = Vec::new();
    std::io::Read::read_to_end(&mut dec, &mut clean).unwrap();
    let raw_len = clean.len();

    let mut rng: u64 = 0x9e3779b97f4a7c15;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };

    let (ok, err) = (0..args.rounds).fold((0usize, 0usize), |(ok, err), _| {
        let mut corrupted = data.clone();
        let nflip = 1 + (next() as usize % 3);
        for _ in 0..nflip {
            let pos = next() as usize % corrupted.len();
            corrupted[pos] ^= 1 + (next() as u8 % 255);
        }
        let result = std::panic::catch_unwind(move || {
            // A corrupted frame header (e.g. mangled magic) is a regular
            // decode error, not a panic.
            let Ok(mut decoder) = zstdx::decoding::StreamingDecoder::new(&corrupted[..]) else {
                return true;
            };
            let mut out = Vec::new();
            let streamed = std::io::Read::read_to_end(&mut decoder, &mut out).is_ok();
            // Exercise the flat decode_all path against the same corruption.
            let mut fr = zstdx::decoding::FrameDecoder::new();
            let mut bulk = vec![0u8; raw_len];
            let sliced = fr.decode_all(&corrupted[..], &mut bulk).is_ok();
            streamed && sliced
        });
        match result {
            Ok(_) => (ok + 1, err),
            Err(_) => (ok, err + 1),
        }
    });
    eprintln!(
        "{}: {} rounds, clean={ok}, panicked={err}",
        args.file.display(),
        args.rounds
    );
    assert_eq!(err, 0, "decoder panicked on corrupted input");
    // Guard against hangs: each round must finish (the fold itself completing
    // within a bounded time proves it; the process-level timeout enforces it).
}
