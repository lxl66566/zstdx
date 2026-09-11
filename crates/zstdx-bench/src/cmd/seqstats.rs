//! Sequence-choice statistics and entropy lower bound for our matcher on a
//! corpus file: per-symbol histograms of the ll/ml/of codes and the
//! theoretical minimum sequence-section size they imply.

use std::{fs, io::Write, path::PathBuf};

use zstdx::{
    Level,
    encoding::{EncodedSequence, MatchGeneratorDriver, Matcher, Sequence},
};

/// Delegating matcher that records every emitted sequence.
struct RecordingMatcher {
    inner: MatchGeneratorDriver,
    triples: Vec<(u32, u32, u32)>, // (ll, ml, of-wire)
}

impl Matcher for RecordingMatcher {
    fn block_tail(&mut self) -> &mut [u8] {
        self.inner.block_tail()
    }

    fn get_last_space(&mut self) -> &[u8] {
        self.inner.get_last_space()
    }

    fn commit_block(&mut self, read: usize) {
        self.inner.commit_block(read);
    }

    fn skip_matching(&mut self) {
        self.inner.skip_matching();
    }

    fn start_matching(&mut self, mut handle_sequence: impl for<'a> FnMut(Sequence<'a>)) {
        let mut lits = Vec::new();
        let mut seqs = Vec::new();
        self.inner.start_matching_into(&mut lits, &mut seqs);
        let mut offset = 0usize;
        for s in &seqs {
            let lits_slice = &lits[offset..offset + s.ll as usize];
            offset += s.ll as usize;
            handle_sequence(Sequence::Triple {
                literals: lits_slice,
                offset: s.of as usize,
                match_len: s.ml as usize,
            });
        }
        handle_sequence(Sequence::Literals {
            literals: &lits[offset..],
        });
        self.triples.extend(seqs.iter().map(|s| (s.ll, s.ml, s.of)));
    }

    fn reset(&mut self, level: Level) {
        self.inner.reset(level);
    }

    fn window_size(&self) -> u64 {
        self.inner.window_size()
    }

    fn repcode_snapshot(&self) -> [u32; 3] {
        self.inner.repcode_snapshot()
    }

    fn restore_repcode(&mut self, rep: [u32; 3]) {
        self.inner.restore_repcode(rep);
    }
}

fn ll_code(len: u32) -> (u8, u32) {
    const LL_META: [(u32, u8); 36] = [
        (0, 0),
        (1, 0),
        (2, 0),
        (3, 0),
        (4, 0),
        (5, 0),
        (6, 0),
        (7, 0),
        (8, 0),
        (9, 0),
        (10, 0),
        (11, 0),
        (12, 0),
        (13, 0),
        (14, 0),
        (15, 0),
        (16, 1),
        (18, 1),
        (20, 1),
        (22, 1),
        (24, 2),
        (28, 2),
        (32, 3),
        (40, 3),
        (48, 4),
        (64, 6),
        (128, 7),
        (256, 8),
        (512, 9),
        (1024, 10),
        (2048, 11),
        (4096, 12),
        (8192, 13),
        (16384, 14),
        (32768, 15),
        (65536, 16),
    ];
    let mut code = 35;
    while code > 0 && LL_META[code].0 > len {
        code -= 1;
    }
    (code as u8, LL_META[code].1 as u32)
}

fn ml_code(len: u32) -> (u8, u32) {
    const ML_META: [(u32, u8); 53] = [
        (3, 0),
        (4, 0),
        (5, 0),
        (6, 0),
        (7, 0),
        (8, 0),
        (9, 0),
        (10, 0),
        (11, 0),
        (12, 0),
        (13, 0),
        (14, 0),
        (15, 0),
        (16, 0),
        (17, 0),
        (18, 0),
        (19, 0),
        (20, 0),
        (21, 0),
        (22, 0),
        (23, 0),
        (24, 0),
        (25, 0),
        (26, 0),
        (27, 0),
        (28, 0),
        (29, 0),
        (30, 0),
        (31, 0),
        (32, 0),
        (33, 0),
        (34, 0),
        (35, 1),
        (37, 1),
        (39, 1),
        (41, 1),
        (43, 2),
        (47, 2),
        (51, 3),
        (59, 3),
        (67, 4),
        (83, 4),
        (99, 5),
        (131, 7),
        (259, 8),
        (515, 9),
        (1027, 10),
        (2051, 11),
        (4099, 12),
        (8195, 13),
        (16387, 14),
        (32771, 15),
        (65539, 16),
    ];
    let mut code = 52;
    while code > 0 && ML_META[code].0 > len {
        code -= 1;
    }
    (code as u8, ML_META[code].1 as u32)
}

fn entropy(hist: &[u64], total: u64) -> f64 {
    let mut h = 0.0;
    for &c in hist {
        if c > 0 {
            let p = c as f64 / total as f64;
            h -= p * p.log2();
        }
    }
    h * total as f64
}

#[derive(clap::Args)]
pub struct Args {
    pub file: PathBuf,
    /// zstd numeric level selecting the matcher strategy.
    #[arg(long, default_value = "1")]
    pub level: i32,
}

pub fn run(args: &Args) {
    struct Sink(Vec<u8>);
    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let raw = fs::read(&args.file).unwrap();

    let matcher = RecordingMatcher {
        inner: MatchGeneratorDriver::new(128 * 1024),
        triples: Vec::new(),
    };
    let mut compressor =
        zstdx::encoding::FrameCompressor::new_with_matcher(matcher, Level::from_zstd(args.level));
    compressor.set_source(raw.as_slice());
    let sink = Sink(Vec::new());
    compressor.set_drain(sink);
    compressor.compress();
    let sink = compressor.take_drain().unwrap();
    fs::write("target/seqstats_out.zst", &sink.0).unwrap();
    let rec = compressor.replace_matcher(RecordingMatcher {
        inner: MatchGeneratorDriver::new(128 * 1024),
        triples: Vec::new(),
    });

    let n = rec.triples.len() as u64;
    let mut llh = [0u64; 36];
    let mut mlh = [0u64; 53];
    let mut ofh = [0u64; 32];
    let (mut ll_add, mut ml_add, mut of_add) = (0u64, 0u64, 0u64);
    let mut ll_sum = 0u64;
    let mut ml_sum = 0u64;
    for (ll, ml, of) in &rec.triples {
        let (lc, lnb) = ll_code(*ll);
        let (mc, mnb) = ml_code(*ml);
        let olog = of.ilog2();
        llh[lc as usize] += 1;
        mlh[mc as usize] += 1;
        ofh[olog as usize] += 1;
        ll_add += lnb as u64;
        ml_add += mnb as u64;
        of_add += olog as u64;
        ll_sum += *ll as u64;
        ml_sum += *ml as u64;
    }
    let code_bits = entropy(&llh, n) + entropy(&mlh, n) + entropy(&ofh, n);
    let add_bits = (ll_add + ml_add + of_add) as f64;
    // 3 initial states per block, table descriptions excluded
    let blocks = (raw.len() as f64 / (128.0 * 1024.0)).ceil();
    let state_bits = blocks * 3.0 * 9.0;
    let total_bits = code_bits + add_bits + state_bits;
    println!(
        "nseq={n} avg_ll={:.2} avg_ml={:.2}",
        ll_sum as f64 / n as f64,
        ml_sum as f64 / n as f64
    );
    println!(
        "entropy bound: code={:.0}B add={:.0}B states={:.0}B total={:.0}B ({:.2} B/seq)",
        code_bits / 8.0,
        add_bits / 8.0,
        state_bits / 8.0,
        total_bits / 8.0,
        total_bits / 8.0 / n as f64
    );
    println!(
        "ll entropy {:.2}b ml {:.2}b of {:.2}b | add ll {:.2}b ml {:.2}b of {:.2}b (per seq)",
        entropy(&llh, n) / n as f64,
        entropy(&mlh, n) / n as f64,
        entropy(&ofh, n) / n as f64,
        ll_add as f64 / n as f64,
        ml_add as f64 / n as f64,
        of_add as f64 / n as f64
    );
    println!("ll hist: {llh:?}");
    println!("ml hist: {mlh:?}");
    println!("of hist: {ofh:?}");
    let _ = EncodedSequence {
        ll: 0,
        ml: 0,
        of: 0,
    };
}
