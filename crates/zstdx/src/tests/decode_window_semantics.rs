//! Pinned window-semantics outcomes for the decode paths, generated from
//! the reference-probe table recorded in the R72 decode-side fix: crafted
//! frames exercising out-of-window offsets against libzstd's generation
//! model (`ZSTD_decodingBufferSize_internal` cap, per-sequence
//! `offset > oLitEnd - virtualStart` corruption rule, dict prefill and its
//! expiry at the first wrap).
//!
//! One-shot and streaming differ BY DESIGN — the reference accepts
//! out-of-window offsets in one-shot mode (the whole output is history,
//! `ZSTD_decompress`) and rejects them in buffered streaming once the
//! ring wraps. Each path is asserted against the probe table, never
//! against the other.
#![cfg(test)]

use alloc::{string::String, vec, vec::Vec};

/// `n` extra bits carrying `v`, preceded by a `1` marker bit, LSB-first —
/// the tail of a one-sequence offset bitstream.
fn bitstream_of(n: u32, v: u32) -> Vec<u8> {
    let region: u64 = (1u64 << n) | v as u64;
    let nbytes = ((n + 1) + 7) / 8;
    let mut out = Vec::new();
    for i in 0..nbytes {
        out.push(((region >> (8 * (nbytes - 1 - i))) & 0xff) as u8);
    }
    out.reverse();
    out
}

/// One compressed block: 10 raw literals, a single RLE-coded sequence
/// (LL 9, ML 2 => 5, OF code `of_code` with `offset - (1<<of_code)` extra
/// bits), output 9 + 5 + 1 trailing literal = 15 bytes. The stream's
/// offset value decodes to `offset - 3` (repcode bias), which the probe
/// table's names carry as-is.
fn seq_block(offset: u32) -> Vec<u8> {
    let of_code = 31 - offset.leading_zeros();
    let extra = offset - (1 << of_code);
    let mut blk = Vec::new();
    blk.push(10u8 << 3);
    blk.extend_from_slice(b"ababababab");
    blk.push(0x01);
    blk.push(0b01_01_01_00);
    blk.extend_from_slice(&[9u8, of_code as u8, 2u8]);
    blk.extend_from_slice(&bitstream_of(of_code, extra));
    blk
}

/// `pre` RLE blocks of `pre_len` bytes, then the sequence block.
/// Shared with the `fuzz_regressions` walker (its reference-agreement
/// pass replays the same classes).
pub(super) fn frame(
    wlog: u32,
    pre: usize,
    pre_len: usize,
    offset: u32,
    fcs: Option<u64>,
) -> Vec<u8> {
    let mut out = vec![0x28u8, 0xb5, 0x2f, 0xfd];
    let mut fhd = 0u8;
    if fcs.is_some() {
        fhd |= 3 << 6;
    }
    out.push(fhd);
    out.push(((wlog - 10) as u8) << 3);
    if let Some(f) = fcs {
        out.extend_from_slice(&f.to_le_bytes());
    }
    for _ in 0..pre {
        let hdr = ((pre_len as u32) << 3) | (1 << 1);
        out.extend_from_slice(&hdr.to_le_bytes()[..3]);
        out.push(0x5a);
    }
    let blk = seq_block(offset);
    let hdr = ((blk.len() as u32) << 3) | (2 << 1) | 1;
    out.extend_from_slice(&hdr.to_le_bytes()[..3]);
    out.extend_from_slice(&blk);
    out
}

/// Raw 4 KiB content dictionary (probe's dict4096.bin).
/// Raw 4096-byte dictionary; shared with the walker (same reason as
/// [`frame`]).
pub(super) fn dict4096() -> Vec<u8> {
    let mut dict = alloc::vec![0x77u8; 4096];
    dict[0] = 0x42;
    dict
}

fn decode_oneshot(frame: &[u8]) -> Result<usize, crate::decoding::errors::FrameDecoderError> {
    use crate::decoding::FrameDecoder;
    let mut dec = FrameDecoder::new();
    let mut out = alloc::vec![0u8; 1 << 20];
    dec.decode_all(frame, &mut out)
}

fn decode_stream(frame: &[u8], chunk: usize) -> Result<usize, String> {
    use crate::{decoding::StreamingDecoder, io::Read};
    let mut dec = StreamingDecoder::new(frame).unwrap();
    let mut buf = alloc::vec![0u8; chunk];
    let mut total = 0usize;
    loop {
        let n = dec.read(&mut buf).map_err(|e| alloc::format!("{e}"))?;
        total += n;
        if n == 0 {
            return Ok(total);
        }
    }
}

fn decode_oneshot_dict(
    frame: &[u8],
    dict: &[u8],
) -> Result<usize, crate::decoding::errors::FrameDecoderError> {
    use crate::decoding::{FrameDecoder, dictionary::Dictionary};
    let mut dec = FrameDecoder::new();
    dec.add_dict(Dictionary::load(dict).unwrap()).unwrap();
    let mut out = alloc::vec![0u8; 1 << 20];
    dec.decode_all(frame, &mut out)
}

fn decode_stream_dict(frame: &[u8], dict: &[u8]) -> Result<usize, String> {
    use crate::{
        decoding::{FrameDecoder, StreamingDecoder, dictionary::Dictionary},
        io::Read,
    };
    let mut decoder = FrameDecoder::new();
    decoder.add_dict(Dictionary::load(dict).unwrap()).unwrap();
    let mut dec = StreamingDecoder::new_with_decoder(frame, decoder).unwrap();
    let mut buf = alloc::vec![0u8; 4096];
    let mut total = 0usize;
    loop {
        let n = dec.read(&mut buf).map_err(|e| alloc::format!("{e}"))?;
        total += n;
        if n == 0 {
            return Ok(total);
        }
    }
}

/// Streaming generation model, window 1 KiB (cap 3136, 1000-byte blocks
/// form 3000-byte generations): offsets past the previous generation's
/// length plus the in-generation position are corruption, while anything
/// the one-shot path accepts as history stays acceptable there.
#[test]
fn window_semantics_small_window() {
    // In-window control: accepted everywhere.
    let ctrl = frame(10, 180, 1000, 900, None);
    assert_eq!(decode_oneshot(&ctrl).unwrap(), 180015);
    assert_eq!(decode_stream(&ctrl, 4096).unwrap(), 180015);

    // Out-of-window far offset: one-shot whole-history accepts, streaming
    // rejects (reach 3009 at the sequence).
    let far = frame(10, 180, 1000, 100000, None);
    assert_eq!(decode_oneshot(&far).unwrap(), 180015);
    assert!(decode_stream(&far, 4096).is_err());

    // Generation boundary: 2100 in-generation, 3100/3168/3200 past it.
    assert_eq!(
        decode_stream(&frame(10, 180, 1000, 2100, None), 4096).unwrap(),
        180015
    );
    for off in [3100u32, 3168, 3200] {
        assert!(
            decode_stream(&frame(10, 180, 1000, off, None), 4096).is_err(),
            "offset {off} must exceed the streaming generation"
        );
        assert!(
            decode_oneshot(&frame(10, 180, 1000, off, None)).is_ok(),
            "offset {off} stays within one-shot history"
        );
    }
}

/// Window 64 KiB (cap 196704): the streaming buffer does NOT enforce the
/// declared window — offsets beyond it stay acceptable while they land in
/// the current generation pair (180000-byte generation from three
/// 60000-byte RLE blocks, reach 180009), and history overflow rejects in
/// every mode.
#[test]
fn window_semantics_wide_window() {
    let accept = [
        ("fa", frame(16, 3, 60000, 100000, None)),
        ("sw70k", frame(16, 3, 60000, 70000, None)),
        ("sw131k", frame(16, 3, 60000, 131000, None)),
        ("sw131.2k", frame(16, 3, 60000, 131200, None)),
        ("sw140k", frame(16, 3, 60000, 140000, None)),
    ];
    for (name, f) in accept {
        assert!(decode_oneshot(&f).is_ok(), "{name}: oneshot");
        assert!(decode_stream(&f, 4096).is_ok(), "{name}: stream");
    }
    // Past the generation pair (six pre blocks, history 360009): streaming
    // rejects while one-shot still holds the whole history.
    for off in [200000u32, 263000, 300000, 360000] {
        let f = frame(16, 6, 60000, off, None);
        assert!(decode_oneshot(&f).is_ok(), "offset {off} oneshot");
        assert!(decode_stream(&f, 4096).is_err(), "offset {off} stream");
    }
    // Past the 180000-byte history: rejected even one-shot.
    for off in [196600u32, 197000, 260000] {
        assert!(decode_oneshot(&frame(16, 3, 60000, off, None)).is_err());
    }
}

/// A declared Frame_Content_Size caps the streaming buffer
/// (`min(fcs, window + 2*block + 64)`) and disables wrapping while it
/// covers the frame; pledges beyond the cap keep the generation model.
#[test]
fn window_semantics_pledged() {
    // fcs above the cap: buffer 3136, wraps, reach 3009 at the sequence.
    let f = frame(10, 180, 1000, 2500, Some(180015));
    assert_eq!(decode_oneshot(&f).unwrap(), 180015);
    assert_eq!(decode_stream(&f, 4096).unwrap(), 180015);

    // fcs below the cap: never wraps, whole-history envelope (one-shot
    // semantics for the streaming path).
    let f = frame(10, 3, 1000, 2500, Some(3015));
    assert_eq!(decode_oneshot(&f).unwrap(), 3015);
    assert_eq!(decode_stream(&f, 4096).unwrap(), 3015);

    // Pledge above the cap with a far offset: generation model rejects
    // under streaming. (libzstd's single-pass shortcut accepts this when
    // one call's output room covers the whole pledged frame AND the input
    // holds the whole frame; zstdx streaming always applies the generation
    // model — a documented deviation for this invalid-frame class.)
    let f = frame(10, 60, 1000, 50000, Some(60015));
    assert_eq!(decode_oneshot(&f).unwrap(), 60015);
    assert!(decode_stream(&f, 4096).is_err());
    assert!(decode_stream(&f, 65536).is_err());

    let f = frame(10, 200, 2000, 7000, Some(400015));
    assert_eq!(decode_oneshot(&f).unwrap(), 400015);
    assert!(decode_stream(&f, 4096).is_err());
}

/// Raw-dict content backfills the window: the dict extends the one-shot
/// history by its length, and streaming keeps it reachable until the first
/// ring wrap discards it (probe's dict_pre frames with dict4096.bin).
#[test]
fn window_semantics_dict() {
    let dict = dict4096();

    // 2000 bytes produced, no wrap yet: dict + output reachable in both.
    let f = frame(10, 2, 1000, 4000, None);
    assert_eq!(decode_oneshot_dict(&f, &dict).unwrap(), 2015);
    assert_eq!(decode_stream_dict(&f, &dict).unwrap(), 2015);

    // 7000 > 4096 + 2009: corruption everywhere.
    let f = frame(10, 2, 1000, 7000, None);
    assert!(decode_oneshot_dict(&f, &dict).is_err());
    assert!(decode_stream_dict(&f, &dict).is_err());

    // 4000 bytes produced wraps the ring (cap 3136): the dict expired, so
    // streaming rejects while one-shot still holds the whole history.
    let f = frame(10, 4, 1000, 4500, None);
    assert_eq!(decode_oneshot_dict(&f, &dict).unwrap(), 4015);
    assert!(decode_stream_dict(&f, &dict).is_err());

    // Without the dictionary every offset above is plain corruption.
    let f = frame(10, 2, 1000, 4000, None);
    assert!(decode_oneshot(&f).is_err());
}

/// The ring executor's dict path (force_dict takeover) holds the
/// whole-history layout while nothing was drained: one-shot dict reach
/// extends across the full dict content, not just the window.
#[test]
fn ring_dict_reach_without_drains() {
    use crate::decoding::decode_buffer::DecodeBuffer;

    let dict = dict4096();
    let produce = |buf: &mut DecodeBuffer| {
        buf.dict_content = dict.clone();
        buf.push(b"0123456789");
        buf.repeat(10, 2000).unwrap();
    };

    // 2010 bytes buffered, nothing drained: an offset reaching into the
    // dict tail must produce the dict's actual bytes.
    let mut buf = DecodeBuffer::new(1024);
    produce(&mut buf);
    let produced = buf.len();
    let offset = 2500; // 2010 buffered + 490 from the dict tail
    buf.repeat(offset, 8).unwrap();
    let out = buf.drain();
    assert_eq!(out.len(), produced + 8);
    assert_eq!(
        &out[produced..],
        &dict[dict.len() - (offset - produced)..][..8]
    );

    // Once bytes were drained the virtual middle is gone: offsets past the
    // retained window are corruption (libzstd would have wrapped and
    // dropped the dict as well).
    let mut buf = DecodeBuffer::new(1024);
    produce(&mut buf);
    assert!(buf.drain_to_window_size().is_some());
    assert_eq!(buf.len(), 1024);
    assert!(buf.repeat(offset, 8).is_err());
}
