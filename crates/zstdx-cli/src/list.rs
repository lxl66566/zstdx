//! `-l`/`--list`: inspect `.zst` files and print frame information,
//! mirroring zstd 1.5.7's output (column layout, verbose block, grouped
//! totals, and the error categories: not-zstd, truncated, frame error).
//!
//! Nothing is decompressed: frame headers are parsed and block headers
//! walked (3 bytes each) only to find frame boundaries, exactly like
//! libzstd's `FIO_analyzeFrames`.

use std::{
    fs::{self, File},
    io::{self, BufReader, IsTerminal, Read, Seek, SeekFrom},
    path::Path,
};

use zstdx::decoding::frame::{FrameHeader, read_frame_header};

use crate::{Cli, Input, PREFIX, ZSTD_MAGIC, collect_input, read_filelist};

/// The short-mode table header, byte-identical to zstd's.
const HEADER_ROW: &str = "Frames  Skips  Compressed  Uncompressed  Ratio  Check  Filename";
/// The grouped-total separator: 65 dashes and a trailing space, zstd's.
const TOTAL_RULE: &str = "----------------------------------------------------------------- ";
/// The frame-header window the reference reads each iteration
/// (`ZSTD_FRAMEHEADERSIZE_MAX`).
const FRAME_HEADER_MAX: usize = 18;
/// The shortest input that can still hold a frame header
/// (`ZSTD_FRAMEHEADERSIZE_MIN(ZSTD_f_zstd1)`): magic, descriptor, and one
/// more field byte. Anything less at EOF makes the file "not compressed by
/// zstd".
const FRAME_HEADER_MIN: usize = 6;
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
        collect_input(
            file,
            cli.recursive,
            &mut inputs,
            &mut ok,
            crate::InputGathering::Raw,
            cli.quiet,
        );
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

/// Walk every frame in the file, accumulating `info`. Mirrors libzstd's
/// `FIO_analyzeFrames`: each iteration reads a frame-header window (up to
/// 18 bytes), parses the header from it, then fseek()s over the frame --
/// a seek may land past EOF, and the landing position (not the bytes
/// actually available) decides the next branch, so the walk tracks `pos`
/// itself.
fn analyze(
    reader: &mut BufReader<File>,
    info: &mut FileInfo,
    compressed: u64,
) -> Result<(), WalkError> {
    let mut pos = 0u64;
    loop {
        let mut window = [0u8; FRAME_HEADER_MAX];
        let n = read_window(reader, &mut window)?;
        if n < FRAME_HEADER_MIN {
            if n == 0 && compressed > 0 {
                // A clean end needs the read to start exactly at EOF; an
                // overshooting seek left pos past the size instead.
                if pos != compressed {
                    return Err(WalkError::Truncated {
                        position: pos,
                        size: compressed,
                    });
                }
                return Ok(());
            }
            // 1-5 bytes cannot hold a frame header; an empty file never
            // held one either.
            return Err(WalkError::NotZstd {
                incomplete_tail: true,
            });
        }
        let magic = u32::from_le_bytes([window[0], window[1], window[2], window[3]]);
        if magic == u32::from_le_bytes(ZSTD_MAGIC) {
            // ZSTD_getFrameContentSize runs before the header is
            // validated: a header that fails to parse still blanks the
            // Uncompressed and Ratio columns.
            let Ok((header, header_size)) = read_frame_header(&mut &window[..n]) else {
                info.decompressed_known = false;
                return Err(WalkError::Frame(BAD_HEADER));
            };
            note_frame_header(info, &header)?;
            // The window consumed n bytes; continue at the frame's first
            // block header.
            let blocks_at = pos + u64::from(header_size);
            seek_to(reader, &mut pos, blocks_at)?;
            walk_blocks(reader, &mut pos)?;
            if header.descriptor.content_checksum_flag() {
                // uses_check is set before the read: a truncated checksum
                // still lists XXH64, like zstd.
                info.uses_check = true;
                let mut checksum = [0u8; 4];
                if reader.read_exact(&mut checksum).is_err() {
                    return Err(WalkError::Frame(BAD_CHECKSUM));
                }
                info.checksum = checksum;
                pos += 4;
            }
            info.frames += 1;
        } else if magic & 0xffff_fff0 == 0x184d_2a50 {
            // A skippable frame: magic and size field sit inside the
            // window; the payload is seeked over, however far that lands.
            let length = u32::from_le_bytes([window[4], window[5], window[6], window[7]]);
            let end = pos + 8 + u64::from(length);
            seek_to(reader, &mut pos, end)?;
            info.skips += 1;
        } else {
            return Err(WalkError::NotZstd {
                incomplete_tail: false,
            });
        }
    }
}

/// Bookkeeping for one parsed frame header; the boundary walk itself lives
/// in the caller, which owns the position cursor.
fn note_frame_header(info: &mut FileInfo, header: &FrameHeader) -> Result<(), WalkError> {
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
    Ok(())
}

/// Hop the 3-byte block headers until the frame's last block, so the next
/// frame's magic is next in the reader. Block bodies are seeked over: the
/// seek may land past EOF, and any failure comes from the next header or
/// checksum read, like the reference's fseek.
fn walk_blocks(reader: &mut BufReader<File>, pos: &mut u64) -> Result<(), WalkError> {
    loop {
        let mut header = [0u8; 3];
        reader
            .read_exact(&mut header)
            .map_err(|_| WalkError::Frame(BAD_BLOCK_HEADER))?;
        *pos += 3;
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
        seek_to(reader, pos, *pos + size)?;
        if last {
            return Ok(());
        }
    }
}

/// Move the reader to an absolute file position (fseek semantics: landing
/// past EOF is fine, the next read reports it) and take the cursor along.
fn seek_to(reader: &mut BufReader<File>, pos: &mut u64, target: u64) -> Result<(), WalkError> {
    reader
        .seek(SeekFrom::Start(target))
        .map_err(|_| WalkError::Frame(NO_MORE_FRAMES))?;
    *pos = target;
    Ok(())
}

/// fread semantics for the header window: everything the buffer asks for,
/// fewer bytes only at EOF; read errors land in the reference's
/// out-of-frames category.
fn read_window(reader: &mut BufReader<File>, window: &mut [u8]) -> Result<usize, WalkError> {
    let mut filled = 0;
    while filled < window.len() {
        match reader.read(&mut window[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {},
            Err(_) => return Err(WalkError::Frame(NO_MORE_FRAMES)),
        }
    }
    Ok(filled)
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
