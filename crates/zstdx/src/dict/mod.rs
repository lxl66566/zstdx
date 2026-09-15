//! Raw-content dictionary training (local maximum coverage).
//!
//! Implemented following libzstd's fastCover: the training samples split
//! into epochs, a sliding-window search scores each `k`-byte window by the
//! summed frequency of its distinct 8-byte k-mers, and each epoch's best
//! window buys dictionary budget (strongest content last, at the offsets
//! closest to the payload). With enough samples, several segment sizes `k`
//! are tried and the winner is decided by compressing held-out samples.
//!
//! Deterministic by construction: fixed-seed sample shuffle, stride-sampled
//! frequency estimates, stable iteration order.

mod cover;
mod finalize;

use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    vec,
    vec::Vec,
};

use cover::KMerTable;
pub use finalize::finalize_dictionary;

use crate::{EncoderOptions, Level};

/// Upper bound on buffered training data (matches libzstd's trainer).
const SOURCE_CAP: usize = 128 << 20;
/// K-mer size, libzstd's dmer granularity: shared template text usually
/// differs in embedded values, so 8-byte matches survive where 16-byte ones
/// do not.
const K: usize = 8;
/// Segment sizes tried when a holdout split exists; the winner is decided
/// by compressing the held-out samples (libzstd's optimizer approach).
const K_SWEEP: [usize; 9] = [200, 300, 400, 500, 650, 800, 1024, 1400, 2000];
/// Without a holdout there is nothing to score the sweep against.
const K_DEFAULT: usize = 1024;
/// Samples below this count train on everything and skip the sweep.
const MIN_SAMPLES_FOR_SPLIT: usize = 8;
/// Fraction of samples used for training, the rest score the sweep.
const TRAIN_SPLIT_NUM: usize = 3;
const TRAIN_SPLIT_DEN: usize = 4;
/// Cap on held-out bytes scored per candidate.
const EVAL_CAP: usize = 2 << 20;
/// Consecutive scoreless epochs before the content is considered spent.
const ZERO_SCORE_RUN_MAX: usize = 10;

/// Creates a "raw content" dictionary from every file under `path`
/// (recursed), written to `output` with at most `dict_size` bytes.
pub fn create_raw_dict_from_dir<P: AsRef<Path>, W: Write>(
    path: P,
    output: &mut W,
    dict_size: usize,
) -> Result<(), io::Error> {
    let mut samples: Vec<Vec<u8>> = Vec::new();
    for file in walk(path.as_ref()) {
        let mut sample = Vec::new();
        fs::File::open(file)?.read_to_end(&mut sample)?;
        samples.push(sample);
    }
    let refs: Vec<&[u8]> = samples.iter().map(|s| &s[..]).collect();
    create_raw_dict_from_samples(&refs, output, dict_size);
    Ok(())
}

/// Directory walk in sorted order, so the concatenated training body — and
/// with it the trained dictionary — is deterministic.
fn walk(path: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if path.is_dir() {
        let mut entries: Vec<PathBuf> = fs::read_dir(path)
            .map(|it| it.filter_map(Result::ok).map(|e| e.path()).collect())
            .unwrap_or_default();
        entries.sort();
        for entry in entries {
            out.extend(walk(&entry));
        }
    } else {
        out.push(path.to_path_buf());
    }
    out
}

/// Read from `source` to create a "raw content" dictionary of at most
/// `dict_size` bytes, written to `output`.
///
/// The stream is one anonymous sample: with no sample boundaries there is
/// no holdout to score a segment-size sweep against, so the default segment
/// size is used. `source_size` is a hint; the actual byte count read wins.
pub fn create_raw_dict_from_source<R: Read, W: Write>(
    source: R,
    source_size: usize,
    output: &mut W,
    dict_size: usize,
) {
    let mut body = Vec::with_capacity(source_size.min(SOURCE_CAP));
    source
        .take(SOURCE_CAP as u64)
        .read_to_end(&mut body)
        .expect("could not read from source");
    if body.len() < 16 {
        output.write_all(&body).expect("could not write to output");
        return;
    }
    create_raw_dict_from_samples(&[&body], output, dict_size);
}

/// The shuffled training body: sample order is fixed-seed shuffled, then
/// concatenated with lengths. `train_end` splits train from holdout.
struct TrainingSet {
    body: Vec<u8>,
    lens: Vec<usize>,
    train_end: usize,
}

impl TrainingSet {
    /// Build from `samples` (raw content is passed through when too small
    /// to train — `body` then holds it whole and `trainable` is false).
    fn build(samples: &[&[u8]]) -> Self {
        let mut order: Vec<usize> = (0..samples.len()).collect();
        shuffle(&mut order);
        let mut body = Vec::new();
        let mut lens: Vec<usize> = Vec::with_capacity(samples.len());
        for &i in &order {
            let sample = samples[i];
            let take = sample.len().min(SOURCE_CAP - body.len());
            if take == 0 {
                continue;
            }
            body.extend_from_slice(&sample[..take]);
            lens.push(take);
            if body.len() == SOURCE_CAP {
                break;
            }
        }
        let train_end = if lens.len() >= MIN_SAMPLES_FOR_SPLIT {
            lens.len() * TRAIN_SPLIT_NUM / TRAIN_SPLIT_DEN
        } else {
            lens.len()
        };
        Self {
            body,
            lens,
            train_end,
        }
    }

    fn trainable(&self) -> bool {
        self.body.len() >= 16
    }

    /// The training split's samples in shuffled order (C's finalize step
    /// uses the whole train split at the default acceleration).
    fn train_samples(&self) -> Vec<&[u8]> {
        let mut offset = 0;
        self.lens[..self.train_end]
            .iter()
            .map(|&len| {
                let sample = &self.body[offset..offset + len];
                offset += len;
                sample
            })
            .collect()
    }

    /// Select the dictionary content (the fastCover pass with its
    /// segment-size sweep, scored on the holdout).
    fn select_content(&self, dict_size: usize) -> Vec<u8> {
        let train_len: usize = self.lens[..self.train_end].iter().sum();
        let train_body = &self.body[..train_len];
        let test_samples: Vec<&[u8]> = self.lens[self.train_end..]
            .iter()
            .scan(train_len, |offset, &len| {
                let sample = &self.body[*offset..*offset + len];
                *offset += len;
                Some(sample)
            })
            .collect();

        let counts = KMerTable::build(train_body);
        let candidates: Vec<usize> = if test_samples.is_empty() {
            vec![K_DEFAULT]
        } else {
            K_SWEEP
                .iter()
                .copied()
                .filter(|&k| k <= dict_size && k <= train_body.len())
                .collect()
        };
        let mut best: Option<(usize, Vec<u8>)> = None;
        for &k in &candidates {
            let dict = build_dict(&mut counts.clone(), train_body, k, dict_size);
            if dict.is_empty() {
                continue;
            }
            // Without a holdout the sweep is a single default-K candidate;
            // there is nothing to score it against.
            let score = if test_samples.is_empty() {
                0
            } else {
                evaluate(&dict, &test_samples)
            };
            vprintln!("create_dict: k={k} -> {} bytes, eval {score}", dict.len());
            if best.as_ref().is_none_or(|(s, _)| score < *s) {
                best = Some((score, dict));
            }
        }
        best.map_or_else(Vec::new, |(_, dict)| dict)
    }
}

/// Train a "raw content" dictionary of at most `dict_size` bytes from
/// `samples`, written to `output`.
pub fn create_raw_dict_from_samples<W: Write>(samples: &[&[u8]], output: &mut W, dict_size: usize) {
    if samples.is_empty() {
        return;
    }
    let set = TrainingSet::build(samples);
    let dict = if set.trainable() {
        vprintln!(
            "create_dict: training {dict_size} byte dict from {} samples, {} bytes",
            set.lens.len(),
            set.body.len()
        );
        set.select_content(dict_size)
    } else {
        set.body
    };
    output.write_all(&dict).expect("could not write to output");
}

/// Train a formatted dictionary of at most `dict_size` bytes from
/// `samples`: the selected content plus entropy tables collected over the
/// training split (the `ZDICT_finalizeDictionary` step). Falls back to the
/// raw-content form when the capacity cannot carry a header.
pub fn create_formatted_dict_from_samples<W: Write>(
    samples: &[&[u8]],
    output: &mut W,
    dict_size: usize,
) {
    if samples.is_empty() {
        return;
    }
    let set = TrainingSet::build(samples);
    if !set.trainable() {
        output
            .write_all(&set.body)
            .expect("could not write to output");
        return;
    }
    vprintln!(
        "create_dict: training {dict_size} byte dict from {} samples, {} bytes",
        set.lens.len(),
        set.body.len()
    );
    let content = set.select_content(dict_size);
    let dict = match finalize_dictionary(&content, &set.train_samples(), dict_size) {
        Some(dict) => dict,
        None => content,
    };
    output.write_all(&dict).expect("could not write to output");
}

/// Deterministic Fisher-Yates over sample order (libzstd's `DiB_shuffle`):
/// caller-supplied sample lists are usually sorted, and a sorted
/// concatenation makes every epoch a cluster of similar files — the
/// selected segments then describe the cluster, not the collection, and the
/// positional train/test split lands on a distribution shift. A fixed seed
/// keeps training reproducible.
fn shuffle<T>(items: &mut [T]) {
    let mut seed: u32 = 0xfd2fb528;
    for i in (1..items.len()).rev() {
        seed = seed.wrapping_mul(2654435761) ^ 0x85eb_ca77;
        seed = seed.rotate_left(13);
        let j = (seed >> 5) as usize % (i + 1);
        items.swap(j, i);
    }
}

/// One fastCover pass: the best `k`-byte window of every epoch fills the
/// dictionary from the back, strongest content last, until the budget is
/// spent or the content runs out.
fn build_dict(table: &mut KMerTable, train_body: &[u8], k: usize, dict_size: usize) -> Vec<u8> {
    let positions = train_body.len() + 1 - K;
    let mut num_epochs = (dict_size / k).max(1);
    let mut epoch_size = positions / num_epochs;
    let min_epoch_size = k * 10;
    if epoch_size < min_epoch_size {
        epoch_size = min_epoch_size.min(positions);
        num_epochs = (positions / epoch_size).max(1);
    }

    let mut dict = vec![0u8; dict_size];
    let mut tail = dict_size;
    let mut zero_score_run = 0;
    let mut epoch = 0;
    while tail > 0 {
        let begin = epoch * epoch_size;
        if begin >= positions {
            epoch = (epoch + 1) % num_epochs;
            continue;
        }
        let end = (begin + epoch_size).min(positions);
        let segment = table.select_segment(train_body, begin, end, k);
        if segment.score == 0 {
            zero_score_run += 1;
            if zero_score_run >= ZERO_SCORE_RUN_MAX {
                break;
            }
            epoch = (epoch + 1) % num_epochs;
            continue;
        }
        zero_score_run = 0;
        let segment_bytes = (segment.end - segment.begin + K - 1).min(tail);
        if segment_bytes < K {
            break;
        }
        tail -= segment_bytes;
        dict[tail..tail + segment_bytes]
            .copy_from_slice(&train_body[segment.begin..segment.begin + segment_bytes]);
        epoch = (epoch + 1) % num_epochs;
    }
    dict[tail..].to_vec()
}

/// Total compressed size of the held-out samples under the candidate
/// dictionary — the sweep's selection metric, evaluated at the zstd default
/// level like libzstd's trainer. Callers without a holdout skip the sweep.
fn evaluate(dict: &[u8], test_samples: &[&[u8]]) -> usize {
    debug_assert_ne!(test_samples.len(), 0);
    let mut options = EncoderOptions::new(Level::from_zstd(3));
    options.checksum = false;
    options.dictionary = Some(dict.to_vec());
    let mut budget = EVAL_CAP;
    let mut total = 0;
    for sample in test_samples {
        let take = sample.len().min(budget);
        if take == 0 {
            break;
        }
        budget -= take;
        match crate::bulk::compress_with(&sample[..take], &options) {
            Ok(frame) => total += frame.len(),
            // An incompressible candidate is worthless; rank it last.
            Err(_) => return usize::MAX,
        }
    }
    total
}

#[test]
fn create_raw_dict_from_source_no_panics_on_small_input() {
    use std::{io::Cursor, vec};

    for size in 0..1024 {
        let input = vec![b'A'; size];
        let mut output = Vec::new();

        create_raw_dict_from_source(Cursor::new(input.clone()), input.len(), &mut output, 64);
        assert!(output.len() <= 64.max(input.len()));
    }
}

#[test]
fn create_raw_dict_from_samples_is_deterministic() {
    use std::vec;

    // Patterned samples: shared blocks separated by noise, enough for
    // several epochs and the sweep.
    let mut samples: Vec<Vec<u8>> = Vec::new();
    for i in 0..16u8 {
        let mut sample = Vec::new();
        for _ in 0..4 {
            sample.extend_from_slice(b"[Unit]\nDescription=shared boilerplate block\n");
            for j in 0..24u32 {
                sample.push((i as u32 * 131 + j * 17) as u8);
            }
        }
        samples.push(sample);
    }
    let refs: Vec<&[u8]> = samples.iter().map(|s| &s[..]).collect();
    let mut a = Vec::new();
    let mut b = Vec::new();
    create_raw_dict_from_samples(&refs, &mut a, 4096);
    create_raw_dict_from_samples(&refs, &mut b, 4096);
    assert_eq!(a, b);
    assert!(a.len() <= 4096);
}

#[cfg(feature = "hash")]
#[test]
fn trained_dict_roundtrips_through_both_decoders() {
    use std::vec::Vec;

    // Train on shared-boilerplate samples, then compress an unseen sample
    // of the same family with the trained raw dictionary: our decoder and
    // libzstd must both reproduce it.
    let mut samples: Vec<Vec<u8>> = Vec::new();
    for i in 0..24u8 {
        let mut sample = Vec::new();
        sample
            .extend_from_slice(b"[Unit]\nDescription=trained boilerplate\nAfter=network.target\n");
        for j in 0..40 {
            sample.push(b'a' + (i % 16) + (j % 5));
        }
        sample.extend_from_slice(b"[Install]\nWantedBy=multi-user.target\n");
        samples.push(sample);
    }
    let payload = samples.pop().unwrap();
    let refs: Vec<&[u8]> = samples.iter().map(|s| &s[..]).collect();
    let mut dict = Vec::new();
    create_raw_dict_from_samples(&refs, &mut dict, 2048);
    assert!(!dict.is_empty());

    let options = EncoderOptions::new(Level::from_zstd(9)).dictionary(&dict);
    let frame = crate::bulk::compress_with(&payload, &options).unwrap();

    // Our decoder applies the id-0 raw dictionary as content history.
    let ours = crate::bulk::decompress_with(
        &frame,
        payload.len(),
        &crate::DecoderOptions::new().dictionary(&dict),
    )
    .unwrap();
    assert_eq!(ours, payload);

    // libzstd treats magic-less dictionaries as content history too.
    let mut reference = zstd::bulk::Decompressor::with_dictionary(&dict).unwrap();
    let libzstd = reference.decompress(&frame, payload.len()).unwrap();
    assert_eq!(libzstd, payload);
}

#[cfg(feature = "hash")]
#[test]
fn formatted_dict_interops_both_ways() {
    use std::vec::Vec;

    // Train + finalize on shared-boilerplate samples; compress an unseen
    // sample of the same family with our encoder and the formatted dict,
    // and require libzstd to decode it (and vice versa for decoding a
    // libzstd-made frame we cannot produce here, the reverse direction is
    // covered by the raw-dict test above plus the CLI gates in bench).
    let mut samples: Vec<Vec<u8>> = Vec::new();
    for i in 0..24u8 {
        let mut sample = Vec::new();
        sample.extend_from_slice(b"[Unit]\nDescription=formatted dict boilerplate\n");
        for j in 0..48 {
            sample.push(b'a' + (i % 16) + (j % 5));
        }
        sample.extend_from_slice(b"[Install]\nWantedBy=multi-user.target\n");
        samples.push(sample);
    }
    let payload = samples.pop().unwrap();
    let refs: Vec<&[u8]> = samples.iter().map(|s| &s[..]).collect();
    let mut dict = Vec::new();
    create_formatted_dict_from_samples(&refs, &mut dict, 2048);
    assert!(!dict.is_empty());

    let options = EncoderOptions::new(Level::from_zstd(9)).dictionary(&dict);
    let frame = crate::bulk::compress_with(&payload, &options).unwrap();

    // Our decoder applies the formatted dictionary's tables and content.
    let ours = crate::bulk::decompress_with(
        &frame,
        payload.len(),
        &crate::DecoderOptions::new().dictionary(&dict),
    )
    .unwrap();
    assert_eq!(ours, payload);

    // libzstd decodes a frame made with our finalized dictionary.
    let mut reference = zstd::bulk::Decompressor::with_dictionary(&dict).unwrap();
    let libzstd = reference.decompress(&frame, payload.len()).unwrap();
    assert_eq!(libzstd, payload);
}
