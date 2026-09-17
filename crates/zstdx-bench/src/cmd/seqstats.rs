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

    fn consider_reach_probe(&mut self, head: &[u8], level: Level) {
        self.inner.consider_reach_probe(head, level);
    }

    fn set_input_shape(&mut self, shape: zstdx::InputShape) {
        self.inner.set_input_shape(shape);
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

    fn load_dictionary(&mut self, content: &[u8], rep: [u32; 3]) {
        self.inner.load_dictionary(content, rep);
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
    /// Reference frame (e.g. libzstd output at the paired level): decode it
    /// through our decoder's seq_dump log and diff its parse against our
    /// encoder's parse of the same raw data.
    #[arg(long)]
    pub ref_frame: Option<PathBuf>,
    /// Number of leading parse divergences to print in diff mode.
    #[arg(long, default_value = "10")]
    pub divergences: usize,
    /// Dictionary (raw content or formatted) to compress against; also used
    /// to decode the reference frame when given.
    #[arg(long)]
    pub dict: Option<PathBuf>,
}

/// Decoder-side repcode history resolution (mirrors
/// `sequence_execution::do_offset_history`, wire values in, actual offset out).
fn resolve_actual(triples: &[(u32, u32, u32)]) -> Vec<(u32, u32, u64)> {
    let mut hist = [1u32, 4, 8];
    triples
        .iter()
        .map(|&(ll, ml, of)| {
            let idx = of.wrapping_sub(1).wrapping_add((ll == 0) as u32);
            let slot = (idx & 3) as usize;
            let slot = if slot == 3 {
                0
            } else {
                slot
            };
            let actual = if of <= 3 {
                hist[slot].saturating_sub((idx == 3) as u32)
            } else {
                of.wrapping_sub(3)
            };
            let keep = idx == 0;
            let rotate_all = idx >= 2;
            hist[2] = if rotate_all {
                hist[1]
            } else {
                hist[2]
            };
            hist[1] = if keep {
                hist[1]
            } else {
                hist[0]
            };
            hist[0] = if keep {
                hist[0]
            } else {
                actual
            };
            (ll, ml, actual as u64)
        })
        .collect()
}

struct ParseStats {
    nseq: usize,
    lit_bytes: u64,
    match_bytes: u64,
    rep_frac: f64,
    avg_ll: f64,
    avg_ml: f64,
    med_of: u64,
    seq_bits: f64, // entropy bound over code histograms + add bits
    lit_bits: f64, // order-0 entropy of the literal bytes
}

fn parse_stats(raw: &[u8], triples: &[(u32, u32, u32)]) -> ParseStats {
    let resolved = resolve_actual(triples);
    let n = triples.len() as u64;
    let mut llh = [0u64; 36];
    let mut mlh = [0u64; 53];
    let mut ofh = [0u64; 32];
    let (mut ll_add, mut ml_add, mut of_add) = (0u64, 0u64, 0u64);
    let mut reps = 0u64;
    let mut lits_hist = [0u64; 256];
    let (mut lit_bytes, mut match_bytes) = (0u64, 0u64);
    // Replay over the raw output to pick up the literal bytes the parse chose.
    let mut pos = 0usize;
    let mut offs: Vec<u64> = Vec::with_capacity(triples.len());
    for (i, &(ll, ml, of)) in triples.iter().enumerate() {
        let (_, _, of_a) = resolved[i];
        let (lc, lnb) = ll_code(ll);
        let (mc, mnb) = ml_code(ml);
        llh[lc as usize] += 1;
        mlh[mc as usize] += 1;
        ofh[of_a.ilog2() as usize] += 1;
        ll_add += lnb as u64;
        ml_add += mnb as u64;
        of_add += of_a.ilog2() as u64;
        if of <= 3 {
            reps += 1;
        }
        lit_bytes += ll as u64;
        match_bytes += ml as u64;
        for &b in &raw[pos..pos + ll as usize] {
            lits_hist[b as usize] += 1;
        }
        pos += ll as usize + ml as usize;
        offs.push(of_a);
    }
    for &b in &raw[pos..] {
        lits_hist[b as usize] += 1;
        lit_bytes += 1;
    }
    offs.sort_unstable();
    let code_bits = entropy(&llh, n) + entropy(&mlh, n) + entropy(&ofh, n);
    let lit_bits = entropy(&lits_hist, lit_bytes.max(1));
    ParseStats {
        nseq: triples.len(),
        lit_bytes,
        match_bytes,
        rep_frac: reps as f64 / n as f64,
        avg_ll: lit_bytes as f64 / n as f64,
        avg_ml: match_bytes as f64 / n as f64,
        med_of: offs[offs.len() / 2],
        seq_bits: code_bits + (ll_add + ml_add + of_add) as f64,
        lit_bits,
    }
}

/// Walk both parses aligned on absolute output position and classify where
/// they disagree.
fn diff_parses(ours: &[(u32, u32, u32)], ref_: &[(u32, u32, u32)], raw: &[u8], show: usize) {
    let total_len = raw.len();
    let o = resolve_actual(ours);
    let r = resolve_actual(ref_);
    // Sequence start positions (literal-run tail excluded from both).
    let starts = |t: &[(u32, u32, u32)]| -> Vec<u64> {
        let mut p = 0u64;
        t.iter()
            .map(|&(ll, ml, _)| {
                let s = p;
                p += ll as u64 + ml as u64;
                s
            })
            .collect()
    };
    let so = starts(ours);
    let sr = starts(ref_);
    let (mut i, mut j) = (0usize, 0usize);
    let mut same = 0u64;
    let mut only_ours = 0u64;
    let mut only_ref = 0u64;
    // ref matches that start where our parse has literals (we missed them)
    let mut missed_mlh = [0u64; 53];
    let mut missed_olog = [0u64; 32];
    let mut shown = 0usize;
    while i < ours.len() && j < ref_.len() {
        match so[i].cmp(&sr[j]) {
            std::cmp::Ordering::Equal => {
                let (oll, oml, oof) = o[i];
                let (rll, rml, rof) = r[j];
                if oll == rll && oml == rml && oof == rof {
                    same += 1;
                } else if shown < show {
                    shown += 1;
                    println!(
                        "  @{} ours (ll={oll} ml={oml} of={oof}) vs ref (ll={rll} ml={rml} \
                         of={rof})",
                        so[i]
                    );
                }
                i += 1;
                j += 1;
            },
            std::cmp::Ordering::Less => {
                only_ours += 1;
                i += 1;
            },
            std::cmp::Ordering::Greater => {
                // ref emits at a position we do not: either covered by our
                // longer match or a match we missed entirely — the missed-
                // match accounting below uses literal coverage, this is the
                // raw stream view
                only_ref += 1;
                j += 1;
            },
        }
    }
    // Coverage view: which byte ranges each side matched. A ref match whose
    // start lies inside one of our literal runs is a match we missed.
    let mut covered = vec![false; total_len];
    let mut p = 0usize;
    for &(ll, ml, _) in ours {
        p += ll as usize;
        for c in covered.iter_mut().skip(p).take(ml as usize) {
            *c = true;
        }
        p += ml as usize;
    }
    // Map each byte to the our-side sequence covering it (index + whether it
    // sits in the sequence's literal or match part), for divergence context.
    let mut owner = vec![(0usize, false); total_len]; // (seq idx, in-match)
    let mut p = 0usize;
    for (i, &(ll, ml, _)) in ours.iter().enumerate() {
        for o in owner.iter_mut().skip(p).take(ll as usize + ml as usize) {
            *o = (i, false);
        }
        for o in owner.iter_mut().skip(p + ll as usize).take(ml as usize) {
            *o = (i, true);
        }
        p += ll as usize + ml as usize;
    }
    let mut missed = 0u64;
    let mut missed_bytes = 0u64;
    let mut missed_in_zero = 0u64;
    let mut p = 0usize;
    let mut ctx_shown = 0u64;
    for (j, &(ll, ml, _)) in ref_.iter().enumerate() {
        p += ll as usize;
        if p < covered.len() && !covered[p] {
            missed += 1;
            missed_bytes += ml as u64;
            missed_mlh[ml_code(ml).0 as usize] += 1;
            missed_olog[r[j].2.ilog2() as usize] += 1;
            if raw[p] == 0 {
                missed_in_zero += 1;
            }
            if ctx_shown < show as u64 {
                ctx_shown += 1;
                let (oi, in_match) = owner[p];
                let (oll, oml, oof) = o[oi];
                let ostart = so[oi];
                println!(
                    "  miss @{p}: ref (ll={ll} ml={ml} of={}) | our seq #{oi} @{} (ll={oll} \
                     ml={oml} of={oof}) pos-in-seq {} (match-part={in_match})",
                    r[j].2,
                    ostart,
                    p - ostart as usize
                );
            }
        }
        p += ml as usize;
    }
    println!("missed in zero-fill region: {missed_in_zero} / {missed}");
    println!(
        "diff: same={same} only_ours={only_ours} only_ref={only_ref} missed_ref_matches={missed} \
         ({missed_bytes} B)"
    );
    let top: Vec<String> = missed_mlh
        .iter()
        .enumerate()
        .filter(|(_, c)| **c > 0)
        .map(|(c, &n)| format!("{}x{}", n, ml_code_val(c)))
        .collect();
    if !top.is_empty() {
        println!("missed ref ml distribution: {}", top.join(" "));
    }
    let top: Vec<String> = missed_olog
        .iter()
        .enumerate()
        .filter(|(_, c)| **c > 0)
        .map(|(b, &n)| format!("{}x2^{}", n, b))
        .collect();
    if !top.is_empty() {
        println!("missed ref offset-log distribution: {}", top.join(" "));
    }
}

fn ml_code_val(code: usize) -> u32 {
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
    ML_META[code].0
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
    // Declare the source length like the bulk paths do (they always know
    // it), so window downsizing and the row-9 reach probe measure the same
    // bytes the bulk encoder produces.
    compressor.set_input_shape(zstdx::InputShape::default().with_len(raw.len() as u64));
    if let Some(dict) = &args.dict {
        let bytes = fs::read(dict).unwrap();
        compressor.set_dictionary(&bytes);
    }
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
    // Joint weak-match anatomy: sequence count for each (ml bucket, wire-of
    // bucket). Wire of 1/2/3 are repcode emissions; larger buckets are
    // offset magnitudes (of-wire = offset+3, so bucket k ≈ offsets
    // 2^(k-1)..2^k). The acceptance-bar design wants to know where the
    // short-match mass sits: cheap near offsets are text's payload, far
    // offsets are the json weak-match pollution candidates.
    let mut joint = [[0u64; 32]; 24]; // [ml min(23), of-wire ilog2]
    for (ll, ml, of) in &rec.triples {
        let m = (*ml as usize).min(23);
        joint[m][of.ilog2() as usize] += 1;
        let _ = ll;
    }
    println!("joint ml x of-wire-log (rows ml 4..16, cols log 0..21):");
    for (m, row) in joint.iter().enumerate().take(17).skip(4) {
        let counts: Vec<u64> = row.iter().take(22).copied().collect();
        let total: u64 = counts.iter().sum();
        if total > 0 {
            println!("  ml={m:2} n={total:6} {counts:?}");
        }
    }
    let _ = EncodedSequence {
        ll: 0,
        ml: 0,
        of: 0,
    };

    // Differential mode: decode the reference frame through our decoder's
    // seq_dump log and compare the two parses of the same raw data.
    #[cfg(not(feature = "seq_dump"))]
    if let Some(ref_frame) = &args.ref_frame {
        let _ = ref_frame;
        eprintln!(
            "error: --ref-frame needs the decoder dump hook; rebuild with `--features seq_dump` \
             (never enabled by default: the hook sits in the decode hot loop and invalidates \
             timing builds)"
        );
        std::process::exit(2);
    }
    #[cfg(feature = "seq_dump")]
    if let Some(ref_frame) = &args.ref_frame {
        let frame = fs::read(ref_frame).unwrap();
        let decoded = match &args.dict {
            None => zstdx::bulk::decompress(&frame, raw.len()).unwrap(),
            Some(dict) => {
                let bytes = fs::read(dict).unwrap();
                let parsed = zstdx::decoding::Dictionary::load(&bytes).unwrap();
                let mut decoder = zstdx::decoding::FrameDecoder::new();
                decoder.add_dict(parsed).unwrap();
                let mut out = vec![0u8; raw.len()];
                decoder.decode_all(&frame, &mut out).unwrap();
                out
            },
        };
        assert_eq!(decoded, raw, "reference frame must decode to the input");
        let dumped = zstdx::decoding::seq_dump::take();
        let ref_triples: Vec<(u32, u32, u32)> = dumped.iter().map(|s| (s.ll, s.ml, s.of)).collect();
        let ours = parse_stats(&raw, &rec.triples);
        let theirs = parse_stats(&raw, &ref_triples);
        let show = |name: &str, s: &ParseStats| {
            println!(
                "{name}: nseq={} lit={}B match={}B rep={:.1}% avg_ll={:.2} avg_ml={:.2} med_of={} \
                 seq_bound={:.0}B lit_bound={:.0}B",
                s.nseq,
                s.lit_bytes,
                s.match_bytes,
                s.rep_frac * 100.0,
                s.avg_ll,
                s.avg_ml,
                s.med_of,
                s.seq_bits / 8.0,
                s.lit_bits / 8.0
            );
        };
        show("ours", &ours);
        show("ref ", &theirs);
        // Cold-start view: literal bytes of sequences starting inside the
        // first tile-period of a tiled corpus (fixed 768 KiB cut: the corpus
        // tiles repo content at ~839 KiB; anything below is cold-start only).
        let cold_lit = |t: &[(u32, u32, u32)]| -> (u64, u64) {
            let mut p = 0usize;
            let (mut l, mut m) = (0u64, 0u64);
            for &(ll, ml, _) in t {
                if p < 786432 {
                    l += ll as u64;
                    m += ml as u64;
                }
                p += ll as usize + ml as usize;
            }
            (l, m)
        };
        let (ol, om) = cold_lit(&rec.triples);
        let (rl, rm) = cold_lit(&ref_triples);
        println!("first-768K: ours lit={ol} match={om} | ref lit={rl} match={rm}");
        // Dump both literal streams (the parse's literal-band choice) for
        // offline inspection.
        let dump_lits = |name: &str, t: &[(u32, u32, u32)]| {
            let mut v = Vec::with_capacity(raw.len());
            let mut quads = Vec::with_capacity(t.len() * 4);
            let mut p = 0usize;
            for &(ll, ml, of) in t {
                v.extend_from_slice(&raw[p..p + ll as usize]);
                quads.extend_from_slice(&[p as u32, ll, ml, of]);
                p += ll as usize + ml as usize;
            }
            v.extend_from_slice(&raw[p..]);
            fs::write(format!("target/lit_{name}.bin"), &v).unwrap();
            // Positioned parse (start, ll, ml, of-wire) for offline banding.
            fs::write(
                format!("target/triples_{name}.bin"),
                quads
                    .iter()
                    .flat_map(|q| q.to_le_bytes())
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        };
        dump_lits("ours", &rec.triples);
        dump_lits("ref", &ref_triples);
        println!(
            "bounds delta (ours-ref): seq {:+.1}% lit {:+.1}%",
            (ours.seq_bits / theirs.seq_bits - 1.0) * 100.0,
            (ours.lit_bits / theirs.lit_bits - 1.0) * 100.0
        );
        diff_parses(&rec.triples, &ref_triples, &raw, args.divergences);
    }
}
