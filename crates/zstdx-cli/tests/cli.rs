//! End-to-end tests of the zstd-compatible CLI surface: default file naming,
//! overwrite protection, stdin/stdout streaming, level flags, `--rm` and
//! exit codes.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const BIN: &str = env!("CARGO_BIN_EXE_zstdx");

/// A scratch directory that removes itself on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(test: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "zstdx-cli-test-{test}-{}-{}",
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

/// Pipe `input` through the CLI, returning the captured stdout.
fn run_piped(args: &[&str], input: &[u8]) -> (std::process::Output, Vec<u8>) {
    let mut child = Command::new(BIN)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A child may exit early on purpose (a rejected flag, a pledge
    // mismatch): feeding it the rest of `input` then hits EPIPE, which is
    // the child decision to report, not a harness failure.
    if let Err(err) = child.stdin.take().unwrap().write_all(input) {
        assert!(
            err.kind() == std::io::ErrorKind::BrokenPipe,
            "feeding {args:?}: {err}"
        );
    }
    let output = child.wait_with_output().unwrap();
    (output.clone(), output.stdout)
}

fn payload() -> Vec<u8> {
    // Repetitive enough to compress well, long enough to span blocks.
    let mut data = Vec::new();
    for i in 0..10_000u32 {
        data.extend_from_slice(format!("line {i}: the quick brown fox\n").as_bytes());
    }
    data
}

#[test]
fn file_roundtrip_default_names() {
    let scratch = Scratch::new("roundtrip");
    let input = scratch.path("data.bin");
    fs::write(&input, payload()).unwrap();

    // `zstdx data.bin` writes data.bin.zst and keeps the input.
    let output = run(&[input.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");
    let compressed = scratch.path("data.bin.zst");
    assert!(compressed.exists());
    assert!(input.exists());

    // Decompression refuses to overwrite the existing input without -f.
    let output = run(&["-d", compressed.to_str().unwrap()]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("already exists"), "{stderr}");

    fs::remove_file(&input).unwrap();
    let output = run(&["-d", compressed.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fs::read(&input).unwrap(), payload());
}

#[test]
fn force_overwrites() {
    let scratch = Scratch::new("force");
    let input = scratch.path("data.bin");
    fs::write(&input, payload()).unwrap();
    run(&[input.to_str().unwrap()]);
    // Second run must fail without -f and succeed with it.
    assert!(!run(&[input.to_str().unwrap()]).status.success());
    assert!(run(&["-f", input.to_str().unwrap()]).status.success());
}

/// The overwrite check guards `-o` targets fed from stdin too, not only
/// file-to-file runs.
#[test]
fn stdin_output_refuses_overwrite() {
    let scratch = Scratch::new("stdin-overwrite");
    let out = scratch.path("out.zst");
    fs::write(&out, b"existing content").unwrap();

    // No tty to answer the prompt on, so the existing file must stay.
    let (output, _) = run_piped(&["-o", out.to_str().unwrap()], b"piped payload");
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("already exists"), "{stderr}");
    assert_eq!(fs::read(&out).unwrap(), b"existing content");

    // With -f the overwrite proceeds.
    let (output, _) = run_piped(&["-f", "-o", out.to_str().unwrap()], b"piped payload");
    assert!(output.status.success(), "{output:?}");
    assert!(
        fs::read(&out)
            .unwrap()
            .starts_with(&[0x28, 0xb5, 0x2f, 0xfd])
    );
}

#[test]
fn stdin_stdout_streaming() {
    let data = payload();
    let (output, compressed) = run_piped(&["-q"], &data);
    assert!(output.status.success(), "{output:?}");
    assert!(compressed.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]));

    let (output, decompressed) = run_piped(&["-d", "-q"], &compressed);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(decompressed, data);
}

#[test]
fn explicit_stdout_flag() {
    let scratch = Scratch::new("stdout");
    let input = scratch.path("data.bin");
    fs::write(&input, payload()).unwrap();

    let output = Command::new(BIN)
        .args(["-c", input.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]));
    // -c keeps the input and does not create the default output file.
    assert!(input.exists());
    assert!(!scratch.path("data.bin.zst").exists());
}

#[test]
fn level_flags_and_fast() {
    let scratch = Scratch::new("levels");
    for (name, args) in [
        ("l19", vec!["-19"]),
        ("l1", vec!["-1"]),
        ("fast", vec!["--fast"]),
        ("fast3", vec!["--fast=3"]),
    ] {
        let input = scratch.path(format!("{name}.bin"));
        fs::write(&input, payload()).unwrap();
        let full_args: Vec<&str> = args
            .iter()
            .copied()
            .chain([input.to_str().unwrap()])
            .collect();
        let output = run(&full_args);
        assert!(output.status.success(), "{name}: {output:?}");
        assert!(scratch.path(format!("{name}.bin.zst")).exists());
    }
    // Level above 19 without --ultra warns and clamps, still succeeds.
    let input = scratch.path("ultra.bin");
    fs::write(&input, payload()).unwrap();
    let output = run(&["-22", input.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("reduced to 19"));
}

#[test]
fn decompress_requires_zst_suffix() {
    let scratch = Scratch::new("suffix");
    let input = scratch.path("data.bin");
    fs::write(&input, payload()).unwrap();
    let output = run(&["-d", input.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown suffix"));
}

#[test]
fn rm_removes_input() {
    let scratch = Scratch::new("rm");
    let input = scratch.path("data.bin");
    fs::write(&input, payload()).unwrap();
    let output = run(&["--rm", input.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");
    assert!(!input.exists());
    assert!(scratch.path("data.bin.zst").exists());
}

/// A failure after the output file was created must not leave the partial
/// output on disk.
#[test]
fn failed_decompress_removes_partial_output() {
    let scratch = Scratch::new("partial");
    let input = scratch.path("data.bin");
    fs::write(&input, payload()).unwrap();
    run(&[input.to_str().unwrap()]);
    let mut compressed = fs::read(scratch.path("data.bin.zst")).unwrap();
    // cut the frame tail: the header still parses, decoding fails mid-frame
    compressed.truncate(compressed.len() - 8);
    let corrupt = scratch.path("corrupt.zst");
    fs::write(&corrupt, &compressed).unwrap();

    let out = scratch.path("restored.bin");
    let output = run(&["-d", corrupt.to_str().unwrap(), "-o", out.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(!out.exists(), "partial output must be removed");
}

#[test]
fn test_mode() {
    let scratch = Scratch::new("test");
    let input = scratch.path("data.bin");
    fs::write(&input, payload()).unwrap();
    run(&[input.to_str().unwrap()]);
    let compressed = scratch.path("data.bin.zst");
    assert!(run(&["-t", compressed.to_str().unwrap()]).status.success());

    let garbage = scratch.path("garbage.zst");
    fs::write(&garbage, b"not a zstd frame at all").unwrap();
    assert!(!run(&["-t", garbage.to_str().unwrap()]).status.success());
}

/// `-t` must report the number of verified bytes, not a constant zero.
#[test]
fn test_mode_reports_decoded_bytes() {
    let scratch = Scratch::new("test-stats");
    let input = scratch.path("data.bin");
    fs::write(&input, payload()).unwrap();
    run(&[input.to_str().unwrap()]);

    let output = run(&["-t", scratch.path("data.bin.zst").to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("tested ok"), "{stderr}");
    assert!(!stderr.contains("0B tested ok"), "{stderr}");
}

#[test]
fn directory_requires_recursive() {
    let scratch = Scratch::new("recursive");
    let dir = scratch.path("dir");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("a.bin"), payload()).unwrap();

    let output = run(&[dir.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("is a directory"));

    let output = run(&["-r", dir.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");
    assert!(dir.join("a.bin.zst").exists());
}

#[test]
fn threads_flag() {
    let scratch = Scratch::new("threads");
    let input = scratch.path("data.bin");
    fs::write(&input, payload()).unwrap();
    let output = run(&["-T0", input.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");
    fs::remove_file(&input).unwrap();
    let output = run(&["-d", "-T2", scratch.path("data.bin.zst").to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fs::read(&input).unwrap(), payload());
}

/// `--long` forces the window log: an unpledged stdin stream's frame
/// header carries the requested exponent, matching zstd's byte for byte.
#[test]
fn long_sets_window_log() {
    let data = payload();
    let (_, frame) = run_piped(&["-q", "--long=20"], &data);
    assert_eq!(&frame[..4], &[0x28, 0xb5, 0x2f, 0xfd], "zstd magic");
    assert_eq!(frame[4], 0x04, "FHD: checksum flag only, no FCS");
    assert_eq!(frame[5], 10 << 3, "window descriptor: 2^20 window");

    let (_, frame) = run_piped(&["-q", "--no-check", "--long=20"], &data);
    assert_eq!(frame[4], 0x00, "--no-check clears the checksum flag");

    // bare --long defaults to 27
    let (_, frame) = run_piped(&["-q", "--long"], &data);
    assert_eq!(frame[5], 17 << 3, "bare --long: 2^27 window");

    // below the format's minimum: rejected
    let (output, _) = run_piped(&["-q", "--long=9"], &data);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--long"));

    // above the engine's ceiling: warned and clamped to 27
    let (output, frame) = run_piped(&["--long=28"], &data);
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("reduced to 27"));
    assert_eq!(frame[5], 17 << 3);

    // a compression option must not ride a decode run
    let (output, _) = run_piped(&["-d", "-q", "--long=20"], &data);
    assert!(!output.status.success());
}

/// `--[no-]check` toggles the frame checksum: the trailer comes and goes,
/// both forms roundtrip, and decode mode rejects the flag loudly (zstd
/// silently reinterprets it there; the engine always validates).
#[test]
fn check_flags_toggle_checksum() {
    let data = payload();
    let (_, checked) = run_piped(&["-q"], &data);
    let (_, unchecked) = run_piped(&["-q", "--no-check"], &data);
    assert_eq!(checked[4] & 0x04, 0x04, "default writes a checksum");
    assert_eq!(unchecked[4] & 0x04, 0x00, "--no-check omits it");
    assert_eq!(checked.len(), unchecked.len() + 4, "checksum trailer size");

    let (output, out) = run_piped(&["-d", "-q"], &unchecked);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(out, data);

    let (output, _) = run_piped(&["-d", "-q", "--check"], &checked);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("compression only"));
}

/// `--stream-size` pledges the stdin stream: the promise lands in the
/// header, a mismatch fails the run (zstd: "Src size is incorrect"), and
/// file inputs are refused instead of silently ignored (zstd ignores).
#[test]
fn stream_size_pledges_and_enforces() {
    let data = payload();
    let exact = &data[..1000];
    let (output, frame) = run_piped(&["-q", "--stream-size=1000"], exact);
    assert!(output.status.success(), "{output:?}");
    // single segment + 2-byte FCS (value - 256) + checksum
    assert_eq!(frame[4], 0x64, "FHD: single segment, FCS class 1, checksum");
    assert_eq!(&frame[5..7], &[0xe8, 0x02], "FCS: 1000 - 256, LE");

    let (output, _) = run_piped(&["-q", "--stream-size=1000"], &data);
    assert!(!output.status.success(), "mismatching input must fail");
    assert!(String::from_utf8_lossy(&output.stderr).contains("pledged"));

    let scratch = Scratch::new("stream-size-file");
    let input = scratch.path("data.bin");
    fs::write(&input, &data).unwrap();
    let output = run(&["--stream-size=1000", input.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("only applies to stdin"));
}

/// `--size-hint` sizes the encoder row without promising anything: the
/// window shrinks to the hinted class and the header stays pledge-free.
#[test]
fn size_hint_sizes_the_window() {
    let data = payload();
    let (output, frame) = run_piped(&["-q", "--size-hint=4096"], &data);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(frame[4], 0x04, "FHD unchanged: a hint never pledges");
    assert_eq!(frame[5], 2 << 3, "window clamps to the hinted 4K class");

    let (output, out) = run_piped(&["-d", "-q"], &frame);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(out, data, "a hint never affects content");

    let scratch = Scratch::new("size-hint-file");
    let input = scratch.path("data.bin");
    fs::write(&input, &data).unwrap();
    let output = run(&["--size-hint=4096", input.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("only applies to stdin"));
}

/// `-M` bounds the decode window (the dominant decode memory): a cap under
/// the frame's window fails, a sufficient one decodes, compression rejects
/// the flag (zstd silently ignores it there), and sub-minimum values are
/// rejected like zstd's out-of-bound parameter error.
#[test]
fn memory_caps_decode_window() {
    let data = payload();
    // an unpledged stdin stream keeps the forced 128 MiB window
    let (_, big) = run_piped(&["-q", "--long=27"], &data);

    let (output, _) = run_piped(&["-d", "-q", "-M", "64M"], &big);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("window_size is too big"), "{stderr}");

    let (output, out) = run_piped(&["-d", "-q", "-M", "128M"], &big);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(out, data);

    let (output, _) = run_piped(&["-d", "-q", "-M", "512"], &big);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("below"));

    let scratch = Scratch::new("memory-compress");
    let input = scratch.path("data.bin");
    fs::write(&input, &data).unwrap();
    let output = run(&["-q", "-M", "1M", input.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("bounds decoding"));
}

/// `--filelist` (alias `--file`) reads the input list from a file, and `-`
/// reads the list itself from stdin.
#[test]
fn filelist_reads_input_list() {
    let scratch = Scratch::new("filelist");
    let a = scratch.path("a.bin");
    let b = scratch.path("b.bin");
    fs::write(&a, payload()).unwrap();
    fs::write(&b, payload()).unwrap();
    let list = scratch.path("list.txt");
    fs::write(&list, format!("{}\n{}\n", a.display(), b.display())).unwrap();

    let output = run(&["-q", "-f", "--filelist", list.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");
    assert!(scratch.path("a.bin.zst").exists());
    assert!(scratch.path("b.bin.zst").exists());

    fs::remove_file(scratch.path("a.bin.zst")).unwrap();
    fs::remove_file(scratch.path("b.bin.zst")).unwrap();
    let output = run(&["-q", "-f", "--file", list.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");
    assert!(scratch.path("a.bin.zst").exists());

    fs::remove_file(scratch.path("a.bin.zst")).unwrap();
    fs::remove_file(scratch.path("b.bin.zst")).unwrap();
    let mut contents = fs::read(&list).unwrap();
    contents.push(b'\n');
    let (output, _) = run_piped(
        &[
            "-q",
            "-f",
            "--filelist",
            "-",
            "-o",
            scratch.path("out.zst").to_str().unwrap(),
        ],
        &contents,
    );
    assert!(!output.status.success(), "-o refuses multiple inputs");
    assert!(
        !scratch.path("out.zst").exists(),
        "refused run leaves no output"
    );
    let (output, _) = run_piped(&["-q", "-f", "--filelist=-"], &contents);
    assert!(output.status.success(), "{output:?}");
    // `--filelist=-` consumes stdin as the list; the listed files compress.
    assert!(scratch.path("b.bin.zst").exists(), "second entry processed");
}

/// `--single-thread` is the `-T1` alias; combining it with `-T` is a
/// conflict, not a silent last-wins.
#[test]
fn single_thread_alias() {
    let scratch = Scratch::new("single-thread");
    let input = scratch.path("data.bin");
    fs::write(&input, payload()).unwrap();
    assert!(
        run(&["-q", "--single-thread", input.to_str().unwrap()])
            .status
            .success()
    );
    let output = run(&["-q", "--single-thread", "-T2", input.to_str().unwrap()]);
    assert!(!output.status.success());
}

/// `--ignore-errors` keeps per-input failures out of the exit code; the
/// failures are still reported.
#[test]
fn ignore_errors_clears_exit_code() {
    let scratch = Scratch::new("ignore-errors");
    let dir = scratch.path("dir");
    fs::create_dir(&dir).unwrap();
    let good = dir.join("good");
    fs::write(&good, payload()).unwrap();
    assert!(run(&["-q", "-f", good.to_str().unwrap()]).status.success());
    fs::remove_file(&good).unwrap();
    fs::write(dir.join("bad.zst"), b"not a zstd frame").unwrap();

    let output = run(&["-d", "-q", "-r", dir.to_str().unwrap()]);
    assert!(!output.status.success(), "the corrupt frame fails the run");
    assert!(String::from_utf8_lossy(&output.stderr).contains("bad.zst"));

    let output = run(&["-d", "-q", "-r", "--ignore-errors", dir.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");
    assert!(dir.join("good").exists(), "the healthy frame still decodes");
}

/// `--[no-]sparse` parse but fail loudly: the writer always emits dense
/// output, and saying so beats a generic unknown-argument error.
#[test]
fn sparse_is_rejected_loudly() {
    let scratch = Scratch::new("sparse");
    let input = scratch.path("data.bin");
    fs::write(&input, payload()).unwrap();
    for flag in ["--sparse", "--no-sparse"] {
        let output = run(&[flag, input.to_str().unwrap()]);
        assert!(!output.status.success(), "{flag} must be rejected");
        assert!(String::from_utf8_lossy(&output.stderr).contains("not supported"));
        assert!(!scratch.path("data.bin.zst").exists());
    }
}

/// `-V`/`--version` prints the crate version plus the compat note.
#[test]
fn version_prints_compat_note() {
    for flag in ["-V", "--version"] {
        let output = run(&[flag]);
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains(env!("CARGO_PKG_VERSION")), "{stdout}");
        assert!(stdout.contains("zstd v1.5.7"), "{stdout}");
    }
}

/// Everything after `--` is a file argument, never a flag: `-3` is a
/// (missing) file, not a compression level.
#[test]
fn end_of_flags_treats_rest_as_files() {
    let output = run(&["--", "-3"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("-3"), "{stderr}");
    assert!(!stderr.contains("level"), "{stderr}");
}

/// The progress override flags conflict; `--no-progress` still compresses.
#[test]
fn progress_flags() {
    let scratch = Scratch::new("progress");
    let input = scratch.path("data.bin");
    fs::write(&input, payload()).unwrap();
    assert!(
        run(&["-q", "--no-progress", input.to_str().unwrap()])
            .status
            .success()
    );
    let output = run(&["--progress", "--no-progress", input.to_str().unwrap()]);
    assert!(!output.status.success());
}
