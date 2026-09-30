//! `-l`/`--list`: inspect `.zst` files and print frame information,
//! mirroring zstd 1.5.7's output (column layout, verbose block, grouped
//! totals, and the error categories: not-zstd, truncated, frame error).
//!
//! Nothing is decompressed: frame headers are parsed and block headers
//! walked (3 bytes each) only to find frame boundaries, exactly like
//! libzstd's `FIO_analyzeFrames`.

use std::{
    fs::{self, File},
    io::{self, BufRead, BufReader, IsTerminal, Read},
    path::Path,
};

use zstdx::decoding::{
    errors::ReadFrameHeaderError,
    frame::{FrameHeader, read_frame_header},
};

use crate::{Cli, Input, PREFIX, collect_input, read_filelist};

/// The short-mode table header, byte-identical to zstd's.
const HEADER_ROW: &str = "Frames  Skips  Compressed  Uncompressed  Ratio  Check  Filename";
/// The grouped-total separator: 65 dashes and a trailing space, zstd's.
const TOTAL_RULE: &str = "----------------------------------------------------------------- ";
/// The shortest tail that can still start a frame (magic + descriptor);
/// anything less at EOF makes the file "not compressed by zstd".
const FRAME_HEADER_MIN: usize = 5;
/// The space zstd leaves for the Uncompressed and Ratio columns when the
/// content size is unknown (2 + 8 + 4 + 2 + 5 + 2).
const UNKNOWN_SIZE_GAP: &str = "                       ";

/// zstd's frame-error detail strings (DISPLAYLEVEL(1) diagnostics, printed
/// to stderr before the "Error while parsing" line).
const BAD_HEADER: &str = "Error: could not decode frame header";
const BAD_CHECKSUM: &str = "Error: could not read checksum";
const BAD_BLOCK_HEADER: &str = "Error while reading block header";
const BAD_BLOCK_TYPE: &str = "Error: unsupported block type";
const NO_MORE_FRAMES: &str = "Error: did not reach end of file but ran out of frames";

/// Per-file frame statistics, zstd's `fileInfo_t`.
#[allow(clippy::struct_excessive_bools)]
struct FileInfo {
    /// Zstandard frames (skippable frames counted separately).
    frames: u32,
    /// Skippable frames.
    skips: u32,
    /// Sum of the declared frame content sizes.
    decompressed: u64,
    /// False once any frame lacks a content-size field.
    decompressed_known: bool,
    /// Any frame in the file declared a checksum (OR, like zstd).
    uses_check: bool,
    /// The last frame's checksum bytes.
    checksum: [u8; 4],
    /// Zstd's mismatch-quirk field: 0 when frames disagree.
    dict_id: u32,
    /// The last Zstandard frame's window (skippable frames never touch it).
    window_size: u64,
}

impl Default for FileInfo {
    /// A zeroed `fileInfo_t`: decompressed sizes count as known until a
    /// frame without a content-size field says otherwise.
    fn default() -> Self {
        Self {
            decompressed_known: true,
            ..Self::zeroed()
        }
    }
}

impl FileInfo {
    /// The grouped total's initial state: zstd zeroes a `fileInfo_t` and
    /// then sets `usesCheck = 1`, so the Check column reads XXH64 until a
    /// listed file without any checksum blanks it.
    fn new_total() -> Self {
        Self {
            uses_check: true,
            ..Self::default()
        }
    }

    /// `memset(&info, 0, sizeof)`.
    fn zeroed() -> Self {
        Self {
            frames: 0,
            skips: 0,
            decompressed: 0,
            decompressed_known: false,
            uses_check: false,
            checksum: [0; 4],
            dict_id: 0,
            window_size: 0,
        }
    }
}

/// Why a file's analysis stopped early, mirroring zstd's `InfoError`.
#[derive(Debug)]
enum WalkError {
    /// Bad magic (or a tail shorter than a minimal frame header): the file
    /// is "not compressed by zstd" and gets no row. The flag notes the
    /// short-tail cause, which zstd reports on stderr first.
    NotZstd { incomplete_tail: bool },
    /// A skippable frame ran past the end of the file: zstd reports the
    /// seek position against the file size.
    Truncated { position: u64, size: u64 },
    /// A frame failed to parse or walk; the partial stats still get a row,
    /// like zstd's `info_frame_error` path. Carries the stderr detail line.
    Frame(&'static str),
}

/// zstd's `UTIL_makeHumanReadableSize`: scale by binary thresholds and pick
/// the precision from the value's figure count (integral values lose the
/// decimals).
struct HumanSize {
    value: f64,
    suffix: &'static str,
    precision: usize,
}

fn human_size(bytes: u64, raw: bool) -> HumanSize {
    const TABLE: [(u64, &str); 6] = [
        (1 << 60, " EiB"),
        (1 << 50, " PiB"),
        (1 << 40, " TiB"),
        (1 << 30, " GiB"),
        (1 << 20, " MiB"),
        (1 << 10, " KiB"),
    ];
    if raw {
        // zstd's -vv listing prints unscaled byte counts.
        return HumanSize {
            value: bytes as f64,
            suffix: " B",
            precision: 0,
        };
    }
    let (value, suffix) = TABLE
        .iter()
        .find_map(|&(threshold, suffix)| {
            (bytes >= threshold).then_some((bytes as f64 / threshold as f64, suffix))
        })
        .unwrap_or((bytes as f64, " B"));
    let integral = value as u64 == bytes;
    let precision = if value >= 100.0 || integral {
        0
    } else if value >= 10.0 {
        1
    } else if value > 1.0 {
        2
    } else {
        3
    };
    HumanSize {
        value,
        suffix,
        precision,
    }
}

impl HumanSize {
    /// zstd's `%N.*f%4s` cell: the value right-aligned in `width`, the
    /// suffix right-aligned in 4.
    fn cell(&self, width: usize) -> String {
        format!(
            "{:>width$.prec$}{:>4}",
            self.value,
            self.suffix,
            width = width,
            prec = self.precision
        )
    }

    /// The verbose-block form `%.*f%s`.
    fn plain(&self) -> String {
        format!(
            "{:.prec$}{}",
            self.value,
            self.suffix,
            prec = self.precision
        )
    }
}

/// The decompressed/compressed ratio zstd reports (zero for an empty
/// numerator and denominator alike, so an empty frame prints 0.000).
fn ratio(compressed: u64, decompressed: u64) -> f64 {
    if compressed == 0 {
        0.0
    } else {
        decompressed as f64 / compressed as f64
    }
}

/// Entry point: list every input, print the grouped total, and return
/// whether every file parsed.
pub fn run(cli: &Cli) -> bool {
    // Stdin cannot be listed: frame boundaries are found by walking, and
    // the walk needs the file's finite, known length.
    let filelist = match &cli.filelist {
        Some(path) => match read_filelist(path) {
            Ok(list) => list,
            Err(err) => {
                eprintln!("{PREFIX}{}: {err}", path.display());
                return false;
            },
        },
        None => Vec::new(),
    };
    let no_files = cli.files.is_empty() && filelist.is_empty();
    let mut inputs = Vec::new();
    let mut ok = true;
    for file in cli.files.iter().chain(&filelist) {
        collect_input(file, cli.recursive, &mut inputs, &mut ok);
    }
    if no_files || inputs.iter().any(|i| matches!(i, Input::Stdin)) {
        if !io::stdin().is_terminal() {
            eprintln!("{PREFIX}--list does not support reading from standard input ");
        }
        if no_files {
            eprintln!("No files given ");
        }
        return false;
    }

    // -vv and beyond print unscaled byte counts, like zstd's
    // g_utilDisplayLevel > 3.
    let raw = cli.verbose >= 2;
    if cli.verbose == 0 {
        println!("{HEADER_ROW}");
    }
    let mut total = FileInfo::new_total();
    let mut total_compressed = 0;
    let mut total_files = 0;
    for input in &inputs {
        let Input::File(path) = input else {
            unreachable!("stdin inputs were rejected above");
        };
        match analyze_file(path) {
            Analyzed::Stats { info, compressed } => {
                display(cli, path, &info, compressed, raw);
                // Totals fold only cleanly parsed files, like zstd's
                // FIO_addFInfo (which never sees failed analyses).
                total.frames += info.frames;
                total.skips += info.skips;
                total.decompressed += info.decompressed;
                total.decompressed_known &= info.decompressed_known;
                total.uses_check &= info.uses_check;
                total_compressed += compressed;
                total_files += 1;
            },
            Analyzed::FrameError {
                info,
                compressed,
                detail,
            } => {
                display(cli, path, &info, compressed, raw);
                if cli.quiet < 1 {
                    eprintln!("{detail} ");
                    eprintln!("Error while parsing \"{}\" ", path.display());
                }
                ok = false;
            },
            Analyzed::NotZstd { incomplete_tail } => {
                if incomplete_tail && cli.quiet < 1 {
                    eprintln!("Error: reached end of file with incomplete frame ");
                }
                println!("File \"{}\" not compressed by zstd ", path.display());
                if cli.verbose > 0 {
                    println!();
                }
                ok = false;
            },
            Analyzed::Truncated { position, size } => {
                if cli.quiet < 1 {
                    // The message embeds a newline, so zstd's ERROR_IF
                    // epilogue (" \n") lands on a line of its own.
                    eprintln!(
                        "Error: seeked to position {position}, which is beyond file size of \
                         {size}\n "
                    );
                }
                println!("File \"{}\" is truncated ", path.display());
                if cli.verbose > 0 {
                    println!();
                }
                ok = false;
            },
            Analyzed::FileError => {
                if cli.verbose > 0 {
                    println!();
                }
                ok = false;
            },
        }
    }
    if cli.verbose == 0 && inputs.len() > 1 {
        println!("{TOTAL_RULE}");
        let compressed = human_size(total_compressed, raw);
        // The grouped Check column ANDs across files; empty when any
        // listed file carried no checksum.
        let check = if total.uses_check {
            "XXH64"
        } else {
            ""
        };
        if total.decompressed_known {
            let decompressed = human_size(total.decompressed, raw);
            println!(
                "{:>6}  {:>5}  {}  {}  {:>5.3}  {:>5}  {} files",
                total.frames + total.skips,
                total.skips,
                compressed.cell(6),
                decompressed.cell(8),
                ratio(total_compressed, total.decompressed),
                check,
                total_files,
            );
        } else {
            println!(
                "{:>6}  {:>5}  {}{UNKNOWN_SIZE_GAP}{check:>5}  {} files",
                total.frames + total.skips,
                total.skips,
                compressed.cell(6),
                total_files,
            );
        }
    }
    ok
}

/// One file's analysis outcome.
enum Analyzed {
    /// Parsed cleanly; the row/block shows.
    Stats {
        info: FileInfo,
        compressed: u64,
    },
    /// A frame failed mid-walk; partial stats still display, like zstd.
    FrameError {
        info: FileInfo,
        compressed: u64,
        detail: &'static str,
    },
    NotZstd {
        incomplete_tail: bool,
    },
    /// A skippable frame ran past the end of the file.
    Truncated {
        position: u64,
        size: u64,
    },
    /// The path could not be opened or is not a regular file.
    FileError,
}

fn analyze_file(path: &Path) -> Analyzed {
    let meta = match fs::metadata(path) {
        Ok(meta) if meta.is_file() => meta,
        _ => {
            eprintln!("Error : {} is not a file ", path.display());
            return Analyzed::FileError;
        },
    };
    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) => {
            eprintln!("Error : {} : {err} ", path.display());
            return Analyzed::FileError;
        },
    };
    let mut info = FileInfo::default();
    let mut reader = BufReader::new(file);
    match analyze(&mut reader, &mut info, meta.len()) {
        Ok(()) => Analyzed::Stats {
            info,
            compressed: meta.len(),
        },
        Err(WalkError::Frame(detail)) => Analyzed::FrameError {
            info,
            compressed: meta.len(),
            detail,
        },
        Err(WalkError::NotZstd { incomplete_tail }) => Analyzed::NotZstd { incomplete_tail },
        Err(WalkError::Truncated { position, size }) => Analyzed::Truncated { position, size },
    }
}

/// Walk every frame in `reader`, accumulating `info`. Mirrors libzstd's
/// `FIO_analyzeFrames`: header parse, FCS/dictID/window bookkeeping, block
/// walk, checksum read.
fn analyze(
    reader: &mut BufReader<File>,
    info: &mut FileInfo,
    compressed: u64,
) -> Result<(), WalkError> {
    loop {
        // A peek, not a read: read_frame_header consumes the magic itself.
        let available = match reader.fill_buf() {
            Ok(buf) => buf.len(),
            Err(_) => return Err(WalkError::Frame(NO_MORE_FRAMES)),
        };
        if available == 0 {
            // zstd only accepts the clean end on a nonempty file: an empty
            // file reports "reached end of file with incomplete frame" and
            // is not compressed data.
            return if compressed == 0 {
                Err(WalkError::NotZstd {
                    incomplete_tail: true,
                })
            } else {
                Ok(())
            };
        }
        if available < FRAME_HEADER_MIN {
            return Err(WalkError::NotZstd {
                incomplete_tail: true,
            });
        }
        // `read_frame_header` takes `impl Read` by value; reborrows keep
        // the caller's handle for the block walk that follows.
        match read_frame_header(&mut *reader) {
            Ok((header, _)) => analyze_frame(reader, info, &header)?,
            Err(ReadFrameHeaderError::SkipFrame { length, .. }) => {
                // The magic and size fields are already consumed; a payload
                // skip landing short of the length means the file was
                // truncated inside the skippable frame.
                let skipped = io::copy(&mut reader.take(u64::from(length)), &mut io::sink())
                    .map_err(|_| WalkError::Frame(NO_MORE_FRAMES))?;
                if skipped < u64::from(length) {
                    // zstd's fseek lands at 8 + length regardless of the
                    // bytes that exist, and reports that position.
                    return Err(WalkError::Truncated {
                        position: 8 + u64::from(length),
                        size: compressed,
                    });
                }
                info.skips += 1;
            },
            Err(ReadFrameHeaderError::BadMagicNumber(_)) => {
                return Err(WalkError::NotZstd {
                    incomplete_tail: false,
                });
            },
            Err(_) => return Err(WalkError::Frame(BAD_HEADER)),
        }
    }
}

/// Bookkeeping and boundary walk for one parsed frame header.
fn analyze_frame(
    reader: &mut BufReader<File>,
    info: &mut FileInfo,
    header: &FrameHeader,
) -> Result<(), WalkError> {
    if header.descriptor.declares_content_size() {
        info.decompressed += header.frame_content_size();
    } else {
        info.decompressed_known = false;
    }
    // zstd's dictID quirk: a disagreement (nonzero against different
    // nonzero) resets the shown id to 0 with a warning; a missing field
    // counts as 0 and can overwrite a previous nonzero id.
    let dict_id = header.dictionary_id().unwrap_or(0);
    if info.dict_id != 0 && info.dict_id != dict_id {
        // zstd prints this warning without a trailing newline.
        eprint!(
            "WARNING: File contains multiple frames with different dictionary IDs. Showing dictID \
             0 instead"
        );
        info.dict_id = 0;
    } else {
        info.dict_id = dict_id;
    }
    info.window_size = header
        .window_size()
        .map_err(|_| WalkError::Frame(BAD_HEADER))?;
    walk_blocks(reader)?;
    if header.descriptor.content_checksum_flag() {
        let mut checksum = [0u8; 4];
        reader
            .read_exact(&mut checksum)
            .map_err(|_| WalkError::Frame(BAD_CHECKSUM))?;
        info.checksum = checksum;
        info.uses_check = true;
    }
    info.frames += 1;
    Ok(())
}

/// Hop the 3-byte block headers until the frame's last block, so the next
/// frame's magic is next in the reader.
fn walk_blocks(reader: &mut BufReader<File>) -> Result<(), WalkError> {
    loop {
        let mut header = [0u8; 3];
        reader
            .read_exact(&mut header)
            .map_err(|_| WalkError::Frame(BAD_BLOCK_HEADER))?;
        let block = u32::from(header[0]) | u32::from(header[1]) << 8 | u32::from(header[2]) << 16;
        let last = block & 1 == 1;
        let block_type = (block >> 1) & 0x3;
        if block_type == 3 {
            return Err(WalkError::Frame(BAD_BLOCK_TYPE));
        }
        // An RLE block stores one byte regardless of the coded size.
        let size = if block_type == 1 {
            1
        } else {
            u64::from(block >> 3)
        };
        let skipped = io::copy(&mut reader.take(size), &mut io::sink())
            .map_err(|_| WalkError::Frame(BAD_BLOCK_HEADER))?;
        if skipped < size {
            return Err(WalkError::Frame(BAD_BLOCK_HEADER));
        }
        if last {
            return Ok(());
        }
    }
}

/// One file's output: the short table row, or the verbose field block.
fn display(cli: &Cli, path: &Path, info: &FileInfo, compressed: u64, raw: bool) {
    if cli.verbose == 0 {
        let frames = info.frames + info.skips;
        let check = if info.uses_check {
            "XXH64"
        } else {
            "None"
        };
        let compressed_cell = human_size(compressed, raw).cell(6);
        if info.decompressed_known {
            println!(
                "{frames:>6}  {:>5}  {compressed_cell}  {}  {:>5.3}  {check:>5}  {}",
                info.skips,
                human_size(info.decompressed, raw).cell(8),
                ratio(compressed, info.decompressed),
                path.display(),
            );
        } else {
            println!(
                "{frames:>6}  {:>5}  {compressed_cell}{UNKNOWN_SIZE_GAP}{check:>5}  {}",
                info.skips,
                path.display(),
            );
        }
        return;
    }
    println!("{} ", path.display());
    println!("# Zstandard Frames: {}", info.frames);
    if info.skips > 0 {
        println!("# Skippable Frames: {}", info.skips);
    }
    println!("DictID: {}", info.dict_id);
    println!(
        "Window Size: {} ({} B)",
        human_size(info.window_size, raw).plain(),
        info.window_size
    );
    println!(
        "Compressed Size: {} ({} B)",
        human_size(compressed, raw).plain(),
        compressed
    );
    if info.decompressed_known {
        println!(
            "Decompressed Size: {} ({} B)",
            human_size(info.decompressed, raw).plain(),
            info.decompressed
        );
        println!("Ratio: {:.4}", ratio(compressed, info.decompressed));
    }
    // The checksum value only shows for exactly one checked frame.
    if info.uses_check && info.frames == 1 {
        println!("Check: XXH64 {:08x}", u32::from_le_bytes(info.checksum));
    } else if info.uses_check {
        println!("Check: XXH64");
    } else {
        println!("Check: None");
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::{HEADER_ROW, TOTAL_RULE, UNKNOWN_SIZE_GAP, human_size};

    /// zstd's size ladder: thresholds, suffixes, and the figure-count
    /// precision rule (integral values lose the decimals).
    #[test]
    fn human_sizes_match_zstd() {
        let h = |b: u64| human_size(b, false);
        assert_eq!(h(13).plain(), "13 B");
        assert_eq!(h(13).cell(6), "    13   B");
        assert_eq!(h(300_022).plain(), "293 KiB");
        assert_eq!(h(1_300_000).plain(), "1.24 MiB");
        assert_eq!(h(2_097_152).plain(), "2.00 MiB");
        assert_eq!(h(1_048_576).plain(), "1.000 MiB");
        assert_eq!(h(100 << 10).plain(), "100 KiB");
        assert_eq!(h(0).plain(), "0 B");
        // -vv prints unscaled byte counts.
        assert_eq!(human_size(300_022, true).plain(), "300022 B");
    }

    /// The fixed strings must keep zstd's exact widths.
    #[test]
    fn fixed_strings_match_zstd_widths() {
        assert_eq!(HEADER_ROW.len(), 63);
        assert_eq!(TOTAL_RULE.len(), 66);
        assert_eq!(UNKNOWN_SIZE_GAP.len(), 23);
    }
}
