//! Compress every corpus shape into a directory so two builds can be
//! compared byte for byte: `dump <dir_a>` on each build, then `diff -r`.
//! Byte equality is the regression gate for optimizations that must not
//! change the encoder's choices.
//!
//! Default dumps the Fastest level as `<shape>.raw.zst`; `--all-levels`
//! dumps the whole ladder as `<shape>.raw.l<zstd-level>.zst`. `--stream`
//! swaps the bulk encoders for the streaming cores (read-side, driven like
//! `matrix --mode enc-stream`): each shape × level lands as the
//! {st, mt} × {pledged, open} grid.
//!
//! Frames are independent and the gates are per-frame, so emission runs on
//! the bounded cell pool (`--parallel`, see `pool`); bytes are
//! deterministic regardless of the schedule.

use std::{
    fs,
    io::Read as _,
    path::{Path, PathBuf},
    sync::Arc,
};

use zstdx::{EncoderOptions, Level, bulk};

use crate::{
    corpus::{LADDER, LevelName, assert_roundtrip, corpus_dir},
    pool::{DEFAULT_MAX_WIDTH, run_cells},
};

#[derive(clap::Args)]
pub struct Args {
    /// Output directory for the compressed snapshots
    pub out_dir: PathBuf,
    /// Dump every ladder level instead of Fastest only
    #[arg(long)]
    pub all_levels: bool,
    /// Compress through the multithreaded job path at this worker count
    /// (default: the single-threaded slice path; with --stream, the mt
    /// cells' worker count, default 4)
    #[arg(long)]
    pub workers: Option<u32>,
    /// Emit through the streaming cores instead of bulk: the read-side
    /// incremental core (the `matrix --mode enc-stream` drive) at
    /// {st, mt} × {pledged, open} per shape/level, tagged `.stream`,
    /// `.stream-pledged`, `.stream-mt<W>`, `.stream-mt<W>-pledged`.
    /// Bytes are deterministic for a fixed flag set; the open mt cells ride
    /// the Growing grid, which is keyed on the worker count by design, so
    /// never diff them across --workers values.
    #[arg(long)]
    pub stream: bool,
    /// With `--stream`: drive the cells through the owned in-memory source
    /// entry (`read::OwnedEncoder`, the zero-copy feed) under the same
    /// tags. Diffing an `--owned` dump against a plain one is the
    /// entry's byte-parity gate: the frames must be identical, because
    /// the job schedule is a pure function of the stream offset.
    #[arg(long, requires = "stream")]
    pub owned: bool,
    /// Maximum frames emitted at once, bounded by a built-in memory budget
    /// over the payload size; 0 = one per logical core, 1 = serial emission
    #[arg(long, default_value_t = DEFAULT_MAX_WIDTH)]
    pub parallel: usize,
}

/// One `--stream` cell: the streaming core and the pledge axis.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StreamCell {
    St { pledged: bool },
    Mt { workers: u32, pledged: bool },
}

impl StreamCell {
    /// The {st, mt} × {pledged, open} grid (the mt worker count mirrors
    /// R61's differential matrix).
    fn grid(workers: u32) -> [Self; 4] {
        [
            Self::St { pledged: false },
            Self::St { pledged: true },
            Self::Mt {
                workers,
                pledged: false,
            },
            Self::Mt {
                workers,
                pledged: true,
            },
        ]
    }

    const fn pledged(self) -> bool {
        matches!(
            self,
            Self::St { pledged: true } | Self::Mt { pledged: true, .. }
        )
    }

    /// Filename tag between the corpus name and the level suffix.
    fn tag(self) -> String {
        match self {
            Self::St { pledged: false } => "stream".to_owned(),
            Self::St { pledged: true } => "stream-pledged".to_owned(),
            Self::Mt {
                workers,
                pledged: false,
            } => format!("stream-mt{workers}"),
            Self::Mt {
                workers,
                pledged: true,
            } => format!("stream-mt{workers}-pledged"),
        }
    }

    /// Options mirroring the bulk dump cell each stream cell twins: the st
    /// cells the plain slice dump (feature-default checksum, open header),
    /// the mt cells the `--workers` dump (checksums off, declared size).
    fn options(self, level: Level, raw: &[u8]) -> EncoderOptions {
        let mut opts = EncoderOptions::new(level);
        if let Self::Mt { workers, .. } = self {
            opts = opts.workers(workers).checksum(false);
        }
        if self.pledged() {
            opts = opts.pledged_size(Some(raw.len() as u64));
        }
        opts
    }
}

/// Drive the read-side streaming core exactly as `matrix --mode enc-stream`
/// does (one `read_to_end` over the whole source, no flush — output is
/// chunk-independent), returning the closed frame. `owned` swaps the Read
/// pump for the zero-copy in-memory feed; the tags stay the same, so a
/// plain dump and an owned dump of the same build must be byte-identical.
fn stream_frame(raw: &[u8], options: EncoderOptions, owned: bool) -> Vec<u8> {
    if owned {
        let source = zstdx::stream::read::OwnedSource::from(raw.to_vec());
        let mut enc = zstdx::stream::read::OwnedEncoder::with_options(source, options)
            .expect("owned encoder");
        let mut comp = Vec::new();
        enc.read_to_end(&mut comp).expect("owned stream read");
        enc.finish().expect("owned stream finish");
        comp
    } else {
        let mut enc =
            zstdx::stream::read::Encoder::with_options(raw, options).expect("stream encoder");
        let mut comp = Vec::new();
        enc.read_to_end(&mut comp).expect("stream read");
        enc.finish().expect("stream finish");
        comp
    }
}

/// Write one frame after gating it: both in-process decoders must
/// reproduce the raw input (corpus gate) and the libzstd CLI must decode
/// the file back to the same bytes (mtcheck's interop shape).
fn write_gated(out_dir: &Path, file: &str, frame: &[u8], raw: &[u8]) {
    assert_roundtrip(frame, raw, file);
    let path = out_dir.join(file);
    fs::write(&path, frame).expect("write frame");
    let cli = std::process::Command::new("zstd")
        .arg("-dcqf")
        .arg(&path)
        .output()
        .expect("spawn zstd CLI");
    assert!(cli.status.success(), "libzstd CLI rejected {file}");
    assert_eq!(&cli.stdout[..], raw, "libzstd CLI mismatch for {file}");
}

/// One frame's encode step: raw bytes in, frame out.
type EncodeFn = Box<dyn FnOnce(&[u8]) -> Vec<u8> + Send>;

/// One frame's emission: the output filename, the shared raw bytes and the
/// encode step that turns them into the frame.
type FrameJob = (String, Arc<Vec<u8>>, EncodeFn);

pub fn run(args: &Args) {
    fs::create_dir_all(&args.out_dir).unwrap();
    let mut names: Vec<String> = Vec::new();
    for entry in fs::read_dir(corpus_dir()).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        // corpus files are generated with a lowercase .raw suffix
        #[allow(clippy::case_sensitive_file_extension_comparisons)]
        if name.ends_with(".raw") {
            names.push(name);
        }
    }
    names.sort();
    let levels: Vec<(LevelName, i32)> = if args.all_levels {
        LADDER.iter().map(|l| (*l, l.pair().1)).collect()
    } else {
        vec![(LevelName::Fastest, 1)]
    };
    let cells = args
        .stream
        .then(|| StreamCell::grid(args.workers.unwrap_or(4)));

    // each shape's bytes are shared across its frames
    let raws: Vec<Arc<Vec<u8>>> = names
        .iter()
        .map(|n| Arc::new(fs::read(corpus_dir().join(n)).unwrap()))
        .collect();
    let mut jobs: Vec<FrameJob> = Vec::new();
    for (name, raw) in names.iter().zip(&raws) {
        for (level, z) in &levels {
            let suffix = if args.all_levels {
                format!(".l{z}")
            } else {
                String::new()
            };
            // copies, not borrows: the boxed closures outlive the loops
            let level = *level;
            if let Some(cells) = &cells {
                let owned = args.owned;
                for &cell in cells {
                    let file = format!("{name}.{}{suffix}.zst", cell.tag());
                    let job = Box::new(move |raw: &[u8]| {
                        stream_frame(raw, cell.options(level.pair().0, raw), owned)
                    });
                    jobs.push((file, Arc::clone(raw), job));
                }
            } else {
                let file = format!("{name}{suffix}.zst");
                let job: EncodeFn = match args.workers {
                    None => Box::new(move |raw: &[u8]| {
                        zstdx::encoding::compress_slice_to_vec(raw, level.pair().0)
                    }),
                    Some(w) => Box::new(move |raw: &[u8]| {
                        let opts = EncoderOptions::new(level.pair().0)
                            .checksum(false)
                            .workers(w);
                        bulk::compress_with(raw, &opts).unwrap()
                    }),
                };
                jobs.push((file, Arc::clone(raw), job));
            }
        }
    }

    let out_dir = args.out_dir.clone();
    let pool_cells: Vec<(String, usize, _)> = jobs
        .into_iter()
        .map(|(file, raw, encode)| {
            let out_dir = out_dir.clone();
            (file.clone(), raw.len(), move || {
                let frame = encode(&raw);
                write_gated(&out_dir, &file, &frame, &raw);
            })
        })
        .collect();
    run_cells(pool_cells, args.parallel);
}
