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

/// Encoder-side crash artifacts: the input layout matches the `encode` fuzz
/// target (byte 0 level, byte 1 option bits, rest payload). Each artifact
/// must roundtrip through our decoder and the reference implementation.
#[test]
fn test_all_encode_artifacts() {
    extern crate std;
    use std::borrow::ToOwned;

    use crate::decoding::FrameDecoder;

    let Some(entries) = super::fixture_entries("../zstdx-fuzz/artifacts/encode") else {
        return;
    };
    for file in entries {
        let file_name = file.unwrap().path();

        let fnstr = file_name.to_str().unwrap().to_owned();
        if !fnstr.contains("/crash-") {
            continue;
        }

        let data = std::fs::read(file_name).unwrap();
        let (level_byte, opt_byte, payload) = match &data[..] {
            [l, o, rest @ ..] => (*l, *o, rest),
            [l] => (*l, 0, &[][..]),
            _ => (0, 0, &[][..]),
        };
        let mut opts =
            crate::EncoderOptions::new(crate::Level::from_zstd(i32::from(level_byte % 23)))
                .checksum(opt_byte & 1 == 1);
        if opt_byte & 2 != 0 {
            opts = opts.pledged_size(Some(payload.len() as u64));
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

        let comp = crate::bulk::compress_with(payload, &opts).unwrap();
        let mut back = alloc::vec::Vec::with_capacity(payload.len() + 16);
        FrameDecoder::new()
            .decode_all_to_vec(&comp, &mut back)
            .unwrap();
        assert_eq!(&back[..], payload, "zstdx decode of own output differs");

        let mut zback = std::vec::Vec::new();
        zstd::stream::copy_decode(comp.as_slice(), &mut zback).unwrap();
        assert_eq!(&zback[..], payload, "zstd decode of zstdx output differs");
    }
}
