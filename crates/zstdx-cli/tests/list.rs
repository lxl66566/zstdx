//! End-to-end tests of `-l`/`--list`: output parity with the system zstd
//! 1.5.7 (byte-for-byte stdout/stderr and exit codes when the reference
//! binary exists), plus self-contained checks of the column layout, the
//! error categories, and the mode conflicts.

use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const BIN: &str = env!("CARGO_BIN_EXE_zstdx");

/// A scratch directory that removes itself on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(test: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "zstdx-cli-list-{test}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self, name: impl AsRef<Path>) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// The reference zstd, when the environment provides one.
fn system_zstd() -> bool {
    Command::new("zstd")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .is_ok_and(|out| out.status.success())
}

/// zstd's ambient -v noise (version banner, worker-thread notes) that our
/// CLI never prints; stripped before comparing stderr.
const ZSTD_NOISE: [&str; 2] = ["Zstandard CLI", "Compressing with"];

fn stdout_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter(|line| !ZSTD_NOISE.iter().any(|noise| line.contains(noise)))
        .fold(String::new(), |mut acc, line| {
            acc.push_str(line);
            acc.push('\n');
            acc
        })
}

/// Run both CLIs over `files` with `flags` and require identical stdout,
/// stderr (modulo zstd's ambient noise) and exit code.
fn assert_parity(flags: &[&str], files: &[PathBuf]) {
    if !system_zstd() {
        return;
    }
    let path_strs: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    let refs: Vec<&str> = path_strs.iter().map(String::as_str).collect();
    let zstd_out = Command::new("zstd")
        .args(flags)
        .args(&refs)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let ours = Command::new(BIN)
        .args(flags)
        .args(&refs)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(stdout_of(&zstd_out), stdout_of(&ours), "{flags:?} {refs:?}");
    assert_eq!(stderr_of(&zstd_out), stderr_of(&ours), "{flags:?} {refs:?}");
    assert_eq!(
        zstd_out.status.code(),
        ours.status.code(),
        "{flags:?} {refs:?}"
    );
}

/// Compress `payload` into `name` with our own CLI (a fixture builder that
/// needs no reference binary).
fn make_frame(scratch: &Scratch, name: &str, payload: &[u8]) -> PathBuf {
    let input = scratch.path(format!("{name}.bin"));
    fs::write(&input, payload).unwrap();
    let out = run(&[input.to_str().unwrap()]);
    assert!(out.status.success(), "{out:?}");
    scratch.path(format!("{name}.bin.zst"))
}

/// A skippable frame: magic 0x184D2A50..5F + 4-byte LE length + payload.
fn skippable_frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = vec![0x50, 0x2a, 0x4d, 0x18];
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(payload);
    frame
}

fn payload() -> Vec<u8> {
    let mut data = Vec::new();
    for i in 0..10_000u32 {
        data.extend_from_slice(format!("line {i}: the quick brown fox\n").as_bytes());
    }
    data
}

/// A single-frame file: the row matches zstd's exactly, column widths and
/// all.
#[test]
fn single_frame_matches_zstd() {
    let scratch = Scratch::new("single");
    let frame = make_frame(&scratch, "data", &payload());
    let out = run(&["-l", frame.to_str().unwrap()]);
    assert!(out.status.success(), "{out:?}");
    let stdout = stdout_of(&out);
    // The header line and the fixed-width count columns; the size columns
    // carry the human-scaled values checked by parity below.
    assert_eq!(
        stdout.lines().next().unwrap(),
        "Frames  Skips  Compressed  Uncompressed  Ratio  Check  Filename"
    );
    let row = stdout.lines().nth(1).unwrap();
    assert!(row.starts_with("     1      0  "), "{row}");
    assert!(
        row.ends_with(&format!("  XXH64  {}", frame.display())),
        "{row}"
    );
    assert_parity(&["-l"], &[frame]);
}

/// Concatenated frames (a multi-frame file) and a skippable frame in the
/// middle: counts, sizes, and ratio aggregated exactly like zstd.
#[test]
fn multi_frame_and_skippable_match_zstd() {
    let scratch = Scratch::new("multi");
    let data = payload();
    let a = make_frame(&scratch, "a", &data);
    let b = make_frame(&scratch, "b", &data[..data.len() / 2]);
    let multi = scratch.path("multi.zst");
    let mut bytes = fs::read(&a).unwrap();
    bytes.extend_from_slice(&fs::read(&b).unwrap());
    fs::write(&multi, &bytes).unwrap();
    let with_skip = scratch.path("with_skip.zst");
    bytes.splice(0..0, skippable_frame(b"skippable payload"));
    fs::write(&with_skip, &bytes).unwrap();

    let out = run(&["-l", multi.to_str().unwrap()]);
    assert!(out.status.success(), "{out:?}");
    assert!(
        stdout_of(&out).contains("     2      0"),
        "{}",
        stdout_of(&out)
    );
    let out = run(&["-l", with_skip.to_str().unwrap()]);
    assert!(out.status.success(), "{out:?}");
    // the Frames column counts zstd and skippable frames together
    assert!(
        stdout_of(&out).contains("     3      1"),
        "{}",
        stdout_of(&out)
    );

    for file in [&multi, &with_skip] {
        assert_parity(&["-l"], std::slice::from_ref(file));
        assert_parity(&["-lv"], std::slice::from_ref(file));
    }
}

/// A frame with no pledged content size blanks the Uncompressed and Ratio
/// columns, exactly like zstd's `decompUnavailable` form.
#[test]
fn unknown_content_size_blanks_columns() {
    let scratch = Scratch::new("nofcs");
    let data = payload();
    let mut child = Command::new(BIN)
        .arg("-q")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&data).unwrap();
    let streamed = child.wait_with_output().unwrap();
    assert!(streamed.status.success());
    let frame = scratch.path("stream.zst");
    fs::write(&frame, &streamed.stdout).unwrap();

    let out = run(&["-l", frame.to_str().unwrap()]);
    assert!(out.status.success(), "{out:?}");
    let stdout = stdout_of(&out);
    let row = stdout.lines().nth(1).unwrap();
    // 23 spaces stand where Uncompressed and Ratio would be.
    assert!(row.contains("                       XXH64"), "{row}");
    assert_parity(&["-l"], &[frame]);
}

/// Multiple files: per-file rows plus the separator and grouped total.
#[test]
fn grouped_total_matches_zstd() {
    let scratch = Scratch::new("group");
    let a = make_frame(&scratch, "a", &payload());
    let b = make_frame(&scratch, "b", &payload()[..4096]);
    let out = run(&["-l", a.to_str().unwrap(), b.to_str().unwrap()]);
    assert!(out.status.success(), "{out:?}");
    let stdout = stdout_of(&out);
    assert_eq!(
        stdout.lines().nth(3),
        Some(format!("{} ", "-".repeat(65)).as_str())
    );
    assert!(stdout.contains(" 2 files"), "{stdout}");
    assert_parity(&["-l"], &[a.clone(), b.clone()]);
    // -v has no table and no total.
    let out = run(&["-lv", a.to_str().unwrap(), b.to_str().unwrap()]);
    assert!(!stdout_of(&out).contains("files"), "{out:?}");
    assert_parity(&["-lv"], &[a, b]);
}

/// A dictID-carrying frame shows its id under -v (reference fixture from
/// the system zstd; skipped where it is absent).
#[test]
fn dict_id_frame_matches_zstd() {
    if !system_zstd() {
        return;
    }
    let scratch = Scratch::new("dictid");
    let mut samples = Vec::new();
    for i in 0..24u32 {
        let mut sample = Vec::new();
        for _ in 0..6 {
            sample.extend_from_slice(b"[Unit]\nDescription=shared boilerplate\n");
            sample.extend((0..48u32).map(|j| b'a' + (i % 16) as u8 + (j % 5) as u8));
        }
        samples.push(sample);
    }
    let sample_dir = scratch.path("samples");
    fs::create_dir(&sample_dir).unwrap();
    for (i, sample) in samples.iter().enumerate() {
        fs::write(sample_dir.join(format!("s{i}")), sample).unwrap();
    }
    let dict = scratch.path("ref.dict");
    let status = Command::new("zstd")
        .args(["--train", sample_dir.to_str().unwrap(), "-r"])
        .arg("-o")
        .arg(dict.to_str().unwrap())
        .arg("--maxdict=4096")
        .status()
        .unwrap();
    assert!(status.success());
    let target = scratch.path("holdout.bin");
    fs::write(&target, &samples[0]).unwrap();
    let frame = scratch.path("holdout.zst");
    let status = Command::new("zstd")
        .args(["-q", "-f", "-D", dict.to_str().unwrap()])
        .arg(target.to_str().unwrap())
        .arg("-o")
        .arg(frame.to_str().unwrap())
        .status()
        .unwrap();
    assert!(status.success());

    let out = run(&["-lv", frame.to_str().unwrap()]);
    assert!(out.status.success(), "{out:?}");
    assert!(stdout_of(&out).contains("DictID: "), "{}", stdout_of(&out));
    assert_parity(&["-lv"], &[frame]);
}

/// Error files: a non-zstd file, an empty file, and a truncated frame each
/// take zstd's path (message on stdout, no row, exit 1), while a truncated
/// block walk still shows the partial row.
#[test]
fn error_files_match_zstd() {
    let scratch = Scratch::new("errors");
    let frame = make_frame(&scratch, "data", &payload());

    let plain = scratch.path("plain.bin");
    fs::write(&plain, b"certainly not a zstd frame").unwrap();
    let out = run(&["-l", plain.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(
        stdout_of(&out).contains(&format!(
            "File \"{}\" not compressed by zstd ",
            plain.display()
        )),
        "{out:?}"
    );

    let empty = scratch.path("empty.zst");
    fs::write(&empty, b"").unwrap();
    let out = run(&["-l", empty.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(
        stdout_of(&out).contains("not compressed by zstd"),
        "{out:?}"
    );

    let mut truncated_bytes = fs::read(&frame).unwrap();
    truncated_bytes.truncate(truncated_bytes.len() / 2);
    let truncated = scratch.path("truncated.zst");
    fs::write(&truncated, &truncated_bytes).unwrap();
    let out = run(&["-l", truncated.to_str().unwrap()]);
    assert!(!out.status.success());
    assert_eq!(stdout_of(&out).lines().count(), 2, "{out:?}");
    assert!(stderr_of(&out).contains("Error while parsing"), "{out:?}");

    assert_parity(&["-l"], &[plain]);
    assert_parity(&["-l"], &[empty]);
    assert_parity(&["-l"], std::slice::from_ref(&truncated));
    assert_parity(&["-lv"], std::slice::from_ref(&truncated));
    // a complete frame plus a stray tail shorter than a minimal frame
    // header reports the incomplete-frame form of not-zstd
    let mut tailed = fs::read(&frame).unwrap();
    tailed.extend_from_slice(b"xyz");
    let tail = scratch.path("tail.zst");
    fs::write(&tail, &tailed).unwrap();
    let out = run(&["-l", tail.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("reached end of file with incomplete frame"),
        "{out:?}"
    );
    assert_parity(&["-l"], &[tail]);
}

/// A skippable frame running past EOF reports the truncated file, with no
/// row, like zstd.
#[test]
fn truncated_skippable_frame() {
    let scratch = Scratch::new("truncskip");
    // The size field promises 32 payload bytes; the file stops 4 short.
    let payload = skippable_frame(b"0123456789abcdef0123456789abcdef");
    let frame = scratch.path("cut.zst");
    fs::write(&frame, &payload[..payload.len() - 4]).unwrap();
    let out = run(&["-l", frame.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(
        stdout_of(&out).contains(&format!("File \"{}\" is truncated ", frame.display())),
        "{out:?}"
    );
    assert_parity(&["-l"], &[frame]);
}

/// Missing files report zstd's "is not a file" and fail without a row.
#[test]
fn missing_file_fails() {
    let scratch = Scratch::new("missing");
    let absent = scratch.path("nosuch.zst");
    let out = run(&["-l", absent.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("is not a file"), "{out:?}");
    assert_eq!(stdout_of(&out).lines().count(), 1, "{out:?}");
}

/// Stdin is not listable, with or without other files.
#[test]
fn stdin_is_rejected() {
    let out = run(&["-l"]);
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("standard input"), "{out:?}");
    assert!(stderr_of(&out).contains("No files given"), "{out:?}");

    let out = run(&["-l", "-"]);
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("standard input"), "{out:?}");
}

/// -l refuses the (de)compression modes, outputs, and --train.
#[test]
fn mode_conflicts() {
    let scratch = Scratch::new("conflicts");
    let frame = make_frame(&scratch, "data", &payload());
    let name = frame.to_str().unwrap();
    for args in [
        vec!["-l", "-d", name],
        vec!["-d", "-l", name],
        vec!["-l", "-z", name],
        vec!["-l", "-t", name],
        vec!["-l", "--train"],
        vec!["-l", "-o", "out", name],
    ] {
        let out = run(&args);
        assert!(!out.status.success(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("cannot be used with"),
            "{args:?}"
        );
    }
    // --maxdict is a --train knob.
    let out = run(&["-l", "--maxdict", "1K", name]);
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("only applies with --train"),
        "{out:?}"
    );
}

/// -q keeps the table (zstd prints rows unconditionally) and -qq still
/// hides nothing on stdout.
#[test]
fn quiet_still_prints_rows() {
    let scratch = Scratch::new("quiet");
    let frame = make_frame(&scratch, "data", &payload());
    for q in [["-l", "-q"], ["-l", "-qq"]] {
        let out = run(&[q[0], q[1], frame.to_str().unwrap()]);
        assert!(out.status.success(), "{out:?}");
        assert_eq!(stdout_of(&out).lines().count(), 2, "{out:?}");
    }
    // the incomplete-tail note is a DISPLAYLEVEL(1) detail: shown, then
    // hidden by -q
    let tail = scratch.path("tail.bin");
    fs::write(&tail, b"abc").unwrap();
    let out = run(&["-l", tail.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("reached end of file"), "{out:?}");
    let out = run(&["-l", "-q", tail.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(!stderr_of(&out).contains("reached end of file"), "{out:?}");
}

/// --filelist feeds -l like positional files.
#[test]
fn filelist_lists() {
    let scratch = Scratch::new("filelist");
    let a = make_frame(&scratch, "a", &payload());
    let b = make_frame(&scratch, "b", &payload()[..4096]);
    let list = scratch.path("list.txt");
    fs::write(&list, format!("{}\n{}\n", a.display(), b.display())).unwrap();
    let out = run(&["-l", "--filelist", list.to_str().unwrap()]);
    assert!(out.status.success(), "{out:?}");
    assert!(stdout_of(&out).contains("2 files"), "{out:?}");
}

/// RLE blocks span one byte in the block walk (a frame full of them must
/// list, and match the reference byte for byte).
#[test]
fn rle_block_frame_lists() {
    let scratch = Scratch::new("rle");
    let frame = make_frame(&scratch, "zeros", &vec![0u8; 300 * 1024]);
    let out = run(&["-l", frame.to_str().unwrap()]);
    assert!(out.status.success(), "{out:?}");
    assert_parity(&["-l"], &[frame]);
}
