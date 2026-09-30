//! End-to-end tests of `--train`: dictionary shape, sizing, output
//! naming, the loud rejections, and interop round-trips against the
//! system zstd in both directions (trained-by-us used by zstd, and
//! trained-by-zstd used by us).
#![cfg(feature = "dict_builder")]

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const BIN: &str = env!("CARGO_BIN_EXE_zstdx");

/// A scratch directory that removes itself on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(test: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "zstdx-cli-train-{test}-{}-{}",
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

/// Run in `dir` (the default output name lands in the CWD).
fn run_in(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn system_zstd() -> bool {
    Command::new("zstd")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Deterministic shared-boilerplate samples with a per-file varying tail.
fn write_samples(dir: &Path, count: usize) -> Vec<Vec<u8>> {
    let mut samples = Vec::new();
    for i in 0..count as u32 {
        let mut sample = Vec::new();
        for _ in 0..8 {
            sample.extend_from_slice(b"[Unit]\nDescription=shared boilerplate block\n");
            sample.extend((0..64u32).map(|j| b'a' + ((i * 7 + j) % 16) as u8));
        }
        samples.push(sample);
    }
    fs::create_dir_all(dir).unwrap();
    for (i, sample) in samples.iter().enumerate() {
        fs::write(dir.join(format!("s{i}")), sample).unwrap();
    }
    samples
}

/// Training writes a formatted dictionary (magic + dictID + entropy
/// tables + content) of at most the requested size, and reports the
/// result on stderr.
#[test]
fn trains_formatted_dictionary() {
    let scratch = Scratch::new("basic");
    let samples = write_samples(&scratch.path("samples"), 24);
    let sample_dir = scratch.path("samples");
    let dict = scratch.path("out.dict");
    let out = run(&[
        "--train",
        "-r",
        sample_dir.to_str().unwrap(),
        "-o",
        dict.to_str().unwrap(),
        "--maxdict=8192",
    ]);
    assert!(out.status.success(), "{out:?}");
    let bytes = fs::read(&dict).unwrap();
    assert!(bytes.len() <= 8192, "{}", bytes.len());
    assert!(bytes.starts_with(&[0x37, 0xa4, 0x30, 0xec]), "dict magic");
    let dict_id = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    assert_ne!(dict_id, 0, "formatted dictionaries carry an id");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&format!("Save dictionary of size {} ", bytes.len())),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!("{} samples", samples.len())),
        "{stderr}"
    );
}

/// zstd's default output name is `dictionary` in the working directory.
#[test]
fn default_output_name_is_dictionary() {
    let scratch = Scratch::new("default-name");
    write_samples(&scratch.path("samples"), 16);
    let out = run_in(&scratch.0, &["--train", "-r", "samples"]);
    assert!(out.status.success(), "{out:?}");
    assert!(scratch.path("dictionary").exists());
}

/// `--maxdict` accepts zstd's size suffixes and caps the dictionary.
#[test]
fn maxdict_suffixes_cap() {
    let scratch = Scratch::new("maxdict");
    write_samples(&scratch.path("samples"), 24);
    let sample_dir = scratch.path("samples").display().to_string();
    for (spec, bound) in [("4096", 4096u64), ("16K", 16384), ("1M", 1 << 20)] {
        let dict = scratch.path(spec.replace('.', "_"));
        let out = run(&[
            "--train",
            "-r",
            &sample_dir,
            "-o",
            dict.to_str().unwrap(),
            &format!("--maxdict={spec}"),
        ]);
        assert!(out.status.success(), "{spec}: {out:?}");
        assert!(
            fs::metadata(&dict).unwrap().len() <= bound,
            "{spec}: {}",
            fs::metadata(&dict).unwrap().len()
        );
    }
    // Below ZDICT's 256-byte floor the request is refused, like zstd.
    let out = run(&["--train", "-r", &sample_dir, "--maxdict=8"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("at least 256"),
        "{out:?}"
    );
}

/// Knobs the trainer does not expose are rejected loudly, not ignored.
#[test]
fn unsupported_knobs_rejected() {
    let scratch = Scratch::new("reject");
    write_samples(&scratch.path("samples"), 8);
    let samples = scratch.path("samples").display().to_string();
    for args in [
        vec!["--train", &samples, "--dictID=5"],
        vec!["--train", &samples, "--train-cover"],
        vec!["--train", &samples, "--train-cover=k=48,d=8"],
        vec!["--train", &samples, "--train-fastcover=f=20"],
        vec!["--train", &samples, "--train-legacy=s=9"],
    ] {
        let out = run(&args);
        assert!(!out.status.success(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("zstdx:"),
            "{args:?}"
        );
        assert!(!scratch.path("dictionary").exists(), "{args:?}");
    }
    // --maxdict outside --train is meaningless
    let out = run(&["--maxdict=1K", &samples]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("only applies with --train"),
        "{out:?}"
    );
}

/// No samples (or stdin) is a hard error, like zstd's "nb of samples too
/// low".
#[test]
fn no_samples_rejected() {
    let out = run(&["--train"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("nb of samples too low"),
        "{out:?}"
    );
    let out = run(&["--train", "-"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("standard input"),
        "{out:?}"
    );
}

/// A zstdx-trained dictionary interoperates with the system zstd: our
/// frames decode there, and its trained dictionary works here.
#[test]
fn roundtrips_with_system_zstd() {
    if !system_zstd() {
        return;
    }
    let scratch = Scratch::new("interop");
    let mut samples = write_samples(&scratch.path("samples"), 24);
    let sample_dir = scratch.path("samples").display().to_string();
    let holdout = samples.pop().unwrap();
    let target = scratch.path("holdout.bin");
    fs::write(&target, &holdout).unwrap();

    // Our trainer, our encoder, their decoder.
    let ours_dict = scratch.path("ours.dict");
    let out = run(&[
        "--train",
        "-r",
        &sample_dir,
        "-o",
        ours_dict.to_str().unwrap(),
        "--maxdict=4096",
    ]);
    assert!(out.status.success(), "{out:?}");
    let frame = scratch.path("ours.zst");
    let out = run(&[
        "-q",
        "-f",
        "-D",
        ours_dict.to_str().unwrap(),
        target.to_str().unwrap(),
        "-o",
        frame.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{out:?}");
    let restored = scratch.path("ours.restored");
    let status = Command::new("zstd")
        .args(["-q", "-d", "-f", "-D", ours_dict.to_str().unwrap()])
        .arg(frame.to_str().unwrap())
        .arg("-o")
        .arg(restored.to_str().unwrap())
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(fs::read(&restored).unwrap(), holdout);

    // Their trainer, our encoder and decoder, their decoder.
    let theirs_dict = scratch.path("theirs.dict");
    let status = Command::new("zstd")
        .args(["-q", "--train", "-r", &sample_dir])
        .arg("-o")
        .arg(theirs_dict.to_str().unwrap())
        .arg("--maxdict=4096")
        .status()
        .unwrap();
    assert!(status.success());
    let frame2 = scratch.path("theirs-src.zst");
    let out = run(&[
        "-q",
        "-f",
        "-D",
        theirs_dict.to_str().unwrap(),
        target.to_str().unwrap(),
        "-o",
        frame2.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{out:?}");
    let restored2 = scratch.path("theirs.restored");
    let status = Command::new("zstd")
        .args(["-q", "-d", "-f", "-D", theirs_dict.to_str().unwrap()])
        .arg(frame2.to_str().unwrap())
        .arg("-o")
        .arg(restored2.to_str().unwrap())
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(fs::read(&restored2).unwrap(), holdout);
    let restored3 = scratch.path("theirs.selfrestored");
    let out = run(&[
        "-q",
        "-d",
        "-f",
        "-D",
        theirs_dict.to_str().unwrap(),
        "-o",
        restored3.to_str().unwrap(),
        frame2.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(fs::read(&restored3).unwrap(), holdout);

    // Their encoder with our dictionary, our decoder.
    let frame3 = scratch.path("zstd-enc.zst");
    let status = Command::new("zstd")
        .args(["-q", "-f", "-D", ours_dict.to_str().unwrap()])
        .arg(target.to_str().unwrap())
        .arg("-o")
        .arg(frame3.to_str().unwrap())
        .status()
        .unwrap();
    assert!(status.success());
    let restored4 = scratch.path("zstd-enc.restored");
    let out = run(&[
        "-q",
        "-d",
        "-f",
        "-D",
        ours_dict.to_str().unwrap(),
        "-o",
        restored4.to_str().unwrap(),
        frame3.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(fs::read(&restored4).unwrap(), holdout);
}

/// Training is deterministic: the same samples yield the same dictionary.
#[test]
fn training_is_deterministic() {
    let scratch = Scratch::new("deterministic");
    write_samples(&scratch.path("samples"), 16);
    let sample_dir = scratch.path("samples").display().to_string();
    let a = scratch.path("a.dict");
    let b = scratch.path("b.dict");
    for dict in [&a, &b] {
        let out = run(&["--train", "-r", &sample_dir, "-o", dict.to_str().unwrap()]);
        assert!(out.status.success(), "{out:?}");
    }
    assert_eq!(fs::read(&a).unwrap(), fs::read(&b).unwrap());
}
