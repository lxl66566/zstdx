#![cfg(test)]

use alloc::{borrow::ToOwned, string::String, vec::Vec};
use std::{ffi::OsStr, fs, path::PathBuf};

use crate::encoding::FrameCompressor;

/// Every non-`.zst` corpus fixture (the decodecorpus set plus any local
/// additions), sorted by path.
fn corpus_files() -> Option<Vec<PathBuf>> {
    let entries = super::fixture_entries("./decodecorpus_files")?;
    let mut files: Vec<_> = entries.into_iter().collect();
    if fs::read_dir("./local_corpus_files").is_ok() {
        files.extend(fs::read_dir("./local_corpus_files").unwrap());
    }
    files.sort_by_key(|x| match x {
        Err(_) => String::new(),
        Ok(entry) => entry.path().to_str().unwrap().to_owned(),
    });
    Some(
        files
            .iter()
            .map(|f| f.as_ref().unwrap().path())
            .filter(|p| p.extension() != Some(OsStr::new("zst")))
            .collect(),
    )
}

fn compress_file(path: &std::path::Path) -> Vec<u8> {
    let input = fs::read(path).unwrap();
    let mut compressed: Vec<u8> = Vec::new();
    let mut compressor = FrameCompressor::new(crate::Level::Fastest);
    compressor.set_source(input.as_slice());
    compressor.set_drain(&mut compressed);
    compressor.compress();
    compressed
}

#[test]
fn corpus_files_roundtrip_our_decompressor() {
    use std::io::Read;

    let Some(files) = corpus_files() else {
        return;
    };
    let mut failures: Vec<PathBuf> = Vec::new();
    for path in &files {
        let input = fs::read(path).unwrap();
        let compressed = compress_file(path);
        let mut output = Vec::new();
        let mut decoder = crate::decoding::StreamingDecoder::new(compressed.as_slice()).unwrap();
        decoder.read_to_end(&mut output).unwrap();
        if input != output {
            failures.push(path.clone());
        }
    }
    assert!(
        failures.is_empty(),
        "Decompression of compressed file failed on the following files: {failures:?}"
    );
}

#[test]
fn corpus_files_roundtrip_reference_decompressor() {
    use alloc::format;

    let Some(files) = corpus_files() else {
        return;
    };
    let mut failures: Vec<(PathBuf, String)> = Vec::new();
    for path in &files {
        let input = fs::read(path).unwrap();
        let compressed = compress_file(path);
        let mut output = Vec::new();
        match zstd::stream::copy_decode(compressed.as_slice(), &mut output) {
            Ok(()) => {
                if input != output {
                    failures.push((path.clone(), "Input didn't equal output".to_owned()));
                }
            },
            Err(e) => {
                failures.push((path.clone(), format!("Decompressor threw an error: {e:?}")));
            },
        }
    }
    assert!(
        failures.is_empty(),
        "Decompression of the compressed file fails on the following files: {failures:?}"
    );
}
