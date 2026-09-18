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
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
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
