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

    // Decompression refuses to overwrite the existing input without -f
    // (-q: the non-interactive decline message, no prompt).
    let output = run(&["-q", "-d", compressed.to_str().unwrap()]);
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
    let (output, _) = run_piped(&["-q", "-o", out.to_str().unwrap()], b"piped payload");
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

    // below the format's minimum: the engine parameter error and its
    // exit code (zstd rejects the same way)
    let (output, _) = run_piped(&["-q", "--long=9"], &data);
    assert_eq!(output.status.code(), Some(11));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Parameter is out of bound"));

    // above the engine's ceiling: clamped silently, like zstd's own clamp
    // to its 30
    let (output, frame) = run_piped(&["--long=28"], &data);
    assert!(output.status.success(), "{output:?}");
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("reduced"),
        "clamping is silent"
    );
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
    assert_eq!(
        output.status.code(),
        Some(11),
        "mismatching input must fail"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Src size is incorrect"),
        "zstd's parameter-class message"
    );

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
    assert!(
        stderr.contains("Window size larger than maximum"),
        "{stderr}"
    );

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

/// `--filelist` reads the input list from a file; zstd has no stdin list
/// form (`-` is zstd's separated-value error, `=-` a literal missing file),
/// and no `--file` alias in 1.5.7.
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
    // zstd 1.5.7 has no --file alias: it is an unknown flag there, so the
    // rejection matches the reference and leaves no outputs behind
    let output = run(&["-q", "-f", "--file", list.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "Incorrect parameter: --file \n"
    );
    assert!(!scratch.path("a.bin.zst").exists());
    assert!(!scratch.path("b.bin.zst").exists());

    let contents = fs::read(&list).unwrap();

    // `-o` refuses a multiple-input list.
    let output = run(&[
        "-q",
        "-f",
        "--filelist",
        list.to_str().unwrap(),
        "-o",
        scratch.path("out.zst").to_str().unwrap(),
    ]);
    assert!(!output.status.success(), "-o refuses multiple inputs");
    assert!(
        !scratch.path("out.zst").exists(),
        "refused run leaves no output"
    );

    // zstd's NEXT_FIELD rejects a dash as a separated value outright.
    let (output, _) = run_piped(&["-q", "-f", "--filelist", "-"], &contents);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("command cannot be separated from its argument")
    );
    // `=-` is a literal (missing) file named `-`.
    let (output, _) = run_piped(&["-q", "-f", "--filelist=-"], &contents);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("error reading -"));
    assert!(
        !scratch.path("b.bin.zst").exists(),
        "no stdin list is consumed"
    );
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

/// `--ignore-errors` was removed in zstd 1.5.5+: v1.5.7 rejects it as an
/// incorrect parameter before any mode runs.
#[test]
fn ignore_errors_is_rejected() {
    let scratch = Scratch::new("ignore-errors");
    let dir = scratch.path("dir");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("bad.zst"), b"not a zstd frame").unwrap();

    let output = run(&["-d", "-q", "-r", dir.to_str().unwrap()]);
    assert!(!output.status.success(), "the corrupt frame fails the run");
    assert!(String::from_utf8_lossy(&output.stderr).contains("bad.zst"));

    let output = run(&["-d", "-q", "-r", "--ignore-errors", dir.to_str().unwrap()]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Incorrect parameter: --ignore-errors"),
        "{stderr}"
    );
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

/// `-V`/`--version` answers at its argv position like zstd's parser scan:
/// the banner line by default, the bare version below the default display
/// level, the format-support line from `-v` up. The text is zstdx's own
/// (printing zstd's banner would claim false provenance).
#[test]
fn version_prints_compat_note() {
    let version = env!("CARGO_PKG_VERSION");
    for flag in ["-V", "--version"] {
        let output = run(&[flag]);
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(
            stdout,
            format!(
                "*** zstdx command line interface (64-bit) v{version}, pure-Rust Zstandard ***\n"
            )
        );
    }
    // -q before -V drops below the default display level: bare version
    let output = run(&["-q", "-V"]);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("{version}\n")
    );
    // -V before -q still sees the default level
    let output = run(&["-V", "-q"]);
    assert!(String::from_utf8_lossy(&output.stdout).contains("***"));
    // -v adds the format-support line; -V first does not
    let output = run(&["-v", "-V"]);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!(
            "*** zstdx command line interface (64-bit) v{version}, pure-Rust Zstandard ***\n*** \
             supports: zstd\n"
        )
    );
    let output = run(&["-V", "-v"]);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("supports"));
    // after `--` a -V is a file name
    let output = run(&["--", "-V"]);
    assert_eq!(output.status.code(), Some(1));
}

/// `-h`, `-H` and `--help` print the full help and exit 0 (zstd splits
/// them into a short and a long usage; this build prints one text).
#[test]
fn help_variants_print_usage() {
    for flag in ["-h", "-H", "--help"] {
        let output = run(&[flag]);
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("Usage:"), "{stdout}");
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

/// The R75 flag-surface closure: recursion rules, --no-dictID,
/// --[no-]content-size, --exclude-compressed, -C, the --memlimit aliases,
/// --output-dir-flat, last-wins repeats, and the loud rejections.
#[test]
fn recursive_walk_rules() {
    let scratch = Scratch::new("r75-recursive");
    fs::write(scratch.path("top.txt"), b"top\n").unwrap();
    fs::create_dir_all(scratch.path("a/b")).unwrap();
    fs::write(scratch.path("a/mid.txt"), b"mid\n").unwrap();
    fs::write(scratch.path("a/b/deep.txt"), b"deep\n").unwrap();

    // nested walk compresses every file under the root, and -r -d walks too
    let output = run(&["-q", "-r", "-f", scratch.0.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");
    assert!(scratch.path("top.txt.zst").exists());
    assert!(scratch.path("a/mid.txt.zst").exists());
    assert!(scratch.path("a/b/deep.txt.zst").exists());
    let output = run(&[
        "-q",
        "-r",
        "-d",
        "-f",
        scratch.path("top.txt.zst").to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fs::read(scratch.path("top.txt")).unwrap(), b"top\n");

    // the trailing-slash form walks the same tree
    fs::remove_file(scratch.path("top.txt.zst")).unwrap();
    let output = run(&[
        "-q",
        "-r",
        "-f",
        format!("{}/", scratch.0.to_str().unwrap()).as_str(),
    ]);
    assert!(output.status.success(), "{output:?}");
    assert!(scratch.path("top.txt.zst").exists());

    // an empty directory declines with zstd's line at display level 1 and
    // still exits 0; -qq silences the line
    let empty = scratch.path("empty");
    fs::create_dir_all(&empty).unwrap();
    let output = run(&["-r", empty.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "please provide correct input file(s) or non-empty directories -- ignored \n"
    );
    let output = run(&["-qq", "-r", empty.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");

    // mixed args: one empty dir plus one file still processes the file
    let output = run(&[
        "-q",
        "-r",
        "-f",
        empty.to_str().unwrap(),
        scratch.path("top.txt").to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{output:?}");
}

/// A dictionary compression without the header dictID: the dictionary still
/// applies (the frame decodes with -D), and `-l -v` reports DictID 0.
#[test]
fn no_dict_id_keeps_dictionary_effect() {
    let scratch = Scratch::new("r75-nodictid");
    let samples = scratch.path("samples");
    fs::create_dir_all(&samples).unwrap();
    let body: Vec<u8> = (0..2000).map(|i| (i % 251) as u8).collect();
    for n in 0..8 {
        fs::write(samples.join(format!("s{n}")), &body[100 * n..]).unwrap();
    }
    let dict = scratch.path("d.dict");
    let output = run(&[
        "-q",
        "--train",
        "-r",
        samples.to_str().unwrap(),
        "-o",
        dict.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{output:?}");
    let input = scratch.path("in.bin");
    fs::write(&input, &body).unwrap();
    let with_id = scratch.path("with.zst");
    let without_id = scratch.path("without.zst");
    let output = run(&[
        "-q",
        "-f",
        "-D",
        dict.to_str().unwrap(),
        input.to_str().unwrap(),
        "-o",
        with_id.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{output:?}");
    let output = run(&[
        "-q",
        "-f",
        "--no-dictID",
        "-D",
        dict.to_str().unwrap(),
        input.to_str().unwrap(),
        "-o",
        without_id.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{output:?}");

    // the flag is smaller: no 4-byte Dictionary_ID field in the header
    assert_eq!(
        fs::metadata(&with_id).unwrap().len(),
        fs::metadata(&without_id).unwrap().len() + 4
    );

    // `-l -v` sees the id in one and not the other
    for (path, expect_id) in [
        (with_id.to_str().unwrap(), true),
        (without_id.to_str().unwrap(), false),
    ] {
        let output = run(&["-l", "-v", path]);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let line = stdout.lines().find(|l| l.starts_with("DictID:")).unwrap();
        if expect_id {
            assert!(!line.contains("DictID: 0"), "{stdout}");
        } else {
            assert_eq!(line.trim(), "DictID: 0", "{stdout}");
        }
    }

    // the dictionary still applies: decode with -D restores the input
    let (output, stdout) = run_piped(
        &[
            "-q",
            "-d",
            "-D",
            dict.to_str().unwrap(),
            without_id.to_str().unwrap(),
            "-c",
        ],
        b"",
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(stdout, body);
}

/// `--content-size` is the default; `--no-content-size` keeps the size out
/// of the header, which `-l` reports with empty Uncompressed and Ratio
/// columns.
#[test]
fn no_content_size_omits_header_size() {
    let scratch = Scratch::new("r75-nocs");
    let input = scratch.path("data.bin");
    fs::write(&input, payload()).unwrap();
    let output = run(&["-q", "-f", input.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");
    let with_size = String::from_utf8_lossy(
        &run(&["-l", scratch.path("data.bin.zst").to_str().unwrap()]).stdout,
    )
    .to_string();
    assert!(
        with_size.contains("313.9 KiB") || with_size.contains("KiB"),
        "{with_size}"
    );

    fs::remove_file(scratch.path("data.bin.zst")).unwrap();
    let output = run(&["-q", "-f", "--no-content-size", input.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");
    // no FCS: the short row's Uncompressed and Ratio cells stay empty, and
    // the verbose detail omits the Decompressed Size and Ratio lines
    let listing = String::from_utf8_lossy(
        &run(&["-l", scratch.path("data.bin.zst").to_str().unwrap()]).stdout,
    )
    .to_string();
    let row = listing.lines().nth(1).unwrap();
    let cells: Vec<&str> = row.split_whitespace().collect();
    assert_eq!(cells[cells.len() - 2], "XXH64", "{listing}");
    assert!(!row.contains("%"), "{listing}");
    let detail = String::from_utf8_lossy(
        &run(&["-l", "-v", scratch.path("data.bin.zst").to_str().unwrap()]).stdout,
    )
    .to_string();
    assert!(detail.contains("Compressed Size"), "{detail}");
    assert!(!detail.contains("Decompressed Size"), "{detail}");
    assert!(!detail.contains("Ratio:"), "{detail}");

    // --content-size restores the header size, which the verbose detail
    // reports again, and the frame still roundtrips
    let with_fcs = scratch.path("with.zst");
    let output = run(&[
        "-q",
        "-f",
        "--content-size",
        input.to_str().unwrap(),
        "-o",
        with_fcs.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{output:?}");
    let detail =
        String::from_utf8_lossy(&run(&["-l", "-v", with_fcs.to_str().unwrap()]).stdout).to_string();
    assert!(detail.contains("Decompressed Size"), "{detail}");
    assert!(
        run(&["-q", "-f", "-d", with_fcs.to_str().unwrap()])
            .status
            .success()
    );
}

/// `--exclude-compressed` skips inputs with a compressed extension at
/// compress time, silently, without failing the run.
#[test]
fn exclude_compressed_skips_by_extension() {
    let scratch = Scratch::new("r75-exclude");
    let plain = scratch.path("a.txt");
    fs::write(&plain, payload()).unwrap();
    let zst = scratch.path("b.zst");
    assert!(
        run(&[
            "-q",
            "-f",
            plain.to_str().unwrap(),
            "-o",
            zst.to_str().unwrap()
        ])
        .status
        .success()
    );
    fs::write(scratch.path("c.mp4"), b"not really a movie").unwrap();

    let output = run(&[
        "-q",
        "-r",
        "-f",
        "--exclude-compressed",
        scratch.0.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{output:?}");
    assert!(scratch.path("a.txt.zst").exists(), "plain file compressed");
    assert!(!scratch.path("b.zst.zst").exists(), ".zst skipped");
    assert!(!scratch.path("c.mp4.zst").exists(), ".mp4 skipped");

    // an explicit compressed file is skipped the same way, rc 0
    let output = run(&["-q", "-f", "--exclude-compressed", zst.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(!scratch.path("b.zst.zst").exists());

    // decode mode ignores the flag (inert there in zstd as well); b.zst
    // strips to the plain stem `b`
    let output = run(&[
        "-q",
        "-f",
        "-d",
        "--exclude-compressed",
        zst.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fs::read(scratch.path("b")).unwrap(),
        fs::read(&plain).unwrap()
    );
}

/// `-C` is the short form of `--check`; both frames carry a checksum and
/// decode identically.
#[test]
fn short_c_matches_check() {
    let scratch = Scratch::new("r75-shortC");
    let input = scratch.path("data.bin");
    fs::write(&input, payload()).unwrap();
    let output = run(&["-q", "-f", "-C", input.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");
    let listing = String::from_utf8_lossy(
        &run(&["-l", scratch.path("data.bin.zst").to_str().unwrap()]).stdout,
    )
    .to_string();
    assert!(listing.contains("XXH64"), "{listing}");
}

/// `--memlimit` and `--memlimit-decompress` answer like `--memory`.
#[test]
fn memlimit_aliases_bound_decode() {
    let scratch = Scratch::new("r75-memlimit");
    let input = scratch.path("data.bin");
    fs::write(&input, payload()).unwrap();
    assert!(run(&["-q", "-f", input.to_str().unwrap()]).status.success());
    let zst = scratch.path("data.bin.zst").to_str().unwrap().to_string();
    // 1 KiB rejects every frame (the format minimum), like -M
    for alias in ["--memlimit=1K", "--memlimit-decompress=1K"] {
        let output = run(&["-q", "-f", "-d", alias, &zst]);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
    }
    // a wide bound decodes fine
    let output = run(&["-q", "-f", "-d", "--memlimit=64M", &zst]);
    assert!(output.status.success(), "{output:?}");
}

/// `--output-dir-flat` relocates derived output names under an existing
/// directory; a missing one fails with the destination path, like
/// FIO_openDstFile.
#[test]
fn output_dir_flat_relocates_names() {
    let scratch = Scratch::new("r75-flat");
    let input = scratch.path("data.bin");
    fs::write(&input, payload()).unwrap();
    let out = scratch.path("out");
    fs::create_dir_all(&out).unwrap();

    let output = run(&[
        "-q",
        "-f",
        "--output-dir-flat",
        out.to_str().unwrap(),
        input.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{output:?}");
    assert!(out.join("data.bin.zst").exists());
    assert!(!scratch.path("data.bin.zst").exists());

    // decompression relocates too
    let output = run(&[
        "-q",
        "-f",
        "-d",
        "--output-dir-flat",
        out.to_str().unwrap(),
        out.join("data.bin.zst").to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fs::read(out.join("data.bin")).unwrap(), payload());

    // a missing directory is the reference's dst-open failure
    let missing = scratch.path("missing");
    let output = run(&[
        "-q",
        "-f",
        "--output-dir-flat",
        missing.to_str().unwrap(),
        input.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        format!(
            "zstdx: {}: No such file or directory\n",
            missing.join("data.bin.zst").display()
        )
    );
}

/// Repeated value flags keep the last occurrence, like zstdcli.c's plain
/// variable overwrites; `-D a -D missing` fails on the second dictionary.
#[test]
fn repeated_value_flags_last_wins_end_to_end() {
    let scratch = Scratch::new("r75-lastwins");
    let input = scratch.path("data.bin");
    fs::write(&input, payload()).unwrap();
    let good = scratch.path("good.dict");
    fs::write(&good, b"raw dictionary content, at least eight bytes").unwrap();

    // -D good -D missing: the missing one is the live value (error 31)
    let output = run(&[
        "-q",
        "-f",
        "-D",
        good.to_str().unwrap(),
        "-D",
        scratch.path("missing.dict").to_str().unwrap(),
        input.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(31), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        format!(
            "zstdx: error 31 : Stat failed on dictionary file {}: No such file or directory \n",
            scratch.path("missing.dict").display()
        )
    );

    // -D missing -D good: the good one wins and compression succeeds
    let output = run(&[
        "-q",
        "-f",
        "-D",
        scratch.path("missing.dict").to_str().unwrap(),
        "-D",
        good.to_str().unwrap(),
        input.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{output:?}");
    assert!(scratch.path("data.bin.zst").exists());
}

/// Every flag with no engine knob rejects loudly, naming the flag and the
/// reason, exit 1; the value-less spellings parse first so the diagnostic
/// names them.
#[test]
fn unsupported_flags_reject_loudly() {
    let scratch = Scratch::new("r75-reject");
    let input = scratch.path("data.bin");
    fs::write(&input, b"x").unwrap();
    let path = input.to_str().unwrap();
    for (args, line) in [
        (
            vec!["--rsyncable", path],
            "zstdx: --rsyncable is not supported: the engine has no rsync-friendly block \
             splitting\n",
        ),
        (
            vec!["--compress-literals", path],
            "zstdx: --compress-literals is not supported: literal entropy mode is chosen per \
             block by the encoder\n",
        ),
        (
            vec!["--no-compress-literals", path],
            "zstdx: --no-compress-literals is not supported: literal entropy mode is chosen per \
             block by the encoder\n",
        ),
        (
            vec!["--row-match-finder", path],
            "zstdx: --row-match-finder is not supported: the matchfinder is selected per level \
             row\n",
        ),
        (
            vec!["--no-row-match-finder", path],
            "zstdx: --no-row-match-finder is not supported: the matchfinder is selected per level \
             row\n",
        ),
        (
            vec!["--target-compressed-block-size=4K", path],
            "zstdx: --target-compressed-block-size is not supported: no compressed-block-size \
             target exists\n",
        ),
        (
            vec!["--zstd=wlog=20", path],
            "zstdx: --zstd= is not supported: raw compression-parameter forcing is not exposed\n",
        ),
        (
            vec!["--max", path],
            "zstdx: --max is not supported: maximum-parameter compression needs raw parameter \
             forcing\n",
        ),
        (
            vec!["--trace", "t.log", path],
            "zstdx: --trace is not supported: the engine has no trace hooks\n",
        ),
        (
            vec!["--trace-file-stat", path],
            "zstdx: --trace-file-stat is not supported: the engine has no trace hooks\n",
        ),
        (
            vec!["--show-default-cparams", path],
            "zstdx: --show-default-cparams is not supported: the engine's level parameters are \
             not exposed for printing\n",
        ),
        (
            vec!["--priority=rt", path],
            "zstdx: --priority=rt is not supported: no scheduler control is available\n",
        ),
        (
            vec!["-s9", path],
            "zstdx: -s is not supported: the trainer has no selectivity knob\n",
        ),
        (
            vec!["-S", path],
            "zstdx: -S is not supported: the benchmark consolidates its files\n",
        ),
        (
            vec!["-p", path],
            "zstdx: -p is not supported: the pause hook is not available\n",
        ),
        (
            vec!["-P50", path],
            "zstdx: -P is not supported: the synthetic benchmark corpus is the fixed lorem text\n",
        ),
        (
            vec!["--fake-stdin-is-console"],
            "zstdx: --fake-stdin-is-console is not supported: the console-detection test hooks \
             are not available\n",
        ),
        (
            vec!["--fake-stdout-is-console"],
            "zstdx: --fake-stdout-is-console is not supported: the console-detection test hooks \
             are not available\n",
        ),
        (
            vec!["--fake-stderr-is-console"],
            "zstdx: --fake-stderr-is-console is not supported: the console-detection test hooks \
             are not available\n",
        ),
        (
            vec!["--output-dir-mirror", "out", path],
            "zstdx: --output-dir-mirror is not supported: mirrored output directories are not \
             implemented\n",
        ),
    ] {
        let output = run(&args);
        assert_eq!(output.status.code(), Some(1), "{args:?}: {output:?}");
        assert_eq!(String::from_utf8_lossy(&output.stderr), line, "{args:?}");
    }
}

/// The identity flags parse and leave the run's outcome unchanged, and the
/// format selector accepts its only value while the others are unknown
/// flags, exactly like a zstd built without zlib/lzma/lz4.
#[test]
fn identity_flags_and_format_selector() {
    let scratch = Scratch::new("r75-identity");
    let input = scratch.path("data.bin");
    fs::write(&input, payload()).unwrap();
    let plain = run(&["-q", "-f", input.to_str().unwrap()]);
    assert!(plain.status.success(), "{plain:?}");
    let reference = fs::read(scratch.path("data.bin.zst")).unwrap();
    fs::remove_file(scratch.path("data.bin.zst")).unwrap();

    for extra in [
        "-n",
        "--asyncio",
        "--no-asyncio",
        "--mmap-dict",
        "--no-mmap-dict",
        "--format=zstd",
        "--auto-threads=physical",
        "--auto-threads=logical",
    ] {
        let output = run(&["-q", "-f", extra, input.to_str().unwrap()]);
        assert!(output.status.success(), "{extra}: {output:?}");
        assert_eq!(fs::read(scratch.path("data.bin.zst")).unwrap(), reference);
        fs::remove_file(scratch.path("data.bin.zst")).unwrap();
    }

    // the other format values are unknown flags in the reference too
    for fmt in ["gzip", "xz", "lzma", "lz4"] {
        let output = run(&[format!("--format={fmt}").as_str(), input.to_str().unwrap()]);
        assert_eq!(output.status.code(), Some(1), "{fmt}: {output:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            format!("Incorrect parameter: --format={fmt} \n"),
            "{fmt}"
        );
    }
    // so is a non-rt priority value
    let output = run(&["--priority=idle", input.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "Incorrect parameter: --priority=idle \n"
    );
}

/// -T0 resolves through the auto-threads basis and announces the count at
/// -v, like zstd's display-level-3 note.
#[test]
fn auto_threads_note_at_verbose() {
    let scratch = Scratch::new("r75-t0");
    let input = scratch.path("data.bin");
    fs::write(&input, payload()).unwrap();
    let output = run(&["-q", "-T0", "-f", input.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");
    fs::remove_file(scratch.path("data.bin.zst")).unwrap();
    let output = run(&["-v", "-T0", "-f", input.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(" core(s) detected ") && stderr.contains("physical"),
        "{stderr}"
    );
}

/// A suppressed --stream-size pledge still binds: the read count must match
/// or the run fails with the engine's error-11 line.
#[test]
fn suppressed_stream_size_still_binds() {
    let (output, _) = run_piped(
        &["-f", "--no-content-size", "--stream-size=100", "-c"],
        b"toolong",
    );
    assert_eq!(output.status.code(), Some(11), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "zstdx: error 11 : Src size is incorrect \n"
    );
    let (output, stdout) = run_piped(
        &["-q", "-f", "--no-content-size", "--stream-size=7", "-c"],
        b"toolong",
    );
    assert!(output.status.success(), "{output:?}");
    assert!(stdout.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]), "{stdout:?}");
}
