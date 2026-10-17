#![cfg(feature = "std")]

use std::{
    io::Write,
    sync::atomic::{AtomicUsize, Ordering},
};

use zstdx::{EncoderOptions, Level, stream::write::Encoder};

/// A shuffled replay of 16-byte PRNG tokens: every position matches some
/// earlier occurrence of the same token, at a distance that changes with
/// every token, so repcode chains (distance-sticky) cannot carry the
/// stream — only the hash tables can, which is exactly the path the
/// 4-GiB entry truncation breaks. The <=16-byte match fill indexes every
/// covered position, keeping the candidates dense.
struct Tokens {
    vocab: Vec<u8>,
    x: u64,
}

impl Tokens {
    fn new() -> Self {
        let mut x = 0x243f_6a88_85a3_08d3u64;
        let mut vocab = vec![0u8; 8192 * 16];
        for b in &mut vocab {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *b = x as u8;
        }
        Self { vocab, x }
    }

    fn next(&mut self) -> u8 {
        self.x ^= self.x << 13;
        self.x ^= self.x >> 7;
        self.x ^= self.x << 17;
        self.x as u8
    }

    /// One MiB of shuffled token replay.
    fn chunk(&mut self) -> Vec<u8> {
        let mut out = Vec::with_capacity(1 << 20);
        while out.len() < 1 << 20 {
            let idx = (self.next() as usize) | ((self.next() as usize) << 8);
            out.extend_from_slice(&self.vocab[(idx % 8192) * 16..][..16]);
        }
        out.truncate(1 << 20);
        out
    }
}

static COUNTING: AtomicUsize = AtomicUsize::new(0);

/// A writer that only counts: the payload is incompressible-modulo-tiles,
/// so the compressed size is the metric and the bytes are not needed.
struct CountOnly;

impl Write for CountOnly {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        COUNTING.fetch_add(buf.len(), Ordering::Relaxed);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn giant_stream_size(level: Level) -> usize {
    const TOTAL: usize = (6 << 30) + (64 << 20);
    let mut enc = Encoder::new(CountOnly, level).unwrap();
    let mut tokens = Tokens::new();
    let mut done = 0usize;
    while done < TOTAL {
        let bytes = tokens.chunk();
        enc.write_all(&bytes).unwrap();
        done += bytes.len();
    }
    enc.finish().unwrap();
    COUNTING.load(Ordering::Relaxed)
}

/// The multithreaded jobs carry the same absolute coordinates past 4 GiB;
/// their strip prefills store entries against the strip base (the fill-time
/// rebase), so the per-job tables resolve the same way.
fn giant_stream_size_mt(level: Level) -> usize {
    const TOTAL: usize = (6 << 30) + (64 << 20);
    let options = EncoderOptions::new(level).workers(4);
    let mut enc = Encoder::with_options(CountOnly, options).unwrap();
    let mut tokens = Tokens::new();
    let mut done = 0usize;
    while done < TOTAL {
        let bytes = tokens.chunk();
        enc.write_all(&bytes).unwrap();
        done += bytes.len();
    }
    enc.finish().unwrap();
    COUNTING.load(Ordering::Relaxed)
}

#[test]
#[ignore = "multi-GiB runtime; run explicitly for the epoch paths"]
fn giant_stream_keeps_matching_past_4gib() {
    // Fastest/Fast ride fast_shift's fill-time rebase; Balanced rides the
    // chain heads' origin epoch (the scan-side roll carries the ST cells
    // past 4 GiB — without it the chain collapses to 2.8 GB on this
    // payload). The mt cells slice buffer-relative job coordinates (the
    // sliding grid), so they never enter the regime: they pin the strip
    // path's parity with the ST bytes instead.
    for level in [Level::Fastest, Level::Fast, Level::Balanced] {
        COUNTING.store(0, Ordering::Relaxed);
        let out = giant_stream_size(level);
        // The fixed regime replays every token as a ~16-byte match at a
        // fresh offset (~30% of input, ~1.8 GiB here); the broken regime
        // emits the ~1.7 GiB past 4 GiB as literals (2.9+ GiB total). The
        // 2-GiB bar separates the classes with margin.
        assert!(
            out < 2 * 1024 * 1024 * 1024,
            "{level:?}: {out} bytes for 6 GiB of token replay"
        );
    }
    for level in [Level::Fastest, Level::Balanced] {
        COUNTING.store(0, Ordering::Relaxed);
        let out = giant_stream_size_mt(level);
        assert!(
            out < 2 * 1024 * 1024 * 1024,
            "mt {level:?}: {out} bytes for 6 GiB of token replay"
        );
    }
}
