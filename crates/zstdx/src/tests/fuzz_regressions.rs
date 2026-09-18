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
