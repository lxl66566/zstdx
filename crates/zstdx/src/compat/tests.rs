//! Tests for the zstd-crate compatibility layer, written in the shapes a
//! ported `zstd` crate user would write.

use std::{
    format,
    io::{Read, Write},
    vec,
    vec::Vec,
};

use crate::compat;

fn payload() -> Vec<u8> {
    (0..300 * 1024).map(|i| (i % 251) as u8).collect()
}

#[test]
fn bulk_roundtrip() {
    let data = payload();
    let compressed = compat::bulk::compress(&data, 3).unwrap();
    let decompressed = compat::bulk::decompress(&compressed, data.len()).unwrap();
    assert_eq!(decompressed, data);

    let mut buffer = vec![0u8; compressed.len()];
    // same level as above: distinct levels now produce distinct bytes
    let written = compat::bulk::compress_to_buffer(&data, &mut buffer, 3).unwrap();
    assert_eq!(&buffer[..written], &compressed[..]);
    // too-small destination is an error
    let too_small = &mut buffer[..written - 1];
    assert!(compat::bulk::compress_to_buffer(&data, too_small, 3).is_err());
    let mut out = vec![0u8; data.len() + 16];
    let n = compat::bulk::decompress_to_buffer(&compressed, &mut out).unwrap();
    assert_eq!(&out[..n], &data[..]);
}

#[test]
fn bulk_structs() {
    let data = payload();
    let mut compressor = compat::bulk::Compressor::new(1).unwrap();
    let compressed = compressor.compress(&data).unwrap();
    let mut decompressor = compat::bulk::Decompressor::new().unwrap();
    let decompressed = decompressor.decompress(&compressed, data.len()).unwrap();
    assert_eq!(decompressed, data);
    // libzstd semantics: level 0 selects the default (3)
    compressor.set_compression_level(0).unwrap();
    let mut at_default = compat::bulk::Compressor::new(3).unwrap();
    assert_eq!(
        compressor.compress(&data).unwrap(),
        at_default.compress(&data).unwrap()
    );
    compressor.set_compression_level(1).unwrap();
    assert_eq!(compressor.compress(&data).unwrap(), compressed);
    assert_eq!(
        compat::bulk::Decompressor::upper_bound(&compressed),
        Some(0) // frames from this crate carry no content size
    );
}

#[test]
fn stream_write_encoder_shapes() {
    let data = payload();
    // the canonical zstd-crate pattern: write, write, finish
    let mut sink = Vec::new();
    {
        let mut enc = compat::stream::write::Encoder::new(&mut sink, 3).unwrap();
        enc.write_all(&data).unwrap();
        enc.finish().unwrap();
    }
    let decompressed = compat::bulk::decompress(&sink, data.len()).unwrap();
    assert_eq!(decompressed, data);

    // auto_finish on drop
    let mut sink = Vec::new();
    {
        let mut enc = compat::stream::write::Encoder::new(&mut sink, 0)
            .unwrap()
            .auto_finish();
        enc.write_all(&data).unwrap();
    }
    assert_eq!(compat::bulk::decompress(&sink, data.len()).unwrap(), data);

    // on_finish callback sees the writer
    let mut sink = Vec::new();
    let seen = std::cell::RefCell::new(0usize);
    {
        let mut enc = compat::stream::write::Encoder::new(&mut sink, 1)
            .unwrap()
            .on_finish(|res: std::io::Result<&mut Vec<u8>>| {
                *seen.borrow_mut() = res.unwrap().len();
            });
        enc.write_all(&data).unwrap();
    }
    assert_eq!(*seen.borrow(), sink.len());

    // parameters before the first write
    let mut sink = Vec::new();
    let mut enc = compat::stream::write::Encoder::new(&mut sink, 1).unwrap();
    enc.set_pledged_src_size(Some(data.len() as u64)).unwrap();
    enc.include_checksum(false).unwrap();
    enc.multithread(1).unwrap();
    enc.write_all(&data).unwrap();
    // after the first write they must fail
    assert!(enc.set_pledged_src_size(None).is_err());
    assert!(enc.include_checksum(true).is_err());
    enc.finish().unwrap();
    assert_eq!(compat::bulk::decompress(&sink, 0).unwrap(), data);
}

#[test]
fn stream_write_encoder_unsupported_bits() {
    let mut enc = compat::stream::write::Encoder::new(Vec::new(), 3).unwrap();
    assert!(enc.multithread(4).is_err());
}

#[test]
fn stream_read_decoder_shapes() {
    let data = payload();
    let compressed = compat::bulk::compress(&data, 3).unwrap();

    // plain read_to_end
    let mut dec = compat::stream::read::Decoder::new(compressed.as_slice()).unwrap();
    let mut out = Vec::new();
    dec.read_to_end(&mut out).unwrap();
    assert_eq!(out, data);

    // window_log_max before the first read
    let mut dec = compat::stream::read::Decoder::new(compressed.as_slice()).unwrap();
    dec.window_log_max(27).unwrap();
    let mut out = Vec::new();
    dec.read_to_end(&mut out).unwrap();
    assert_eq!(out, data);
    // ... and after it started it must fail
    assert!(dec.window_log_max(20).is_err());

    // single_frame on a concatenated stream
    let mut doubled = compressed.clone();
    doubled.extend_from_slice(&compressed);
    let mut dec = compat::stream::read::Decoder::new(doubled.as_slice())
        .unwrap()
        .single_frame();
    let mut out = Vec::new();
    dec.read_to_end(&mut out).unwrap();
    assert_eq!(out, data);
}

#[test]
fn stream_write_decoder_shapes() {
    let data = payload();
    let compressed = compat::bulk::compress(&data, 3).unwrap();
    let mut sink = Vec::new();
    {
        let mut dec = compat::stream::write::Decoder::new(&mut sink).unwrap();
        for chunk in compressed.chunks(17 * 1024) {
            dec.write_all(chunk).unwrap();
        }
        dec.flush().unwrap();
    }
    assert_eq!(sink, data);

    // auto_flush variant
    let mut sink = Vec::new();
    {
        let mut dec = compat::stream::write::Decoder::new(&mut sink)
            .unwrap()
            .auto_flush();
        dec.write_all(&compressed).unwrap();
    }
    assert_eq!(sink, data);
}

#[test]
fn read_encoder_shape() {
    let data = payload();
    let mut enc = compat::stream::read::Encoder::new(data.as_slice(), 1).unwrap();
    let mut compressed = Vec::new();
    enc.read_to_end(&mut compressed).unwrap();
    assert_eq!(compat::bulk::decompress(&compressed, 0).unwrap(), data);
}

#[test]
fn one_liners() {
    let data = payload();
    let compressed = compat::stream::encode_all(data.as_slice(), 3).unwrap();
    assert_eq!(
        compat::stream::decode_all(compressed.as_slice()).unwrap(),
        data
    );
    let mut sink = Vec::new();
    compat::stream::copy_decode(compressed.as_slice(), &mut sink).unwrap();
    assert_eq!(sink, data);
    let mut re_encoded = Vec::new();
    compat::stream::copy_encode(data.as_slice(), &mut re_encoded, 0).unwrap();
    assert_eq!(
        compat::stream::decode_all(re_encoded.as_slice()).unwrap(),
        data
    );
}

#[test]
fn interop_with_the_zstd_crate() {
    let data = payload();
    // we decode what libzstd produced ...
    let zstd_made = zstd::bulk::compress(&data, 3).unwrap();
    assert_eq!(compat::bulk::decompress(&zstd_made, 0).unwrap(), data);
    let mut out = Vec::new();
    compat::stream::copy_decode(zstd_made.as_slice(), &mut out).unwrap();
    assert_eq!(out, data);
    // ... and libzstd decodes what we produced
    let ours = compat::bulk::compress(&data, 3).unwrap();
    let mut decoded = Vec::new();
    zstd::stream::copy_decode(ours.as_slice(), &mut decoded).unwrap();
    assert_eq!(decoded, data);
}

#[test]
fn dictionary_decoding_interop() {
    // Train a dictionary with libzstd, compress against it with libzstd, and
    // decode through the compat layer with the same dictionary.
    let samples: Vec<Vec<u8>> = (0..64u32)
        .map(|i| {
            format!(
                "{{\"user\":\"user_{i}\",\"event\":\"click\",\"score\":{},\"tags\":[\"a\",\"b\"\
                 ]}}\n",
                i % 7
            )
            .into_bytes()
        })
        .collect();
    // the training backend is optional in some builds; nothing to test then
    let Ok(dict) = zstd::dict::from_samples(&samples, 8 * 1024) else {
        return;
    };
    let content =
        b"{\"user\":\"user_5\",\"event\":\"click\",\"score\":3,\"tags\":[\"a\",\"b\"]}".repeat(64);

    let mut compressor = zstd::bulk::Compressor::with_dictionary(3, &dict).unwrap();
    let compressed = compressor.compress(&content).unwrap();
    let dict_id = zstd::zstd_safe::get_dict_id_from_frame(&compressed);
    assert!(dict_id.is_some(), "libzstd must stamp the dict id");

    let mut dec =
        compat::stream::read::Decoder::with_dictionary(compressed.as_slice(), &dict).unwrap();
    let mut out = Vec::new();
    dec.read_to_end(&mut out).unwrap();
    assert_eq!(out, content);
}
