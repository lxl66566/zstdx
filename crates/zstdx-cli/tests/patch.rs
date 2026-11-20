//! End-to-end tests of `--patch-from`: diff-style (de)compression against
//! a reference file, with interop against the system zstd (both
//! directions) when the reference binary exists, plus the edge cases
//! (stdin pledge, conflicts, missing/empty reference).

use std::{fs, process::Command};

mod common;

use common::{Scratch, run, run_piped, stderr_of, system_zstd};

/// A reference file and a near-copy target: a patch of the target against
/// the reference must compress far below the reference-free size.
fn fixtures() -> (Vec<u8>, Vec<u8>) {
    let mut reference = Vec::new();
    for i in 0..3000u32 {
        reference
            .extend_from_slice(format!("record {i}: {}\n", i.wrapping_mul(2654435761)).as_bytes());
    }
    let mut target = reference.clone();
    // a handful of edits, far apart: most of the file still matches
    for &pos in &[7usize, 11_000, 40_000, 90_000] {
        if pos + 8 < target.len() {
            target[pos..pos + 8].copy_from_slice(b"modified");
        }
    }
    target.extend_from_slice(b"appended trailer\n");
    (reference, target)
}

/// The patch roundtrip through our CLI alone, and the sanity that the
/// reference actually pays: the patch stays far under a plain frame.
#[test]
fn patch_roundtrip_and_ratio() {
    let scratch = Scratch::new("patch", "roundtrip");
    let (reference, target) = fixtures();
    let ref_path = scratch.path("ref");
    let target_path = scratch.path("target");
    fs::write(&ref_path, &reference).unwrap();
    fs::write(&target_path, &target).unwrap();

    let patch = scratch.path("target.patch.zst");
    let out = run(&[
        "--patch-from",
        ref_path.to_str().unwrap(),
        target_path.to_str().unwrap(),
        "-o",
        patch.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));

    let restored = scratch.path("target.restored");
    let out = run(&[
        "-d",
        "--patch-from",
        ref_path.to_str().unwrap(),
        patch.to_str().unwrap(),
        "-o",
        restored.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(fs::read(&restored).unwrap(), target);

    // without the reference the same file barely compresses: the patch
    // must be at least 3x smaller on this fixture
    let plain = scratch.path("target.zst");
    let out = run(&[target_path.to_str().unwrap(), "-o", plain.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let patch_len = fs::read(&patch).unwrap().len();
    let plain_len = fs::read(&plain).unwrap().len();
    assert!(
        patch_len * 3 < plain_len,
        "patch {patch_len} vs plain {plain_len}"
    );
}

/// Our `--patch-from` output decodes with the real zstd CLI, and zstd's
/// `--patch-from` output decodes with ours.
#[test]
fn libzstd_interop_both_directions() {
    if !system_zstd() {
        eprintln!("zstd binary not found; skipping libzstd patch interop test");
        return;
    }
    let scratch = Scratch::new("patch", "interop");
    let (reference, target) = fixtures();
    let ref_path = scratch.path("ref");
    fs::write(&ref_path, &reference).unwrap();
    let target_path = scratch.path("target");
    fs::write(&target_path, &target).unwrap();

    // ours compresses, zstd applies
    let ours = scratch.path("ours.patch.zst");
    let out = run(&[
        "--patch-from",
        ref_path.to_str().unwrap(),
        target_path.to_str().unwrap(),
        "-o",
        ours.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let zstd_restored = scratch.path("zstd.restored");
    let status = Command::new("zstd")
        .args([
            "-q",
            "-d",
            "-f",
            "--patch-from",
            ref_path.to_str().unwrap(),
            "-o",
            zstd_restored.to_str().unwrap(),
            ours.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success(), "zstd -d --patch-from failed");
    assert_eq!(fs::read(&zstd_restored).unwrap(), target);

    // zstd compresses, ours applies
    let theirs = scratch.path("theirs.patch.zst");
    let status = Command::new("zstd")
        .args([
            "-q",
            "-f",
            "--patch-from",
            ref_path.to_str().unwrap(),
            "-3",
            "-o",
            theirs.to_str().unwrap(),
            target_path.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success(), "zstd --patch-from failed");
    let our_restored = scratch.path("our.restored");
    let out = run(&[
        "-d",
        "--patch-from",
        ref_path.to_str().unwrap(),
        theirs.to_str().unwrap(),
        "-o",
        our_restored.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(fs::read(&our_restored).unwrap(), target);
}

/// Stdin compression needs `--stream-size` (the window rule sizes off the
/// source); with the pledge the full roundtrip works through pipes.
#[test]
fn stdin_needs_stream_size() {
    let scratch = Scratch::new("patch", "stdin");
    let (reference, target) = fixtures();
    let ref_path = scratch.path("ref");
    fs::write(&ref_path, &reference).unwrap();

    let (out, _) = run_piped(&["--patch-from", ref_path.to_str().unwrap(), "-c"], &target);
    assert!(!out.status.success(), "an unpledged stdin must fail");
    assert!(
        stderr_of(&out).contains("--patch-from with stdin requires --stream-size"),
        "{}",
        stderr_of(&out)
    );

    let (out, patch) = run_piped(
        &[
            "--patch-from",
            ref_path.to_str().unwrap(),
            "--stream-size",
            &target.len().to_string(),
            "-c",
        ],
        &target,
    );
    assert!(out.status.success(), "{}", stderr_of(&out));

    // and it applies back through a pipe
    let (out, restored) = run_piped(
        &["-d", "--patch-from", ref_path.to_str().unwrap(), "-c"],
        &patch,
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(restored, target);
}

/// The reference-file edge cases: a missing REF reports the path, an
/// empty REF compresses (as a frame with no history), and decoding a
/// patch without its REF fails.
#[test]
fn reference_edge_cases() {
    let scratch = Scratch::new("patch", "edges");
    let (reference, target) = fixtures();
    let ref_path = scratch.path("ref");
    fs::write(&ref_path, &reference).unwrap();
    let target_path = scratch.path("target");
    fs::write(&target_path, &target).unwrap();

    let out = run(&[
        "--patch-from",
        scratch.path("missing").to_str().unwrap(),
        target_path.to_str().unwrap(),
        "-o",
        scratch.path("out.zst").to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("missing"),
        "the diagnostic names the file: {}",
        stderr_of(&out)
    );

    let empty = scratch.path("empty");
    fs::write(&empty, b"").unwrap();
    let patch = scratch.path("empty-ref.zst");
    let out = run(&[
        "--patch-from",
        empty.to_str().unwrap(),
        target_path.to_str().unwrap(),
        "-o",
        patch.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let restored = scratch.path("empty-ref.out");
    let out = run(&[
        "-d",
        "--patch-from",
        empty.to_str().unwrap(),
        patch.to_str().unwrap(),
        "-o",
        restored.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(fs::read(&restored).unwrap(), target);
    // an empty reference contributes no history: the frame decodes alone
    let out = run(&["-d", "-f", patch.to_str().unwrap(), "-c"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(out.stdout, target);

    // a real patch needs its reference on decode
    let patch = scratch.path("target.patch.zst");
    let out = run(&[
        "--patch-from",
        ref_path.to_str().unwrap(),
        target_path.to_str().unwrap(),
        "-o",
        patch.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let out = run(&["-d", patch.to_str().unwrap(), "-c"]);
    assert!(!out.status.success(), "decoding without the REF must fail");
}

/// `--patch-from` is single-input and exclusive with `-D`; the level cap
/// does not apply (patch mode implies `--ultra`, as zstdcli.c sets it).
#[test]
fn flag_conflicts_and_ultra() {
    let scratch = Scratch::new("patch", "conflicts");
    let (reference, target) = fixtures();
    let ref_path = scratch.path("ref");
    fs::write(&ref_path, &reference).unwrap();
    let a = scratch.path("a");
    fs::write(&a, &target).unwrap();
    let b = scratch.path("b");
    fs::write(&b, &target).unwrap();

    let out = run(&[
        "--patch-from",
        ref_path.to_str().unwrap(),
        a.to_str().unwrap(),
        b.to_str().unwrap(),
        "-c",
    ]);
    assert!(!out.status.success(), "two inputs must reject");
    assert!(
        stderr_of(&out).contains("multiple files"),
        "{}",
        stderr_of(&out)
    );

    let out = run(&[
        "--patch-from",
        ref_path.to_str().unwrap(),
        "-D",
        ref_path.to_str().unwrap(),
        a.to_str().unwrap(),
    ]);
    assert!(!out.status.success(), "-D must conflict");

    // level 20 without --ultra: no reduction warning, the patch works
    let out = run(&[
        "--patch-from",
        ref_path.to_str().unwrap(),
        "--level=20",
        a.to_str().unwrap(),
        "-c",
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        !stderr_of(&out).contains("reduced"),
        "patch mode implies --ultra: {}",
        stderr_of(&out)
    );
    assert!(!out.stdout.is_empty());
}
