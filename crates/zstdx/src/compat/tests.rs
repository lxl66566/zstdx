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
    // bulk frames carry the content size (see bulk_content_size_parity);
    // streaming-shaped ones do not (checked in upper_bound_parity below)
    assert_eq!(
        compat::bulk::Decompressor::upper_bound(&compressed),
        Some(data.len())
    );
}

/// The zstd crate's own `has_content_size` test: bulk functions include
/// the content size by default. Empty input declares an empty frame.
#[test]
fn bulk_content_size_parity() {
    let data = payload();
    let compressed = compat::bulk::compress(&data, 3).unwrap();
    assert_eq!(
        zstd::zstd_safe::get_frame_content_size(&compressed)
            .unwrap()
            .map(|size| usize::try_from(size).unwrap()),
        Some(data.len())
    );
    // same for the struct path (which compress_to_buffer routes through)
    let mut compressor = compat::bulk::Compressor::new(3).unwrap();
    let mut buffer = vec![0u8; data.len() + 1024];
    let written = compressor.compress_to_buffer(&data, &mut buffer).unwrap();
    assert_eq!(
        zstd::zstd_safe::get_frame_content_size(&buffer[..written])
            .unwrap()
            .map(|size| usize::try_from(size).unwrap()),
        Some(data.len())
    );
    // empty input: a declared zero-size frame, not an unknown size
    let empty = compat::bulk::compress(&[], 3).unwrap();
    assert_eq!(
        zstd::zstd_safe::get_frame_content_size(&empty)
            .unwrap()
            .map(|size| usize::try_from(size).unwrap()),
        Some(0)
    );
    assert_eq!(
        compat::bulk::decompress(&empty, 0).unwrap(),
        Vec::<u8>::new()
    );
}

/// The zstd crate's dictionary lifecycle on `bulk::Compressor`:
/// `set_compression_level` clears a registered dictionary,
/// `set_dictionary(level, dict)` re-registers one at a new level, and an
/// empty slice detaches it.
#[test]
fn bulk_compressor_dictionary_lifecycle() {
    let dict: Vec<u8> = b"[Unit]\nDescription=shared boilerplate block\n".repeat(48);
    let content = b"[Unit]\nDescription=shared boilerplate block\npayload line\n".repeat(64);

    let mut compressor = compat::bulk::Compressor::with_dictionary(3, &dict).unwrap();
    let with_dict = compressor.compress(&content).unwrap();
    // changing the level drops the dictionary: the frame decodes bare
    compressor.set_compression_level(1).unwrap();
    let bare = compressor.compress(&content).unwrap();
    assert_eq!(
        compat::bulk::decompress(&bare, content.len()).unwrap(),
        content
    );
    assert_ne!(bare, with_dict);
    // re-registering at the same level reproduces the frame (the level
    // changes how the content compresses, dictionary or not)
    compressor.set_dictionary(3, &dict).unwrap();
    let again = compressor.compress(&content).unwrap();
    assert_eq!(again, with_dict);
    assert!(
        compat::bulk::Decompressor::new()
            .unwrap()
            .decompress(&again, 0)
            .is_err()
    );
    // an empty dictionary detaches (a bare frame at the current level)
    compressor.set_dictionary(5, &[]).unwrap();
    let mut at_five = compat::bulk::Compressor::new(5).unwrap();
    assert_eq!(
        compressor.compress(&content).unwrap(),
        at_five.compress(&content).unwrap()
    );
}

/// The raw-dictionary attachment parses lazily, once per attachment: the
/// first compression resolves the parse and every later call reuses it
/// (the CDict-role hoist), so repeated calls and the prepared-handle path
/// produce byte-identical frames — a pure caching change. Re-setting the
/// dictionary (or the level, which detaches it) invalidates the cache.
#[test]
fn bulk_raw_dictionary_parses_once_and_caches() {
    let dict: Vec<u8> = b"[Unit]\nDescription=shared boilerplate block\n".repeat(48);
    let content = b"[Unit]\nDescription=shared boilerplate block\npayload line\n".repeat(64);

    let mut raw = compat::bulk::Compressor::with_dictionary(3, &dict).unwrap();
    let first = raw.compress(&content).unwrap();
    // repeated calls: identical bytes (the cached parse is invisible)
    assert_eq!(raw.compress(&content).unwrap(), first);
    assert_eq!(raw.compress(&content).unwrap(), first);
    // and identical to the prepared-handle path on the same bytes/level
    let prepared = compat::dict::EncoderDictionary::copy(&dict, 3);
    let mut via_prepared = compat::bulk::Compressor::with_prepared_dictionary(&prepared).unwrap();
    assert_eq!(via_prepared.compress(&content).unwrap(), first);

    // a level change detaches (the cache dies with the attachment): the
    // bare frame at the new level matches a fresh context's
    raw.set_compression_level(1).unwrap();
    let mut plain = compat::bulk::Compressor::new(1).unwrap();
    assert_eq!(
        raw.compress(&content).unwrap(),
        plain.compress(&content).unwrap()
    );

    // re-registering the dictionary parses afresh and reproduces the frame
    raw.set_dictionary(3, &dict).unwrap();
    assert_eq!(raw.compress(&content).unwrap(), first);
    // a different dictionary re-registers cleanly over the cache
    let other = dict.iter().copied().rev().collect::<Vec<u8>>();
    raw.set_dictionary(3, &other).unwrap();
    let swapped = raw.compress(&content).unwrap();
    let mut fresh = compat::bulk::Compressor::with_dictionary(3, &other).unwrap();
    assert_eq!(swapped, fresh.compress(&content).unwrap());
    assert_ne!(swapped, first);
}

/// An invalid raw dictionary errors at the first compression (not at
/// attachment), keeps erroring deterministically on every later call, and
/// a valid re-set recovers — exactly the pre-cache timing, replayed from
/// the cached failure.
#[test]
fn bulk_raw_dictionary_invalid_errors_at_first_compress() {
    use crate::decoding::dictionary::MAGIC_NUM;
    let mut compressor = compat::bulk::Compressor::new(3).unwrap();
    compressor.set_dictionary(3, &MAGIC_NUM).unwrap();
    let first = compressor.compress(b"payload").unwrap_err();
    // deterministic replay on the next calls
    assert_eq!(
        format!("{}", compressor.compress(b"payload").unwrap_err()),
        format!("{first}")
    );
    assert!(compressor.compress(b"payload").is_err());
    // a valid re-set invalidates the cached failure
    let dict: Vec<u8> = b"[Unit]\nDescription=shared boilerplate block\n".repeat(48);
    compressor.set_dictionary(3, &dict).unwrap();
    let frame = compressor.compress(b"payload").unwrap();
    let mut fresh = compat::bulk::Compressor::with_dictionary(3, &dict).unwrap();
    assert_eq!(frame, fresh.compress(b"payload").unwrap());
}

/// Prepared dictionaries through the whole compat surface: the parse is
/// shared, compression runs at the dictionary's level, and a dictionary
/// that failed to parse replays the failure at first use (libzstd's
/// behavior, since `copy` is infallible there too).
#[test]
fn bulk_prepared_dictionaries() {
    let dict: Vec<u8> = b"[Unit]\nDescription=shared boilerplate block\n".repeat(48);
    let content = b"[Unit]\nDescription=shared boilerplate block\npayload line\n".repeat(64);

    let encoder_dict = compat::dict::EncoderDictionary::copy(&dict, 3);
    let mut compressor = compat::bulk::Compressor::with_prepared_dictionary(&encoder_dict).unwrap();
    let compressed = compressor.compress(&content).unwrap();
    // the prepared dictionary's frames still declare the content size
    assert_eq!(
        zstd::zstd_safe::get_frame_content_size(&compressed)
            .unwrap()
            .map(|size| usize::try_from(size).unwrap()),
        Some(content.len())
    );

    let decoder_dict = compat::dict::DecoderDictionary::copy(&dict);
    let mut decompressor =
        compat::bulk::Decompressor::with_prepared_dictionary(&decoder_dict).unwrap();
    assert_eq!(decompressor.decompress(&compressed, 0).unwrap(), content);
    // set_dictionary / set_prepared_dictionary swap dictionaries mid-life
    let mut plain = compat::bulk::Decompressor::new().unwrap();
    plain.set_prepared_dictionary(&decoder_dict).unwrap();
    assert_eq!(plain.decompress(&compressed, 0).unwrap(), content);
    plain.set_dictionary(&dict).unwrap();
    assert_eq!(plain.decompress(&compressed, 0).unwrap(), content);
    // and a prepared encoder dictionary attaches the same way
    let mut re = compat::bulk::Compressor::new(1).unwrap();
    re.set_prepared_dictionary(&encoder_dict).unwrap();
    assert_eq!(re.compress(&content).unwrap(), compressed);
}

/// A dictionary that does not parse still constructs (the zstd crate's
/// `copy` is infallible too — its `CDict`/`DDict` creation panics there,
/// this port replays the failure as an error instead). The encoder side
/// surfaces it at the first compression, like its byte-dictionary path;
/// the decoder side, which validates at attach, refuses it at the
/// constructor.
#[test]
fn prepared_dictionaries_replay_parse_errors() {
    use crate::decoding::dictionary::MAGIC_NUM;
    let encoder_dict = compat::dict::EncoderDictionary::copy(&MAGIC_NUM, 3);
    let mut compressor = compat::bulk::Compressor::with_prepared_dictionary(&encoder_dict).unwrap();
    let first = compressor.compress(b"payload").unwrap_err();
    // deterministic replay on the next call
    assert_eq!(
        format!("{}", compressor.compress(b"payload").unwrap_err()),
        format!("{first}")
    );

    let decoder_dict = compat::dict::DecoderDictionary::copy(&MAGIC_NUM);
    assert!(compat::bulk::Decompressor::with_prepared_dictionary(&decoder_dict).is_err());
    let mut plain = compat::bulk::Decompressor::new().unwrap();
    assert!(plain.set_prepared_dictionary(&decoder_dict).is_err());
    // the decompressor stays usable after a refused attach
    let data = payload();
    let compressed = compat::bulk::compress(&data, 3).unwrap();
    assert_eq!(plain.decompress(&compressed, 0).unwrap(), data);
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
fn stream_write_encoder_multithread() {
    let data = payload();
    // multithread(4) before the first write engages the native MT core; the
    // frame decodes through the compat layer like any other
    let mut sink = Vec::new();
    let mut enc = compat::stream::write::Encoder::new(&mut sink, 3).unwrap();
    enc.multithread(4).unwrap();
    enc.write_all(&data).unwrap();
    // parameters (including multithread itself) fail once streaming started
    assert!(enc.multithread(2).is_err());
    enc.finish().unwrap();
    assert_eq!(compat::bulk::decompress(&sink, data.len()).unwrap(), data);

    // workers <= 1 stays single-threaded and still roundtrips
    let mut sink = Vec::new();
    let mut enc = compat::stream::write::Encoder::new(&mut sink, 3).unwrap();
    enc.multithread(0).unwrap();
    enc.write_all(&data).unwrap();
    enc.finish().unwrap();
    assert_eq!(compat::bulk::decompress(&sink, data.len()).unwrap(), data);
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

/// The read encoder takes multithread like the write encoder: worker counts
/// engage the native MT core before the first read, and the parameter is
/// rejected once streaming started.
#[test]
fn stream_read_encoder_multithread() {
    let data = payload();
    for workers in [2u32, 4] {
        let mut enc = compat::stream::read::Encoder::new(data.as_slice(), 3).unwrap();
        enc.multithread(workers).unwrap();
        let mut compressed = Vec::new();
        enc.read_to_end(&mut compressed).unwrap();
        // parameters fail once streaming started
        assert!(enc.multithread(2).is_err());
        enc.finish().unwrap();
        assert_eq!(
            compat::bulk::decompress(&compressed, data.len()).unwrap(),
            data
        );
    }

    // workers <= 1 stays single-threaded and still roundtrips
    let mut enc = compat::stream::read::Encoder::new(data.as_slice(), 3).unwrap();
    enc.multithread(0).unwrap();
    let mut compressed = Vec::new();
    enc.read_to_end(&mut compressed).unwrap();
    enc.finish().unwrap();
    assert_eq!(
        compat::bulk::decompress(&compressed, data.len()).unwrap(),
        data
    );
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

/// Both bulk struct paths honor a dictionary attached at construction:
/// `decompress` (the growing-Vec path) must decode through the same
/// decompressor as `decompress_to_buffer`, not a fresh contextless one.
#[test]
fn bulk_decompressor_applies_dictionary() {
    let dict: Vec<u8> = b"[Unit]\nDescription=shared boilerplate block\n".repeat(48);
    let content = b"[Unit]\nDescription=shared boilerplate block\npayload line\n".repeat(64);
    let mut compressor = compat::bulk::Compressor::with_dictionary(3, &dict).unwrap();
    let compressed = compressor.compress(&content).unwrap();

    let mut dec = compat::bulk::Decompressor::with_dictionary(&dict).unwrap();
    assert_eq!(dec.decompress(&compressed, 0).unwrap(), content);
    let mut out = vec![0u8; content.len()];
    let written = dec.decompress_to_buffer(&compressed, &mut out).unwrap();
    assert_eq!(&out[..written], &content);

    // Without the dictionary the frame is undecodable (matches into the
    // dictionary's history have no referent).
    assert!(
        compat::bulk::Decompressor::new()
            .unwrap()
            .decompress(&compressed, 0)
            .is_err()
    );
}

/// A failed materialize must hand the writer back instead of dropping it:
/// `get_mut` and `get_ref` stay usable afterwards.
#[test]
fn failed_materialize_keeps_writer() {
    use crate::decoding::dictionary::MAGIC_NUM;
    // the dictionary magic alone does not parse as a dictionary
    let mut encoder =
        compat::stream::write::Encoder::with_dictionary(Vec::new(), 3, &MAGIC_NUM).unwrap();
    assert!(encoder.write(b"payload").is_err());
    encoder.get_mut().extend_from_slice(b"kept");
    assert_eq!(encoder.get_ref().as_slice(), b"kept");
    // retrying keeps failing on the same options without swallowing the writer
    assert!(encoder.write(b"more").is_err());
    assert_eq!(encoder.get_ref().as_slice(), b"kept");
}

/// The read decoder mirrors the writer contract: a failed materialize hands
/// the source back, so repeated reads fail deterministically (no panic on a
/// missing source) and get_mut/finish stay usable.
#[test]
fn failed_materialize_keeps_source() {
    use crate::decoding::dictionary::MAGIC_NUM;
    // the dictionary magic alone does not parse as a dictionary, so nothing
    // is read from the source before the failure
    let mut decoder =
        compat::stream::read::Decoder::with_dictionary(b"compressed".as_slice(), &MAGIC_NUM)
            .unwrap();
    let mut out = [0u8; 4];
    let first = format!("{:?}", decoder.read(&mut out).unwrap_err());
    assert_eq!(format!("{:?}", decoder.read(&mut out).unwrap_err()), first);
    assert_eq!(*decoder.get_mut(), b"compressed");
    assert_eq!(decoder.finish(), b"compressed");
}

/// A transient error while materializing the first frame must not brick the
/// decoder: the retry re-runs the construction and succeeds once the source
/// serves data.
#[test]
fn transient_materialize_error_is_retryable() {
    struct FlakySource {
        data: Vec<u8>,
        pos: usize,
        failed: bool,
    }
    impl Read for FlakySource {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if !self.failed {
                self.failed = true;
                return Err(std::io::ErrorKind::WouldBlock.into());
            }
            let n = (self.data.len() - self.pos).min(buf.len());
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    let data = payload();
    let compressed = compat::bulk::compress(&data, 3).unwrap();
    let mut decoder = compat::stream::read::Decoder::new(FlakySource {
        data: compressed,
        pos: 0,
        failed: false,
    })
    .unwrap();
    // the first read hits the transient error ...
    assert!(decoder.read(&mut [0u8; 16]).is_err());
    // ... and the retry decodes the whole stream once the source recovers
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).unwrap();
    assert_eq!(out, data);
}

/// `upper_bound` must mirror `ZSTD_getFrameContentSize` (through
/// `zstd_safe`): a header-declared size as `Some`, a header without the
/// FCS field as `None` — unknown, not a bogus zero — a complete skippable
/// frame as `Some(0)`, and unreadable heads as `None`.
#[test]
fn upper_bound_parity_with_get_frame_content_size() {
    let data = payload();
    let frames: Vec<Vec<u8>> = vec![
        // no FCS field (streaming shape, unpledged)
        compat::stream::encode_all(data.as_slice(), 3).unwrap(),
        // FCS declared (the bulk paths pledge it, as libzstd's one-shot does)
        compat::bulk::compress(&data, 3).unwrap(),
        // FCS declared (libzstd's one-shot always does)
        zstd::bulk::compress(&data, 3).unwrap(),
        // a complete skippable frame (4 content bytes)
        [0x50u8, 0x2a, 0x4d, 0x18, 4, 0, 0, 0, 1, 2, 3, 4].to_vec(),
        // a truncated skippable head
        [0x50u8, 0x2a, 0x4d, 0x18, 4, 0, 0].to_vec(),
        // a truncated zstd head past the magic
        compat::bulk::compress(&data, 3).unwrap()[..6].to_vec(),
        // not a frame at all
        b"junk junk junk".to_vec(),
    ];
    for frame in &frames {
        let reference = zstd::zstd_safe::get_frame_content_size(frame)
            .ok()
            .flatten()
            .and_then(|size| usize::try_from(size).ok());
        assert_eq!(
            compat::bulk::Decompressor::upper_bound(frame),
            reference,
            "upper_bound diverged from the reference on {} bytes",
            frame.len()
        );
    }
}

/// The compat trainers mirror `zstd::dict`: the continuous/sample-list/
/// sample-iterator entry points agree with each other, mismatched sizes
/// error with the exact upstream message, and a trained dictionary
/// round-trips through the compat stream shapes (`io::copy` through an
/// auto-finishing dictionary encoder, decode through a dictionary decoder)
/// the way the zstd crate's own `test_dict_training` does.
#[cfg(feature = "dict_builder")]
#[test]
fn dict_trainers_and_roundtrip() {
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
    let from_list = compat::dict::from_samples(&samples, 8 * 1024).unwrap();

    // the continuous form over the same data trains the same dictionary
    let mut data = Vec::new();
    let mut sizes = Vec::new();
    for sample in &samples {
        data.extend_from_slice(sample);
        sizes.push(sample.len());
    }
    assert_eq!(
        compat::dict::from_continuous(&data, &sizes, 8 * 1024).unwrap(),
        from_list
    );
    // ... and the sample-iterator form feeds the continuous form
    let cursors = samples.iter().map(|s| Ok(std::io::Cursor::new(s.clone())));
    assert_eq!(
        compat::dict::from_sample_iterator(cursors, 8 * 1024).unwrap(),
        from_list
    );
    // mismatched sizes error with the zstd crate's exact message
    let err =
        compat::dict::from_continuous(&data, &sizes[..sizes.len() - 1], 8 * 1024).unwrap_err();
    assert_eq!(format!("{err}"), "sample sizes don't add up");
    // read errors propagate out of the sample iterator
    let failing = [Err::<std::io::Cursor<&[u8]>, _>(std::io::Error::other(
        "boom",
    ))];
    assert_eq!(
        format!(
            "{}",
            compat::dict::from_sample_iterator(failing, 8 * 1024).unwrap_err()
        ),
        "boom"
    );

    // the trained dictionary round-trips through the stream shapes with
    // io::copy, exactly as the zstd crate's own training test does
    let content =
        b"{\"user\":\"user_5\",\"event\":\"click\",\"score\":3,\"tags\":[\"a\",\"b\"]}".repeat(64);
    let mut buffer = Vec::new();
    {
        let mut encoder =
            compat::stream::write::Encoder::with_dictionary(&mut buffer, 1, &from_list)
                .unwrap()
                .auto_finish();
        std::io::copy(&mut content.as_slice(), &mut encoder).unwrap();
    }
    let mut result = Vec::new();
    {
        let mut decoder =
            compat::stream::read::Decoder::with_dictionary(buffer.as_slice(), &from_list).unwrap();
        decoder.read_to_end(&mut result).unwrap();
    }
    assert_eq!(result, content);

    // from_files: each file one sample, agreeing with from_samples
    let dir = std::env::temp_dir().join("zstdx-r52-dict-train");
    std::fs::create_dir_all(&dir).unwrap();
    let paths: Vec<std::path::PathBuf> = samples
        .iter()
        .enumerate()
        .map(|(i, sample)| {
            let path = dir.join(format!("sample-{i}"));
            std::fs::write(&path, sample).unwrap();
            path
        })
        .collect();
    assert_eq!(
        compat::dict::from_files(&paths, 8 * 1024).unwrap(),
        from_list
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The write encoder's parameter surface: `window_log` and
/// `include_contentsize` before the first write (rejected after), with
/// the zstd crate's `test_flush` shape (flush mid-stream, then finish).
#[test]
fn stream_write_encoder_parameters() {
    let data = payload();

    // window_log still round-trips (clamped by the format's 10..=27)
    let mut sink = Vec::new();
    let mut enc = compat::stream::write::Encoder::new(&mut sink, 3).unwrap();
    enc.window_log(10).unwrap();
    enc.write_all(&data).unwrap();
    assert!(enc.window_log(12).is_err());
    enc.finish().unwrap();
    assert_eq!(compat::bulk::decompress(&sink, data.len()).unwrap(), data);

    // include_contentsize(false): the pledge sizes the frame but the
    // header carries no content-size field
    let mut sink = Vec::new();
    let mut enc = compat::stream::write::Encoder::new(&mut sink, 3).unwrap();
    enc.set_pledged_src_size(Some(data.len() as u64)).unwrap();
    enc.include_contentsize(false).unwrap();
    enc.write_all(&data).unwrap();
    assert!(enc.include_contentsize(true).is_err());
    enc.finish().unwrap();
    assert_eq!(
        zstd::zstd_safe::get_frame_content_size(&sink).unwrap(),
        None,
        "the header must not declare the content size"
    );
    assert_eq!(compat::bulk::decompress(&sink, 0).unwrap(), data);

    // include_contentsize(true) (the default): the pledge is written
    let mut sink = Vec::new();
    let mut enc = compat::stream::write::Encoder::new(&mut sink, 3).unwrap();
    enc.set_pledged_src_size(Some(data.len() as u64)).unwrap();
    enc.write_all(&data).unwrap();
    enc.finish().unwrap();
    assert_eq!(
        zstd::zstd_safe::get_frame_content_size(&sink)
            .unwrap()
            .map(|size| usize::try_from(size).unwrap()),
        Some(data.len())
    );

    // the zstd crate's test_flush shape: flush mid-stream, finish, decode
    let sink = Vec::new();
    let mut enc = compat::stream::write::Encoder::new(sink, 19).unwrap();
    enc.write_all(b"hello").unwrap();
    enc.flush().unwrap();
    let sink = enc.finish().unwrap();
    assert_eq!(
        compat::stream::decode_all(sink.as_slice()).unwrap(),
        b"hello"
    );
}

/// The read encoder gains the same parameter setters as the write side,
/// plus the dictionary and buffer constructors.
#[test]
fn stream_read_encoder_parameters() {
    let data = payload();

    // with_dictionary / with_buffer
    let dict: Vec<u8> = b"[Unit]\nDescription=shared boilerplate block\n".repeat(48);
    let mut enc =
        compat::stream::read::Encoder::with_dictionary(data.as_slice(), 3, &dict).unwrap();
    let mut compressed = Vec::new();
    enc.read_to_end(&mut compressed).unwrap();
    enc.finish().unwrap();
    let mut dec =
        compat::stream::read::Decoder::with_dictionary(compressed.as_slice(), &dict).unwrap();
    let mut out = Vec::new();
    dec.read_to_end(&mut out).unwrap();
    assert_eq!(out, data);

    let mut enc = compat::stream::read::Encoder::with_buffer(data.as_slice(), 3).unwrap();
    // parameters before the first read; rejected after it started
    enc.set_pledged_src_size(Some(data.len() as u64)).unwrap();
    enc.include_checksum(false).unwrap();
    enc.window_log(10).unwrap();
    enc.include_contentsize(false).unwrap();
    let mut compressed = Vec::new();
    enc.read_to_end(&mut compressed).unwrap();
    assert!(enc.set_pledged_src_size(None).is_err());
    assert!(enc.include_checksum(true).is_err());
    assert!(enc.window_log(20).is_err());
    assert!(enc.include_contentsize(true).is_err());
    enc.finish().unwrap();
    // the pledge did not reach the header (contentsize off), but the
    // stream still enforced nothing and round-trips
    assert_eq!(
        zstd::zstd_safe::get_frame_content_size(&compressed).unwrap(),
        None
    );
    assert_eq!(compat::bulk::decompress(&compressed, 0).unwrap(), data);
}

/// A writer that counts flushes, for the auto-flush semantics test.
struct CountingFlush<'a> {
    sink: &'a mut Vec<u8>,
    count: &'a std::cell::Cell<usize>,
}

impl Write for CountingFlush<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.sink.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.count.set(self.count.get() + 1);
        Ok(())
    }
}

/// `AutoFlushDecoder` follows the zstd crate's semantics: writes buffer,
/// the wrapper's drop performs the flush and reports it to `on_flush`.
#[test]
fn stream_write_decoder_auto_flush_on_drop() {
    let data = payload();
    let compressed = compat::bulk::compress(&data, 3).unwrap();
    let mut sink = Vec::new();
    let flushes = std::cell::Cell::new(0usize);
    let callback_ran = std::cell::Cell::new(false);
    {
        let mut counting = CountingFlush {
            sink: &mut sink,
            count: &flushes,
        };
        {
            let mut dec = compat::stream::write::Decoder::new(&mut counting)
                .unwrap()
                .on_flush(|res| {
                    res.unwrap();
                    callback_ran.set(true);
                });
            dec.write_all(&compressed).unwrap();
            assert_eq!(flushes.get(), 0, "writes buffer; the flush comes at drop");
        }
        assert_eq!(flushes.get(), 1);
    }
    assert!(callback_ran.get());
    assert_eq!(sink, data);

    // plain auto_flush flushes at drop too, and into_inner hands the
    // decoder back without a flush
    let flushes = std::cell::Cell::new(0usize);
    let mut sink = Vec::new();
    {
        let mut counting = CountingFlush {
            sink: &mut sink,
            count: &flushes,
        };
        let mut dec = compat::stream::write::Decoder::new(&mut counting)
            .unwrap()
            .auto_flush();
        dec.write_all(&compressed).unwrap();
        let mut dec = dec.into_inner();
        assert_eq!(flushes.get(), 0);
        dec.flush().unwrap();
        assert_eq!(flushes.get(), 1);
    }
    assert_eq!(sink, data);
}

/// Prepared dictionaries on the stream types: compression runs at the
/// dictionary's level, decode attaches the shared parse, and an invalid
/// prepared dictionary is refused where each side validates.
#[test]
fn stream_prepared_dictionaries() {
    use crate::decoding::dictionary::MAGIC_NUM;

    let dict: Vec<u8> = b"[Unit]\nDescription=shared boilerplate block\n".repeat(48);
    let content = b"[Unit]\nDescription=shared boilerplate block\npayload line\n".repeat(64);
    let encoder_dict = compat::dict::EncoderDictionary::copy(&dict, 3);
    let decoder_dict = compat::dict::DecoderDictionary::copy(&dict);

    // write encoder -> read decoder through prepared dictionaries
    let mut compressed = Vec::new();
    let mut enc =
        compat::stream::write::Encoder::with_prepared_dictionary(&mut compressed, &encoder_dict)
            .unwrap();
    enc.write_all(&content).unwrap();
    enc.finish().unwrap();
    let mut dec = compat::stream::read::Decoder::with_prepared_dictionary(
        compressed.as_slice(),
        &decoder_dict,
    )
    .unwrap();
    let mut out = Vec::new();
    dec.read_to_end(&mut out).unwrap();
    assert_eq!(out, content);

    // the read encoder takes one too, and the write decoder decodes through it
    let mut enc =
        compat::stream::read::Encoder::with_prepared_dictionary(content.as_slice(), &encoder_dict)
            .unwrap();
    let mut round = Vec::new();
    enc.read_to_end(&mut round).unwrap();
    enc.finish().unwrap();
    let mut out = Vec::new();
    {
        let mut dec =
            compat::stream::write::Decoder::with_prepared_dictionary(&mut out, &decoder_dict)
                .unwrap();
        dec.write_all(&round).unwrap();
        dec.flush().unwrap();
    }
    assert_eq!(out, content);

    // invalid prepared dictionaries: refused at the stream constructors
    // (the parse replays there), on every side
    let bad_enc = compat::dict::EncoderDictionary::copy(&MAGIC_NUM, 3);
    let bad_dec = compat::dict::DecoderDictionary::copy(&MAGIC_NUM);
    assert!(
        compat::stream::write::Encoder::with_prepared_dictionary(Vec::new(), &bad_enc).is_err()
    );
    assert!(
        compat::stream::read::Encoder::with_prepared_dictionary(content.as_slice(), &bad_enc)
            .is_err()
    );
    assert!(
        compat::stream::read::Decoder::with_prepared_dictionary(content.as_slice(), &bad_dec)
            .is_err()
    );
    assert!(
        compat::stream::write::Decoder::with_prepared_dictionary(Vec::new(), &bad_dec).is_err()
    );
}

/// The re-exports mirror the zstd crate's: `compat::Encoder` is the write
/// encoder, `compat::Decoder` the read decoder, both also visible at
/// `compat::stream::` together with `AutoFinishEncoder`.
#[test]
fn reexport_shape_matches_zstd() {
    let mut sink = Vec::new();
    let mut enc: compat::Encoder<&mut Vec<u8>> = compat::Encoder::new(&mut sink, 1).unwrap();
    enc.write_all(b"payload").unwrap();
    let _ = enc.finish().unwrap();
    let mut dec: compat::Decoder<&[u8]> = compat::Decoder::new(sink.as_slice()).unwrap();
    let mut out = Vec::new();
    dec.read_to_end(&mut out).unwrap();
    assert_eq!(out, b"payload");

    let mut sink = Vec::new();
    let _: compat::stream::Encoder<&mut Vec<u8>> =
        compat::stream::Encoder::new(&mut sink, 1).unwrap();
    let _: compat::stream::Decoder<&[u8]> = compat::stream::Decoder::new(sink.as_slice()).unwrap();
    let _: compat::stream::AutoFinishEncoder<&mut Vec<u8>> =
        compat::stream::write::Encoder::new(&mut sink, 1)
            .unwrap()
            .auto_finish();
}

/// Seeded corruption smoke over the prepared-dictionary decode surface
/// (R53): pseudo-random mutations and truncations of a dict frame walk the
/// bulk struct, the read decoder, and the write decoder in both its flush
/// shapes (explicit, and the auto-flush wrapper's drop flush). Every
/// outcome must be an `io::Result`, never a panic; a surviving decode is
/// allowed (a mutation can land in an undecoded region), but whatever a
/// truncated prefix decodes to must be a prefix of the true payload.
#[test]
fn prepared_dict_corruption_smoke() {
    let Ok(dict_raw) = std::fs::read("./dict_tests/dictionary") else {
        return;
    };
    let encoder_dict = compat::dict::EncoderDictionary::copy(&dict_raw, 3);
    let decoder_dict = compat::dict::DecoderDictionary::copy(&dict_raw);

    let content = b"[Unit]\nDescription=shared boilerplate block\n".repeat(256);
    let frame = {
        let mut compressor =
            compat::bulk::Compressor::with_prepared_dictionary(&encoder_dict).unwrap();
        compressor.compress(&content).unwrap()
    };
    // the frame really is dictionary-bound (a bare decode refuses it)
    assert!(
        compat::bulk::Decompressor::new()
            .unwrap()
            .decompress(&frame, 0)
            .is_err()
    );

    let bulk_decode = |corrupt: &[u8]| {
        compat::bulk::Decompressor::with_prepared_dictionary(&decoder_dict)
            .unwrap()
            .decompress(corrupt, 1)
    };
    let read_decode = |corrupt: &[u8]| {
        compat::stream::read::Decoder::with_prepared_dictionary(corrupt, &decoder_dict)
            .unwrap()
            .read_to_end(&mut Vec::new())
    };
    let write_decode = |corrupt: &[u8]| -> std::io::Result<()> {
        let mut sink = Vec::new();
        let mut dec =
            compat::stream::write::Decoder::with_prepared_dictionary(&mut sink, &decoder_dict)
                .unwrap();
        dec.write_all(corrupt)?;
        Write::flush(&mut dec)?;
        Ok(())
    };
    let drop_flush_decode = |corrupt: &[u8]| -> std::io::Result<()> {
        let mut sink = Vec::new();
        {
            let mut dec =
                compat::stream::write::Decoder::with_prepared_dictionary(&mut sink, &decoder_dict)
                    .unwrap()
                    .auto_flush();
            dec.write_all(corrupt)?;
        } // the drop performs the flush
        Ok(())
    };

    let mut s = 0x0123_4567_89ab_cdefu64;
    let mut rand = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    for round in 0..96u64 {
        let mut corrupt = frame.clone();
        // 1-4 byte flips, seeded
        for _ in 0..=(round % 4) {
            let at = (rand() as usize) % corrupt.len();
            corrupt[at] ^= (rand() >> 32) as u8 | 1;
        }
        let _ = bulk_decode(&corrupt);
        let _ = read_decode(&corrupt);
        let _ = write_decode(&corrupt);
        let _ = drop_flush_decode(&corrupt);

        // a truncated prefix: a surviving bulk decode must be a prefix of
        // the payload, never longer than it
        let cut = (rand() as usize) % corrupt.len();
        if let Ok(out) = bulk_decode(&corrupt[..cut]) {
            assert!(out.len() <= content.len());
            assert_eq!(&out[..], &content[..out.len()]);
        }
    }

    // edge dictionaries through the prepared surface: an empty raw dict
    // constructs, compresses and decodes like a bare frame
    let empty_dict = compat::dict::EncoderDictionary::copy(&[], 3);
    let empty_dec_dict = compat::dict::DecoderDictionary::copy(&[]);
    let mut compressor = compat::bulk::Compressor::with_prepared_dictionary(&empty_dict).unwrap();
    let one = compressor.compress(b"x").unwrap();
    let mut decoder =
        compat::bulk::Decompressor::with_prepared_dictionary(&empty_dec_dict).unwrap();
    assert_eq!(decoder.decompress(&one, 0).unwrap(), b"x");
}

/// `include_dictid` across the compat surface: the flag lands in the
/// frame header (no Dictionary_ID), the dictionary still applies, and the
/// zstd crate's own suppressed frames decode through the compat layer.
/// (R55)
#[test]
fn include_dictid_parity() {
    let samples: Vec<Vec<u8>> = (0..96u32)
        .map(|i| {
            format!(
                "{{\"user\":\"user_{i}\",\"event\":\"click\",\"score\":{},\"tags\":[\"a\",\"b\"\
                 ]}}\n",
                i % 7
            )
            .into_bytes()
        })
        .collect();
    let Ok(dict) = zstd::dict::from_samples(&samples, 8 * 1024) else {
        return;
    };
    let content = b"{\"user\":\"user_5\",\"event\":\"click\",\"score\":3,\"tags\":[\"a\",\"b\"]}\n"
        .repeat(96);

    // write-side stream encoder: parameter before the first write
    let mut encoder =
        compat::stream::write::Encoder::with_dictionary(Vec::new(), 3, &dict).unwrap();
    encoder.include_dictid(false).unwrap();
    encoder.write_all(&content).unwrap();
    let bare = encoder.finish().unwrap();
    assert_eq!(zstd::zstd_safe::get_dict_id_from_frame(&bare), None);
    // the setter refuses once streaming started
    let mut encoder =
        compat::stream::write::Encoder::with_dictionary(Vec::new(), 3, &dict).unwrap();
    encoder.write_all(&content).unwrap();
    assert!(encoder.include_dictid(false).is_err());

    // read-side stream encoder
    let mut rencoder =
        compat::stream::read::Encoder::with_dictionary(content.as_slice(), 3, &dict).unwrap();
    rencoder.include_dictid(false).unwrap();
    let mut rbare = Vec::new();
    rencoder.read_to_end(&mut rbare).unwrap();
    assert_eq!(zstd::zstd_safe::get_dict_id_from_frame(&rbare), None);

    // bulk compressor: the flag survives a dictionary re-register
    let mut compressor = compat::bulk::Compressor::with_dictionary(3, &dict).unwrap();
    compressor.include_dictid(false).unwrap();
    compressor.set_dictionary(5, &dict).unwrap();
    let cbare = compressor.compress(&content).unwrap();
    assert_eq!(zstd::zstd_safe::get_dict_id_from_frame(&cbare), None);

    // every suppressed frame still needs the dictionary to decode, and
    // decodes through the compat layer with it
    for frame in [&bare, &rbare, &cbare] {
        let mut decoder =
            compat::stream::read::Decoder::with_dictionary(frame.as_slice(), &dict).unwrap();
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).unwrap();
        assert_eq!(out, content);
    }

    // libzstd's suppressed frames decode through the compat layer
    let mut zenc = zstd::bulk::Compressor::with_dictionary(3, &dict).unwrap();
    zenc.include_dictid(false).unwrap();
    let zbare = zenc.compress(&content).unwrap();
    let mut decoder =
        compat::stream::read::Decoder::with_dictionary(zbare.as_slice(), &dict).unwrap();
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).unwrap();
    assert_eq!(out, content);
}

/// `long_distance_matching` across the compat surface (R55): the upstream
/// bool maps true to force-enable (with libzstd's window-27 default when
/// no window was set) and false back to auto — it cannot express an
/// explicit disable. Both engines' frames decode on either side.
#[test]
fn long_distance_matching_parity() {
    use std::io::Read as _;

    // 4 MiB of a 1 MiB tile: repeats beyond the stock fast window.
    let tile: Vec<u8> = {
        let mut tile = vec![0u8; 1 << 20];
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        for b in &mut tile {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *b = (state >> 33) as u8;
        }
        tile
    };
    let mut data = tile.clone();
    data.extend_from_slice(&tile);
    data.extend_from_slice(&tile);
    data.extend_from_slice(&tile);

    // write-side stream encoder: enable before the first write
    let mut encoder = compat::stream::write::Encoder::new(Vec::new(), 1).unwrap();
    encoder.long_distance_matching(true).unwrap();
    encoder.write_all(&data).unwrap();
    let enabled = encoder.finish().unwrap();
    // false restores the auto default: byte-identical to a fresh encoder
    let mut auto_encoder = compat::stream::write::Encoder::new(Vec::new(), 1).unwrap();
    auto_encoder.write_all(&data).unwrap();
    let auto = auto_encoder.finish().unwrap();
    let mut off_encoder = compat::stream::write::Encoder::new(Vec::new(), 1).unwrap();
    off_encoder.long_distance_matching(true).unwrap();
    off_encoder.long_distance_matching(false).unwrap();
    off_encoder.write_all(&data).unwrap();
    assert_eq!(off_encoder.finish().unwrap(), auto);
    assert!(enabled.len() < auto.len());
    // the parameter refuses once streaming started
    let mut started = compat::stream::write::Encoder::new(Vec::new(), 1).unwrap();
    started.write_all(&data).unwrap();
    assert!(started.long_distance_matching(true).is_err());

    // read-side stream encoder and the bulk compressor agree with it
    let mut rencoder = compat::stream::read::Encoder::new(data.as_slice(), 1).unwrap();
    rencoder.long_distance_matching(true).unwrap();
    let mut renabled = Vec::new();
    rencoder.read_to_end(&mut renabled).unwrap();
    assert_eq!(renabled, enabled);
    let mut compressor = compat::bulk::Compressor::new(1).unwrap();
    compressor.long_distance_matching(true).unwrap();
    // The bulk compressor pledges the content size (its frames carry the
    // field; the unpledged stream frames above do not), so the equality
    // target is the pledged zstdx entry with the same switch, not the
    // stream frame.
    let pledged = crate::bulk::compress_with(
        &data,
        &crate::EncoderOptions::new(crate::Level::Fastest)
            .long_distance_matching(crate::LdmMode::Enable)
            .pledged_size(Some(data.len() as u64)),
    )
    .unwrap();
    assert_eq!(compressor.compress(&data).unwrap(), pledged);
    assert_eq!(crate::bulk::decompress(&pledged, data.len()).unwrap(), data);

    // both engines decode both engines' frames
    assert_eq!(
        compat::stream::decode_all(enabled.as_slice()).unwrap(),
        data
    );
    let mut zenc = zstd::stream::write::Encoder::new(Vec::new(), 1).unwrap();
    zenc.long_distance_matching(true).unwrap();
    zenc.write_all(&data).unwrap();
    let zenabled = zenc.finish().unwrap();
    assert!(zenabled.len() < auto.len(), "libzstd LDM must engage too");
    let mut zauto = zstd::stream::write::Encoder::new(Vec::new(), 1).unwrap();
    zauto.write_all(&data).unwrap();
    let zauto_frame = zauto.finish().unwrap();
    assert_eq!(
        compat::stream::decode_all(zauto_frame.as_slice()).unwrap(),
        data
    );
    assert_eq!(
        zstd::stream::decode_all(enabled.as_slice()).unwrap(),
        data,
        "libzstd decodes the compat layer's LDM-enabled frame"
    );
}

/// `with_ref_prefix` across the compat surface (R55): the prefix is raw
/// content history on both sides — the engine's raw-dictionary load — so
/// a prefixed frame carries no dictionary id, decodes only with the
/// prefix, and interops with libzstd's refPrefix in both directions.
#[test]
fn ref_prefix_parity() {
    use std::io::Read as _;

    // A prefix of records and a payload sharing its head records.
    let records = |from: usize, to: usize| -> Vec<u8> {
        let mut out = Vec::new();
        for i in from..to {
            out.extend_from_slice(
                format!("record-{i:05}: frame match distance window\n").as_bytes(),
            );
        }
        out
    };
    let prefix = records(0, 4096);
    let content = records(0, 8192);

    // encode with the prefix: smaller than without it (the shared head is
    // history), and the frame header carries no dictionary id.
    let mut encoder =
        compat::stream::write::Encoder::with_ref_prefix(Vec::new(), 3, &prefix).unwrap();
    encoder.write_all(&content).unwrap();
    let prefixed = encoder.finish().unwrap();
    let mut plain = compat::stream::write::Encoder::new(Vec::new(), 3).unwrap();
    plain.write_all(&content).unwrap();
    let unprefixed = plain.finish().unwrap();
    assert!(prefixed.len() < unprefixed.len());
    assert_eq!(zstd::zstd_safe::get_dict_id_from_frame(&prefixed), None);

    // decode through the compat layer's own ref-prefix decoder
    let mut decoder =
        compat::stream::read::Decoder::with_ref_prefix(prefixed.as_slice(), &prefix).unwrap();
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).unwrap();
    assert_eq!(out, content);

    // interop: libzstd's refPrefix entries both ways
    let mut zdec =
        zstd::stream::read::Decoder::with_ref_prefix(prefixed.as_slice(), &prefix).unwrap();
    let mut zout = Vec::new();
    zdec.read_to_end(&mut zout).unwrap();
    assert_eq!(zout, content);
    let mut zenc = zstd::stream::write::Encoder::with_ref_prefix(Vec::new(), 3, &prefix).unwrap();
    zenc.write_all(&content).unwrap();
    let zprefixed = zenc.finish().unwrap();
    assert!(zprefixed.len() < unprefixed.len());
    let mut decoder =
        compat::stream::read::Decoder::with_ref_prefix(zprefixed.as_slice(), &prefix).unwrap();
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).unwrap();
    assert_eq!(out, content);
}
