#[test]
fn test_all_artifacts() {
    extern crate std;
    use std::{borrow::ToOwned, fs::File};

    use crate::decoding::{BlockDecodingStrategy, FrameDecoder};

    let mut frame_dec = FrameDecoder::new();

    let Some(entries) = super::fixture_entries("../zstdx-fuzz/artifacts/decode") else {
        return;
    };
    for file in entries {
        let file_name = file.unwrap().path();

        let fnstr = file_name.to_str().unwrap().to_owned();
        if !fnstr.contains("/crash-") {
            continue;
        }

        let mut f = File::open(file_name.clone()).unwrap();

        // ignore errors. It just should never panic on invalid input
        let _: Result<_, _> = frame_dec
            .reset(&mut f)
            .and_then(|()| frame_dec.decode_blocks(&mut f, BlockDecodingStrategy::All));
    }
}

/// Encoder-side crash artifacts: the input layout matches the `encode`
/// fuzz target — `[level, opts, k, rest]` with chunk `i` at zstd level
/// `(level + i) % 23` walking one thread's pooled slice state through
/// consecutive levels; the parser is mirrored byte for byte in
/// `crates/zstdx-fuzz/fuzz_targets/encode.rs` and `encode_stream.rs`
/// (separate crates, so it is duplicated). Every chunk must roundtrip
/// through our decoder and the reference implementation. The artifacts
/// are walked twice — forward, then in REVERSE — so one artifact's
/// ladder leaves pool residue that the next artifact's mixed levels must
/// survive.
#[test]
fn test_all_encode_artifacts() {
    extern crate std;
    use std::borrow::ToOwned;

    use crate::decoding::FrameDecoder;

    let Some(entries) = super::fixture_entries("../zstdx-fuzz/artifacts/encode") else {
        return;
    };
    let mut artifacts = alloc::vec::Vec::new();
    for file in entries {
        let file_name = file.unwrap().path();

        let fnstr = file_name.to_str().unwrap().to_owned();
        if !fnstr.contains("/crash-") {
            continue;
        }

        artifacts.push(std::fs::read(file_name).unwrap());
    }

    for (pass, data) in artifacts.iter().chain(artifacts.iter().rev()).enumerate() {
        let (level_byte, opt_byte, k, rest) = match &data[..] {
            [l, o, k, rest @ ..] => (*l, *o, *k, rest),
            [l, o] => (*l, *o, 0, &[][..]),
            [l] => (*l, 0, 0, &[][..]),
            _ => (0, 0, 0, &[][..]),
        };

        let mut pos = 0;
        for i in 0..=k {
            let chunk = if i < k && rest.len() - pos >= 2 {
                let len = u16::from_le_bytes([rest[pos], rest[pos + 1]]) as usize;
                let start = pos + 2;
                let end = (start + len).min(rest.len());
                pos = end;
                &rest[start..end]
            } else {
                let chunk = &rest[pos..];
                pos = rest.len();
                chunk
            };

            let level = crate::Level::from_zstd((i32::from(level_byte) + i32::from(i)) % 23);
            let mut opts = crate::EncoderOptions::new(level).checksum(opt_byte & 1 == 1);
            if opt_byte & 2 != 0 {
                opts = opts.pledged_size(Some(chunk.len() as u64));
            }
            let window = (opt_byte >> 2) & 0x0f;
            if window != 0 {
                opts = opts.with_input_shape(
                    crate::InputShape::default().with_window_log(u32::from(window) + 9),
                );
            }
            let ldm = match (opt_byte >> 6) & 3 {
                1 => crate::LdmMode::Enable,
                2 => crate::LdmMode::Disable,
                _ => crate::LdmMode::Auto,
            };
            opts = opts.long_distance_matching(ldm);

            let comp = crate::bulk::compress_with(chunk, &opts).unwrap();
            let mut back = alloc::vec::Vec::with_capacity(chunk.len() + 16);
            FrameDecoder::new()
                .decode_all_to_vec(&comp, &mut back)
                .unwrap();
            assert_eq!(
                &back[..],
                chunk,
                "pass {pass}: zstdx decode of own output differs"
            );

            let mut zback = std::vec::Vec::new();
            zstd::stream::copy_decode(comp.as_slice(), &mut zback).unwrap();
            assert_eq!(
                &zback[..],
                chunk,
                "pass {pass}: zstd decode of zstdx output differs"
            );
        }
    }
}
