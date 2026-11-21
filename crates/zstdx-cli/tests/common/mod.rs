//! Shared end-to-end harness for the zstdx-cli integration tests: the
//! binary path, a self-cleaning scratch directory, and the runner helpers
//! every test file uses.
#![allow(dead_code)]

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub const BIN: &str = env!("CARGO_BIN_EXE_zstdx");

/// A scratch directory that removes itself on drop. `family` names the
/// calling test file, so a scratch leaked by a killed run identifies its
/// source.
pub struct Scratch(PathBuf);

impl Scratch {
    pub fn new(family: &str, test: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "zstdx-cli-{family}-{test}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    pub fn path(&self, name: impl AsRef<Path>) -> PathBuf {
        self.0.join(name)
    }

    /// The scratch directory itself, for runners that take a working
    /// directory or print the path.
    pub fn root(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Run the CLI with `args`, no stdin.
pub fn run(args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// Pipe `input` through the CLI, returning the captured stdout.
pub fn run_piped(args: &[&str], input: &[u8]) -> (std::process::Output, Vec<u8>) {
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

/// The reference zstd, when the environment provides one.
pub fn system_zstd() -> bool {
    Command::new("zstd")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .is_ok_and(|out| out.status.success())
}

pub fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// stderr text with line endings normalized to `\n`: the binary mirrors the
/// reference zstd's CRT text mode, which emits `\r\n` on Windows.
pub fn stderr_text(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n")
}

/// The stdout counterpart of [`stderr_text`].
pub fn stdout_text(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}
