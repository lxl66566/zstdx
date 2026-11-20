//! End-to-end tests of `--adapt`: chunked multi-frame compression through
//! the real binary, the CLI surface of the bounds (parse failures, the
//! combinations zstdx cannot honor), and interop against the system zstd
//! (decode direction) when the reference binary exists.

use std::{
    fs,
    process::{Command, Stdio},
};

mod common;

use common::{Scratch, run, run_piped, stderr_of, system_zstd};

/// Count frame magic occurrences: the chunk-frame structure of `--adapt`.
fn frame_count(bytes: &[u8]) -> usize {
    bytes
        .windows(4)
        .filter(|w| w == &[0x28, 0xb5, 0x2f, 0xfd])
        .count()
}

/// A >8 MiB compressible payload: at least one full chunk frame plus a
/// partial tail under the CLI's fixed 8 MiB chunk.
fn payload(mib: usize) -> Vec<u8> {
    let block: Vec<u8> = (0..u8::MAX).cycle().take(96 * 1024).collect();
    let mut data = Vec::with_capacity(mib << 20);
    while data.len() < mib << 20 {
        data.extend_from_slice(&block);
    }
    data
}

/// The compress run succeeds, splits into chunk frames, and round-trips
/// through our own decoder.
#[test]
fn adapt_roundtrip_multi_frame() {
    let scratch = Scratch::new("adapt", "roundtrip");
    let data = payload(9);
    let src = scratch.path("src.bin");
    fs::write(&src, &data).unwrap();
    let zst = scratch.path("src.bin.zst");
    let out = run(&["--adapt=min=1,max=3", src.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(zst.exists(), "default .zst output name");
    let frames = frame_count(&fs::read(&zst).unwrap());
    assert!(frames >= 2, "a 9 MiB input must split into chunk frames");

    let restored = scratch.path("restored.bin");
    let out = run(&[
        "-d",
        "-f",
        "-o",
        restored.to_str().unwrap(),
        zst.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(fs::read(&restored).unwrap(), data);
}

/// The system zstd decodes our multi-frame output (interop, decode
/// direction), skipped when the reference binary is absent.
#[test]
fn adapt_output_decodes_with_system_zstd() {
    if !system_zstd() {
        eprintln!("skipping: system zstd not found");
        return;
    }
    let scratch = Scratch::new("adapt", "interop");
    let data = payload(9);
    let zst = scratch.path("a.zst");
    let (out, compressed) = run_piped(&["--adapt=max=3", "-c"], &data);
    assert!(out.status.success(), "{}", stderr_of(&out));
    fs::write(&zst, &compressed).unwrap();

    let restored = scratch.path("restored.bin");
    let status = Command::new("zstd")
        .args(["-d", "-f", "-o"])
        .arg(&restored)
        .arg(&zst)
        .stdin(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "system zstd rejects our chunk frames");
    assert_eq!(fs::read(&restored).unwrap(), data);
}

/// stdin through the adaptive path frames and decodes, including the
/// empty-stream edge (one empty frame, like every other path).
#[test]
fn adapt_stdin_and_empty_stream() {
    let data = payload(9);
    let (out, compressed) = run_piped(&["--adapt=min=1,max=3", "-c"], &data);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(frame_count(&compressed) >= 2);
    let (out, restored) = run_piped(&["-d", "-c"], &compressed);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(restored, data);

    let (out, empty_frame) = run_piped(&["--adapt", "-c"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(!empty_frame.is_empty(), "empty input still frames");
    let (out, restored) = run_piped(&["-d", "-c"], &empty_frame);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(restored.is_empty());
}

/// The bounds surface: parse failures and the combinations zstdx cannot
/// honor reject loudly instead of being silently dropped.
#[test]
fn adapt_rejects_what_it_cannot_honor() {
    for args in [
        vec!["--adapt=min=9,max=2", "-c"],
        vec!["--adapt=bogus", "-c"],
        vec!["--adapt=min=1", "--stream-size=1K", "-c"],
        vec!["--adapt", "--size-hint=1K", "-c"],
        vec!["--adapt", "-b3"],
    ] {
        let (out, _) = run_piped(&args, b"payload");
        assert!(!out.status.success(), "{args:?} must fail");
        let stderr = stderr_of(&out);
        assert!(stderr.contains("adapt"), "{args:?}: {stderr}");
    }
}
