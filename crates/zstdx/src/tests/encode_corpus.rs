#[test]
fn test_encode_corpus_files_uncompressed_our_decompressor() {
    extern crate std;
    use alloc::{borrow::ToOwned, string::String, vec::Vec};
    use std::{ffi::OsStr, fs, io::Read, path::PathBuf, println};

    use crate::encoding::FrameCompressor;

    let mut failures: Vec<PathBuf> = Vec::new();
    let Some(entries) = super::fixture_entries("./decodecorpus_files") else {
        return;
    };
    let mut files: Vec<_> = entries.into_iter().collect();
    if fs::read_dir("./local_corpus_files").is_ok() {
        files.extend(fs::read_dir("./local_corpus_files").unwrap());
    }

    files.sort_by_key(|x| match x {
        Err(_) => String::new(),
        Ok(entry) => entry.path().to_str().unwrap().to_owned(),
    });

    for entry in files.iter().map(|f| f.as_ref().unwrap()) {
        let path = entry.path();
        if path.extension() == Some(OsStr::new("zst")) {
            continue;
        }

        println!("Trying file: {path:?}");
        let input = fs::read(entry.path()).unwrap();
        let mut compressed_file: Vec<u8> = Vec::new();
        let mut compressor = FrameCompressor::new(crate::Level::Fastest);
        compressor.set_source(input.as_slice());
        compressor.set_drain(&mut compressed_file);

        compressor.compress();
        let mut decompressed_output = Vec::new();
        let mut decoder =
            crate::decoding::StreamingDecoder::new(compressed_file.as_slice()).unwrap();
        decoder.read_to_end(&mut decompressed_output).unwrap();

        if input != decompressed_output {
            failures.push(path);
        }
    }

    assert!(
        failures.is_empty(),
        "Decompression of compressed file failed on the following files: {failures:?}"
    );
}

#[test]
fn test_encode_corpus_files_uncompressed_original_decompressor() {
    extern crate std;
    use alloc::{borrow::ToOwned, format, vec::Vec};
    use std::{ffi::OsStr, fs, path::PathBuf, println, string::String};

    use crate::encoding::FrameCompressor;

    let mut failures: Vec<(PathBuf, String)> = Vec::new();
    let Some(entries) = super::fixture_entries("./decodecorpus_files") else {
        return;
    };
    let mut files: Vec<_> = entries.into_iter().collect();
    if fs::read_dir("./local_corpus_files").is_ok() {
        files.extend(fs::read_dir("./local_corpus_files").unwrap());
    }

    files.sort_by_key(|x| match x {
        Err(_) => String::new(),
        Ok(entry) => entry.path().to_str().unwrap().to_owned(),
    });

    for entry in files.iter().map(|f| f.as_ref().unwrap()) {
        let path = entry.path();
        if path.extension() == Some(OsStr::new("zst")) {
            continue;
        }
        println!("Trying file: {path:?}");
        let input = fs::read(entry.path()).unwrap();

        let mut compressed_file: Vec<u8> = Vec::new();
        let mut compressor = FrameCompressor::new(crate::Level::Fastest);
        compressor.set_source(input.as_slice());
        compressor.set_drain(&mut compressed_file);
        compressor.compress();
        let mut decompressed_output = Vec::new();
        // zstd::stream::copy_decode(compressed_file.as_slice(), &mut decompressed_output).unwrap();
        match zstd::stream::copy_decode(compressed_file.as_slice(), &mut decompressed_output) {
            Ok(()) => {
                if input != decompressed_output {
                    failures.push((path.clone(), "Input didn't equal output".to_owned()));
                }
            },
            Err(e) => {
                failures.push((path.clone(), format!("Decompressor threw an error: {e:?}")));
            },
        }

        assert!(
            failures.is_empty(),
            "Decompression of the compressed file fails on the following files: {failures:?}"
        );
    }
}

#[test]
fn test_encode_corpus_files_compressed_our_decompressor() {
    extern crate std;
    use alloc::{borrow::ToOwned, string::String, vec::Vec};
    use std::{ffi::OsStr, fs, io::Read, path::PathBuf, println};

    use crate::encoding::FrameCompressor;

    let mut failures: Vec<PathBuf> = Vec::new();
    let Some(entries) = super::fixture_entries("./decodecorpus_files") else {
        return;
    };
    let mut files: Vec<_> = entries.into_iter().collect();
    if fs::read_dir("./local_corpus_files").is_ok() {
        files.extend(fs::read_dir("./local_corpus_files").unwrap());
    }

    files.sort_by_key(|x| match x {
        Err(_) => String::new(),
        Ok(entry) => entry.path().to_str().unwrap().to_owned(),
    });

    for entry in files.iter().map(|f| f.as_ref().unwrap()) {
        let path = entry.path();
        if path.extension() == Some(OsStr::new("zst")) {
            continue;
        }
        println!("Trying file: {path:?}");
        let input = fs::read(entry.path()).unwrap();

        let mut compressed_file: Vec<u8> = Vec::new();
        let mut compressor = FrameCompressor::new(crate::Level::Fastest);
        compressor.set_source(input.as_slice());
        compressor.set_drain(&mut compressed_file);

        compressor.compress();
        let mut decompressed_output = Vec::new();
        let mut decoder =
            crate::decoding::StreamingDecoder::new(compressed_file.as_slice()).unwrap();
        decoder.read_to_end(&mut decompressed_output).unwrap();

        if input != decompressed_output {
            failures.push(path);
        }
    }

    assert!(
        failures.is_empty(),
        "Decompression of compressed file failed on the following files: {failures:?}"
    );
}

#[test]
fn test_encode_corpus_files_compressed_original_decompressor() {
    extern crate std;
    use alloc::{borrow::ToOwned, format, vec::Vec};
    use std::{ffi::OsStr, fs, path::PathBuf, println, string::String};

    use crate::encoding::FrameCompressor;

    let mut failures: Vec<(PathBuf, String)> = Vec::new();
    let Some(entries) = super::fixture_entries("./decodecorpus_files") else {
        return;
    };
    let mut files: Vec<_> = entries.into_iter().collect();
    if fs::read_dir("./local_corpus_files").is_ok() {
        files.extend(fs::read_dir("./local_corpus_files").unwrap());
    }

    files.sort_by_key(|x| match x {
        Err(_) => String::new(),
        Ok(entry) => entry.path().to_str().unwrap().to_owned(),
    });

    for entry in files.iter().map(|f| f.as_ref().unwrap()) {
        let path = entry.path();
        if path.extension() == Some(OsStr::new("zst")) {
            continue;
        }
        println!("Trying file: {path:?}");
        let input = fs::read(entry.path()).unwrap();

        let mut compressed_file: Vec<u8> = Vec::new();
        let mut compressor = FrameCompressor::new(crate::Level::Fastest);
        compressor.set_source(input.as_slice());
        compressor.set_drain(&mut compressed_file);
        compressor.compress();
        let mut decompressed_output = Vec::new();
        // zstd::stream::copy_decode(compressed_file.as_slice(), &mut decompressed_output).unwrap();
        match zstd::stream::copy_decode(compressed_file.as_slice(), &mut decompressed_output) {
            Ok(()) => {
                if input != decompressed_output {
                    failures.push((path.clone(), "Input didn't equal output".to_owned()));
                }
            },
            Err(e) => {
                failures.push((path.clone(), format!("Decompressor threw an error: {e:?}")));
            },
        }

        assert!(
            failures.is_empty(),
            "Decompression of the compressed file fails on the following files: {failures:?}"
        );
    }
}
